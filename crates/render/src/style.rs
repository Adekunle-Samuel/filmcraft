//! Style analysis: the measurable "treatment" of a reference video (`media.analyze`).
//!
//! A [`StyleProfile`] describes pacing (shot lengths, cuts per minute), colour (per-shot Oklab
//! tonal statistics, luma percentiles, saturation, white-balance cast, and an overall summary),
//! loudness, speech rhythm (from a transcript) and format. This module holds the serde types and
//! the pure statistics; the engine decodes the media and runs the analysis as a job.
//!
//! Colour statistics are measured on display-referred SDR values: luma percentiles use Rec. 709
//! luma weights on the sRGB-encoded signal (0 = black, 1 = white), saturation is HSV saturation of
//! that signal, and the Oklab statistics are [`crate::color_match::stats`] (the ones Apply Match
//! solves on). The summary labels use fixed, documented thresholds (`docs/style-analysis.md`).

use filmcraft_project::transcript::{Word, normalize_word};
use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

use crate::Image;
use crate::color_match;

/// Version of the [`StyleProfile`] JSON shape.
pub const PROFILE_VERSION: u32 = 1;
/// Most representative keyframes kept in a profile (one per shot).
pub const MAX_KEYFRAMES: usize = 24;
/// Pauses at least this long (seconds) count towards [`SpeechProfile::pause_share`].
pub const LONG_PAUSE_SECONDS: f64 = 0.5;

/// The analysed treatment of one media item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StyleProfile {
    pub version: u32,
    /// The analysed project item (informational once saved to the style library).
    pub item: u64,
    pub name: String,
    /// Frames decoded for shot and colour analysis.
    pub analyzed_frames: usize,
    /// Media time between two analysed frames (cut times are accurate to this).
    pub sample_interval_seconds: f64,
    pub format: FormatProfile,
    pub shots: ShotProfile,
    /// Overall colour (None without picture).
    pub color: Option<ColorSummary>,
    /// One representative frame per shot (at most [`MAX_KEYFRAMES`], spread over the shots).
    pub keyframes: Vec<Keyframe>,
    /// Programme loudness (None without sound).
    pub audio: Option<AudioProfile>,
    /// Speech rhythm (None without a transcript).
    pub speech: Option<SpeechProfile>,
    /// What the analysis could not do (no picture, decode errors, …).
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FormatProfile {
    pub width: u32,
    pub height: u32,
    /// Display aspect ratio: a common name ("16:9", "9:16", "4:5", "2.39:1"…) or the reduced ratio.
    pub aspect: String,
    pub fps: f64,
    pub duration_seconds: f64,
    pub has_video: bool,
    pub has_audio: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShotProfile {
    pub count: usize,
    pub median_seconds: f64,
    pub p10_seconds: f64,
    pub p90_seconds: f64,
    pub mean_seconds: f64,
    pub cuts_per_minute: f64,
    /// Cut times in seconds from the start of the analysed range.
    pub cut_times: Vec<f64>,
    /// The same cuts as media times in ticks (exact; for edits).
    pub cut_ticks: Vec<i64>,
}

/// Oklab statistics of one tonal range (see [`crate::color_match::stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ToneBand {
    /// Share of the frame in this range (0..1).
    pub share: f32,
    /// Mean Oklab L, a, b.
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Tones {
    pub shadows: ToneBand,
    pub midtones: ToneBand,
    pub highlights: ToneBand,
}

/// Colour statistics of one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FrameColor {
    /// Rec. 709 luma of the display signal (0..1): 5th, 50th and 95th percentiles.
    pub luma_p5: f32,
    pub luma_p50: f32,
    pub luma_p95: f32,
    /// Mean HSV saturation of the display signal (0..1).
    pub saturation: f32,
    /// Mean Oklab chroma.
    pub chroma: f32,
    /// White-balance cast: mean Oklab (a, b) of the midtones (+a red / −a green, +b yellow / −b blue).
    pub cast: [f32; 2],
    pub tones: Tones,
}

/// A representative frame of a shot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Keyframe {
    pub shot: usize,
    /// Media time (ticks).
    pub time: i64,
    /// Seconds from the start of the analysed range.
    pub seconds: f64,
    pub color: Option<FrameColor>,
}

/// The overall look (shot keyframes weighted by shot length).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorSummary {
    pub luma_p5: f32,
    pub luma_p50: f32,
    pub luma_p95: f32,
    /// `lumaP95 − lumaP5`.
    pub contrast: f32,
    /// "low" (< 0.5) | "medium" | "high" (> 0.8).
    pub contrast_label: String,
    pub saturation: f32,
    pub chroma: f32,
    /// "muted" (chroma < 0.03) | "natural" | "vivid" (> 0.09).
    pub saturation_label: String,
    /// Midtone Oklab b: + warm (yellow), − cool (blue).
    pub warmth: f32,
    /// Midtone Oklab a: + magenta/red, − green.
    pub tint: f32,
    /// "warm" (warmth > 0.012) | "neutral" | "cool" (< −0.012).
    pub temperature_label: String,
    /// Blacks above 0.1 (a matte / faded look).
    pub lifted_blacks: bool,
    /// Blacks at or under 0.02.
    pub crushed_blacks: bool,
    /// Whites held under 0.8 (rolled-off highlights).
    pub crushed_whites: bool,
    /// Whites at or over 0.98.
    pub clipped_whites: bool,
    pub tones: Tones,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioProfile {
    /// EBU R128 / BS.1770 integrated loudness (None when silent).
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub sample_rate: u32,
    pub channels: u32,
    pub analyzed_seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpeechProfile {
    pub words: usize,
    /// Words per minute of the analysed duration.
    pub words_per_minute: f64,
    /// Median gap between consecutive words (seconds).
    pub median_pause_seconds: f64,
    /// Share of the analysed duration (0..1) spent in gaps of at least 0.5 s.
    pub pause_share: f64,
    /// Gaps of at least 0.5 s.
    pub long_pauses: usize,
    pub fillers: usize,
    pub fillers_per_minute: f64,
}

#[inline]
fn fin(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

#[inline]
fn fin64(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// Linear-interpolated percentile `p` (0..1) of an ascending slice (0 when empty).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else { return 0.0 };
    let p = if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.5 };
    let x = p * last as f64;
    let i = (x.floor() as usize).min(last);
    let j = (i + 1).min(last);
    let (a, b) = (sorted.get(i).copied().unwrap_or(0.0), sorted.get(j).copied().unwrap_or(0.0));
    a + (b - a) * (x - i as f64)
}

/// Colour statistics of a frame (premultiplied linear Rec. 709, as the renderer makes it).
/// `None` when no pixel is opaque.
pub fn frame_color(img: &Image) -> Option<FrameColor> {
    if img.w == 0 || img.h == 0 || img.px.len() < img.w.saturating_mul(img.h).saturating_mul(4) {
        return None;
    }
    let img = color_match::shrink(img, 256);
    const BINS: usize = 1024;
    let mut hist = vec![0u32; BINS];
    let mut n = 0u64;
    let mut sat = 0f64;
    for p in img.px.as_chunks::<4>().0 {
        // NaN alpha fails the test too
        if p[3].is_nan() || p[3] <= 1e-3 {
            continue;
        }
        let e = [p[0], p[1], p[2]].map(|v| {
            let v = fin(v / p[3]).clamp(0.0, 1.0);
            filmcraft_color::linear_to_srgb(v)
        });
        let y = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
        let bin = ((y * (BINS - 1) as f32).round().max(0.0) as usize).min(BINS - 1);
        if let Some(h) = hist.get_mut(bin) {
            *h = h.saturating_add(1);
        }
        let (mx, mn) = (e[0].max(e[1]).max(e[2]), e[0].min(e[1]).min(e[2]));
        if mx > 1e-4 {
            sat += ((mx - mn) / mx) as f64;
        }
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let pct = |q: f64| -> f32 {
        let target = (q * n as f64).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (i, c) in hist.iter().enumerate() {
            acc += *c as u64;
            if acc >= target {
                return i as f32 / (BINS - 1) as f32;
            }
        }
        1.0
    };
    let st = color_match::stats(&img, false);
    let band = |k: usize| {
        let b = st.bands.get(k).copied().unwrap_or_default();
        ToneBand { share: fin(b[0]), l: fin(b[1]), a: fin(b[2]), b: fin(b[3]) }
    };
    let tones = Tones { shadows: band(0), midtones: band(1), highlights: band(2) };
    Some(FrameColor {
        luma_p5: pct(0.05),
        luma_p50: pct(0.5),
        luma_p95: pct(0.95),
        saturation: fin((sat / n as f64) as f32),
        chroma: fin(st.chroma),
        cast: [tones.midtones.a, tones.midtones.b],
        tones,
    })
}

/// Shot-length statistics from the cut times (`cuts`: media times of each new shot's first
/// analysed frame) in the analysed `range`.
pub fn shot_profile(cuts: &[Tick], range: TimeRange) -> ShotProfile {
    let dur = range.duration.max(Tick::ZERO);
    let mut inner: Vec<Tick> = cuts.iter().copied().filter(|t| *t > range.start && *t < range.end()).collect();
    inner.sort();
    inner.dedup();
    let mut bounds = Vec::with_capacity(inner.len() + 2);
    bounds.push(range.start);
    bounds.extend(inner.iter().copied());
    bounds.push(range.end());
    let mut lengths: Vec<f64> = bounds.windows(2).filter_map(|w| Some((*w.get(1)? - *w.first()?).seconds())).filter(|l| *l > 0.0).collect();
    lengths.sort_by(f64::total_cmp);
    let total = dur.seconds();
    let minutes = total / 60.0;
    ShotProfile {
        count: lengths.len().max(1),
        median_seconds: fin64(percentile(&lengths, 0.5)),
        p10_seconds: fin64(percentile(&lengths, 0.1)),
        p90_seconds: fin64(percentile(&lengths, 0.9)),
        mean_seconds: if lengths.is_empty() { 0.0 } else { fin64(lengths.iter().sum::<f64>() / lengths.len() as f64) },
        cuts_per_minute: if minutes > 1e-9 { fin64(inner.len() as f64 / minutes) } else { 0.0 },
        cut_times: inner.iter().map(|t| fin64((*t - range.start).seconds())).collect(),
        cut_ticks: inner.iter().map(|t| t.0).collect(),
    }
}

/// The overall look from `(keyframe colour, weight)` pairs (weights: shot lengths). `None`
/// without frames.
pub fn color_summary(frames: &[(FrameColor, f64)]) -> Option<ColorSummary> {
    let total: f64 = frames.iter().map(|(_, w)| if w.is_finite() { w.max(0.0) } else { 0.0 }).sum();
    if frames.is_empty() {
        return None;
    }
    // all weights zero: plain mean
    let weight = |w: f64| if total > 1e-12 && w.is_finite() { w.max(0.0) / total } else { 1.0 / frames.len() as f64 };
    let mean = |f: &dyn Fn(&FrameColor) -> f32| -> f32 { fin(frames.iter().map(|(c, w)| f(c) as f64 * weight(*w)).sum::<f64>() as f32) };
    let band = |pick: &dyn Fn(&Tones) -> ToneBand| ToneBand {
        share: mean(&|c| pick(&c.tones).share),
        l: mean(&|c| pick(&c.tones).l),
        a: mean(&|c| pick(&c.tones).a),
        b: mean(&|c| pick(&c.tones).b),
    };
    let tones = Tones { shadows: band(&|t| t.shadows), midtones: band(&|t| t.midtones), highlights: band(&|t| t.highlights) };
    let (p5, p50, p95) = (mean(&|c| c.luma_p5), mean(&|c| c.luma_p50), mean(&|c| c.luma_p95));
    let contrast = (p95 - p5).max(0.0);
    let chroma = mean(&|c| c.chroma);
    let (warmth, tint) = (mean(&|c| c.cast[1]), mean(&|c| c.cast[0]));
    Some(ColorSummary {
        luma_p5: p5,
        luma_p50: p50,
        luma_p95: p95,
        contrast,
        contrast_label: if contrast < 0.5 {
            "low"
        } else if contrast > 0.8 {
            "high"
        } else {
            "medium"
        }
        .into(),
        saturation: mean(&|c| c.saturation),
        chroma,
        saturation_label: if chroma < 0.03 {
            "muted"
        } else if chroma > 0.09 {
            "vivid"
        } else {
            "natural"
        }
        .into(),
        warmth,
        tint,
        temperature_label: if warmth > 0.012 {
            "warm"
        } else if warmth < -0.012 {
            "cool"
        } else {
            "neutral"
        }
        .into(),
        lifted_blacks: p5 > 0.1,
        crushed_blacks: p5 <= 0.02,
        crushed_whites: p95 < 0.8,
        clipped_whites: p95 >= 0.98,
        tones,
    })
}

/// Indices of at most `max` items spread evenly over `0..n` (all of them when `n <= max`).
pub fn spread(n: usize, max: usize) -> Vec<usize> {
    if n <= max {
        return (0..n).collect();
    }
    if max == 0 {
        return Vec::new();
    }
    if max == 1 {
        return vec![n / 2];
    }
    let mut v: Vec<usize> = (0..max).map(|j| ((j as u128 * (n - 1) as u128 + (max - 1) as u128 / 2) / (max - 1) as u128) as usize).collect();
    v.dedup();
    v
}

/// Speech rhythm of the transcript words inside `range` (media time). `fillers` are compared to
/// each word with case and punctuation ignored (single words).
pub fn speech_profile(words: &[Word], range: TimeRange, fillers: &[&str]) -> Option<SpeechProfile> {
    let inside: Vec<&Word> = words.iter().filter(|w| w.end > w.start && w.start >= range.start && w.end <= range.end()).collect();
    if inside.is_empty() {
        return None;
    }
    let minutes = range.duration.seconds() / 60.0;
    let per_min = |n: usize| if minutes > 1e-9 { fin64(n as f64 / minutes) } else { 0.0 };
    let mut gaps: Vec<f64> = inside.windows(2).filter_map(|w| Some((w.get(1)?.start - w.first()?.end).seconds())).filter(|g| *g > 0.0).collect();
    gaps.sort_by(f64::total_cmp);
    let long: Vec<f64> = gaps.iter().copied().filter(|g| *g >= LONG_PAUSE_SECONDS).collect();
    let total = range.duration.seconds();
    let norm: Vec<String> = fillers.iter().map(|f| normalize_word(f)).filter(|f| !f.is_empty()).collect();
    let n_fill = inside.iter().filter(|w| norm.contains(&normalize_word(&w.text))).count();
    Some(SpeechProfile {
        words: inside.len(),
        words_per_minute: per_min(inside.len()),
        median_pause_seconds: fin64(percentile(&gaps, 0.5)),
        pause_share: if total > 1e-9 { fin64((long.iter().sum::<f64>() / total).clamp(0.0, 1.0)) } else { 0.0 },
        long_pauses: long.len(),
        fillers: n_fill,
        fillers_per_minute: per_min(n_fill),
    })
}

/// Display aspect of `w`×`h` pixels with pixel aspect `par` (num, den).
pub fn aspect_label(w: u32, h: u32, par: (u32, u32)) -> String {
    if w == 0 || h == 0 {
        return "unknown".into();
    }
    let (pn, pd) = if par.0 == 0 || par.1 == 0 { (1u64, 1u64) } else { (par.0 as u64, par.1 as u64) };
    let (dw, dh) = (w as u64 * pn, h as u64 * pd);
    let r = dw as f64 / dh as f64;
    const COMMON: [(&str, f64); 13] = [
        ("16:9", 16.0 / 9.0),
        ("9:16", 9.0 / 16.0),
        ("4:3", 4.0 / 3.0),
        ("3:4", 3.0 / 4.0),
        ("1:1", 1.0),
        ("4:5", 0.8),
        ("5:4", 1.25),
        ("3:2", 1.5),
        ("2:3", 2.0 / 3.0),
        ("2:1", 2.0),
        ("1.85:1", 1.85),
        ("2.39:1", 2.39),
        ("21:9", 64.0 / 27.0),
    ];
    let best = COMMON.iter().min_by(|a, b| ((a.1 - r).abs() / a.1).total_cmp(&((b.1 - r).abs() / b.1)));
    if let Some((name, v)) = best
        && (v - r).abs() / v < 0.015
    {
        return (*name).into();
    }
    let g = gcd(dw, dh).max(1);
    let (a, b) = (dw / g, dh / g);
    if a <= 64 && b <= 64 { format!("{a}:{b}") } else { format!("{r:.2}:1") }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_time::TICKS_PER_SECOND;

    fn secs(s: f64) -> Tick {
        Tick::from_seconds_f64(s)
    }

    #[test]
    fn shot_stats_from_cuts() {
        let range = TimeRange::new(Tick::ZERO, secs(60.0));
        // shots of 10, 20, 30 s
        let p = shot_profile(&[secs(10.0), secs(30.0)], range);
        assert_eq!(p.count, 3);
        assert!((p.median_seconds - 20.0).abs() < 1e-6, "{p:?}");
        assert!((p.cuts_per_minute - 2.0).abs() < 1e-6);
        assert_eq!(p.cut_times.len(), 2);
        assert!((p.cut_times[1] - 30.0).abs() < 1e-6);
        // no cuts: one shot; cuts outside the range are ignored; empty range is sane
        let p = shot_profile(&[secs(-1.0), secs(99.0)], range);
        assert_eq!((p.count, p.cut_times.len()), (1, 0));
        assert!((p.median_seconds - 60.0).abs() < 1e-6);
        let p = shot_profile(&[], TimeRange::new(Tick::ZERO, Tick::ZERO));
        assert_eq!(p.count, 1);
        assert_eq!(p.cuts_per_minute, 0.0);
        let p = shot_profile(&[Tick(i64::MAX / 8)], TimeRange::new(Tick::MIN, Tick(TICKS_PER_SECOND)));
        assert!(p.median_seconds.is_finite());
    }

    #[test]
    fn percentiles_and_spread() {
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 0.5), 2.0);
        assert_eq!(percentile(&[1.0, 3.0], f64::NAN), 2.0);
        assert_eq!(spread(5, 24), vec![0, 1, 2, 3, 4]);
        let s = spread(100, 24);
        assert_eq!(s.len(), 24);
        assert_eq!((s[0], s[23]), (0, 99));
        assert_eq!(spread(10, 1), vec![5]);
        assert!(spread(10, 0).is_empty());
    }

    #[test]
    fn frame_colour_of_a_grey_ramp_and_a_warm_frame() {
        let mut img = Image::new(100, 10);
        for x in 0..100 {
            for y in 0..10 {
                let v = filmcraft_color::srgb_to_linear(x as f32 / 99.0);
                let i = (y * 100 + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }
        let c = frame_color(&img).unwrap();
        assert!((c.luma_p50 - 0.5).abs() < 0.03, "{c:?}");
        assert!(c.luma_p5 < 0.08 && c.luma_p95 > 0.92, "{c:?}");
        assert!(c.saturation < 1e-3 && c.chroma < 1e-3, "{c:?}");
        let warm = Image::filled(32, 18, [0.5, 0.3, 0.1, 1.0]);
        let w = frame_color(&warm).unwrap();
        assert!(w.cast[1] > 0.03 && w.saturation > 0.3, "{w:?}");
        let s = color_summary(&[(c, 1.0), (w, 3.0)]).unwrap();
        assert_eq!(s.temperature_label, "warm");
        // transparent / empty / NaN frames don't count or crash
        assert!(frame_color(&Image::new(8, 8)).is_none());
        assert!(frame_color(&Image { w: 4, h: 4, px: vec![] }).is_none());
        let nan = frame_color(&Image::filled(8, 8, [f32::NAN, 0.2, f32::INFINITY, 1.0])).unwrap();
        assert!(nan.luma_p50.is_finite() && nan.chroma.is_finite());
        assert!(color_summary(&[]).is_none());
        assert!(color_summary(&[(c, f64::NAN), (w, 0.0)]).is_some_and(|s| s.contrast.is_finite()));
    }

    #[test]
    fn speech_rates_pauses_and_fillers() {
        let word = |t: &str, a: f64, b: f64| Word { text: t.into(), start: secs(a), end: secs(b), speaker: None, confidence: 1.0 };
        let words = vec![word("So,", 0.0, 0.4), word("um", 0.5, 0.8), word("this", 2.0, 2.3), word("works.", 2.4, 3.0), word("late", 70.0, 71.0)];
        let p = speech_profile(&words, TimeRange::new(Tick::ZERO, secs(30.0)), &["um", "uh"]).unwrap();
        assert_eq!(p.words, 4);
        assert!((p.words_per_minute - 8.0).abs() < 1e-6, "{p:?}");
        assert_eq!((p.fillers, p.long_pauses), (1, 1));
        assert!((p.pause_share - 1.2 / 30.0).abs() < 1e-6, "{p:?}");
        assert!((p.median_pause_seconds - 0.1).abs() < 1e-6, "{p:?}");
        assert!(speech_profile(&[], TimeRange::new(Tick::ZERO, secs(1.0)), &[]).is_none());
        assert!(speech_profile(&words, TimeRange::new(Tick::ZERO, Tick::ZERO), &[]).is_none());
    }

    #[test]
    fn aspect_names() {
        assert_eq!(aspect_label(1920, 1080, (1, 1)), "16:9");
        assert_eq!(aspect_label(1080, 1920, (1, 1)), "9:16");
        assert_eq!(aspect_label(1080, 1350, (1, 1)), "4:5");
        assert_eq!(aspect_label(720, 480, (32, 27)), "16:9");
        assert_eq!(aspect_label(1000, 1000, (0, 0)), "1:1");
        assert_eq!(aspect_label(4096, 1716, (1, 1)), "2.39:1");
        assert_eq!(aspect_label(1234, 567, (1, 1)), "2.18:1");
        assert_eq!(aspect_label(0, 10, (1, 1)), "unknown");
        assert_eq!(aspect_label(u32::MAX, 1, (u32::MAX, 1)), format!("{:.2}:1", u32::MAX as f64 * u32::MAX as f64));
    }

    #[test]
    fn profile_json_is_camel_case_and_round_trips() {
        let p = StyleProfile { version: PROFILE_VERSION, analyzed_frames: 3, ..Default::default() };
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("analyzedFrames").is_some() && v.get("sampleIntervalSeconds").is_some(), "{v}");
        assert!(v["shots"].get("cutsPerMinute").is_some());
        let back: StyleProfile = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }
}
