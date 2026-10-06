//! Track parameters (`track`, `audioTrack`: an id, or "V1" / "A2"): a track the active sequence
//! does not have is a parameter error, never a silent default.

use serde_json::json;

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// `timeline.place {audioTrack: "A2"}` on a sequence with only A1 used to fall back to the
/// source-patched track and overwrite what was on A1.
#[test]
fn naming_a_missing_track_is_an_error_not_a_default() {
    let mut s = demo();
    let seq = s.execute("file.newSequence", json!({"name": "One of each", "video": 1, "audio": 1})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (1, 1), "{seq}");
    let (v1, a1) = (q.video_tracks[0].id, q.audio_tracks[0].id);
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    // omitted tracks keep their default: the source-patched V1 / A1
    s.execute("timeline.place", json!({"item": item.0, "time": 0})).unwrap();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    for (key, name) in [("audioTrack", json!("A2")), ("track", json!("V2")), ("audioTrack", json!(999_999)), ("track", json!("V0")), ("track", json!("X1"))] {
        let mut p = json!({"item": item.0, "time": 0});
        p[key] = name.clone();
        let e = s.execute("timeline.place", p).unwrap_err();
        assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.place"), "a parameter error of the command: {e:?}");
        let e = e.to_string();
        let shown = name.as_str().map(str::to_string).unwrap_or_else(|| name.to_string());
        assert!(e.contains(&format!("no track {shown} in this sequence")), "{e}");
    }
    assert!(s.execute("timeline.place", json!({"item": item.0, "time": 0, "audioTrack": "A2"})).unwrap_err().to_string().contains("audioTrack"));
    // hostile names are errors too, never a panic
    for name in ["", "V", "é1", "1", "V-1", "A1.5", "V99999999999999999999"] {
        assert!(s.execute("timeline.place", json!({"item": item.0, "time": 0, "track": name})).is_err(), "{name:?}");
    }
    // every command that takes a track by name refuses one that is not there
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let e = s.execute("timeline.move", json!({"moves": [{"clip": clip.0, "track": "V2", "time": 0}]})).unwrap_err();
    assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.move"), "{e:?}");
    assert!(s.execute("sequence.closeGap", json!({"track": "A2", "time": 0})).is_err());
    assert!(s.execute("sequence.deleteTrack", json!({"track": "V2"})).is_err());
    assert!(s.execute("sequence.deleteTrack", json!({"track": 999_999})).is_err());
    assert!(s.execute("timeline.setTrack", json!({"track": "A2", "muted": true})).is_err());
    assert!(s.execute("timeline.setTargeting", json!({"track": "V2", "targeted": true})).is_err());
    assert!(s.execute("timeline.razor", json!({"track": "A2", "time": 10})).is_err());
    assert!(s.execute("sequence.goToNextGapInTrack", json!({"track": "V2"})).is_err());
    assert_eq!(*s.project, *before, "nothing was edited");
    assert_eq!(s.history.undo.len(), undo, "and nothing was added to the undo history");
    // tracks that exist still resolve by name (either case) and by id
    s.execute("timeline.place", json!({"item": item.0, "seconds": 60.0, "track": "v1", "audioTrack": a1.0})).unwrap();
    s.execute("timeline.setTrack", json!({"track": v1.0, "locked": true})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "a1", "muted": true})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].locked && q.audio_tracks[0].muted);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.active_sequence().unwrap().audio_tracks[0].muted);
}
