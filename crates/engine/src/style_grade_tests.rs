//! `lumetri.matchToItem` and `lumetri.bakeLut` on the demo project.

use filmcraft_color::{Lut, Lut3d};
use filmcraft_media::DemoScene;
use filmcraft_project::{ClipId, ItemId, ParamValue};
use filmcraft_render::{Image, RenderOptions};
use serde_json::json;

use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The first V1 clip under the playhead, selected.
fn pick_clip(s: &mut Session) -> ClipId {
    let q = s.active_sequence().unwrap();
    let id = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    s.state.selection = vec![id];
    id
}

fn item_named(s: &Session, name: &str) -> ItemId {
    *s.project.items.iter().find(|(_, i)| i.name == name).unwrap().0
}

fn lumetri(s: &Session, clip: ClipId) -> Option<filmcraft_project::EffectInstance> {
    s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().find(|e| e.effect == "lumetri").cloned()
}

fn render_clip(s: &Session, project: &filmcraft_project::Project, clip: ClipId) -> Image {
    let seq = s.state.active_sequence.unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = RenderOptions { scale: 0.125, ..Default::default() };
    let q = project.sequence(seq).unwrap();
    let it = q.find_item(clip).unwrap().1;
    let t = it.start + filmcraft_time::Tick(it.duration.0 / 2);
    filmcraft_render::render_clip(project, seq, clip, t, opts, &provider).unwrap()
}

/// Oklab distance of two frames' statistics (tonal L/a/b and chroma).
fn distance(a: &Image, b: &Image) -> f32 {
    let (x, y) = (filmcraft_render::color_match::stats(a, false), filmcraft_render::color_match::stats(b, false));
    let mut d = (x.chroma - y.chroma).abs() * 2.0;
    for k in 0..3 {
        d += (x.bands[k][1] - y.bands[k][1]).abs() + 2.0 * ((x.bands[k][2] - y.bands[k][2]).abs() + (x.bands[k][3] - y.bands[k][3]).abs());
    }
    d
}

#[test]
fn match_to_item_moves_the_clip_toward_the_reference_in_one_undo_step() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let own = s.active_sequence().unwrap().find_item(clip).unwrap().1.item;
    let reference = item_named(&s, DemoScene::CityNight.file_name());
    assert_ne!(own, reference);
    assert!(lumetri(&s, clip).is_none(), "the demo clip starts without Lumetri");
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let ref_img = filmcraft_render::render_item(&s.project, reference, filmcraft_time::Tick::from_seconds_f64(7.0), 0.1, &provider).unwrap();
    let before = distance(&render_clip(&s, &s.project, clip), &ref_img);
    let undo = s.history.undo.len();
    let r = s.execute("lumetri.matchToItem", json!({"item": reference.0, "samples": 4})).unwrap();
    let c = &r["clips"][0];
    assert_eq!(c["clip"], clip.0);
    assert!(c["distanceAfter"].as_f64().unwrap() < c["distanceBefore"].as_f64().unwrap() * 0.8, "{r}");
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step, Lumetri added inside it");
    let e = lumetri(&s, clip).expect("Lumetri added");
    assert!(
        ["wheel_shadows", "wheel_midtones", "wheel_highlights"]
            .iter()
            .any(|w| e.param(w).and_then(|p| p.value.as_vec2()).is_some_and(|v| v.x.abs() + v.y.abs() > 1e-3))
    );
    let after = distance(&render_clip(&s, &s.project, clip), &ref_img);
    assert!(after < before, "the rendered clip moved toward the reference: {before} → {after}");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(lumetri(&s, clip).is_none(), "undo restores");
    // explicit clips, an existing Lumetri is reused (not duplicated)
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    s.execute("lumetri.matchToItem", json!({"clips": [clip.0], "item": reference.0, "samples": 1, "faceDetection": false})).unwrap();
    let n = s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.iter().filter(|e| e.effect == "lumetri").count();
    assert_eq!(n, 1);
}

#[test]
fn match_to_item_hostile_params() {
    let mut empty = Session::default();
    assert!(!empty.is_enabled("lumetri.matchToItem") && !empty.is_enabled("lumetri.bakeLut"), "no sequence");
    assert!(empty.execute("lumetri.matchToItem", json!({"item": 1})).is_err());
    let mut s = demo();
    let clip = pick_clip(&mut s).0;
    let reference = item_named(&s, DemoScene::Forest.file_name()).0;
    let seq = s.state.active_sequence.unwrap().0;
    let rev = s.revision;
    for p in [
        json!({}),
        json!({"item": "x"}),
        json!({"item": 999_999}),
        json!({"item": u64::MAX}),
        json!({"item": reference, "samples": 0}),
        json!({"item": reference, "samples": 13}),
        json!({"item": reference, "samples": -1}),
        json!({"item": reference, "samples": 1e6}),
        json!({"item": reference, "samples": "NaN"}),
        json!({"item": reference, "clips": [999_999]}),
        json!({"item": reference, "clips": ["x"]}),
        json!({"item": reference, "clips": 5}),
        json!({"item": reference, "clips": []}),
        json!({"item": item_named(&s, "Ambient_Score.wav").0, "clips": [clip]}),
    ] {
        assert!(s.execute("lumetri.matchToItem", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.revision, rev, "nothing changed");
    // a sequence is a valid reference (rendered as the Program would show it)
    let other = s.project.items.iter().find(|(id, i)| matches!(i.kind, filmcraft_project::ItemKind::Sequence(_)) && id.0 != seq).map(|(id, _)| id.0);
    if let Some(other) = other {
        // an empty sequence has no picture: an error, not a crash
        let _ = s.execute("lumetri.matchToItem", json!({"item": other, "clips": [clip]}));
    }
    s.execute("lumetri.matchToItem", json!({"item": seq, "clips": [clip], "samples": 2})).unwrap();
}

#[test]
fn bake_identity_lut_registers_and_applies() {
    let mut s = demo();
    let dir = tmp_dir("bake-identity");
    s.style.set_dir(&dir);
    let clip = pick_clip(&mut s);
    assert!(s.execute("lumetri.bakeLut", json!({"clip": clip.0, "name": "x"})).is_err(), "no Lumetri yet");
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    let undo = s.history.undo.len();
    let r = s.execute("lumetri.bakeLut", json!({"clip": clip.0, "size": 17, "name": "Neutral"})).unwrap();
    assert_eq!(r["warnings"], json!([]), "{r}");
    assert!(r["input"].as_str().unwrap().contains("SDR Rec. 709"));
    let path = dir.join("luts/Neutral.cube");
    assert_eq!(r["path"], path.to_string_lossy().as_ref());
    let lut = Lut::parse(&std::fs::read_to_string(&path).unwrap(), None).unwrap();
    let cube = lut.cube.unwrap();
    assert_eq!(cube.size, 17);
    let id = Lut3d::identity(17);
    let err = cube.data.iter().zip(&id.data).flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs())).fold(0f32, f32::max);
    assert!(err < 1e-3, "identity bake max error {err}");
    // registered like lut.import (one undo step) and usable as a Creative Look
    assert_eq!(s.history.undo.len(), undo + 1);
    let list = s.execute("lut.list", json!({})).unwrap();
    assert!(list["library"].as_array().unwrap().iter().any(|l| l["ref"] == r["ref"] && l["name"] == "Neutral"), "{list}");
    s.execute("lumetri.setLook", json!({"clip": clip.0, "lut": r["ref"]})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.luts.is_empty(), "undo removes the library entry");
}

#[test]
fn baked_grade_through_the_lut_matches_the_direct_render() {
    let mut s = demo();
    let dir = tmp_dir("bake-grade");
    s.style.set_dir(&dir);
    let clip = pick_clip(&mut s);
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    s.edit_sequence("grade", |q, _, _| {
        let e = q.find_item_mut(clip).unwrap().1.effects.iter_mut().find(|e| e.effect == "lumetri").unwrap();
        e.params.get_mut("exposure").unwrap().value = ParamValue::Float(0.5);
        e.params.get_mut("saturation").unwrap().value = ParamValue::Float(150.0);
        e.params.get_mut("vignette_amount").unwrap().value = ParamValue::Float(-1.5);
        Ok(())
    })
    .unwrap();
    let r = s.execute("lumetri.bakeLut", json!({"clip": clip.0, "name": "Punchy"})).unwrap();
    assert_eq!(r["size"], 33);
    let w = r["warnings"].as_array().unwrap();
    assert!(w.len() == 1 && w[0].as_str().unwrap().contains("Vignette"), "{r}");
    // direct: the grade without its vignette; via LUT: a fresh Lumetri with the baked Input LUT
    let seq = s.state.active_sequence.unwrap();
    let mut direct = (*s.project).clone();
    let mut via = (*s.project).clone();
    direct
        .sequence_mut(seq)
        .unwrap()
        .find_item_mut(clip)
        .unwrap()
        .1
        .effects
        .iter_mut()
        .find(|e| e.effect == "lumetri")
        .unwrap()
        .params
        .get_mut("vignette_amount")
        .unwrap()
        .value = ParamValue::Float(0.0);
    let mut fresh = filmcraft_project::find_effect("lumetri").unwrap().instance();
    fresh.params.get_mut("input_lut").unwrap().value = ParamValue::Text(r["ref"].as_str().unwrap().into());
    *via.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1.effects.iter_mut().find(|e| e.effect == "lumetri").unwrap() = fresh;
    let (a, b) = (render_clip(&s, &direct, clip), render_clip(&s, &via, clip));
    let enc = |v: f32| filmcraft_color::linear_to_srgb(v.clamp(0.0, 1.0));
    let err = a.px.iter().zip(&b.px).map(|(x, y)| (enc(*x) - enc(*y)).abs()).fold(0f32, f32::max);
    assert!(err < 0.03, "LUT vs direct max display error {err}");
    let plain = render_clip(
        &s,
        &{
            let mut p = (*s.project).clone();
            p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1.effects.retain(|e| e.effect != "lumetri");
            p
        },
        clip,
    );
    assert!(a.px.iter().zip(&plain.px).map(|(x, y)| (enc(*x) - enc(*y)).abs()).fold(0f32, f32::max) > 0.05, "the grade is visible");
}

#[test]
fn bake_lut_hostile_params() {
    let mut s = demo();
    let dir = tmp_dir("bake-hostile");
    s.style.set_dir(&dir);
    let clip = pick_clip(&mut s).0;
    s.execute("effects.apply", json!({"clips": [clip], "effect": "lumetri"})).unwrap();
    let rev = s.revision;
    let long = "n".repeat(65);
    for p in [
        json!({}),
        json!({"clip": clip}),
        json!({"name": "x"}),
        json!({"clip": 999_999, "name": "x"}),
        json!({"clip": u64::MAX, "name": "x"}),
        json!({"clip": clip, "name": "x", "size": 0}),
        json!({"clip": clip, "name": "x", "size": 1}),
        json!({"clip": clip, "name": "x", "size": 16}),
        json!({"clip": clip, "name": "x", "size": 1e6}),
        json!({"clip": clip, "name": "x", "size": -33}),
        json!({"clip": clip, "name": "x", "size": "NaN"}),
        json!({"clip": clip, "name": "../escape"}),
        json!({"clip": clip, "name": "a/b"}),
        json!({"clip": clip, "name": ""}),
        json!({"clip": clip, "name": long}),
        json!({"clip": clip, "name": 5}),
    ] {
        assert!(s.execute("lumetri.bakeLut", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.revision, rev);
    assert!(!dir.join("escape.cube").exists() && !dir.join("luts").join("escape.cube").exists());
    // an audio clip can't be baked
    let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    assert!(s.execute("lumetri.bakeLut", json!({"clip": audio, "name": "x"})).is_err());
    // no data directory: an error (unit tests never use the user's)
    let mut t = demo();
    let c = pick_clip(&mut t).0;
    t.execute("effects.apply", json!({"clips": [c], "effect": "lumetri"})).unwrap();
    assert!(t.execute("lumetri.bakeLut", json!({"clip": c, "name": "x"})).is_err());
}
