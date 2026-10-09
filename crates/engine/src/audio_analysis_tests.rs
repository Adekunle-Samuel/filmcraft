//! `audio.detectSilence` / `audio.removeSilence` / `audio.loudness` on synthetic speech-like audio.

use std::sync::Arc;

use filmcraft_project::{ClipId, Transcript, Word};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::*;

const SR: u32 = 48_000;

/// A voiced buzz (harmonics of 140 Hz under a syllable envelope) where `active(t)`, else zero.
fn speechy(secs: f64, active: impl Fn(f64) -> bool) -> Vec<f32> {
    let n = (secs * SR as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            if !active(t) {
                return 0.0;
            }
            let mut v = 0.0;
            for k in 1..20 {
                v += (2.0 * std::f64::consts::PI * 140.0 * k as f64 * t).sin() / k as f64;
            }
            let syl = 0.6 - 0.4 * (2.0 * std::f64::consts::PI * 4.0 * t).cos();
            (v * syl * 0.2) as f32
        })
        .collect()
}

/// Speech 0–2 s, silence 2–3.5 s, speech 3.5–5.5 s, silence 5.5–6.5 s, speech 6.5–7 s.
fn talk() -> Vec<f32> {
    speechy(7.0, |t| t < 2.0 || (3.5..5.5).contains(&t) || t >= 6.5)
}

fn session_with(x: &[f32]) -> (Session, ClipId, filmcraft_project::ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "talk", "audio": 2, "video": 1, "fps": 25})).unwrap();
    let inter: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/talk.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let clip = ClipId(r["clips"][0].as_u64().unwrap());
    s.execute("edit.deselectAll", json!({})).unwrap();
    (s, clip, item)
}

fn silences(r: &Value) -> Vec<(f64, f64)> {
    r["silences"].as_array().unwrap().iter().map(|x| (x["startSeconds"].as_f64().unwrap(), x["endSeconds"].as_f64().unwrap())).collect()
}

#[test]
fn detects_the_gaps_between_speech() {
    let (mut s, _, _) = session_with(&talk());
    let n0 = s.history.undo.len();
    let r = s.execute("audio.detectSilence", json!({})).unwrap();
    let sil = silences(&r);
    assert_eq!(sil.len(), 2, "{r}");
    // padded by 80 ms and snapped inward to 25 fps frames (40 ms)
    assert!((sil[0].0 - 2.08).abs() < 0.05 && (sil[0].1 - 3.42).abs() < 0.05, "{sil:?}");
    assert!((sil[1].0 - 5.58).abs() < 0.05 && (sil[1].1 - 6.42).abs() < 0.05, "{sil:?}");
    assert!(r["voiced"].as_array().unwrap().len() >= 3);
    assert!(r["thresholdDb"].as_f64().unwrap() < 0.0);
    // detection is read-only
    assert_eq!(s.history.undo.len(), n0);
}

#[test]
fn remove_is_one_undo_step_and_shortens_the_sequence() {
    let (mut s, _, _) = session_with(&talk());
    let before = s.active_sequence().unwrap().duration();
    let n0 = s.history.undo.len();
    let r = s.execute("audio.removeSilence", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), n0 + 1, "one undo step");
    assert_eq!(r["removed"], 2, "{r}");
    let after = s.active_sequence().unwrap().duration();
    let cut = (before - after).seconds();
    assert!((cut - r["seconds"].as_f64().unwrap()).abs() < 1e-6);
    assert!(cut > 2.0 && cut < 2.5, "{cut}");
    // nothing left to remove
    let again = s.execute("audio.detectSilence", json!({})).unwrap();
    assert_eq!(again["count"], 0, "{again}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().duration(), before);
    assert_eq!(s.history.undo.len(), n0);
}

#[test]
fn a_quiet_transcribed_word_inside_a_silence_is_kept() {
    let (mut s, _, item) = session_with(&talk());
    // a whispered word at 2.6–2.9 s that the envelope can't hear
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (w, a, b) in [("hello", 0.2, 1.8), ("psst", 2.6, 2.9), ("again", 3.6, 5.4)] {
        t.words.push(Word::new(w, Tick::from_seconds_f64(a), Tick::from_seconds_f64(b)));
    }
    t.normalize();
    s.execute("transcript.set", json!({"item": item.0, "transcript": serde_json::to_value(&t).unwrap()})).unwrap();
    let r = s.execute("audio.detectSilence", json!({})).unwrap();
    let sil = silences(&r);
    for (a, b) in &sil {
        assert!(*b <= 2.6 - 0.08 + 1e-6 || *a >= 2.9 + 0.08 - 1e-6, "silence {a}–{b} cuts into the word: {sil:?}");
    }
    let raw = s.execute("audio.detectSilence", json!({"respectTranscript": false})).unwrap();
    assert!(silences(&raw).iter().any(|(a, b)| *a < 2.6 && *b > 2.9), "{raw}");
}

#[test]
fn loudness_reports_the_mix() {
    let (mut s, _, _) = session_with(&talk());
    let r = s.execute("audio.loudness", json!({})).unwrap();
    let i = r["integratedLufs"].as_f64().unwrap();
    assert!(i < -5.0 && i > -40.0, "{r}");
    assert!(r["truePeakDbtp"].as_f64().unwrap() <= 1.0);
    assert!((r["analysedSeconds"].as_f64().unwrap() - 7.0).abs() < 0.05);
    let part = s.execute("audio.loudness", json!({"startSeconds": 2.1, "endSeconds": 3.4})).unwrap();
    assert!(part["integratedLufs"].is_null(), "digital silence has no integrated loudness: {part}");
}

#[test]
fn disabled_without_a_sequence() {
    let mut s = Session::default();
    for id in ["audio.detectSilence", "audio.removeSilence", "audio.loudness"] {
        assert!(s.execute(id, json!({})).is_err(), "{id}");
    }
}

#[test]
fn hostile_parameters_are_errors_not_panics() {
    let (mut s, _, _) = session_with(&talk());
    let n0 = s.history.undo.len();
    let bad = [
        json!({"thresholdDb": 50}),
        json!({"thresholdDb": "loud"}),
        json!({"minSeconds": -1}),
        json!({"padSeconds": "x"}),
        json!({"startSeconds": 5, "endSeconds": 1}),
        json!({"startSeconds": -3}),
        json!({"endSeconds": 1e300, "startSeconds": 1e300}),
    ];
    for p in bad {
        assert!(s.execute("audio.detectSilence", p.clone()).is_err(), "{p}");
        assert!(s.execute("audio.removeSilence", p.clone()).is_err(), "{p}");
    }
    // clamped, not refused
    for p in [json!({"minSeconds": 1e9}), json!({"padSeconds": 1e9}), json!({"minSeconds": 0}), json!({"endSeconds": 1e12})] {
        assert!(s.execute("audio.detectSilence", p.clone()).is_ok(), "{p}");
    }
    assert_eq!(s.history.undo.len(), n0, "failed removals leave no undo step");
}

#[test]
fn empty_sequence_is_an_error() {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "empty"})).unwrap();
    let e = s.execute("audio.detectSilence", json!({})).unwrap_err().to_string();
    assert!(e.contains("empty"), "{e}");
}

#[test]
fn the_assistant_tools_reach_the_commands() {
    let (mut s, _, _) = session_with(&talk());
    let r = s
        .execute(
            "tools.call",
            json!({"name": "find_silences", "input": {"min_seconds": 0.5, "pad_seconds": null, "threshold_db": null, "start_seconds": null, "end_seconds": null}}),
        )
        .unwrap();
    assert_eq!(r["result"]["count"], 2, "{r}");
    let r = s.execute("tools.call", json!({"name": "measure_loudness", "input": {"start_seconds": null, "end_seconds": null}})).unwrap();
    assert!(r["result"]["integratedLufs"].as_f64().is_some(), "{r}");
    assert!(s.execute("tools.call", json!({"name": "find_silences", "input": {"item": 3}})).is_err(), "unknown fields are refused");
}
