//! Nested sequences: a sequence can never contain itself, whichever
//! command is asked to put it there, and Nest… keeps what was selected.

use super::*;
use filmcraft_project::TrackKind;
use filmcraft_time::TimeRange;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip_ids(s: &Session, kind: TrackKind, idx: usize) -> Vec<u64> {
    s.active_sequence().unwrap().tracks(kind)[idx].items.iter().map(|i| i.id.0).collect()
}

/// Nest the second V1 clip of the active sequence; returns (outer sequence, nested sequence).
fn nest_one(s: &mut Session, name: &str) -> (ItemId, ItemId) {
    let outer = s.state.active_sequence.unwrap();
    let v = clip_ids(s, TrackKind::Video, 0)[1];
    s.execute("timeline.select", json!({"clips": [v]})).unwrap();
    let r = s.execute("clip.nest", json!({"name": name})).unwrap();
    (outer, ItemId(r["sequence"].as_u64().unwrap()))
}

/// The command fails with the self-nesting message and changes nothing (project and undo history).
fn refused(s: &mut Session, command: &str, params: serde_json::Value) {
    let (before, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute(command, params).expect_err(command).to_string();
    assert!(e.contains("cannot be nested inside itself"), "{command}: {e}");
    assert!(Arc::ptr_eq(&before, &s.project), "{command} left the project alone");
    assert_eq!(s.history.undo.len(), undo, "{command} added no undo step");
    assert_eq!(s.project.nest_cycle(), None);
}

#[test]
fn a_sequence_cannot_be_placed_in_itself() {
    let mut s = demo();
    let a = s.state.active_sequence.unwrap();
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0}));
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0, "insert": true}));
    // from the Source monitor as well
    s.execute("source.open", json!({"item": a.0})).unwrap();
    refused(&mut s, "source.insert", json!({}));
    refused(&mut s, "source.overwrite", json!({}));
}

#[test]
fn a_sequence_cannot_be_placed_in_a_sequence_it_contains() {
    let mut s = demo();
    let (outer, nested) = nest_one(&mut s, "Inner");
    // outer holds Inner: Inner cannot take outer, nor itself
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "timeline.place", json!({"item": outer.0, "seconds": 0.0}));
    refused(&mut s, "timeline.place", json!({"item": nested.0, "seconds": 0.0}));
    // one level further: a third sequence that holds outer cannot go into Inner either
    s.execute("sequence.open", json!({"item": outer.0})).unwrap();
    let third = ItemId(s.execute("file.newSequence", json!({"name": "Third"})).unwrap()["sequence"].as_u64().unwrap());
    s.execute("sequence.open", json!({"item": third.0})).unwrap();
    s.execute("timeline.place", json!({"item": outer.0, "seconds": 0.0})).expect("a nest of a nest is fine");
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "timeline.place", json!({"item": third.0, "seconds": 0.0}));
    // the other direction stays allowed
    s.execute("sequence.open", json!({"item": third.0})).unwrap();
    s.execute("timeline.place", json!({"item": nested.0, "seconds": 30.0})).expect("Inner may be used twice");
}

#[test]
fn pasting_a_nest_into_its_own_sequence_is_refused() {
    let mut s = demo();
    let (_, nested) = nest_one(&mut s, "Inner");
    // copy the nest clip (selected by Nest…), open the nested sequence and paste it there
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("sequence.open", json!({"item": nested.0})).unwrap();
    refused(&mut s, "edit.paste", json!({}));
    refused(&mut s, "edit.pasteInsert", json!({}));
}

#[test]
fn a_project_opened_with_a_cycle_can_still_be_edited_and_repaired() {
    let mut s = demo();
    let a = s.state.active_sequence.unwrap();
    // what a damaged project file would hold: the sequence on its own track
    let mut p = (*s.project).clone();
    let rate = p.sequence(a).unwrap().settings.frame_rate;
    let it = p.make_track_item(a, TrackKind::Video, Tick(900 * filmcraft_time::TICKS_PER_SECOND), TimeRange::new(Tick::ZERO, rate.tick_of(24)), rate).unwrap();
    let bad = it.id;
    p.sequence_mut(a).unwrap().video_tracks[0].items.push(it);
    s.project = Arc::new(p);
    assert_eq!(s.project.nest_cycle(), Some(a));
    // unrelated edits still work
    let first = clip_ids(&s, TrackKind::Video, 0)[0];
    s.execute("timeline.select", json!({"clips": [first]})).unwrap();
    s.execute("clip.enable", json!({})).expect("edits are not locked out");
    // and removing the clip repairs it, after which the guard is back
    s.execute("timeline.select", json!({"clips": [bad.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(s.project.nest_cycle(), None);
    refused(&mut s, "timeline.place", json!({"item": a.0, "seconds": 1.0}));
}

/// The first V1 transition that joins two clips: (transition, outgoing clip, incoming clip).
fn joining_transition(s: &Session) -> (filmcraft_project::Transition, ClipId, ClipId) {
    let v1 = &s.active_sequence().unwrap().video_tracks[0];
    let t = v1.transitions.iter().find(|t| t.from.is_some() && t.to.is_some()).expect("the demo has a transition between two clips").clone();
    let (from, to) = (t.from.unwrap(), t.to.unwrap());
    (t, from, to)
}

fn frame_at(s: &Session, t: Tick) -> filmcraft_render::Image {
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, ..Default::default() };
    filmcraft_render::render_sequence(&s.project, s.state.active_sequence.unwrap(), t, opts, &provider)
}

#[test]
fn nest_keeps_transitions_track_names_and_channel_layouts() {
    let mut s = demo();
    let outer = s.state.active_sequence.unwrap();
    s.edit_sequence("setup", |q, _, _| {
        q.video_tracks[0].name = "Picture".into();
        q.audio_tracks[0].name = "Dialogue".into();
        q.audio_tracks[1].channels = filmcraft_project::AudioChannels::Mono;
        Ok(())
    })
    .unwrap();
    let (trn, from, to) = joining_transition(&s);
    let mid = trn.start + Tick(trn.duration.0 / 2);
    let before = frame_at(&s, mid);
    let start = s.active_sequence().unwrap().find_item(from).unwrap().1.start;
    let audio_transitions = s.active_sequence().unwrap().audio_tracks[0].transitions.len();
    s.execute("timeline.select", json!({"clips": [from.0, to.0]})).unwrap();
    let linked: Vec<ClipId> = s.state.selection.clone();
    let nested = ItemId(s.execute("clip.nest", json!({"name": "Inner"})).unwrap()["sequence"].as_u64().unwrap());

    let q = s.project.sequence(nested).unwrap();
    q.check().unwrap();
    // the transition came along, on the same track, at the same place relative to its clips
    let inner = q.video_tracks[0].transitions.iter().find(|t| t.id == trn.id).expect("the transition is inside the nest");
    assert_eq!((inner.start, inner.duration, inner.from, inner.to), (trn.start - start, trn.duration, Some(from), Some(to)));
    // so did the one between the linked sound clips, if the demo has it
    let inner_audio =
        q.audio_tracks[0].transitions.iter().filter(|t| t.from.is_some_and(|c| linked.contains(&c)) && t.to.is_some_and(|c| linked.contains(&c))).count();
    let outer_q = s.project.sequence(outer).unwrap();
    assert_eq!(outer_q.audio_tracks[0].transitions.len() + inner_audio, audio_transitions, "audio transitions moved, none lost");
    // tracks are laid out like the parent's
    assert_eq!(q.video_tracks[0].name, "Picture");
    assert_eq!(q.audio_tracks[0].name, "Dialogue");
    assert_eq!(q.audio_tracks[1].channels, filmcraft_project::AudioChannels::Mono);
    // the parent no longer holds the transition, and shows the same picture through the nest
    assert!(outer_q.video_tracks[0].transitions.iter().all(|t| t.id != trn.id));
    outer_q.check().unwrap();
    let after = frame_at(&s, mid);
    assert_eq!((before.w, before.h), (after.w, after.h));
    let worst = before.px.iter().zip(&after.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(worst < 0.01, "the frame inside the transition is unchanged by nesting (worst channel difference {worst})");
}

#[test]
fn nest_leaves_transitions_to_clips_outside_the_selection_out() {
    let mut s = demo();
    let (trn, from, _to) = joining_transition(&s);
    // only the outgoing clip is nested: the transition needs both, so it is not carried
    s.execute("timeline.select", json!({"clips": [from.0]})).unwrap();
    let nested = ItemId(s.execute("clip.nest", json!({"name": "Half"})).unwrap()["sequence"].as_u64().unwrap());
    let q = s.project.sequence(nested).unwrap();
    q.check().unwrap();
    assert!(q.all_tracks().all(|t| t.transitions.iter().all(|t| t.id != trn.id)));
    s.active_sequence().unwrap().check().unwrap();
}
