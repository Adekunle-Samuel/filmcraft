//! Audio Track Mixer commands and automation recording.

use super::*;
use filmcraft_project::mixer::{LANE_VOLUME, MASTER_STRIP};
use filmcraft_project::{AutomationMode, ParamValue};
use filmcraft_time::TICKS_PER_SECOND;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn sec(x: f64) -> i64 {
    (x * TICKS_PER_SECOND as f64) as i64
}

fn a1(s: &Session) -> &filmcraft_project::Track {
    &s.active_sequence().unwrap().audio_tracks[0]
}

fn vol_at(s: &Session, x: f64) -> f64 {
    a1(s).lane_value(LANE_VOLUME, Tick(sec(x)))
}

fn set_mode(s: &mut Session, strip: &str, mode: &str) {
    s.execute("mixer.setStrip", json!({"strip": strip, "mode": mode})).unwrap();
}

#[test]
fn every_mixer_command_is_registered_with_a_label() {
    for id in [
        "mixer.inspect",
        "mixer.setStrip",
        "mixer.setValue",
        "mixer.touch",
        "mixer.release",
        "mixer.recordStart",
        "mixer.recordStop",
        "mixer.addSubmix",
        "mixer.deleteSubmix",
        "mixer.addInsert",
        "mixer.removeInsert",
        "mixer.setInsert",
        "mixer.addSend",
        "mixer.setSend",
        "mixer.removeSend",
        "mixer.setKeyframe",
        "mixer.deleteKeyframe",
        "mixer.moveKeyframe",
        "mixer.clearLane",
        "mixer.writeAutomation",
    ] {
        let c = commands::find(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(!c.label.is_empty() && !c.params.is_empty());
    }
    let s = Session::default();
    assert!(s.clone_disabled("mixer.setStrip"), "needs a sequence");
}

impl Session {
    fn clone_disabled(&self, id: &str) -> bool {
        !self.is_enabled(id)
    }
}

#[test]
fn strip_settings_undo_redo_and_inspect() {
    let mut s = demo();
    s.execute("mixer.setStrip", json!({"strip": "A1", "volumeDb": -6.0, "pan": -30.0, "mode": "Touch", "recordArm": true, "solo": true})).unwrap();
    let t = a1(&s);
    assert_eq!((t.volume_db, t.pan, t.mixer.mode, t.mixer.record_arm, t.solo), (-6.0, -30.0, AutomationMode::Touch, true, true));
    s.execute("mixer.setStrip", json!({"strip": "Mix", "volumeDb": -3.0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().master_volume_db, -3.0);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(a1(&s).volume_db, 0.0);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(a1(&s).volume_db, -6.0);
    let v = s.execute("mixer.inspect", json!({})).unwrap();
    let strips = v["strips"].as_array().unwrap();
    assert_eq!(strips.last().unwrap()["ref"], "Mix");
    assert_eq!(strips[0]["ref"], "A1");
    assert_eq!(strips[0]["mode"], "Touch");
    // errors
    assert!(s.execute("mixer.setStrip", json!({"strip": "A99"})).is_err());
    assert!(s.execute("mixer.setValue", json!({"strip": "Mix", "lane": "pan", "value": 3})).is_err(), "Mix has no pan");
}

#[test]
fn submixes_sends_and_inserts() {
    let mut s = demo();
    let r = s.execute("mixer.addSubmix", json!({"name": "Reverb"})).unwrap();
    assert_eq!(r["ref"], "S1");
    s.execute("mixer.addSubmix", json!({})).unwrap();
    s.execute("mixer.addSend", json!({"strip": "A1", "target": "S1", "levelDb": -6.0, "preFader": true})).unwrap();
    s.execute("mixer.addInsert", json!({"strip": "S1", "effect": "studio_reverb"})).unwrap();
    s.execute("mixer.addInsert", json!({"strip": "A1", "effect": "dynamics", "postFader": true})).unwrap();
    s.execute("mixer.setInsert", json!({"strip": "A1", "slot": 0, "params": {"threshold": -30.0}})).unwrap();
    s.execute("mixer.setStrip", json!({"strip": "S1", "output": "S2"})).unwrap();
    assert!(s.execute("mixer.setStrip", json!({"strip": "S2", "output": "S1"})).is_err(), "no feedback");
    assert!(s.execute("mixer.addSend", json!({"strip": "S2", "target": "S1"})).is_err(), "no feedback");
    assert!(s.execute("mixer.addInsert", json!({"strip": "A1", "effect": "gaussian_blur"})).is_err());
    let q = s.active_sequence().unwrap();
    assert_eq!(q.audio_tracks[0].mixer.sends[0].level_db, -6.0);
    assert!(q.audio_tracks[0].effects[0].post_fader);
    assert_eq!(q.audio_tracks[0].effects[0].param("threshold").unwrap().value, ParamValue::Float(-30.0));
    // the mix renders through the graph (the demo has audio on A1)
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let b = filmcraft_render::audio::mix_sequence(&s.project, q, 0, 4_800, &provider);
    assert_eq!(b.frames(), 4_800);
    // deleting a submix re-routes and drops sends
    let s1 = q.submix_tracks[0].id.0;
    s.execute("mixer.deleteSubmix", json!({"strip": s1})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.audio_tracks[0].mixer.sends.is_empty());
    assert_eq!(q.submix_tracks.len(), 1);
    s.execute("mixer.removeInsert", json!({"strip": "A1", "slot": 0})).unwrap();
    assert!(a1(&s).effects.is_empty());
}

#[test]
fn keyframes_by_command_and_pen_edits() {
    let mut s = demo();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "time": sec(1.0), "value": -10.0})).unwrap();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "time": sec(2.0), "value": 0.0})).unwrap();
    assert_eq!(vol_at(&s, 0.0), -10.0);
    assert!((vol_at(&s, 1.5) + 5.0).abs() < 1e-9);
    s.execute("mixer.moveKeyframe", json!({"strip": "A1", "time": sec(2.0), "newTime": sec(3.0), "value": -20.0})).unwrap();
    assert!((vol_at(&s, 2.0) + 15.0).abs() < 1e-9);
    s.execute("mixer.deleteKeyframe", json!({"strip": "A1", "time": sec(1.0)})).unwrap();
    assert_eq!(vol_at(&s, 0.0), -20.0);
    // a fader move on an automated lane (Read, stopped) adds a keyframe at the playhead
    s.execute("playhead.set", json!({"seconds": 5.0})).unwrap();
    s.execute("mixer.setValue", json!({"strip": "A1", "value": -3.0})).unwrap();
    assert_eq!(vol_at(&s, 5.0), -3.0);
    assert_eq!(a1(&s).lane_keyframes(LANE_VOLUME).len(), 2);
    // mute keyframes hold
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": "mute", "time": sec(1.0), "value": true})).unwrap();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": "mute", "time": sec(2.0), "value": false})).unwrap();
    assert_eq!(a1(&s).lane_value("mute", Tick(sec(1.9))), 1.0);
    s.execute("mixer.clearLane", json!({"strip": "A1", "lane": "volume"})).unwrap();
    assert!(a1(&s).lane_keyframes(LANE_VOLUME).is_empty());
    // effect parameter lanes
    s.execute("mixer.addInsert", json!({"strip": "A1", "effect": "amplify"})).unwrap();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": "fx.0.gain", "time": sec(1.0), "value": 6.0})).unwrap();
    assert!(a1(&s).automated_lanes().contains(&"fx.0.gain".to_string()));
}

/// Simulate a fader gesture: `(seconds, dB)` samples as the UI would send them each frame.
fn gesture(s: &mut Session, strip: &str, pts: &[(f64, f64)]) {
    for &(t, v) in pts {
        s.execute("mixer.touch", json!({"strip": strip, "lane": "volume", "value": v, "time": sec(t)})).unwrap();
    }
}

#[test]
fn touch_records_while_held_and_returns_over_automatch() {
    let mut s = demo();
    set_mode(&mut s, "A1", "Touch");
    s.execute("mixer.recordStart", json!({"time": 0})).unwrap();
    assert!(s.execute("mixer.recordStart", json!({})).is_err(), "already recording");
    // hold -10 dB from 1 s to 2 s (frames every 1/30 s)
    let pts: Vec<(f64, f64)> = (0..=30).map(|i| (1.0 + i as f64 / 30.0, -10.0)).collect();
    gesture(&mut s, "A1", &pts);
    // the audio follows the fader while recording
    assert_eq!(s.previews.live.get(a1(&s).id, LANE_VOLUME).map(|o| o.value), Some(-10.0));
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(2.0)})).unwrap();
    let r = s.execute("mixer.recordStop", json!({"time": sec(5.0)})).unwrap();
    assert_eq!(r["lanes"], 1);
    assert!(!s.previews.live.is_active());
    assert_eq!(vol_at(&s, 0.5), 0.0, "before the touch: unchanged");
    assert_eq!(vol_at(&s, 1.5), -10.0);
    assert!((vol_at(&s, 2.5) + 5.0).abs() < 0.01, "halfway back after 0.5 s of the 1 s automatch: {}", vol_at(&s, 2.5));
    assert_eq!(vol_at(&s, 3.5), 0.0, "back to the old automation");
    // thinned: a held value is a few keyframes, not 31
    assert!(a1(&s).lane_keyframes(LANE_VOLUME).len() <= 5, "{:?}", a1(&s).lane_keyframes(LANE_VOLUME));
    // one undo step removes the pass
    s.execute("edit.undo", json!({})).unwrap();
    assert!(a1(&s).lane_keyframes(LANE_VOLUME).is_empty());
}

#[test]
fn touch_over_existing_automation_and_custom_automatch() {
    let mut s = demo();
    s.execute("prefs.set", json!({"key": "audio.automatchTime", "value": 2.0})).unwrap();
    s.execute("mixer.writeAutomation", json!({"strip": "A1", "points": [[0, -20.0], [sec(8.0), -20.0]]})).unwrap();
    set_mode(&mut s, "A1", "Touch");
    s.execute("mixer.recordStart", json!({"time": sec(0.5)})).unwrap();
    gesture(&mut s, "A1", &[(1.0, 0.0), (1.5, 0.0)]);
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(1.5)})).unwrap();
    s.execute("mixer.recordStop", json!({"time": sec(6.0)})).unwrap();
    assert_eq!(vol_at(&s, 0.9), -20.0);
    assert_eq!(vol_at(&s, 1.25), 0.0);
    assert!((vol_at(&s, 2.5) + 10.0).abs() < 0.05, "{}", vol_at(&s, 2.5));
    assert_eq!(vol_at(&s, 4.0), -20.0);
    assert_eq!(vol_at(&s, 7.0), -20.0);
}

#[test]
fn latch_holds_until_stop() {
    let mut s = demo();
    s.execute("mixer.writeAutomation", json!({"strip": "A1", "points": [[0, 0.0], [sec(8.0), 0.0]]})).unwrap();
    set_mode(&mut s, "A1", "Latch");
    s.execute("mixer.recordStart", json!({"time": 0})).unwrap();
    gesture(&mut s, "A1", &[(1.0, -4.0), (1.5, -8.0), (2.0, -12.0)]);
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(2.0)})).unwrap();
    s.execute("mixer.recordStop", json!({"time": sec(4.0)})).unwrap();
    assert_eq!(vol_at(&s, 0.5), 0.0);
    assert!((vol_at(&s, 1.25) + 6.0).abs() < 1e-6);
    assert_eq!(vol_at(&s, 3.0), -12.0, "latched");
    assert_eq!(vol_at(&s, 3.99), -12.0);
    assert!(vol_at(&s, 4.5) > -12.0, "existing automation after the stop resumes");
}

#[test]
fn write_overwrites_from_start_and_switches_to_touch() {
    let mut s = demo();
    s.execute("mixer.writeAutomation", json!({"strip": "A1", "points": [[0, -30.0], [sec(8.0), 0.0]]})).unwrap();
    s.execute("mixer.setStrip", json!({"strip": "A1", "mode": "Write"})).unwrap();
    s.execute("mixer.recordStart", json!({"time": sec(2.0)})).unwrap();
    let start_val = -30.0 + 30.0 * 2.0 / 8.0;
    // untouched until 3 s: the fader's value at the start is written
    gesture(&mut s, "A1", &[(3.0, -6.0), (3.5, -6.0)]);
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(3.5)})).unwrap();
    s.execute("mixer.recordStop", json!({"time": sec(5.0)})).unwrap();
    assert!((vol_at(&s, 1.0) - (-30.0 + 30.0 / 8.0)).abs() < 0.05, "before the pass: unchanged");
    assert!((vol_at(&s, 2.5) - start_val).abs() < 0.05, "{}", vol_at(&s, 2.5));
    assert_eq!(vol_at(&s, 4.0), -6.0);
    assert_eq!(vol_at(&s, 4.9), -6.0, "Write holds the last value to the stop");
    assert_eq!(a1(&s).mixer.mode, AutomationMode::Touch, "Switch to Touch after Write");
    // pan and mute are written too
    assert!(a1(&s).lane_keyframes("pan").len() >= 2 && a1(&s).lane_keyframes("mute").len() >= 2);
    // the preference can keep Write
    s.execute("prefs.set", json!({"key": "audio.switchToTouchAfterWrite", "value": false})).unwrap();
    s.execute("mixer.setStrip", json!({"strip": "A1", "mode": "Write"})).unwrap();
    s.execute("mixer.recordStart", json!({"time": sec(6.0)})).unwrap();
    s.execute("mixer.recordStop", json!({"time": sec(7.0)})).unwrap();
    assert_eq!(a1(&s).mixer.mode, AutomationMode::Write);
}

#[test]
fn read_mode_does_not_record_and_commits_once_without_a_pass() {
    let mut s = demo();
    s.execute("mixer.recordStart", json!({"time": 0})).unwrap();
    gesture(&mut s, "A1", &[(1.0, -6.0), (2.0, -9.0)]);
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(2.0)})).unwrap();
    let r = s.execute("mixer.recordStop", json!({"time": sec(3.0)})).unwrap();
    assert_eq!(r["lanes"], 0);
    assert!(a1(&s).lane_keyframes(LANE_VOLUME).is_empty());
    // stopped: a drag is heard live, then committed as one undo step
    let undo0 = s.history.undo.len();
    gesture(&mut s, "A1", &[(0.0, -2.0), (0.0, -4.0), (0.0, -5.0)]);
    assert_eq!(a1(&s).volume_db, 0.0);
    s.execute("mixer.release", json!({"strip": "A1"})).unwrap();
    assert_eq!(a1(&s).volume_db, -5.0);
    assert_eq!(s.history.undo.len(), undo0 + 1);
}

#[test]
fn recorded_pass_plays_back_through_the_mix() {
    let mut s = demo();
    // only A1 (the demo has music on another track)
    s.execute("mixer.setStrip", json!({"strip": "A1", "solo": true})).unwrap();
    set_mode(&mut s, "A1", "Latch");
    s.execute("mixer.recordStart", json!({"time": 0})).unwrap();
    gesture(&mut s, "A1", &[(0.5, -96.0)]);
    s.execute("mixer.release", json!({"strip": "A1", "time": sec(0.5)})).unwrap();
    s.execute("mixer.recordStop", json!({"time": sec(2.0)})).unwrap();
    let q = s.active_sequence().unwrap();
    let seq_id = s.state.active_sequence.unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let b = s.previews.mix(&s.project, seq_id, 48_000, 4_800, &provider);
    assert!(b.channels[0].iter().all(|x| x.abs() < 1e-6), "faded out by the recorded automation");
    let before = filmcraft_render::audio::mix_sequence(&s.project, q, 0, 4_800, &provider);
    assert!(before.channels[0].iter().any(|x| x.abs() > 1e-4), "audible before the move");
    // master strip automation
    s.execute("mixer.setKeyframe", json!({"strip": "Mix", "time": 0, "value": -6.0})).unwrap();
    assert_eq!(s.active_sequence().unwrap().master_volume_at(Tick(sec(1.0))), -6.0);
    assert!(s.active_sequence().unwrap().strip(MASTER_STRIP).is_some());
}
