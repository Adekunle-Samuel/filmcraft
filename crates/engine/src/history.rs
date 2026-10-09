//! Undo-history commands for agents (`edit.historyMark`, `edit.collapseSince`).
//!
//! An agent that runs several commands for one request (an assistant turn) takes a mark first and
//! folds everything done since into one undo step at the end, so the user undoes the whole request
//! with one Undo. The fold is refused when the history below the mark changed in between (the
//! user undid past it and edited), so the user's own steps are never swallowed. Inside the engine,
//! [`crate::Session::grouped`] does the same for a closure, with rollback on error.

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, str_p};
use crate::{Result, Session};

/// Longest undo label kept (characters).
const MAX_LABEL: usize = 200;

fn has_undo(s: &Session) -> std::result::Result<(), String> {
    if s.history.can_undo() { Ok(()) } else { Err("there are no undo steps to combine".into()) }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec { id: "edit.historyMark", label: "History Mark", menu: &[], shortcut: None, params: "{}", enabled: always, run: mark, journal: false },
        CommandSpec {
            id: "edit.collapseSince",
            label: "Combine Undo Steps",
            menu: &[],
            shortcut: None,
            params: r#"{"mark":n,"token":n,"label":str?}"#,
            enabled: has_undo,
            run: collapse_since,
            journal: true,
        },
    ]
}

/// `edit.historyMark {}` → `{mark, token}`: the undo depth now and a token for
/// `edit.collapseSince`.
fn mark(s: &mut Session, _: &Value) -> Result<Value> {
    let (mark, token) = s.history_mark();
    Ok(json!({"mark": mark, "token": token}))
}

/// `edit.collapseSince {mark, token, label?}` → `{collapsed}`: fold the undo steps made since
/// the mark into one step named `label` (default "Assistant").
fn collapse_since(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "edit.collapseSince";
    let int = |k: &str| -> Result<u64> {
        p.get(k).and_then(Value::as_u64).ok_or_else(|| bad(ID, format!("`{k}` must be a non-negative integer from edit.historyMark")))
    };
    let mark = usize::try_from(int("mark")?).map_err(|_| bad(ID, "`mark` is out of range"))?;
    let token = int("token")?;
    let label: String = str_p(p, "label").map(str::trim).filter(|l| !l.is_empty()).unwrap_or("Assistant").chars().take(MAX_LABEL).collect();
    let collapsed = s.collapse_since(mark, token, &label)?;
    Ok(json!({"collapsed": collapsed, "label": label}))
}
