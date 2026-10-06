//! Editing with nested sequences: the nest toggle ("Insert and overwrite sequences as nests or
//! individual clips").
//! Premiere's behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_project::{TrackItem, TrackKind};
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clips(s: &Session, kind: TrackKind, idx: usize) -> Vec<TrackItem> {
    s.active_sequence().unwrap().tracks(kind)[idx].items.clone()
}

/// A subsequence of the first three V1 clips of the demo (with their sound and the transitions
/// between them), and a new empty sequence, which is left active.
fn source_and_empty(s: &mut Session) -> (ItemId, ItemId) {
    let v1: Vec<u64> = clips(s, TrackKind::Video, 0).iter().take(3).map(|c| c.id.0).collect();
    s.execute("timeline.select", json!({"clips": v1})).unwrap();
    let source = ItemId(s.execute("sequence.makeSubsequence", json!({"name": "Source"})).unwrap()["sequence"].as_u64().unwrap());
    let empty = ItemId(s.execute("file.newSequence", json!({"name": "Target"})).unwrap()["sequence"].as_u64().unwrap());
    s.execute("sequence.open", json!({"item": empty.0})).unwrap();
    (source, empty)
}

#[test]
fn the_nest_toggle_is_on_by_default_and_can_be_set() {
    let mut s = demo();
    assert!(!s.state.sequences_as_clips);
    assert_eq!(s.execute("sequence.nestSequences", json!({})).unwrap()["nest"], false);
    assert!(s.state.sequences_as_clips);
    assert_eq!(s.execute("sequence.nestSequences", json!({})).unwrap()["nest"], true);
    assert_eq!(s.execute("sequence.nestSequences", json!({"on": false})).unwrap()["nest"], false);
    assert_eq!(s.execute("sequence.nestSequences", json!({"on": false})).unwrap()["nest"], false);
    assert!(s.state.sequences_as_clips);
}

#[test]
fn with_the_toggle_on_a_sequence_edits_in_as_one_nest() {
    let mut s = demo();
    let (source, _) = source_and_empty(&mut s);
    let r = s.execute("timeline.place", json!({"item": source.0, "seconds": 0.0})).unwrap();
    assert_eq!(r["clips"].as_array().unwrap().len(), 2, "one picture and one sound clip");
    let v1 = clips(&s, TrackKind::Video, 0);
    assert_eq!(v1.len(), 1);
    assert_eq!(v1[0].item, source);
}

#[test]
fn with_the_toggle_off_a_sequence_edits_in_as_its_clips_with_their_transitions() {
    let mut s = demo();
    let (source, _) = source_and_empty(&mut s);
    let src = s.project.sequence(source).unwrap().clone();
    let (src_v, src_a) = (src.video_tracks[0].clone(), src.audio_tracks[0].clone());
    assert!(src_v.items.len() == 3 && !src_v.transitions.is_empty(), "the source has transitions to carry");
    s.execute("sequence.nestSequences", json!({"on": false})).unwrap();
    let undo = s.history.undo.len();
    let at = s.sequence_rate().tick_of(48);
    s.execute("timeline.place", json!({"item": source.0, "time": at.0})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let (v, a) = (&q.video_tracks[0], &q.audio_tracks[0]);
    // the same clips, at the same places from `at`, as copies with ids of their own
    assert_eq!(v.items.len(), 3);
    for (new, old) in v.items.iter().zip(&src_v.items).chain(a.items.iter().zip(&src_a.items)) {
        assert_eq!((new.item, new.start, new.duration, new.source_in), (old.item, old.start + at, old.duration, old.source_in));
        assert_ne!(new.id, old.id);
    }
    assert!(v.items.iter().all(|i| s.project.sequence(i.item).is_none()), "no nest");
    // picture and sound stay linked pair by pair, with links of their own
    for (pic, snd) in v.items.iter().zip(&a.items) {
        assert!(pic.link.is_some() && pic.link == snd.link);
    }
    assert!(v.items.iter().zip(&src_v.items).all(|(n, o)| n.link != o.link));
    // the transitions came along, between the copies
    assert_eq!((v.transitions.len(), a.transitions.len()), (src_v.transitions.len(), src_a.transitions.len()));
    for (new, old) in v.transitions.iter().zip(&src_v.transitions) {
        assert_eq!((new.start, new.duration), (old.start + at, old.duration));
        assert!(new.id != old.id && new.from.is_none_or(|c| v.item(c).is_some()) && new.to.is_none_or(|c| v.item(c).is_some()));
    }
    // the copies are what is selected, and the source sequence is untouched
    assert_eq!(s.state.selection.len(), v.items.len() + a.items.len());
    assert_eq!(s.project.sequence(source).unwrap(), &src);
}

#[test]
fn source_tracks_with_clips_go_to_consecutive_tracks_and_missing_tracks_are_added() {
    let mut s = demo();
    let (source, target) = source_and_empty(&mut s);
    // in the source, put the third clip's picture on V3 (V2 stays empty)
    s.execute("sequence.open", json!({"item": source.0})).unwrap();
    let third = clips(&s, TrackKind::Video, 0)[2].clone();
    s.execute("timeline.move", json!({"moves": [{"clip": third.id.0, "track": "V3", "time": third.start.0}], "linked": false})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[2].items.len(), 1);
    s.execute("sequence.open", json!({"item": target.0})).unwrap();
    s.execute("sequence.nestSequences", json!({"on": false})).unwrap();
    // dropped on V1: source V1 and V3 land on V1 and V2
    s.execute("timeline.place", json!({"item": source.0, "track": "V1", "seconds": 0.0})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!((q.video_tracks[0].items.len(), q.video_tracks[1].items.len(), q.video_tracks[2].items.len()), (2, 1, 0));
    assert_eq!(q.video_tracks[1].items[0].item, third.item);
    // dropped on the top track, V3: the second lane needs a V4, which is added
    assert_eq!(q.video_tracks.len(), 3);
    s.execute("timeline.place", json!({"item": source.0, "track": "V3", "seconds": 60.0})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!(q.video_tracks.len(), 4);
    assert_eq!((q.video_tracks[2].items.len(), q.video_tracks[3].items.len()), (2, 1));
    assert_eq!(q.video_tracks[3].name, "Video 4");
    // one undo removes the clips and the track again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3);
}

#[test]
fn a_sequence_can_be_edited_into_itself_as_clips_but_not_as_a_nest() {
    let mut s = demo();
    let main = s.state.active_sequence.unwrap();
    let before = clips(&s, TrackKind::Video, 0).len();
    assert!(s.execute("timeline.place", json!({"item": main.0, "seconds": 120.0})).is_err());
    s.execute("sequence.nestSequences", json!({"on": false})).unwrap();
    s.execute("timeline.place", json!({"item": main.0, "seconds": 120.0})).expect("its clips are just clips");
    assert_eq!(clips(&s, TrackKind::Video, 0).len(), before * 2);
    assert_eq!(s.project.nest_cycle(), None);
}
