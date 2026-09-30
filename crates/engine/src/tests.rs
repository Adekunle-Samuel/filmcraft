use super::*;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip_ids(s: &Session, track: usize) -> Vec<u64> {
    s.active_sequence().unwrap().video_tracks[track].items.iter().map(|i| i.id.0).collect()
}

#[test]
fn demo_project_is_valid_and_renders() {
    let s = demo();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 6);
    assert!(q.duration() > Tick::ZERO);
    let img = s.render_program(0.125).unwrap();
    assert_eq!((img.w, img.h), (240, 135));
    assert!(img.px.chunks(4).any(|p| p[3] > 0.9));
}

#[test]
fn add_edit_undo_redo() {
    let mut s = demo();
    let before = clip_ids(&s, 0).len();
    s.execute("playhead.set", json!({"seconds": 2.0})).unwrap();
    s.execute("sequence.addEditAllTracks", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip_ids(&s, 0).len(), before + 1);
    assert!(s.execute("edit.redo", json!({})).is_err(), "disabled when nothing to redo");
}

#[test]
fn source_insert_three_point() {
    let mut s = demo();
    let item = s
        .project
        .root
        .children
        .iter()
        .find_map(|c| {
            if let filmcraft_project::BinEntry::Bin(b) = c {
                b.children.first().and_then(|e| if let filmcraft_project::BinEntry::Item(i) = e { Some(*i) } else { None })
            } else {
                None
            }
        })
        .unwrap();
    s.execute("source.open", json!({"item": item.0})).unwrap();
    let r = s.sequence_rate();
    s.execute("project.setMarks", json!({"item": item.0, "in": r.tick_of(24).0, "out": r.tick_of(47).0})).unwrap();
    s.execute("playhead.set", json!({"frame": 0})).unwrap();
    let dur0 = s.active_sequence().unwrap().duration();
    s.execute("source.insert", json!({})).unwrap();
    let dur1 = s.active_sequence().unwrap().duration();
    assert_eq!(dur1 - dur0, r.tick_of(24), "one second inserted, sequence rippled");
    assert_eq!(s.playhead(), r.tick_of(24), "playhead moves to end of edit");
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn trim_linked_and_ripple_delete() {
    let mut s = demo();
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    s.execute("timeline.trim", json!({"clip": first.id.0, "edge": "out", "mode": "regular", "deltaFrames": -12})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items[0].duration, first.duration - s.sequence_rate().tick_of(12));
    let a = q.audio_tracks[0].items.iter().find(|i| i.link == first.link).unwrap();
    assert_eq!(a.duration, q.video_tracks[0].items[0].duration, "linked audio trimmed too");
    s.execute("timeline.select", json!({"clips": [first.id.0]})).unwrap();
    assert_eq!(s.state.selection.len(), 2, "linked selection adds audio");
    assert!(s.execute("edit.rippleDelete", json!({})).is_err(), "music on sync-locked A2 blocks the ripple");
    s.execute("timeline.setTrack", json!({"track": "A2", "syncLock": false})).unwrap();
    s.execute("edit.rippleDelete", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items[0].start, s.sequence_rate().tick_of(12), "the trim gap remains");
}

#[test]
fn effects_and_keyframes() {
    let mut s = demo();
    let c = s.active_sequence().unwrap().video_tracks[0].items[1].id.0;
    s.execute("effects.apply", json!({"clips": [c], "effect": "Gaussian Blur"})).unwrap();
    s.execute("effects.setParam", json!({"clip": c, "effect": "gaussian_blur", "param": "blurriness", "value": 25.0})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.find_item(filmcraft_project::ClipId(c)).unwrap().1;
    assert_eq!(it.effects[0].effect, "gaussian_blur", "standard effects render before intrinsics");
    assert_eq!(it.effects[0].params["blurriness"].value, filmcraft_project::ParamValue::Float(25.0));
    s.execute("effects.toggleAnimation", json!({"clip": c, "effect": "motion", "param": "scale"})).unwrap();
    let it = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(c)).unwrap().1.clone();
    assert!(it.effect("motion").unwrap().params["scale"].is_animated());
}

#[test]
fn transitions_and_markers() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let cut = q.video_tracks[0].items[2].start;
    s.execute("playhead.set", json!({"time": cut.0})).unwrap();
    s.execute("sequence.applyVideoTransition", json!({"effect": "wipe"})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].transitions.iter().any(|t| t.effect.effect == "wipe"));
    let n = q.markers.len();
    s.execute("markers.add", json!({"name": "Test"})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), n + 1);
}

#[test]
fn commands_are_unique_and_described() {
    let mut ids = std::collections::HashSet::new();
    for c in command_specs() {
        assert!(ids.insert(c.id), "duplicate {}", c.id);
    }
    assert!(command_specs().len() > 90, "{}", command_specs().len());
    let s = demo();
    let list = commands::inspect_project(&s);
    assert!(list["root"]["children"].as_array().unwrap().len() >= 4);
}

#[test]
fn save_and_open_roundtrip() {
    let dir = std::env::temp_dir().join(format!("fc-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("demo.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    assert!(!s.is_dirty());
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(*t.project, *s.project);
    // media re-created from generator refs
    assert!(t.render_program(0.1).is_some());
    std::fs::remove_dir_all(dir).ok();
}
