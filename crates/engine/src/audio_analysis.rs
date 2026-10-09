//! Audio analysis of the active sequence's mix (`audio.*`): waveform silence detection and
//! removal, and a loudness report. Used by the Assistant's talking-head cleanup, and plain
//! commands for everyone else.
//!
//! | command | does |
//! |---|---|
//! | `audio.detectSilence` | silences of the sequence mix (level envelope, adaptive threshold), never inside a transcript word |
//! | `audio.removeSilence` | the same ranges, ripple-deleted on every unlocked track in one undo step |
//! | `audio.loudness` | integrated loudness, loudness range, max short-term / momentary, sample and true peak (EBU R128) |
//!
//! Detection reads the mixed programme ([`filmcraft_render::audio::mix_sequence`]), so muted
//! tracks, clip gain and effects count the way they sound. The envelope is 10 ms hops of a 50 ms
//! window; [`filmcraft_audio_dsp::silence::detect`] does the rest. Silence ranges are snapped
//! inward to frames (a cut never eats into a neighbouring voiced frame), and when the sequence
//! has a transcript every word (± `padSeconds`) is cut out of them, so a quiet word is never
//! removed.

use serde_json::{Value, json};

use filmcraft_audio_dsp::silence::{self, SilenceOptions};
use filmcraft_project::Sequence;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, bad, bool_p, has_seq};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// Analysis hop and smoothing window.
const HOP_S: f64 = 0.01;
const WINDOW_HOPS: usize = 5;
/// Longest span analysed in one call (pass `startSeconds` / `endSeconds` for longer sequences).
pub const MAX_ANALYSIS_S: f64 = 4.0 * 3600.0;
/// Most silence ranges reported (the rest are dropped with `truncated: true`).
const MAX_REPORTED: usize = 20_000;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

/// A finite, non-negative seconds parameter (`None` when absent).
fn secs_p(p: &Value, k: &str, cmd: &str) -> Result<Option<f64>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && x >= 0.0 => Ok(Some(x)),
            _ => Err(bad(cmd, format!("`{k}` must be a non-negative number of seconds"))),
        },
    }
}

/// The analysed span `[a, b)` of the sequence in sample units.
fn span(seq: &Sequence, p: &Value, cmd: &str) -> Result<(i64, i64, u32)> {
    let sr = seq.settings.sample_rate.max(1);
    let dur = seq.duration().seconds();
    let a = secs_p(p, "startSeconds", cmd)?.unwrap_or(0.0).min(dur);
    let b = secs_p(p, "endSeconds", cmd)?.unwrap_or(dur).min(dur);
    if b <= a {
        return Err(bad(cmd, "nothing to analyse: the sequence (or the startSeconds..endSeconds span) is empty"));
    }
    if b - a > MAX_ANALYSIS_S {
        return Err(bad(cmd, format!("the span is longer than {} h; analyse it in parts with startSeconds / endSeconds", MAX_ANALYSIS_S / 3600.0)));
    }
    let a0 = Tick::from_seconds_f64(a).to_units_floor(sr as i64);
    let a1 = Tick::from_seconds_f64(b).to_units_floor(sr as i64);
    Ok((a0, a1, sr))
}

/// Stream the sequence mix over samples `[a0, a1)` in one-second chunks.
fn for_each_mix_chunk(s: &Session, seq: &Sequence, a0: i64, a1: i64, sr: u32, mut f: impl FnMut(&[f32], &[f32])) {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let chunk = sr as i64;
    let mut pos = a0;
    while pos < a1 {
        let n = (a1 - pos).min(chunk).max(0) as usize;
        if n == 0 {
            break;
        }
        let b = filmcraft_render::audio::mix_sequence(&s.project, seq, pos, n, &provider);
        let l = b.channels.first().map(Vec::as_slice).unwrap_or(&[]);
        let r = b.channels.get(1).map(Vec::as_slice).unwrap_or(l);
        f(l, r);
        pos += n as i64;
    }
}

/// Level envelope (dBFS per 10 ms hop, 50 ms centred window) of the sequence mix over `[a0, a1)`.
fn mix_envelope(s: &Session, seq: &Sequence, a0: i64, a1: i64, sr: u32) -> Vec<f32> {
    let hop = ((sr as f64 * HOP_S).round() as usize).max(1);
    let hops = ((a1 - a0).max(0) as usize).div_ceil(hop);
    let mut power: Vec<f64> = Vec::with_capacity(hops);
    // carry a partial hop across chunk boundaries
    let (mut acc, mut cnt) = (0.0f64, 0usize);
    for_each_mix_chunk(s, seq, a0, a1, sr, |l, r| {
        for (x, y) in l.iter().zip(r) {
            acc += 0.5 * ((*x as f64).powi(2) + (*y as f64).powi(2));
            cnt += 1;
            if cnt == hop {
                power.push(acc / hop as f64);
                acc = 0.0;
                cnt = 0;
            }
        }
    });
    if cnt > 0 {
        power.push(acc / cnt as f64);
    }
    let h = WINDOW_HOPS / 2;
    let n = power.len();
    (0..n)
        .map(|k| {
            let (lo, hi) = (k.saturating_sub(h), (k + h + 1).min(n));
            let win = power.get(lo..hi).unwrap_or(&[]);
            let m = win.iter().sum::<f64>() / win.len().max(1) as f64;
            (10.0 * m.max(1e-12).log10()).max(-120.0) as f32
        })
        .collect()
}

/// `ranges` minus `holes` (both sorted by start; holes may overlap).
fn subtract(ranges: Vec<TimeRange>, holes: &[TimeRange]) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for r in ranges {
        let mut start = r.start;
        let end = r.end();
        for h in holes.iter().filter(|h| h.end() > r.start && h.start < end) {
            if h.start > start {
                out.push(TimeRange::from_bounds(start, h.start));
            }
            start = start.max(h.end());
            if start >= end {
                break;
            }
        }
        if end > start {
            out.push(TimeRange::from_bounds(start, end));
        }
    }
    out
}

/// Snap a removal inward to whole frames; `None` when less than a frame is left.
fn snap_inward(r: TimeRange, rate: FrameRate) -> Option<TimeRange> {
    let mut a = rate.snap(r.start);
    if a < r.start {
        a += rate.frame_duration();
    }
    let b = rate.snap(r.end());
    (b > a).then(|| TimeRange::from_bounds(a, b))
}

/// What `audio.detectSilence` found, in sequence time.
pub struct Detection {
    pub threshold_db: f32,
    pub ranges: Vec<TimeRange>,
    /// Voiced regions in sequence seconds.
    pub voiced: Vec<(f64, f64)>,
    pub analysed_s: f64,
    /// Silences dropped because a transcript word was inside them.
    pub word_guarded: usize,
}

/// Find the silences of the active sequence (see the module docs).
pub fn detect(s: &Session, p: &Value, cmd: &str) -> Result<Detection> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (a0, a1, sr) = span(seq, p, cmd)?;
    let opts = SilenceOptions {
        threshold_db: match p.get("thresholdDb") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_f64() {
                Some(t) if t.is_finite() && (-120.0..=0.0).contains(&t) => Some(t as f32),
                _ => return Err(bad(cmd, "`thresholdDb` must be between -120 and 0 dBFS")),
            },
        },
        min_silence_s: secs_p(p, "minSeconds", cmd)?.unwrap_or(0.5).clamp(0.05, 30.0) as f32,
        pad_s: secs_p(p, "padSeconds", cmd)?.unwrap_or(0.08).min(2.0) as f32,
        ..SilenceOptions::default()
    };
    let env = mix_envelope(s, seq, a0, a1, sr);
    let rep = silence::detect(&env, HOP_S as f32, &opts);
    let offset = a0 as f64 / sr as f64;
    let rate = seq.settings.frame_rate;
    let raw: Vec<TimeRange> = rep
        .silences
        .iter()
        .filter_map(|(x, y)| snap_inward(TimeRange::from_bounds(Tick::from_seconds_f64(offset + x), Tick::from_seconds_f64(offset + y)), rate))
        .collect();
    let respect = bool_p(p, "respectTranscript").unwrap_or(true);
    let (ranges, guarded) = if respect {
        let pad = Tick::from_seconds_f64(opts.pad_s as f64);
        let mut words: Vec<TimeRange> =
            crate::transcript::sequence_words(s).iter().map(|w| TimeRange::from_bounds(w.start - pad, w.end.max(w.start) + pad)).collect();
        words.sort_by_key(|r| r.start);
        let before = raw.len();
        let kept = subtract(raw, &words);
        let min = Tick::from_seconds_f64(opts.min_silence_s as f64 * 0.5);
        let kept: Vec<TimeRange> = kept.into_iter().filter(|r| r.duration >= min).filter_map(|r| snap_inward(r, rate)).collect();
        let guarded = before.saturating_sub(kept.len());
        (kept, guarded)
    } else {
        (raw, 0)
    };
    Ok(Detection {
        threshold_db: rep.threshold_db,
        ranges,
        voiced: rep.voiced.iter().map(|(x, y)| (offset + x, offset + y)).collect(),
        analysed_s: rep.duration_s,
        word_guarded: guarded,
    })
}

fn detection_json(d: &Detection) -> Value {
    let total: f64 = d.ranges.iter().map(|r| r.duration.seconds()).sum();
    let truncated = d.ranges.len() > MAX_REPORTED || d.voiced.len() > MAX_REPORTED;
    json!({
        "thresholdDb": d.threshold_db,
        "analysedSeconds": d.analysed_s,
        "silenceSeconds": total,
        "count": d.ranges.len(),
        "silences": d.ranges.iter().take(MAX_REPORTED).map(|r| json!({
            "start": r.start.0, "end": r.end().0,
            "startSeconds": r.start.seconds(), "endSeconds": r.end().seconds(),
        })).collect::<Vec<_>>(),
        "voiced": d.voiced.iter().take(MAX_REPORTED).map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        "wordGuarded": d.word_guarded,
        "truncated": truncated,
    })
}

fn detect_silence(s: &mut Session, p: &Value) -> Result<Value> {
    let d = detect(s, p, "audio.detectSilence")?;
    Ok(detection_json(&d))
}

fn remove_silence(s: &mut Session, p: &Value) -> Result<Value> {
    let d = detect(s, p, "audio.removeSilence")?;
    let n = d.ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "seconds": 0.0, "thresholdDb": d.threshold_db}));
    }
    let ranges = d.ranges.clone();
    let total = s.edit_sequence("Remove Silence", |q, ctx, _| Ok(filmcraft_edit::transcript::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({
        "removed": n,
        "ticks": total.0,
        "seconds": total.0 as f64 / TICKS_PER_SECOND as f64,
        "thresholdDb": d.threshold_db,
        "wordGuarded": d.word_guarded,
    }))
}

fn finite_or_null(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

fn loudness(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.loudness";
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (a0, a1, sr) = span(seq, p, cmd)?;
    let mut meter = filmcraft_audio_dsp::LoudnessMeter::new(sr as f64, 2);
    for_each_mix_chunk(s, seq, a0, a1, sr, |l, r| meter.process(&[l, r]));
    let m = meter.summary();
    Ok(json!({
        "integratedLufs": finite_or_null(m.integrated_lufs),
        "loudnessRangeLu": finite_or_null(m.loudness_range_lu),
        "maxMomentaryLufs": finite_or_null(m.max_momentary_lufs),
        "maxShortTermLufs": finite_or_null(m.max_short_term_lufs),
        "samplePeakDbfs": finite_or_null(m.sample_peak_dbfs),
        "truePeakDbtp": finite_or_null(m.true_peak_dbtp),
        "analysedSeconds": (a1 - a0).max(0) as f64 / sr as f64,
    }))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "audio.detectSilence",
            "Detect Silence",
            &[],
            r#"{"thresholdDb":dbfs|null(auto)?,"minSeconds":f=0.5?,"padSeconds":f=0.08?,"startSeconds":f?,"endSeconds":f?,"respectTranscript":bool=true?}"#,
            has_seq,
            detect_silence,
            false,
        ),
        spec(
            "audio.removeSilence",
            "Remove Silence",
            &["Sequence", "Transcript"],
            r#"{"thresholdDb":dbfs|null(auto)?,"minSeconds":f=0.5?,"padSeconds":f=0.08?,"startSeconds":f?,"endSeconds":f?,"respectTranscript":bool=true?}"#,
            has_seq,
            remove_silence,
            true,
        ),
        spec("audio.loudness", "Measure Loudness", &[], r#"{"startSeconds":f?,"endSeconds":f?}"#, has_seq, loudness, false),
    ]
}
