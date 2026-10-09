//! [`SessionHost`] follows the background jobs tools start, to the end, with progress: the
//! `export_variations` batch (one job for several export-queue items) and a transcription. Exports
//! always ask first, and a declined export queues nothing.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use filmcraft_agent::{Authorization, SessionHost, ToolCall, ToolHost};
use filmcraft_engine::Session;
use filmcraft_project::{ItemId, ItemKind, Label, Transcript, Word};
use filmcraft_speech::FixedTranscriber;
use filmcraft_time::Tick;
use serde_json::{Value, json};

/// The demo project with a second copy of its sequence; returns both ids.
fn two_sequences() -> (Session, [u64; 2]) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let first = s.state.active_sequence.unwrap();
    let seq = s.active_sequence().unwrap().clone();
    let p = Arc::make_mut(&mut s.project);
    p.item_mut(first).unwrap().name = "Short".into();
    let second = p.add_item("Vertical 9:16", Label::Iris, ItemKind::Sequence(Box::new(seq)), None);
    (s, [first.0, second.0])
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall { id: "t1".into(), name: name.into(), input }
}

fn queue_len(s: &mut Session) -> usize {
    s.execute("export.queue.list", json!({})).unwrap()["items"].as_array().map_or(0, Vec::len)
}

#[test]
fn export_variations_asks_then_follows_the_batch_to_the_end() {
    let (mut s, ids) = two_sequences();
    let dir = scratch("variations");
    let input = json!({"sequences": ids, "preset": "Waveform Audio 48 kHz 16-bit", "folder": dir.to_string_lossy(), "overwrite": null});
    let mut asked = Vec::new();
    let out = {
        let mut host = SessionHost::new(
            &mut s,
            Box::new(|c, why| {
                asked.push((c.name.clone(), why.to_string()));
                true
            }),
        );
        let c = call("export_variations", input.clone());
        assert_eq!(host.authorize(&c), Authorization::Allow);
        let mut progress = Vec::new();
        let out = host.call(&c, &AtomicBool::new(false), &mut |p| progress.push(p)).unwrap();
        assert!(!progress.is_empty(), "the host reported the batch's progress");
        assert!(progress.iter().all(|p| p.fraction.is_some_and(|f| (0.0..=1.0).contains(&f))), "{progress:?}");
        out
    };
    assert_eq!(asked, [("export_variations".to_string(), "it writes files".to_string())]);
    assert_eq!(out.json["finished"], true, "{}", out.json);
    let files = out.json["started"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for (f, name) in files.iter().zip(["Short.wav", "Vertical 9_16.wav"]) {
        let p = std::path::PathBuf::from(f["path"].as_str().unwrap());
        assert_eq!(p, dir.join(name));
        assert!(p.exists(), "{p:?} was written when the tool returned");
    }
    assert!(out.json["result"]["error"].is_null(), "{}", out.json);

    // again: the files exist, so the call fails and nothing is queued
    let before = queue_len(&mut s);
    let e = SessionHost::new(&mut s, Box::new(|_, _| true)).call(&call("export_variations", input.clone()), &AtomicBool::new(false), &mut |_| {});
    assert!(e.unwrap_err().contains("already exist"));
    assert_eq!(queue_len(&mut s), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_declined_export_queues_nothing() {
    let (mut s, ids) = two_sequences();
    let dir = scratch("declined");
    let c = call("export_variations", json!({"sequences": ids, "preset": "YouTube 1080p Full HD", "folder": dir.to_string_lossy(), "overwrite": true}));
    {
        let mut host = SessionHost::read_mostly(&mut s);
        assert_eq!(host.authorize(&c), Authorization::Deny("the user declined".into()));
    }
    assert_eq!(queue_len(&mut s), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn transcribe_is_followed_until_the_transcript_is_stored() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let a = &s.active_sequence().unwrap().audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (w, a, b) in [("Hello", 0.2, 0.5), ("um", 0.6, 0.9), ("world.", 1.0, 1.4)] {
        t.words.push(Word::new(w, sin + Tick::from_seconds_f64(a), sin + Tick::from_seconds_f64(b)));
    }
    t.normalize();
    s.transcriber = Some(Arc::new(FixedTranscriber { transcript: t, id: "fixed".into() }));
    let input = json!({"items": [item.0], "language": null, "model": null, "keep_fillers": null, "regions": null});
    let out = {
        let mut host = SessionHost::read_mostly(&mut s);
        let c = call("transcribe", input);
        assert_eq!(host.authorize(&c), Authorization::Allow, "transcribing needs no click");
        host.call(&c, &AtomicBool::new(false), &mut |_| {}).unwrap()
    };
    assert_eq!(out.json["finished"], true, "{}", out.json);
    assert_eq!(s.project.transcripts.get(&ItemId(item.0)).map(|t| t.words.len()), Some(3));
}
