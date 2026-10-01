use serde_json::json;

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;

fn demo() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].items[0].clone();
    // middle of the first V1 clip (past its fade from black)
    s.set_playhead(it.start + Tick(it.duration.0 / 2));
    s.state.selection = vec![it.id];
    (s, it.id)
}

fn px(img: &filmcraft_render::Image, fx: f32, fy: f32) -> [f32; 4] {
    img.get(((img.w as f32 * fx) as usize).min(img.w - 1), ((img.h as f32 * fy) as usize).min(img.h - 1))
}

fn dist(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn masked_effect_applies_only_inside_the_mask() {
    let (mut s, clip) = demo();
    let plain = s.render_program(0.25).unwrap();
    s.execute("effects.apply", json!({"effect": "invert"})).unwrap();
    let inverted = s.render_program(0.25).unwrap();
    // ellipse mask on Invert, in the left half of the frame
    let r = s.execute("masks.add", json!({"effect": "invert", "shape": "ellipse", "center": [480, 540], "size": [600, 600]})).unwrap();
    assert_eq!(r["name"], "Mask (1)");
    assert_eq!(r["mask"], 0);
    let masked = s.render_program(0.25).unwrap();
    // inside: inverted; outside: untouched
    assert!(dist(px(&masked, 0.25, 0.5), px(&inverted, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&masked, 0.8, 0.5), px(&plain, 0.8, 0.5)) < 1e-4);
    assert!(dist(px(&plain, 0.25, 0.5), px(&inverted, 0.25, 0.5)) > 0.05, "invert changes the picture");
    // inverted mask swaps the regions
    s.execute("masks.set", json!({"clip": clip.0, "effect": "invert", "mask": 0, "inverted": true})).unwrap();
    let inv = s.render_program(0.25).unwrap();
    assert!(dist(px(&inv, 0.25, 0.5), px(&plain, 0.25, 0.5)) < 1e-4);
    assert!(dist(px(&inv, 0.8, 0.5), px(&inverted, 0.8, 0.5)) < 1e-4);
    // undo ×2 → no mask
    s.undo();
    s.undo();
    let back = s.render_program(0.25).unwrap();
    assert_eq!(back.px, inverted.px);
    assert!(s.state.selected_mask.is_none(), "selection follows undo");
}

#[test]
fn opacity_mask_cuts_out_the_clip() {
    let (mut s, clip) = demo();
    // V1 only: an opacity mask leaves black outside
    s.execute("masks.add", json!({"clip": clip.0, "shape": "polygon", "center": [960, 540], "size": [960, 540]})).unwrap();
    s.execute("masks.set", json!({"feather": 0})).unwrap();
    let img = s.render_program(0.25).unwrap();
    assert!(px(&img, 0.5, 0.5)[3] > 0.99, "inside opaque");
    let corner = px(&img, 0.05, 0.05);
    assert!(corner[0] + corner[1] + corner[2] < 1e-4, "outside cut: {corner:?}");
    let list = s.execute("masks.list", json!({})).unwrap();
    let m = &list["masks"][0];
    assert_eq!(m["effectId"], "opacity");
    assert_eq!(m["path"]["vertices"].as_array().unwrap().len(), 4);
    assert_eq!(m["selected"], true);
}

#[test]
fn keyframed_mask_path_interpolates() {
    let (mut s, clip) = demo();
    let it = s.active_sequence().unwrap().find_item(clip).unwrap().1.clone();
    s.set_playhead(it.start);
    s.execute("masks.add", json!({"effect": "opacity", "shape": "polygon", "center": [400, 400], "size": [200, 200]})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "path"})).unwrap();
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    let t1 = it.start + rate.tick_of(10);
    s.set_playhead(t1);
    s.execute("masks.translate", json!({"delta": [100, -50]})).unwrap();
    // feather keyframes too, through the generic keyframe command
    s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather"})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather", "value": 30.0})).unwrap();
    let mid = it.start + rate.tick_of(5);
    let l = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    let v0 = &l["masks"][0]["path"]["vertices"][0]["p"];
    assert!((v0[0].as_f64().unwrap() - 350.0).abs() < 1e-6, "{v0}");
    assert!((v0[1].as_f64().unwrap() - 275.0).abs() < 1e-6, "{v0}");
    assert_eq!(l["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    let end = s.execute("masks.list", json!({"time": t1.0})).unwrap();
    assert_eq!(end["masks"][0]["feather"], 30.0);
    // vertex edit at a keyframe replaces that keyframe; handles mirror unless broken
    s.execute("masks.moveVertex", json!({"vertex": 1, "handle": "out", "delta": [0, 40]})).unwrap();
    let e = s.execute("masks.list", json!({})).unwrap();
    let v1 = &e["masks"][0]["path"]["vertices"][1];
    assert_eq!(v1["out"], json!([0.0, 40.0]));
    assert_eq!(v1["in"], json!([-0.0, -40.0]));
    assert_eq!(e["masks"][0]["pathKeyframes"].as_array().unwrap().len(), 2);
    // add / remove vertices keep all keyframes interpolable
    s.execute("masks.addVertex", json!({"after": 0, "at": [500, 220]})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 5);
    s.execute("masks.removeVertex", json!({"vertex": 1})).unwrap();
    let a = s.execute("masks.list", json!({"time": mid.0})).unwrap();
    assert_eq!(a["masks"][0]["path"]["vertices"].as_array().unwrap().len(), 4);
}

#[test]
fn merged_drag_is_one_undo_step_and_project_round_trips() {
    let (mut s, _) = demo();
    s.execute("effects.apply", json!({"effect": "gaussian_blur"})).unwrap();
    s.execute("masks.add", json!({"effect": "gaussian_blur", "shape": "ellipse"})).unwrap();
    let n = s.history.undo.len();
    for _ in 0..5 {
        s.execute("masks.translate", json!({"delta": [3, 1], "merge": "drag-1"})).unwrap();
    }
    assert_eq!(s.history.undo.len(), n + 1);
    s.execute("masks.set", json!({"mode": "subtract", "trackMethod": "position", "expansion": -4})).unwrap();
    let bytes = filmcraft_format::encode(&s.project, false);
    let back = filmcraft_format::decode(&bytes).unwrap().project;
    assert_eq!(&back, &*s.project);
    let l = s.execute("masks.list", json!({})).unwrap();
    assert_eq!(l["masks"][0]["mode"], "Subtract");
    assert_eq!(l["masks"][0]["trackMethod"], "Position");
    assert!(s.execute("masks.add", json!({"effect": "motion"})).is_err(), "Motion has no masks");
}
