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
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["errors"][0].as_str().unwrap().contains("grade.lut: no LUT"), "{r}");
    let r = s.execute("plan.validate", json!({"plan": {"version": 1, "title": "x", "audio": {"targetLufs": -60}}})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["warnings"][0].as_str().unwrap().contains("using -30"), "{r}");

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
fn apply_makes_a_new_sequence_in_one_undo_step() {
    let (mut s, src) = session();
    let before = (*s.project).clone();
    let src_before = s.active_sequence().unwrap().clone();
    let steps = s.history.undo.len();
    let mut plan = cut_um();
    plan["captions"] = json!({"burnIn": true});
    plan["markers"] = json!([{"timeS": 3.0, "name": "late"}]);
    plan["grade"] = json!({"preset": "Faded Print"});
    let h = hash(&mut s, &plan);
    let r = s.execute("plan.apply", json!({"plan": plan, "sourceHash": h})).unwrap();
    let id = ItemId(r["sequence"].as_u64().unwrap());
    assert_ne!(id, src);
    assert_eq!(s.state.active_sequence, Some(id));
    assert!(s.state.open_sequences.contains(&id));
    assert_eq!(s.project.item(id).unwrap().name, format!("{} \u{2014} No ums", before.item(src).unwrap().name));
    assert_eq!(s.project.sequence(src).unwrap(), &src_before, "the source is untouched");
    assert_eq!(s.history.undo.len(), steps + 1, "one undo step");
    assert_eq!(words(&mut s), ["Hello", "world.", "Second", "speaker", "here."]);
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let removed = Tick::from_seconds_f64(r["removedS"].as_f64().unwrap());
    assert!(removed > Tick::ZERO);
    assert_eq!(q.duration(), src_before.duration() - removed, "{r}");
    assert!(r["captions"].as_u64().unwrap() >= 1);
    assert_eq!(q.caption_tracks[0].captions.len() as u64, r["captions"].as_u64().unwrap());
    assert!(q.markers.iter().any(|m| m.name == "late"));
    assert_eq!(r["skipped"], json!([]));
    assert_eq!(r["exportParams"], json!({"sequence": id.0, "burnCaptions": true}));
    assert_eq!(r["grade"]["preset"], "Faded Print");

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before, "undo restores the project exactly");
    assert_eq!(s.state.active_sequence, Some(src));
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.project.sequence(id).is_some());
}

#[test]
fn apply_in_place_edits_the_source() {
    let (mut s, src) = session();
    let before = (*s.project).clone();
    let n = s.project.sequences().count();
    let plan = json!({"version": 1, "title": "x", "output": {"mode": "inPlace"}, "cleanup": {"fillers": [], "pauses": {"minS": 1.0, "keepS": 0.1}}});
    let h = hash(&mut s, &plan);
    let r = s.execute("plan.apply", json!({"plan": plan, "sourceHash": h})).unwrap();
    assert_eq!(r["sequence"], src.0);
    assert_eq!(s.project.sequences().count(), n);
    assert_eq!(words(&mut s).len(), 5);
    assert!(s.active_sequence().unwrap().duration() < before.sequence(src).unwrap().duration());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before);
}

#[test]
fn a_stale_preview_is_refused() {
    let (mut s, _) = session();
    let h = hash(&mut s, &cut_um());
    s.execute("transcript.extract", json!({"from": 5})).unwrap();
    let before = (*s.project).clone();
    let e = s.execute("plan.apply", json!({"plan": cut_um(), "sourceHash": h})).unwrap_err().to_string();
    assert!(e.contains("changed since plan.preview"), "{e}");
    let e = s.execute("plan.apply", json!({"plan": cut_um(), "sourceHash": "nope"})).unwrap_err().to_string();
    assert!(e.contains("changed since plan.preview"), "{e}");
    assert!(s.execute("plan.apply", json!({"plan": cut_um()})).unwrap_err().to_string().contains("sourceHash"));
    assert_eq!(*s.project, before);
    // a new transcript also makes the preview stale
    let h = hash(&mut s, &cut_um());
    let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
    s.execute("transcript.set", json!({"item": item.0, "transcript": {"language": "en", "words": [{"text": "x", "start": 0, "end": 1000}]}})).unwrap();
    assert!(s.execute("plan.apply", json!({"plan": cut_um(), "sourceHash": h})).is_err());
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
        assert!(s.execute("plan.apply", p.clone()).is_err());
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
    for id in ["plan.validate", "plan.preview", "plan.apply", "plan.applyVariations"] {
        let e = s.execute(id, json!({"plan": {"version": 1, "title": "x"}, "sourceHash": "x"})).unwrap_err();
        assert!(matches!(e, EngineError::Disabled(..)), "{id}: {e}");
    }
}

#[test]
fn variations_make_n_sequences_in_one_undo_step() {
    let (mut s, src) = session();
    let before = (*s.project).clone();
    let steps = s.history.undo.len();
    let n = s.project.sequences().count();
    let plans = json!([
        cut_um(),
        {"version": 1, "title": "Short", "output": {"name": "Short"}, "cuts": {"keepOnly": [{"from": 3, "to": 5}]}},
        {"version": 1, "title": "Tight", "cleanup": {"pauses": {"minS": 0.5, "keepS": 0.1}}},
    ]);
    let h = hash(&mut s, &cut_um());
    let r = s.execute("plan.applyVariations", json!({"plans": plans, "sourceHash": h})).unwrap();
    let seqs = r["sequences"].as_array().unwrap();
    assert_eq!(seqs.len(), 3);
    assert_eq!(s.project.sequences().count(), n + 3);
    assert_eq!(s.history.undo.len(), steps + 1);
    assert_eq!(seqs[1]["name"], "Short \u{2014} v2");
    assert!(seqs[0]["name"].as_str().unwrap().ends_with("No ums \u{2014} v1"));
    let ids: Vec<ItemId> = seqs.iter().map(|v| ItemId(v["sequence"].as_u64().unwrap())).collect();
    assert_eq!(s.state.active_sequence, Some(ids[0]));
    assert_eq!(words(&mut s).len(), 5);
    s.execute("sequence.open", json!({"item": ids[1].0})).unwrap();
    assert_eq!(words(&mut s), ["Second", "speaker", "here."]);
    assert_eq!(s.project.sequence(src).unwrap(), before.sequence(src).unwrap());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before);

    // refused: none, too many, in place, stale
    for bad in [json!([]), json!(vec![cut_um(); 7]), json!([{"version": 1, "title": "x", "output": {"mode": "inPlace"}}]), json!("x")] {
        assert!(s.execute("plan.applyVariations", json!({"plans": bad, "sourceHash": h})).is_err());
    }
    assert!(s.execute("plan.applyVariations", json!({"plans": [cut_um()], "sourceHash": "stale"})).is_err());
    assert_eq!(*s.project, before);
}

// ---- look and sound: grade, loudness, caption style (A1.4) ----

fn apply_plan(s: &mut Session, plan: &Value) -> Value {
    let h = hash(s, plan);
    s.execute("plan.apply", json!({"plan": plan, "sourceHash": h})).unwrap()
}

fn with(mut plan: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        plan[k] = v.clone();
    }
    plan
}

fn item_named(s: &Session, name: &str) -> ItemId {
    *s.project.items.iter().find(|(_, i)| i.name == name).unwrap().0
}

/// A built-in look LUT: (reference, name).
fn builtin_look(s: &mut Session) -> (String, String) {
    let r = s.execute("lut.list", json!({})).unwrap();
    let b = r["builtin"].as_array().unwrap().iter().find(|b| b["input"] == false).unwrap().clone();
    (b["ref"].as_str().unwrap().to_string(), b["name"].as_str().unwrap().to_string())
}

/// The active sequence's picture clips (V1 and the V2 overlay of the demo).
fn picture_clips(s: &Session) -> Vec<filmcraft_project::TrackItem> {
    let q = s.active_sequence().unwrap();
    q.video_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .filter(|it| matches!(s.project.item(it.item).map(|i| &i.kind), Some(filmcraft_project::ItemKind::Media(m)) if m.info.video.is_some()))
        .cloned()
        .collect()
}

#[test]
fn every_field_applies_in_one_undoable_step() {
    let (mut s, _) = session();
    let (_, lut_name) = builtin_look(&mut s);
    let forest = item_named(&s, filmcraft_media::DemoScene::Forest.file_name());
    let all = json!({
        "captions": {"style": {"size": 0.05, "case": "upper"}, "burnIn": true},
        "grade": {"matchItem": forest.0, "lut": lut_name, "lutStrength": 0.5, "preset": "Faded Print"},
        "audio": {"targetLufs": -20},
    });
    let cases = [
        json!({"grade": {"lut": lut_name, "lutStrength": 0.5}}),
        json!({"grade": {"preset": "Faded Print"}}),
        json!({"grade": {"matchItem": forest.0}}),
        json!({"audio": {"targetLufs": -20}}),
        json!({"captions": {"style": {"color": "#ffdd00"}, "burnIn": true}}),
        all,
    ];
    for extra in cases {
        let plan = with(cut_um(), extra.clone());
        let before = (*s.project).clone();
        let (steps, journal) = (s.history.undo.len(), s.journal.len());
        let r = apply_plan(&mut s, &plan);
        assert_eq!(r["skipped"], json!([]), "{extra}: {r}");
        assert_eq!(s.history.undo.len(), steps + 1, "{extra}: one undo step");
        assert_eq!(s.journal.len(), journal + 1, "{extra}: only plan.apply is journaled");
        assert_eq!(s.journal.last().unwrap().0, "plan.apply");
        s.active_sequence().unwrap().check().unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.project, before, "{extra}: undo restores the project exactly");
    }
}

#[test]
fn grade_lut_preset_and_match() {
    let (mut s, _) = session();
    let (lut_ref, lut_name) = builtin_look(&mut s);
    let plan = with(cut_um(), json!({"grade": {"lut": lut_name, "lutStrength": 0.5, "preset": "faded print"}}));
    let r = apply_plan(&mut s, &plan);
    assert_eq!(r["grade"]["lut"], lut_ref.as_str(), "{r}");
    let clips = picture_clips(&s);
    assert!(clips.len() >= 6);
    assert_eq!(r["grade"]["clips"], clips.len());
    for it in &clips {
        let l: Vec<_> = it.effects.iter().filter(|e| e.effect == "lumetri").collect();
        assert_eq!(l.len(), 2, "the look's Lumetri and the preset's");
        assert_eq!(l[0].param("look_lut").unwrap().value, filmcraft_project::ParamValue::Text(lut_ref.clone()));
        assert_eq!(l[0].param("look_intensity").unwrap().value.as_f64(), Some(50.0));
    }
    s.execute("edit.undo", json!({})).unwrap();
    // a reference by its lib: id works too
    let r = apply_plan(&mut s, &with(cut_um(), json!({"grade": {"lut": lut_ref}})));
    assert_eq!(r["grade"]["lut"], lut_ref.as_str());
    s.execute("edit.undo", json!({})).unwrap();
    let forest = item_named(&s, filmcraft_media::DemoScene::Forest.file_name());
    let r = apply_plan(&mut s, &with(cut_um(), json!({"grade": {"matchItem": forest.0}})));
    assert_eq!(r["grade"]["matched"], picture_clips(&s).len(), "{r}");
    let moved = picture_clips(&s).iter().any(|it| {
        it.effects.iter().filter(|e| e.effect == "lumetri").any(|e| {
            ["wheel_shadows", "wheel_midtones", "wheel_highlights"]
                .iter()
                .any(|w| e.param(w).and_then(|p| p.value.as_vec2()).is_some_and(|v| v.x.abs() + v.y.abs() > 1e-3))
        })
    });
    assert!(moved, "the wheels were matched");
}

#[test]
fn loudness_reaches_the_target() {
    let (mut s, _) = session();
    for (target, want) in [(-20.0, -20.0), (-3.0, -5.0), (-45.0, -30.0)] {
        let r = apply_plan(&mut s, &with(cut_um(), json!({"audio": {"targetLufs": target}})));
        let l = &r["loudness"];
        assert_eq!(l["targetLufs"], want, "{r}");
        let m = s.execute("audio.loudness", json!({})).unwrap()["integratedLufs"].as_f64().unwrap();
        assert!((m - want).abs() <= 1.0, "measured {m} LUFS for a {want} target: {r}");
        assert!((l["afterLufs"].as_f64().unwrap() - m).abs() < 0.01);
        if target != want {
            assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("audio.targetLufs")), "{r}");
        }
        s.execute("edit.undo", json!({})).unwrap();
    }
}

#[test]
fn caption_style_template_and_burn_in() {
    let (mut s, _) = session();
    let style = json!({"size": 0.06, "color": "#ffdd00", "background": false, "outline": 3, "position": "middle",
                       "align": "left", "case": "upper", "sparkle": true, "font": "x".repeat(5000)});
    let plan = with(cut_um(), json!({"captions": {"style": style, "template": "Lower Third", "burnIn": true}}));
    let r = apply_plan(&mut s, &plan);
    let t = &s.active_sequence().unwrap().caption_tracks[0];
    assert!((t.style.size - 64.8).abs() < 0.01);
    assert_eq!(t.style.color, [255, 221, 0, 255]);
    assert!(!t.style.background);
    assert_eq!(t.style.outline, 3.0);
    assert_eq!(t.style.anchor, filmcraft_project::CaptionAnchor::Middle);
    assert_eq!(t.style.align, filmcraft_project::CaptionAlign::Left);
    assert_eq!(t.style.font, "Inter", "the oversized font name is ignored");
    assert!(t.captions.iter().all(|c| c.text == c.text.to_uppercase()) && t.captions.iter().any(|c| c.text.contains("HELLO")));
    let w: Vec<&str> = r["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert!(w.iter().any(|w| w.contains("captions.style.sparkle: unknown key")), "{w:?}");
    assert!(w.iter().any(|w| w.contains("captions.style.font: longer than")), "{w:?}");
    assert!(w.iter().any(|w| w.contains("captions.template")), "{w:?}");
    assert!(w.iter().all(|w| w.chars().count() < 400), "warnings never echo huge strings");
    assert_eq!(r["skipped"], json!(["captions.template"]));
    assert_eq!(r["exportParams"]["burnCaptions"], true);
}

#[test]
fn hostile_look_and_sound_values() {
    let (mut s, _) = session();
    let music = item_named(&s, "Ambient_Score.wav");
    let before = (*s.project).clone();
    let errors = [
        json!(r#"{"version":1,"title":"x","audio":{"targetLufs":NaN}}"#),
        json!({"version": 1, "title": "x", "audio": {"targetLufs": 1e308}}),
        json!({"version": 1, "title": "x", "audio": {"targetLufs": "loud"}}),
        json!({"version": 1, "title": "x", "output": {"aspect": "21:9"}}),
        json!({"version": 1, "title": "x", "output": {"aspect": 1.78}}),
        json!({"version": 1, "title": "x", "grade": {"lut": "lib:nope"}}),
        json!({"version": 1, "title": "x", "grade": {"lut": "builtin:../../etc/passwd"}}),
        json!({"version": 1, "title": "x", "grade": {"lut": "x".repeat(5000)}}),
        json!({"version": 1, "title": "x", "grade": {"lut": "a.cube", "lutStrength": f64::MAX}}),
        json!({"version": 1, "title": "x", "grade": {"preset": "Nope"}}),
        json!({"version": 1, "title": "x", "grade": {"matchItem": 999_999}}),
        json!({"version": 1, "title": "x", "grade": {"matchItem": music.0}}),
        json!({"version": 1, "title": "x", "captions": {"style": "big and yellow"}}),
        json!({"version": 1, "title": "x", "captions": {"template": "t".repeat(5000)}}),
    ];
    for plan in errors {
        let v = s.execute("plan.validate", json!({"plan": plan})).unwrap();
        assert_eq!(v["ok"], false, "{plan}: {v}");
        assert!(s.execute("plan.preview", json!({"plan": plan})).is_err());
        assert!(s.execute("plan.apply", json!({"plan": plan, "sourceHash": "0000000000000000"})).is_err());
    }
    assert_eq!(*s.project, before);
    // lenient: odd style keys and values are warnings, the edit still applies
    let mut odd = serde_json::Map::new();
    for i in 0..500 {
        odd.insert(format!("key{i}"), json!("v".repeat(1000)));
    }
    odd.insert("size".into(), json!(f64::MAX));
    odd.insert("color".into(), json!([1e300, -5, 3]));
    let plan = with(cut_um(), json!({"captions": {"style": Value::Object(odd)}}));
    let r = apply_plan(&mut s, &plan);
    assert!(!r["warnings"].as_array().unwrap().is_empty());
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].style, filmcraft_project::CaptionStyle::default());
}
