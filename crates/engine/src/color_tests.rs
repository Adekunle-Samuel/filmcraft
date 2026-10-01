use serde_json::json;

use crate::Session;
use filmcraft_color::{Lut, Lut3d};
use filmcraft_project::{ClipId, ParamValue};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The first V1 clip under the playhead, selected.
fn pick_clip(s: &mut Session) -> ClipId {
    let q = s.active_sequence().unwrap();
    let t = s.playhead();
    let c = q.video_tracks[0].item_at(t).or(q.video_tracks[0].items.first()).unwrap();
    let id = c.id;
    s.set_playhead(c.start + filmcraft_time::Tick(1000));
    s.state.selection = vec![id];
    id
}

fn lumetri_text(s: &Session, clip: ClipId, p: &str) -> String {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    let e = it.effects.iter().find(|e| e.effect == "lumetri").unwrap();
    match e.param(p).map(|v| &v.value) {
        Some(ParamValue::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn mean(img: &filmcraft_render::Image) -> [f32; 3] {
    let mut m = [0f64; 3];
    for p in img.px.chunks_exact(4) {
        for k in 0..3 {
            m[k] += p[k] as f64;
        }
    }
    let n = (img.px.len() / 4) as f64;
    m.map(|v| (v / n) as f32)
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-color-{}-{name}", std::process::id()));
    d.to_string_lossy().to_string()
}

#[test]
fn import_lut_set_input_and_look_then_undo() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    let before = mean(&s.render_program(0.125).unwrap());
    // a LUT that swaps red and blue
    let path = tmp("swap.cube");
    std::fs::write(&path, Lut::from_cube(Lut3d::from_fn(17, |c| [c[2], c[1], c[0]])).to_cube()).unwrap();
    let r = s.execute("lumetri.setInputLut", json!({"path": path})).unwrap();
    let lref = r["lut"].as_str().unwrap().to_string();
    assert!(lref.starts_with("lib:"));
    assert_eq!(s.project.luts.len(), 1);
    assert_eq!(lumetri_text(&s, clip, "input_lut"), lref);
    let after = mean(&s.render_program(0.125).unwrap());
    assert!((after[0] - before[2]).abs() < 0.02 && (after[2] - before[0]).abs() < 0.02, "{before:?} → {after:?}");
    // importing the same file again reuses the library entry
    let again = s.execute("lut.import", json!({"path": path})).unwrap();
    assert_eq!(again["reused"], true);
    // Input LUT section switch: Basic Correction off bypasses it
    s.execute("lumetri.setSection", json!({"section": "basic", "on": false})).unwrap();
    let off = mean(&s.render_program(0.125).unwrap());
    assert!((off[0] - before[0]).abs() < 0.01, "{off:?} vs {before:?}");
    s.execute("lumetri.setSection", json!({"section": "basic"})).unwrap();
    // built-in look by reference
    s.execute("lumetri.setLook", json!({"lut": "builtin:look-monochrome"})).unwrap();
    let mono = mean(&s.render_program(0.125).unwrap());
    assert!((mono[0] - mono[2]).abs() < 0.03, "{mono:?}");
    assert!(s.execute("lumetri.setLook", json!({"lut": "lib:nope"})).is_err());
    // the library and the references survive save/load
    let lib = s.execute("lut.list", json!({})).unwrap();
    assert_eq!(lib["library"].as_array().unwrap().len(), 1);
    assert!(lib["builtin"].as_array().unwrap().iter().any(|b| b["ref"] == "builtin:slog3-sgamut3cine-to-rec709"));
    let js = serde_json::to_string(&*s.project).unwrap();
    let back: filmcraft_project::Project = serde_json::from_str(&js).unwrap();
    assert_eq!(back.luts, s.project.luts);
    // undo the look, the section toggles and the input LUT
    for _ in 0..4 {
        s.execute("edit.undo", json!({})).unwrap();
    }
    assert_eq!(lumetri_text(&s, clip, "look_lut"), "");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn export_builtin_and_library_luts() {
    let mut s = demo();
    for (fmt, ext) in [("cube", "cube"), ("3dl", "3dl")] {
        let path = tmp(&format!("slog3.{ext}"));
        s.execute("lut.export", json!({"lut": "builtin:slog3-sgamut3cine-to-rec709", "path": path, "format": fmt})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let back = Lut::parse(&text, filmcraft_color::LutFormat::from_path(&path)).unwrap();
        let orig = filmcraft_render::luts::resolve(None, "builtin:slog3-sgamut3cine-to-rec709").unwrap();
        for c in [[0.4, 0.4, 0.4], [0.6, 0.3, 0.2]] {
            let (a, b) = (orig.apply(c), back.apply(c));
            assert!((0..3).all(|k| (a[k] - b[k]).abs() < 2e-3), "{fmt}: {a:?} vs {b:?}");
        }
        // round trip through the library
        let r = s.execute("lut.import", json!({"path": path})).unwrap();
        assert!(r["ref"].as_str().unwrap().starts_with("lib:"));
        let _ = std::fs::remove_file(&path);
    }
    assert_eq!(s.project.luts.len(), 2);
    let id = s.project.luts[0].id.clone();
    s.execute("lut.remove", json!({"id": id})).unwrap();
    assert_eq!(s.project.luts.len(), 1);
    assert!(s.execute("lut.import", json!({"path": "/nonexistent.cube"})).is_err());
}

#[test]
fn set_param_fills_in_parameters_missing_from_old_instances() {
    let mut s = demo();
    let clip = pick_clip(&mut s);
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"})).unwrap();
    // simulate an instance saved before the section switches existed
    s.edit_sequence("strip", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).unwrap();
        let e = it.effects.iter_mut().find(|e| e.effect == "lumetri").unwrap();
        e.params.remove("vignette_on");
        Ok(())
    })
    .unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "lumetri", "param": "vignette_on", "value": false})).unwrap();
}
