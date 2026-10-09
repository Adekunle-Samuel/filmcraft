//! Waveform silence detection (Assistant ▸ talking-head cleanup, `audio.detectSilence`).
//!
//! Works on a level envelope ([`crate::ducking::envelope_db`], or any dBFS-per-hop series) and
//! needs no speech model. The threshold is either given or derived from the recording itself:
//! between its noise floor (10th percentile of the hops above −100 dBFS) and its speech level
//! (95th percentile), 30 % of the way up and at least 6 dB above the floor, the same rule the
//! speech crate uses to tighten word bounds, but never closer than [`MAX_BELOW_SPEECH_DB`] to the
//! speech level. The cap matters when the gaps are digital silence (edited or gated audio): they
//! are left out of the floor, so the "floor" percentile then falls on the quiet parts of the speech
//! itself, and without the cap the threshold would land inside the words and break them up.
//!
//! A hop is *voiced* when its level is above the threshold. Voiced runs closer than `bridge_s`
//! are joined (the short dips inside and between words) and voiced blips shorter than
//! `min_voiced_s` (clicks) are dropped. Every gap of at least `min_silence_s` between voiced
//! regions (and before the first / after the last) is a silence; it is shrunk by `pad_s` on each
//! side that touches voice, so cuts leave a little air around the speech. Times are seconds from
//! the start of the envelope.
//!
//! Pure function on plain slices; hostile parameters (NaN, negative, huge) are clamped, never
//! panic.

/// Detection settings. Out-of-range values are clamped by [`detect`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceOptions {
    /// Voiced/silent threshold in dBFS; `None` derives it from the envelope ([`auto_threshold_db`]).
    pub threshold_db: Option<f32>,
    /// Shortest gap reported as a silence (seconds, 0.05…30).
    pub min_silence_s: f32,
    /// Air kept next to the voice on each side of a silence (seconds, 0…2).
    pub pad_s: f32,
    /// Voiced runs closer than this are one region (seconds, 0…1).
    pub bridge_s: f32,
    /// Voiced blips shorter than this are ignored (seconds, 0…1).
    pub min_voiced_s: f32,
}

impl Default for SilenceOptions {
    fn default() -> Self {
        Self { threshold_db: None, min_silence_s: 0.5, pad_s: 0.08, bridge_s: 0.12, min_voiced_s: 0.05 }
    }
}

/// What [`detect`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SilenceReport {
    /// The threshold used (dBFS).
    pub threshold_db: f32,
    /// Silences to remove `(start_s, end_s)`, sorted, non-overlapping, already padded.
    pub silences: Vec<(f64, f64)>,
    /// Voiced regions `(start_s, end_s)` (after bridging and blip removal), sorted.
    pub voiced: Vec<(f64, f64)>,
    /// Length of the analysed signal (seconds).
    pub duration_s: f64,
}

/// The automatic threshold stays at least this far below the speech level (dB).
pub const MAX_BELOW_SPEECH_DB: f32 = 24.0;

/// Most regions [`detect`] returns (a pathological envelope can't make a huge allocation).
pub const MAX_REGIONS: usize = 100_000;

fn clamp_f(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

/// The speech/silence threshold of an envelope in dBFS (see the module docs); −60 dBFS when the
/// envelope is empty or digital silence.
pub fn auto_threshold_db(env_db: &[f32]) -> f32 {
    let mut s: Vec<f32> = env_db.iter().copied().filter(|v| v.is_finite() && *v > -100.0).collect();
    if s.is_empty() {
        return -60.0;
    }
    s.sort_by(f32::total_cmp);
    let last = s.len() - 1;
    let q = |p: f32| s.get(((last as f32) * p) as usize).copied().unwrap_or(-60.0);
    let (floor, speech) = (q(0.10), q(0.95));
    (floor + 0.3 * (speech - floor)).max(floor + 6.0).min(speech - MAX_BELOW_SPEECH_DB)
}

/// Find the silences of a level envelope with one value per `hop_s` seconds.
pub fn detect(env_db: &[f32], hop_s: f32, opts: &SilenceOptions) -> SilenceReport {
    if !hop_s.is_finite() || hop_s <= 0.0 || env_db.is_empty() {
        return SilenceReport { threshold_db: opts.threshold_db.unwrap_or(-60.0), ..Default::default() };
    }
    // the f32 hop rounded to whole microseconds, so 0.01 means exactly 0.01 s
    let hop = (hop_s as f64 * 1e6).round() / 1e6;
    let duration = env_db.len() as f64 * hop;
    let threshold = match opts.threshold_db {
        Some(t) => clamp_f(t, -120.0, 0.0, -60.0),
        None => auto_threshold_db(env_db),
    };
    let min_silence = clamp_f(opts.min_silence_s, 0.05, 30.0, 0.5) as f64;
    let pad = clamp_f(opts.pad_s, 0.0, 2.0, 0.08) as f64;
    let bridge = clamp_f(opts.bridge_s, 0.0, 1.0, 0.12) as f64;
    let min_voiced = clamp_f(opts.min_voiced_s, 0.0, 1.0, 0.05) as f64;

    // raw voiced runs
    let mut raw: Vec<(f64, f64)> = Vec::new();
    let mut start: Option<usize> = None;
    for (k, &v) in env_db.iter().enumerate() {
        let on = v.is_finite() && v > threshold;
        match (on, start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                raw.push((s as f64 * hop, k as f64 * hop));
                start = None;
            }
            _ => {}
        }
        if raw.len() >= MAX_REGIONS {
            break;
        }
    }
    if let Some(s) = start {
        raw.push((s as f64 * hop, duration));
    }
    // bridge short dips, then drop blips
    let mut voiced: Vec<(f64, f64)> = Vec::new();
    for r in raw {
        match voiced.last_mut() {
            Some(last) if r.0 - last.1 < bridge - 1e-9 => last.1 = r.1,
            _ => voiced.push(r),
        }
    }
    voiced.retain(|(a, b)| b - a >= min_voiced - 1e-9);

    // gaps between voiced regions (and the head / tail) are silences
    let mut silences: Vec<(f64, f64)> = Vec::new();
    let mut cursor = 0.0f64;
    let mut prev_voiced = false;
    for &(a, b) in voiced.iter().chain(std::iter::once(&(duration, duration))) {
        let next_voiced = a < duration;
        if a - cursor >= min_silence - 1e-9 {
            let s = if prev_voiced { cursor + pad } else { cursor };
            let e = if next_voiced { a - pad } else { a };
            if e > s {
                silences.push((s, e));
            }
        }
        cursor = b;
        prev_voiced = true;
        if silences.len() >= MAX_REGIONS {
            break;
        }
    }
    SilenceReport { threshold_db: threshold, silences, voiced, duration_s: duration }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms hops: `(level_db, hops)` runs.
    fn env(runs: &[(f32, usize)]) -> Vec<f32> {
        runs.iter().flat_map(|&(v, n)| std::iter::repeat_n(v, n)).collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn finds_gaps_between_speech_and_pads_them() {
        // 1 s silence, 2 s speech, 1.5 s silence, 1 s speech, 0.2 s silence (too short)
        let e = env(&[(-80.0, 100), (-20.0, 200), (-80.0, 150), (-20.0, 100), (-80.0, 20)]);
        let r = detect(&e, 0.01, &SilenceOptions::default());
        assert_eq!(r.voiced.len(), 2);
        assert_eq!(r.silences.len(), 2, "{:?}", r.silences);
        // head silence: no pad at the start, pad before the voice
        assert!(close(r.silences[0].0, 0.0) && close(r.silences[0].1, 1.0 - 0.08));
        // middle: padded on both sides
        assert!(close(r.silences[1].0, 3.0 + 0.08) && close(r.silences[1].1, 4.5 - 0.08));
        assert!(close(r.duration_s, 5.7));
    }

    #[test]
    fn bridges_dips_inside_words_and_drops_clicks() {
        // speech with a 50 ms dip, then a 20 ms click inside a long silence
        let e = env(&[(-20.0, 100), (-80.0, 5), (-20.0, 100), (-80.0, 100), (-10.0, 2), (-80.0, 100)]);
        let r = detect(&e, 0.01, &SilenceOptions { threshold_db: Some(-50.0), ..Default::default() });
        assert_eq!(r.voiced.len(), 1, "{:?}", r.voiced);
        assert_eq!(r.silences.len(), 1);
        assert!(close(r.silences[0].0, 2.05 + 0.08) && close(r.silences[0].1, 4.07));
    }

    #[test]
    fn auto_threshold_sits_between_floor_and_speech() {
        let e = env(&[(-70.0, 300), (-20.0, 300)]);
        let t = auto_threshold_db(&e);
        assert!(t > -70.0 && t < -20.0, "{t}");
        assert_eq!(auto_threshold_db(&[]), -60.0);
        assert_eq!(auto_threshold_db(&[-120.0; 10]), -60.0);
    }

    #[test]
    fn digital_silence_gaps_do_not_break_up_the_words() {
        // speech whose level swings 0…−10 dB (syllables) between gaps of exact zeros: the floor
        // percentile only sees speech, the cap keeps the threshold well below it
        let mut e = Vec::new();
        for _ in 0..3 {
            e.extend(env(&[(-120.0, 80)]));
            for k in 0..120 {
                e.push(-10.0 * ((k % 20) as f32 / 19.0));
            }
        }
        let t = auto_threshold_db(&e);
        assert!(t <= -MAX_BELOW_SPEECH_DB + 1e-3 && t > -100.0, "{t}");
        let r = detect(&e, 0.01, &SilenceOptions::default());
        assert_eq!(r.voiced.len(), 3, "one region per word run: {:?}", r.voiced);
        assert_eq!(r.silences.len(), 3);
    }

    #[test]
    fn all_silence_and_all_speech() {
        let r = detect(&env(&[(-90.0, 300)]), 0.01, &SilenceOptions { threshold_db: Some(-50.0), ..Default::default() });
        assert_eq!(r.silences.len(), 1);
        assert!(close(r.silences[0].0, 0.0) && close(r.silences[0].1, 3.0));
        let r = detect(&env(&[(-10.0, 300)]), 0.01, &SilenceOptions { threshold_db: Some(-50.0), ..Default::default() });
        assert!(r.silences.is_empty());
    }

    #[test]
    fn hostile_parameters_never_panic() {
        let e = env(&[(-80.0, 100), (-20.0, 100), (-80.0, 100)]);
        for hop in [0.0, -1.0, f32::NAN, f32::INFINITY, 1e-9, 1e9] {
            for v in [f32::NAN, f32::INFINITY, -f32::INFINITY, -1e9, 1e9, 0.0] {
                let o = SilenceOptions { threshold_db: Some(v), min_silence_s: v, pad_s: v, bridge_s: v, min_voiced_s: v };
                let r = detect(&e, hop, &o);
                assert!(r.silences.iter().all(|(a, b)| a.is_finite() && b.is_finite() && b > a));
            }
        }
        let weird = [f32::NAN, f32::INFINITY, -f32::INFINITY, 0.0, -200.0];
        let _ = detect(&weird, 0.01, &SilenceOptions::default());
        let _ = auto_threshold_db(&weird);
    }
}
