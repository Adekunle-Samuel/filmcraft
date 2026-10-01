use serde_json::json;

use crate::Session;
use filmcraft_project::graphic::{LayerContent, eval_layer, layer_indices};
use filmcraft_project::{ClipId, ItemKind};
use filmcraft_time::Tick;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn layers(s: &Session, clip: ClipId) -> Vec<filmcraft_project::graphic::LayerSpec> {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    layer_indices(&it.effects).into_iter().filter_map(|i| eval_layer(&it.effects[i], Tick::ZERO, (q.settings.width, q.settings.height))).collect()
}

fn text_of(l: &filmcraft_project::graphic::LayerSpec) -> String {
    match &l.content {
        LayerContent::Text(t) => t.text.clone(),
        _ => panic!("not text"),
    }
}

#[test]
fn new_text_makes_a_graphic_clip_above_the_footage() {
    let mut s = demo();
    let t = s.playhead();
    let r = s.execute("graphics.newText", json!({"text": "Hello", "position": [200, 300]})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (tid, it) = q.find_item(clip).unwrap();
    let track_index = q.video_tracks.iter().position(|tr| tr.id == tid).unwrap();
    let below_busy = q.video_tracks[..track_index].iter().any(|tr| tr.item_at(t).is_some());
    assert!(below_busy || track_index == 0, "placed above the clip under the playhead");
    assert_eq!(it.name, "Hello");
    assert!(matches!(s.project.item(it.item).unwrap().kind, ItemKind::Graphic { .. }));
    assert_eq!(it.duration, q.settings.frame_rate.snap_nearest(Tick::from_seconds_f64(5.0)));
    assert!(!it.has_standard_effects(), "layers are not fx");
    assert_eq!(s.state.selection, vec![clip]);
    let ls = layers(&s, clip);
    assert_eq!(ls.len(), 1);
    assert_eq!(text_of(&ls[0]), "Hello");
    assert_eq!((ls[0].transform.position.x, ls[0].transform.position.y), (200.0, 300.0));

    // a second text layer in the same clip, then a shape
    let r2 = s.execute("graphics.newText", json!({"text": "World", "clip": clip.0})).unwrap();
    assert_eq!(r2["layer"], 1);
    s.execute("graphics.newShape", json!({"shape": "ellipse", "clip": clip.0, "size": [100, 50]})).unwrap();
    assert_eq!(layers(&s, clip).len(), 3);
    let list = s.execute("graphics.list", json!({"clip": clip.0})).unwrap();
    assert_eq!(list["layers"][2]["kind"], "Ellipse");
    assert_eq!(list["layers"][1]["name"], "World");

    // undo removes the shape layer again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layers(&s, clip).len(), 2);
}

#[test]
fn typing_coalesces_into_one_undo_step_and_props_set() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": ""})).unwrap();
    let clip = ClipId(r["clip"].as_u64().unwrap());
    let before = s.history.undo.len();
    for (i, t) in ["T", "Ti", "Tit", "Titl", "Title"].iter().enumerate() {
        s.execute("graphics.setText", json!({"clip": clip.0, "layer": 0, "text": t, "merge": i > 0})).unwrap();
    }
    assert_eq!(s.history.undo.len(), before + 1, "one undo step for the typing session");
    assert_eq!(text_of(&layers(&s, clip)[0]), "Title");
    s.execute("graphics.set", json!({"clip": clip.0, "props": {"font_style": "Bold", "fontSize": 140, "align": "center", "fill_color": "#ffcc00", "stroke": true, "caps": "small caps"}})).unwrap();
    let l = &layers(&s, clip)[0];
    let LayerContent::Text(t) = &l.content else { panic!() };
    assert_eq!((t.style.as_str(), t.size, t.align, t.caps), ("Bold", 140.0, 1, 2));
    assert_eq!(l.appearance.strokes.len(), 1);
    assert!((l.appearance.fill.unwrap()[1] - 0.8).abs() < 0.01);
    assert!(s.execute("graphics.set", json!({"clip": clip.0, "props": {"nope": 1}})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(text_of(&layers(&s, clip)[0]), "");
}

#[test]
fn align_and_distribute_layers() {
    let mut s = demo();
    let (w, h) = {
        let q = s.active_sequence().unwrap();
        (q.settings.width as f64, q.settings.height as f64)
    };
    let r = s.execute("graphics.newShape", json!({"shape": "rectangle", "position": [300, 200], "size": [100, 60]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": clip, "position": [500, 400], "size": [40, 40]})).unwrap();
    s.execute("graphics.newShape", json!({"shape": "rectangle", "clip": clip, "position": [1500, 900], "size": [40, 40]})).unwrap();
    s.execute("graphics.align", json!({"clip": clip, "layers": [0], "align": "left"})).unwrap();
    s.execute("graphics.align", json!({"clip": clip, "layers": [0], "align": "bottom"})).unwrap();
    let l = layers(&s, ClipId(clip));
    assert!((l[0].transform.position.x - 50.0).abs() < 1e-6, "left edge on the frame edge");
    assert!((l[0].transform.position.y - (h - 30.0)).abs() < 1e-6);
    s.execute("graphics.align", json!({"clip": clip, "layers": [1, 2], "align": "vcenter", "to": "selection"})).unwrap();
    let l = layers(&s, ClipId(clip));
    assert!((l[1].transform.position.y - l[2].transform.position.y).abs() < 1e-6);
    s.execute("graphics.distribute", json!({"clip": clip, "layers": [0, 1, 2], "axis": "horizontal"})).unwrap();
    let l = layers(&s, ClipId(clip));
    let xs: Vec<f64> = l.iter().map(|x| x.transform.position.x).collect();
    assert!(((xs[1] - xs[0]) - (xs[2] - xs[1])).abs() < 1e-6, "{xs:?}");
    let _ = w;
}

#[test]
fn graphic_renders_in_the_program_and_survives_save() {
    let mut s = demo();
    let r = s.execute("graphics.newText", json!({"text": "BIG", "size": 300, "position": [100, 500]})).unwrap();
    let clip = r["clip"].as_u64().unwrap();
    s.execute("graphics.set", json!({"clip": clip, "props": {"fill_color": "#ff0000"}})).unwrap();
    let img = s.render_program(0.25).unwrap();
    let red = img.px.chunks(4).filter(|p| p[0] > 0.9 && p[1] < 0.05 && p[2] < 0.05).count();
    assert!(red > 200, "red text in the program: {red}");
    let dir = std::env::temp_dir().join(format!("fc-gfx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("g.fcproj");
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    let mut s2 = Session::default();
    s2.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(s2.project.items, s.project.items, "graphic clips round-trip");
    let _ = std::fs::remove_dir_all(&dir);
    let fonts = s.execute("fonts.list", json!({"system": false})).unwrap();
    assert!(fonts.as_array().unwrap().iter().any(|f| f["family"] == "Inter"));
}
