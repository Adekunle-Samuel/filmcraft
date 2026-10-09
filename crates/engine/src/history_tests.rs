//! `Session::grouped`, `edit.historyMark` and `edit.collapseSince`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::{EngineError, Session};
use filmcraft_time::Tick;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// One undoable step that renames the project.
fn step(s: &mut Session, n: u32) {
    s.edit(&format!("Step {n}"), |p, _| {
        p.name = format!("v{n}");
        Ok(())
    })
    .unwrap();
}

fn labels(s: &Session) -> Vec<String> {
    s.history.undo.iter().map(|e| e.0.clone()).collect()
}

fn state_json(s: &Session) -> Value {
    serde_json::to_value(&s.state).unwrap()
}

#[test]
fn grouped_folds_every_step_into_one() {
    let mut s = demo();
    step(&mut s, 1);
    let base = s.project.clone();
    let r = s
        .grouped("Assistant: tidy", |s| {
            step(s, 2);
            s.execute("markers.markIn", json!({"time": Tick::from_seconds_f64(1.0).0}))?;
            step(s, 3);
            Ok(7)
        })
        .unwrap();
    assert_eq!(r, 7);
    assert_eq!(labels(&s), ["Step 1", "Assistant: tidy"]);
    let done = s.project.clone();
    assert_eq!(done.name, "v3");

    // one undo takes all of it back, redo brings it all again
    assert_eq!(s.undo().as_deref(), Some("Assistant: tidy"));
    assert!(Arc::ptr_eq(&s.project, &base));
    assert_eq!(s.active_sequence().unwrap().mark_in, None);
    assert_eq!(s.redo().as_deref(), Some("Assistant: tidy"));
    assert!(Arc::ptr_eq(&s.project, &done));
    assert!(s.active_sequence().unwrap().mark_in.is_some());
}

#[test]
fn grouped_without_changes_adds_no_step() {
    let mut s = demo();
    step(&mut s, 1);
    s.grouped("Nothing", |s| s.execute("project.inspect", json!({})).map(|_| ())).unwrap();
    assert_eq!(labels(&s), ["Step 1"]);
}

#[test]
fn grouped_rolls_back_on_error() {
    let mut s = demo();
    step(&mut s, 1);
    step(&mut s, 2);
    s.undo(); // a redo step that must survive
    let (base, state, undo, redo, journal) = (s.project.clone(), state_json(&s), labels(&s), s.history.redo.len(), s.journal.len());
    let rev = s.revision;
    let e = s
        .grouped("Doomed", |s| -> crate::Result<()> {
            step(s, 3);
            s.execute("markers.markIn", json!({"time": Tick::from_seconds_f64(1.0).0}))?;
            s.state.selection.clear();
            s.state.snapping = !s.state.snapping;
            Err(EngineError::Other("no".into()))
        })
        .unwrap_err();
    assert_eq!(e.to_string(), "no");
    assert!(Arc::ptr_eq(&s.project, &base), "project unchanged");
    assert_eq!(state_json(&s), state, "editor state unchanged");
    assert_eq!(labels(&s), undo, "history unchanged");
    assert_eq!(s.history.redo.len(), redo, "redo kept");
    assert_eq!(s.journal.len(), journal, "journal unchanged");
    assert!(s.revision > rev, "frontends refresh");
    assert_eq!(s.history.limit, 200);
    assert_eq!(s.redo().as_deref(), Some("Step 2"));
}

#[test]
fn grouped_rolls_back_on_panic() {
    let mut s = demo();
    let base = s.project.clone();
    let e = s
        .grouped("Buggy", |s| -> crate::Result<()> {
            step(s, 1);
            panic!("a bug")
        })
        .unwrap_err();
    assert!(e.to_string().contains("internal error"), "{e}");
    assert!(Arc::ptr_eq(&s.project, &base));
    assert!(s.history.undo.is_empty());
    assert_eq!(s.history.limit, 200);
}

#[test]
fn grouped_keeps_the_history_limit() {
    let mut s = demo();
    s.history.limit = 3;
    for n in 1..=3 {
        step(&mut s, n);
    }
    s.grouped("Group", |s| {
        for n in 10..20 {
            step(s, n);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(labels(&s), ["Step 2", "Step 3", "Group"], "trimmed back to the limit after folding");
    assert_eq!(s.history.limit, 3);
    s.undo();
    assert_eq!(s.project.name, "v3");
}

#[test]
fn nested_groups_fold_into_the_outer_one() {
    let mut s = demo();
    s.grouped("Outer", |s| {
        step(s, 1);
        s.grouped("Inner", |s| {
            step(s, 2);
            step(s, 3);
            Ok(())
        })?;
        // a failing inner group rolls back only its own part
        let _ = s.grouped("Inner fail", |s| -> crate::Result<()> {
            step(s, 4);
            Err(EngineError::Other("x".into()))
        });
        Ok(())
    })
    .unwrap();
    assert_eq!(labels(&s), ["Outer"]);
    assert_eq!(s.project.name, "v3");
}

fn mark(s: &mut Session) -> (u64, u64) {
    let r = s.execute("edit.historyMark", json!({})).unwrap();
    (r["mark"].as_u64().unwrap(), r["token"].as_u64().unwrap())
}

#[test]
fn collapse_since_folds_a_turn_into_one_step() {
    let mut s = demo();
    step(&mut s, 1);
    let base = s.project.clone();
    let (m, t) = mark(&mut s);
    assert_eq!(m, 1);
    let journal = s.journal.len();
    step(&mut s, 2);
    s.execute("markers.markIn", json!({"time": Tick::from_seconds_f64(2.0).0})).unwrap();
    step(&mut s, 3);
    let done = s.project.clone();
    let r = s.execute("edit.collapseSince", json!({"mark": m, "token": t, "label": "Assistant: clean up"})).unwrap();
    assert_eq!(r["collapsed"], 3, "{r}");
    assert_eq!(labels(&s), ["Step 1", "Assistant: clean up"]);
    assert_eq!(s.journal.len(), journal + 2, "markIn and collapseSince are journaled");

    // undo / redo the whole turn
    s.execute("edit.undo", json!({})).unwrap();
    assert!(Arc::ptr_eq(&s.project, &base));
    s.execute("edit.redo", json!({})).unwrap();
    assert!(Arc::ptr_eq(&s.project, &done));

    // folding again is harmless (still one step); nothing since a fresh mark folds nothing
    let r = s.execute("edit.collapseSince", json!({"mark": m, "token": t})).unwrap();
    assert_eq!(r["collapsed"], 1);
    assert_eq!(r["label"], "Assistant");
    let (m2, t2) = mark(&mut s);
    let r = s.execute("edit.collapseSince", json!({"mark": m2, "token": t2})).unwrap();
    assert_eq!(r["collapsed"], 0);
    assert_eq!(s.history.undo.len(), 2);
}

#[test]
fn collapse_since_from_an_empty_history() {
    let mut s = demo();
    let name = s.project.name.clone();
    let (m, t) = mark(&mut s);
    assert_eq!(m, 0);
    // nothing to combine: the command is disabled
    let e = s.execute("edit.collapseSince", json!({"mark": m, "token": t})).unwrap_err();
    assert!(matches!(e, EngineError::Disabled(..)), "{e}");
    step(&mut s, 1);
    step(&mut s, 2);
    let r = s.execute("edit.collapseSince", json!({"mark": m, "token": t, "label": "Turn"})).unwrap();
    assert_eq!(r["collapsed"], 2);
    assert_eq!(labels(&s), ["Turn"]);
    s.undo();
    assert_eq!(s.project.name, name);
}

#[test]
fn collapse_since_survives_the_history_limit() {
    let mut s = demo();
    s.history.limit = 4;
    for n in 1..=4 {
        step(&mut s, n);
    }
    let (m, t) = mark(&mut s);
    for n in 5..=7 {
        step(&mut s, n); // each trims one old step off the front
    }
    let r = s.execute("edit.collapseSince", json!({"mark": m, "token": t, "label": "Turn"})).unwrap();
    assert_eq!(r["collapsed"], 3);
    assert_eq!(labels(&s), ["Step 4", "Turn"]);
    s.undo();
    assert_eq!(s.project.name, "v4");
}

#[test]
fn collapse_since_refuses_when_the_user_rewrote_history() {
    let mut s = demo();
    step(&mut s, 1);
    step(&mut s, 2);
    let (m, t) = mark(&mut s);
    step(&mut s, 3);
    // the user undoes past the mark and edits: the turn's base is gone
    s.undo();
    s.undo();
    step(&mut s, 9);
    step(&mut s, 10);
    let before = labels(&s);
    let e = s.execute("edit.collapseSince", json!({"mark": m, "token": t})).unwrap_err().to_string();
    assert!(e.contains("history mark 2"), "{e}");
    assert_eq!(labels(&s), before, "nothing folded");
}

#[test]
fn collapse_since_hostile_params() {
    let mut s = demo();
    step(&mut s, 1);
    let (m, t) = mark(&mut s);
    step(&mut s, 2);
    let before = labels(&s);
    for p in [
        json!({}),
        json!({"mark": m}),
        json!({"token": t}),
        json!({"mark": u64::MAX, "token": t}),
        json!({"mark": -1, "token": t}),
        json!({"mark": 1.5, "token": t}),
        json!({"mark": "1", "token": t}),
        json!({"mark": f64::MAX, "token": t}),
        json!({"mark": m, "token": -3}),
        json!({"mark": m, "token": 0}),
        json!({"mark": m, "token": t + 1_000_000}),
        json!({"mark": 0, "token": t}),
        json!({"mark": m + 1, "token": t}),
        json!({"mark": m, "token": t, "label": 5}),
    ] {
        let r = s.execute("edit.collapseSince", p.clone());
        if p.get("label").is_some() {
            // a non-string label falls back to the default
            assert!(r.is_ok(), "{p}: {r:?}");
            continue;
        }
        assert!(r.is_err(), "{p} should fail");
        assert_eq!(labels(&s), before, "{p} changed the history");
    }
    // tokens die with the project
    let (m, t) = mark(&mut s);
    s.execute("file.newProject", json!({})).unwrap();
    step(&mut s, 1);
    let e = s.execute("edit.collapseSince", json!({"mark": m, "token": t})).unwrap_err().to_string();
    assert!(e.contains("unknown or expired token"), "{e}");
    // a huge label is cut, not stored whole
    let (m, t) = mark(&mut s);
    step(&mut s, 2);
    let r = s.execute("edit.collapseSince", json!({"mark": m, "token": t, "label": "x".repeat(100_000)})).unwrap();
    assert_eq!(r["label"].as_str().unwrap().len(), 200);
}

#[test]
fn old_marks_expire() {
    let mut s = demo();
    step(&mut s, 1);
    let (m, t) = mark(&mut s);
    for _ in 0..crate::MAX_HISTORY_MARKS {
        mark(&mut s);
    }
    assert_eq!(s.history.marks.len(), crate::MAX_HISTORY_MARKS);
    step(&mut s, 2);
    assert!(s.execute("edit.collapseSince", json!({"mark": m, "token": t})).is_err());
}
