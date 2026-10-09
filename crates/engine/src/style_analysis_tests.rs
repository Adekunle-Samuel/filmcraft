//! `media.analyze`, `media.analysis` and the style library on procedural demo media and on a
//! movie with known cuts made with our own ProRes encoder.

use std::path::Path;
use std::sync::Arc;

use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::transcript::{Transcript, Word};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{session_with, tmp_dir};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// A demo footage item (procedural picture and sound).
fn footage(s: &Session) -> ItemId {
    let mut v: Vec<(ItemId, String)> = s
        .project
        .items
        .iter()
        .filter(|(_, i)| matches!(&i.kind, ItemKind::Media(m) if m.info.kind == filmcraft_media::MediaKind::Movie && m.info.video.is_some() && m.info.audio.is_some()))
        .map(|(id, i)| (*id, i.name.clone()))
        .collect();
    v.sort();
    v[0].0
}

/// A 24 fps ProRes movie of demo scenes one after the other: `shots` = (scene, frames).
fn make_cut_movie(path: &Path, shots: &[(DemoScene, i64)], w: u32, h: u32) {
    let rate = FrameRate::FPS_24;
    let mut p = Project::new("fixture");
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut sources: Vec<(ItemId, filmcraft_media::SharedSource)> = Vec::new();
    let mut at = Tick::ZERO;
    for (k, (scene, frames)) in shots.iter().enumerate() {
        let dur = rate.tick_of(*frames);
        let src = GeneratorSource::new(Generator::Demo(*scene), w, h, rate, rate.tick_of(200));
        let clip = MediaClip {
            media: MediaRef::Generator(Generator::Demo(*scene)),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let item = p.add_item(&format!("shot{k}"), Label::Iris, ItemKind::Media(clip), None);
        let mut v = p.make_track_item(item, TrackKind::Video, at, TimeRange::new(rate.tick_of(10 * k as i64), dur), rate).unwrap();
        for e in &mut v.effects {
            filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
        }
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        sources.push((item, Arc::new(src)));
        at += dur;
    }
    let settings =
        filmcraft_export::ExportSettings { format: filmcraft_export::Format::ProRes, path: path.to_string_lossy().into_owned(), ..Default::default() };
    let provider = move |id: ItemId| sources.iter().find(|(i, _)| *i == id).map(|(_, s)| s.clone());
    filmcraft_export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).unwrap();
}

fn finite(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        Value::Array(a) => a.iter().all(finite),
        Value::Object(o) => o.values().all(finite),
        _ => true,
    }
}

#[test]
fn analyze_demo_media_gives_sane_numbers_without_touching_the_project() {
    let mut s = demo();
    let item = footage(&s);
    let (rev, undo) = (s.revision, s.history.undo.len());
    assert!(s.execute("media.analysis", json!({"item": item.0})).is_err(), "nothing cached yet");
    let r = s.execute("media.analyze", json!({"item": item.0, "maxFrames": 24, "wait": true})).unwrap();
    let pr = &r["profile"];
    assert!(finite(pr), "{pr}");
    assert_eq!(pr["version"], 1);
    assert_eq!(pr["analyzedFrames"], 24);
    assert!(pr["shots"]["count"].as_u64().unwrap() >= 1, "{pr}");
    assert!(pr["shots"]["medianSeconds"].as_f64().unwrap() > 0.0);
    assert_eq!(pr["format"]["aspect"], "16:9");
    assert_eq!((pr["format"]["width"].as_u64(), pr["format"]["height"].as_u64()), (Some(1920), Some(1080)));
    assert!((pr["format"]["fps"].as_f64().unwrap() - 23.976).abs() < 0.01);
    let c = &pr["color"];
    let (p5, p95) = (c["lumaP5"].as_f64().unwrap(), c["lumaP95"].as_f64().unwrap());
    assert!((0.0..=1.0).contains(&p5) && p5 <= p95 && p95 <= 1.0, "{c}");
    assert!(["warm", "neutral", "cool"].contains(&c["temperatureLabel"].as_str().unwrap()));
    let keys = pr["keyframes"].as_array().unwrap();
    assert!(!keys.is_empty() && keys.len() <= 24);
    assert!(keys[0]["color"]["tones"]["midtones"]["share"].as_f64().is_some(), "{}", keys[0]);
    // the demo footage has sound: an integrated loudness
    assert!(pr["audio"]["integratedLufs"].as_f64().is_some_and(|l| l < 0.0 && l > -70.0), "{}", pr["audio"]);
    assert_eq!(pr["speech"], Value::Null, "no transcript");
    // cached on the session, not in the project
    assert_eq!(s.execute("media.analysis", json!({"item": item.0})).unwrap()["profile"], *pr);
    assert_eq!((s.revision, s.history.undo.len()), (rev, undo), "analysis is not an edit");
    assert!(!serde_json::to_string(&*s.project).unwrap().contains("analyzedFrames"));
}

#[test]
fn analyze_finds_the_cuts_and_speech_rhythm() {
    let dir = tmp_dir("style-cuts");
    let path = dir.join("cuts.mov");
    // cuts at frames 20 and 36 of 50
    make_cut_movie(&path, &[(DemoScene::OceanSunset, 20), (DemoScene::CityNight, 16), (DemoScene::Forest, 14)], 160, 90);
    let (mut s, items, _) = session_with(&[&path]);
    let rate = FrameRate::FPS_24;
    // a transcript: 4 words, one filler, one long pause
    let sec = Tick::from_seconds_f64;
    let w = |t: &str, a: f64, b: f64| Word { text: t.into(), start: sec(a), end: sec(b), speaker: None, confidence: 1.0 };
    let tr = Transcript {
        language: "en".into(),
        source: "manual".into(),
        speakers: vec![],
        words: vec![w("Um,", 0.0, 0.2), w("hello", 0.3, 0.6), w("there", 1.4, 1.7), w("friend.", 1.75, 2.0)],
    };
    Arc::make_mut(&mut s.project).transcripts.insert(items[0], Arc::new(tr));
    let r = s.execute("media.analyze", json!({"item": items[0].0, "wait": true})).unwrap();
    let pr = &r["profile"];
    assert_eq!(pr["analyzedFrames"], 50, "every frame of a short clip");
    assert_eq!(pr["shots"]["count"], 3, "{}", pr["shots"]);
    assert_eq!(pr["shots"]["cutTicks"], json!([rate.tick_of(20).0, rate.tick_of(36).0]));
    let dur = 50.0 / 24.0;
    assert!((pr["shots"]["cutsPerMinute"].as_f64().unwrap() - 2.0 / (dur / 60.0)).abs() < 1e-6);
    assert_eq!(pr["keyframes"].as_array().unwrap().len(), 3);
    assert_eq!(pr["format"]["aspect"], "16:9");
    let sp = &pr["speech"];
    assert_eq!((sp["words"].as_u64(), sp["fillers"].as_u64(), sp["longPauses"].as_u64()), (Some(4), Some(1), Some(1)), "{sp}");
    assert!((sp["wordsPerMinute"].as_f64().unwrap() - 4.0 / (dur / 60.0)).abs() < 1e-6);
    // a subclip of the last shot only: one shot
    let sub = s.execute("clip.makeSubclip", json!({"item": items[0].0, "start": rate.tick_of(36).0, "end": rate.tick_of(50).0, "name": "tail"})).unwrap();
    let r = s.execute("media.analyze", json!({"item": sub["item"], "wait": true})).unwrap();
    assert_eq!(r["profile"]["shots"]["count"], 1, "{}", r["profile"]["shots"]);
    assert_eq!(r["profile"]["analyzedFrames"], 14);
}

#[test]
fn background_job_and_cancel() {
    let mut s = demo();
    let item = footage(&s);
    let r = s.execute("media.analyze", json!({"item": item.0, "maxFrames": 12})).unwrap();
    let job = r["job"].as_u64().unwrap();
    // a second analysis of the same item while it runs is refused
    let again = s.execute("media.analyze", json!({"item": item.0}));
    let t0 = std::time::Instant::now();
    while !s.style.jobs.is_empty() {
        s.poll_persistence();
        assert!(t0.elapsed().as_secs() < 300, "job never finished");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // (unless the first job had already finished)
    if let Err(e) = again {
        assert!(e.to_string().contains("already"), "{e}");
    }
    let j = s.execute("jobs.list", json!({})).unwrap();
    let j = j.as_array().unwrap().iter().find(|x| x["id"] == job).unwrap().clone();
    assert_eq!(j["finished"], true);
    assert_eq!(j["done"], j["total"]);
    assert!(s.style.cache.contains_key(&item));
    // a cancelled job caches nothing
    s.style.cache.clear();
    let r = s.execute("media.analyze", json!({"item": item.0, "maxFrames": 600})).unwrap();
    s.execute("jobs.cancel", json!({"job": r["job"]})).unwrap();
    let t0 = std::time::Instant::now();
    while !s.style.jobs.is_empty() {
        s.poll_persistence();
        assert!(t0.elapsed().as_secs() < 300);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.style.cache.is_empty());
    assert!(s.execute("media.analysis", json!({"item": item.0})).is_err());
}

#[test]
fn hostile_params_and_disabled_states() {
    let mut empty = Session::default();
    assert!(!empty.is_enabled("media.analyze"), "no media");
    assert!(!empty.is_enabled("style.save"), "nothing analysed");
    assert!(empty.execute("media.analyze", json!({"item": 1})).is_err());
    let mut s = demo();
    let item = footage(&s).0;
    let seq = s.state.active_sequence.unwrap().0;
    for p in [
        json!({}),
        json!({"item": "x"}),
        json!({"item": u64::MAX}),
        json!({"item": seq}),
        json!({"item": item, "maxFrames": 0}),
        json!({"item": item, "maxFrames": 601}),
        json!({"item": item, "maxFrames": -5}),
        json!({"item": item, "maxFrames": 1e9}),
        json!({"item": item, "maxFrames": "NaN"}),
        json!({"item": item, "maxFrames": [1]}),
    ] {
        assert!(s.execute("media.analyze", p.clone()).is_err(), "{p}");
    }
    for p in [json!({}), json!({"item": -1}), json!({"item": u64::MAX})] {
        assert!(s.execute("media.analysis", p.clone()).is_err(), "{p}");
    }
    assert!(s.style.jobs.is_empty() && s.style.cache.is_empty());
}

#[test]
fn style_library_save_list_delete() {
    let mut s = demo();
    let dir = tmp_dir("style-lib");
    s.style.set_dir(&dir);
    let item = footage(&s);
    assert!(s.execute("style.save", json!({"name": "Look", "item": item.0})).is_err(), "not analysed");
    s.execute("media.analyze", json!({"item": item.0, "maxFrames": 6, "wait": true})).unwrap();
    assert!(s.is_enabled("style.save"));
    let long = "x".repeat(65);
    for name in ["../evil", "a/b", "a\\b", "", "   ", ".hidden", "c:x", "tab\there", long.as_str()] {
        assert!(s.execute("style.save", json!({"name": name, "item": item.0})).is_err(), "{name:?}");
    }
    assert!(s.execute("style.save", json!({"name": "Look", "item": 999_999})).is_err());
    assert!(s.execute("style.save", json!({"item": item.0})).is_err());
    let r = s.execute("style.save", json!({"name": " Brand Look ", "item": item.0})).unwrap();
    assert_eq!(r["name"], "Brand Look");
    assert!(dir.join("styles/Brand Look.json").is_file());
    assert!(!dir.join("evil.json").exists());
    // a broken file is reported, not fatal
    std::fs::write(dir.join("styles/broken.json"), b"{nope").unwrap();
    let l = s.execute("style.list", json!({})).unwrap();
    assert_eq!(l["styles"].as_array().unwrap().len(), 1, "{l}");
    assert_eq!(l["styles"][0]["name"], "Brand Look");
    assert_eq!(l["styles"][0]["profile"]["item"], item.0);
    assert_eq!(l["errors"][0]["name"], "broken");
    // overwrite is atomic and keeps one file
    s.execute("style.save", json!({"name": "Brand Look", "item": item.0})).unwrap();
    assert_eq!(s.execute("style.list", json!({})).unwrap()["styles"].as_array().unwrap().len(), 1);
    assert!(s.execute("style.delete", json!({"name": "../styles/Brand Look"})).is_err());
    s.execute("style.delete", json!({"name": "Brand Look"})).unwrap();
    assert!(!dir.join("styles/Brand Look.json").exists());
    assert!(s.execute("style.delete", json!({"name": "Brand Look"})).is_err());
    assert!(s.execute("style.delete", json!({})).is_err());
    // no data directory (unit tests never fall back to the user's): an error, not a crash
    let mut t = demo();
    let it = footage(&t);
    t.execute("media.analyze", json!({"item": it.0, "maxFrames": 2, "wait": true})).unwrap();
    assert!(t.execute("style.save", json!({"name": "x", "item": it.0})).is_err());
    assert_eq!(t.execute("style.list", json!({})).unwrap()["styles"], json!([]));
}
