//! `plan.*`: validate, preview, apply (one undo step, new sequence by default), stale previews,
//! hostile plans and variations.

use serde_json::{Value, json};

use crate::{EngineError, Session};
use filmcraft_project::ItemId;
use filmcraft_time::Tick;

/// The demo project with a transcript on the first A1 clip's media:
/// "Hello um world." (0.2–1.4 s), a 1.2 s pause, "Second speaker here." (2.6–4.0 s), in clip time.
fn session() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = &q.audio_tracks[0].items[0];
    let (item, sin) = (a.item, a.source_in);
    let sec = |x: f64| (sin + Tick::from_seconds_f64(x)).0;
    let words: Vec<Value> = [("Hello", 0.2, 0.5), ("um", 0.6, 0.9), ("world.", 1.0, 1.4), ("Second", 2.6, 3.0), ("speaker", 3.0, 3.5), ("here.", 3.5, 4.0)]
        .iter()
        .map(|(t, a, b)| json!({"text": t, "start": sec(*a), "end": sec(*b)}))
        .collect();
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "en", "words": words}})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    (s, seq)
}

fn words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

fn hash(s: &mut Session, plan: &Value) -> String {
    s.execute("plan.preview", json!({"plan": plan})).unwrap()["sourceHash"].as_str().unwrap().to_string()
}

fn cut_um() -> Value {
    json!({"version": 1, "title": "No ums", "cuts": {"removeWords": [{"from": 1, "reason": "filler"}]}})
}

#[test]
fn validate_and_preview() {
    let (mut s, seq) = session();
    let r = s.execute("plan.validate", json!({"plan": {"version": 1, "title": "x"}})).unwrap();
    assert_eq!(r, json!({"ok": true, "errors": [], "warnings": []}));
    let r = s.execute("plan.validate", json!({"plan": {"version": 1, "title": "x", "cuts": {"removeWords": [{"from": 40, "reason": "r"}]}}})).unwrap();
    assert_eq!(r["ok"], false);
    assert!(r["errors"][0].as_str().unwrap().contains("word 40 is out of range"), "{r}");
    let r = s.execute("plan.validate", json!({"plan": "{not json"})).unwrap();
    assert_eq!(r["ok"], false);
    let r = s.execute("plan.validate", json!({"plan": {"version": 1, "title": "x", "grade": {"lut": "a.cube"}}})).unwrap();
    assert!(r["warnings"][0].as_str().unwrap().contains("grade"), "{r}");

    let mut plan = cut_um();
    plan["captions"] = json!({"maxChars": 20, "lines": 1});
    plan["markers"] = json!([{"word": 3, "name": "Second part"}]);
    let r = s.execute("plan.preview", json!({"plan": plan})).unwrap();
    assert_eq!(r["sequence"], seq.0);
    assert_eq!(r["removals"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["removals"][0]["text"], "um");
    assert_eq!(r["removals"][0]["kind"], "word");
    assert_eq!(r["removals"][0]["reason"], "filler");
    assert!(r["after"].as_f64().unwrap() < r["before"].as_f64().unwrap());
    assert_eq!(r["words"], json!({"total": 6, "kept": 5}));
    assert!(r["captions"].as_u64().unwrap() >= 2, "{r}");
    assert_eq!(r["markers"][0]["name"], "Second part");
    let h = r["sourceHash"].as_str().unwrap();
    assert_eq!(h.len(), 16);
    // stable: the same state gives the same hash, also in another session
    assert_eq!(hash(&mut s, &cut_um()), h);
    let (mut t, _) = session();
    assert_eq!(hash(&mut t, &cut_um()), h);
    // previews change nothing
    assert_eq!(s.history.undo.last().unwrap().0, "Set Transcript");
    assert_eq!(words(&mut s).len(), 6);
}

#[test]
fn hostile_plans_are_errors() {
    let (mut s, _) = session();
    let before = (*s.project).clone();
    let huge_ranges: Vec<Value> = (0..10_001).map(|i| json!({"startS": i as f64 * 0.001, "endS": i as f64 * 0.001 + 0.0005, "reason": "r"})).collect();
    let big = "a".repeat(3 << 20);
    let cases = [
        json!({"plan": {"version": 1, "title": "x", "surprise": true}}),
        json!({"plan": {"version": 1, "title": "x", "cuts": {"removeRanges": [{"startS": null, "endS": 1, "reason": "r"}]}}}),
        json!({"plan": r#"{"version":1,"title":"x","cuts":{"removeRanges":[{"startS":NaN,"endS":1,"reason":"r"}]}}"#}),
        json!({"plan": {"version": 1, "title": "x", "cuts": {"removeRanges": huge_ranges}}}),
        json!({"plan": {"version": 1, "title": "x", "cuts": {"removeWords": [{"from": 6, "reason": "r"}]}}}),
        json!({"plan": {"version": 1, "title": "x", "cuts": {"removeWords": [{"from": 18446744073709551615u64, "reason": "r"}]}}}),
        json!({"plan": {"version": 1, "title": "x", "rationale": big.clone()}}),
        json!({"plan": format!(r#"{{"version":1,"title":"x","rationale":"{big}"}}"#)}),
        json!({"plan": {"version": 1, "title": "x", "source": {"sequence": 999_999}}}),
        json!({"plan": {"version": 7, "title": "x"}}),
        json!({"plan": 42}),
        json!({}),
        json!(null),
    ];
    for c in cases {
        let mut p = c.clone();
        let e = s.execute("plan.preview", p.clone()).unwrap_err();
        assert!(matches!(e, EngineError::BadParams { .. }), "{e}");
        p["sourceHash"] = json!("0000000000000000");
        let v = s.execute("plan.validate", p).unwrap();
        assert_eq!(v["ok"], false, "{v}");
    }
    let e = s.execute("plan.preview", json!({"plan": format!(r#"{{"version":1,"title":"x","rationale":"{big}"}}"#)})).unwrap_err().to_string();
    assert!(e.contains("limit is 2097152"), "{e}");
    assert_eq!(*s.project, before);
}

#[test]
fn disabled_without_a_sequence() {
    let mut s = Session::default();
    for id in ["plan.validate", "plan.preview"] {
        let e = s.execute(id, json!({"plan": {"version": 1, "title": "x"}, "sourceHash": "x"})).unwrap_err();
        assert!(matches!(e, EngineError::Disabled(..)), "{id}: {e}");
    }
}
