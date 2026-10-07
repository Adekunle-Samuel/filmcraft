//! The Timeline's sequence tabs: every sequence has its own view (zoom, scroll, track heights);
//! closing the other tabs; reordering. Premiere's behaviour was observed in Premiere Pro 26.5.2.

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
fn close_others_keeps_one_tab_and_shows_it() {
    let (mut s, [main, b, c]) = three_tabs();
    // from the active tab
    assert_eq!(s.execute("sequence.closeOthers", json!({})).unwrap()["closed"], json!(2));
    assert_eq!((s.state.open_sequences.clone(), s.state.active_sequence), (vec![c], Some(c)));
    // from another tab: that one becomes the active one
    let (mut s, _) = three_tabs();
    s.drain_events();
    s.execute("sequence.closeOthers", json!({"item": b.0})).unwrap();
    assert_eq!((s.state.open_sequences.clone(), s.state.active_sequence), (vec![b], Some(b)));
    assert!(s.drain_events().contains(&Event::OpenSequence(b)));
    // a sequence that is not open cannot be the one to keep
    s.execute("sequence.close", json!({})).unwrap();
    assert!(s.execute("sequence.closeOthers", json!({"item": main.0})).is_err());
}

#[test]
fn a_tab_moves_to_another_place_among_the_tabs() {
    let (mut s, [main, b, c]) = three_tabs();
    let (revision, steps) = (s.revision, s.history.undo.len());
    s.execute("sequence.moveTab", json!({"item": c.0, "index": 0})).unwrap();
    assert_eq!(s.state.open_sequences, [c, main, b]);
    assert_eq!(s.state.active_sequence, Some(c), "moving a tab does not change which is shown");
    s.execute("sequence.moveTab", json!({"item": c.0, "index": 1})).unwrap();
    assert_eq!(s.state.open_sequences, [main, c, b]);
    // past the end is the end; the active tab is the default
    assert_eq!(s.execute("sequence.moveTab", json!({"index": u64::MAX})).unwrap()["index"], json!(2));
    assert_eq!(s.state.open_sequences, [main, b, c]);
    // not an undo step, not an edit
    assert_eq!((s.revision, s.history.undo.len()), (revision, steps));
    s.execute("sequence.close", json!({"item": b.0})).unwrap();
    assert!(s.execute("sequence.moveTab", json!({"item": b.0, "index": 0})).is_err());
    assert!(s.execute("sequence.moveTab", json!({"item": c.0})).is_err());
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
