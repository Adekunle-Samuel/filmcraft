//! The Timeline's sequence tabs: every sequence has its own view (zoom, scroll, track heights).
//! Premiere's behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_project::SequenceView;
use serde_json::json;

/// The demo project with two more sequences open: tabs [main, b, c], `c` active.
fn three_tabs() -> (Session, [ItemId; 3]) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let main = s.state.active_sequence.unwrap();
    let mut new = |name: &str| ItemId(s.execute("file.newSequence", json!({"name": name})).unwrap()["sequence"].as_u64().unwrap());
    let (b, c) = (new("B"), new("C"));
    assert_eq!(s.state.open_sequences, [main, b, c]);
    assert_eq!(s.state.active_sequence, Some(c));
    (s, [main, b, c])
}

fn view(pps: f64, scroll: f64) -> SequenceView {
    SequenceView { pps, scroll, v_scroll: 12.0, a_scroll: 7.0, video_track_h: 90.0, audio_track_h: 40.0 }
}

#[test]
fn the_view_of_a_deleted_sequence_is_forgotten() {
    let (mut s, [main, b, c]) = three_tabs();
    for id in [main, b, c] {
        s.state.timeline_views.insert(id, view(40.0, 0.0));
    }
    s.execute("project.delete", json!({"items": [b.0]})).unwrap();
    assert_eq!(s.state.timeline_views.keys().copied().collect::<Vec<_>>(), [main, c]);
    assert_eq!(s.state.open_sequences, [main, c]);
}
