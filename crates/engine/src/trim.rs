//! Trim mode: selected edit points and keyboard trimming (Premiere's trim workflow).
//!
//! An edit point is one edge of a clip plus a trim kind. Trim and Ripple act on that edge; Roll acts
//! on the edge shared with the adjacent clip on the same track. Trimming delegates to the
//! `timeline.trim` / `timeline.roll` commands so linked partners and sync locks behave the same as
//! with the mouse.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_project::{ClipId, Sequence, TrackId};
use filmcraft_time::{Tick, TimeRange};

use crate::{EngineError, Result, Session};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrimKind {
    /// Regular trim: the edge moves, leaving a gap or covering a neighbour's gap.
    Trim,
    /// Ripple: the edge moves and later material shifts to close/open the difference.
    Ripple,
    /// Roll: the shared edge between two adjacent clips moves; duration is unchanged.
    Roll,
}

impl TrimKind {
    fn next(self) -> Self {
        match self {
            TrimKind::Ripple => TrimKind::Roll,
            TrimKind::Roll => TrimKind::Trim,
            TrimKind::Trim => TrimKind::Ripple,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPoint {
    pub clip: ClipId,
    /// The clip's Out edge (else its In edge).
    pub out: bool,
    pub kind: TrimKind,
}

/// Time of an edit point in the sequence.
pub fn edit_time(seq: &Sequence, ep: &EditPoint) -> Option<Tick> {
    let (_, it) = seq.find_item(ep.clip)?;
    Some(if ep.out { it.end() } else { it.start })
}

/// The (left, right) clips of a roll at this edit point.
pub fn roll_pair(seq: &Sequence, ep: &EditPoint) -> Option<(ClipId, ClipId)> {
    let (tid, it) = seq.find_item(ep.clip)?;
    let track = seq.track(tid)?;
    if ep.out {
        let right = track.items.iter().find(|o| o.start == it.end())?;
        Some((it.id, right.id))
    } else {
        let left = track.items.iter().find(|o| o.end() == it.start)?;
        Some((left.id, it.id))
    }
}

fn kind_p(p: &Value) -> Option<TrimKind> {
    serde_json::from_value(p.get("kind")?.clone()).ok()
}

/// `trim.selectEditPoint {clip, edge: "in"|"out", kind, add?}`
pub fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| EngineError::Other("need `clip`".into()))?;
    let out = p.get("edge").and_then(Value::as_str) != Some("in");
    let kind = kind_p(p).unwrap_or(TrimKind::Trim);
    s.active_sequence().and_then(|q| q.find_item(clip)).ok_or_else(|| EngineError::Other(format!("no clip {}", clip.0)))?;
    let ep = EditPoint { clip, out, kind };
    if p.get("add").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(i) = s.state.edit_points.iter().position(|e| e.clip == clip && e.out == out) {
            s.state.edit_points.remove(i);
        } else {
            s.state.edit_points.push(ep);
        }
    } else {
        s.state.edit_points = vec![ep];
    }
    s.state.selection.clear();
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// `trim.selectNearest {kind: "rippleIn"|"rippleOut"|"roll"|"trimIn"|"trimOut"}`: the edit point
/// nearest the playhead on each targeted track (all tracks when none are targeted).
pub fn select_nearest(s: &mut Session, p: &Value) -> Result<Value> {
    let which = p.get("kind").and_then(Value::as_str).unwrap_or("roll");
    let (kind, want_out) = match which {
        "rippleIn" => (TrimKind::Ripple, Some(false)),
        "rippleOut" => (TrimKind::Ripple, Some(true)),
        "trimIn" => (TrimKind::Trim, Some(false)),
        "trimOut" => (TrimKind::Trim, Some(true)),
        _ => (TrimKind::Roll, None),
    };
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let targeted = s.state.active_sequence.and_then(|id| s.state.targeting.get(&id)).map(|t| t.targeted.clone()).unwrap_or_default();
    let mut points = Vec::new();
    for tr in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        if !targeted.is_empty() && !targeted.contains(&tr.id) {
            continue;
        }
        // nearest edge on this track
        let mut best: Option<(i64, EditPoint)> = None;
        for it in &tr.items {
            for out in [false, true] {
                if want_out.is_some_and(|w| w != out) {
                    continue;
                }
                let t = if out { it.end() } else { it.start };
                let ep = EditPoint { clip: it.id, out, kind };
                if kind == TrimKind::Roll && roll_pair(seq, &ep).is_none() {
                    continue;
                }
                let d = (t - ph).0.abs();
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, ep));
                }
            }
        }
        if let Some((_, ep)) = best {
            // one roll point per edge: skip the partner's mirror of an edge already chosen
            if kind == TrimKind::Roll && points.iter().any(|e: &EditPoint| roll_pair(seq, e) == roll_pair(seq, &ep)) {
                continue;
            }
            points.push(ep);
        }
    }
    s.state.edit_points = points;
    s.state.selection.clear();
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// `trim.toggleType`: cycle Ripple → Roll → Trim for every selected edit point.
pub fn toggle_type(s: &mut Session) -> Result<Value> {
    for e in s.state.edit_points.iter_mut() {
        e.kind = e.kind.next();
    }
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// Trim every selected edit point by `delta` (edit point movement; positive = later).
fn trim_all(s: &mut Session, delta: Tick) -> Result<Value> {
    if s.state.edit_points.is_empty() {
        return Err(EngineError::Other("no edit points selected".into()));
    }
    let pts = s.state.edit_points.clone();
    let mut applied = Vec::new();
    for ep in &pts {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let r = match ep.kind {
            TrimKind::Roll => {
                let Some((l, r)) = roll_pair(seq, ep) else { continue };
                s.execute("timeline.roll", json!({"left": l.0, "right": r.0, "delta": delta.0}))?
            }
            k => s.execute(
                "timeline.trim",
                json!({"clip": ep.clip.0, "edge": if ep.out { "out" } else { "in" }, "mode": if k == TrimKind::Ripple { "ripple" } else { "regular" }, "delta": delta.0}),
            )?,
        };
        applied.push(r.get("delta").cloned().unwrap_or(Value::Null));
    }
    Ok(json!({"applied": applied}))
}

/// `trim.nudge {frames}`: Trim Forward/Backward (±1), Many (±5 by default).
pub fn nudge(s: &mut Session, p: &Value) -> Result<Value> {
    let frames = p.get("frames").and_then(Value::as_i64).unwrap_or(1);
    let d = s.sequence_rate().tick_of(frames);
    trim_all(s, d)
}

/// `trim.extendToPlayhead`: move each selected edit point to the playhead.
pub fn extend_to_playhead(s: &mut Session) -> Result<Value> {
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let first = s.state.edit_points.first().and_then(|e| edit_time(seq, e)).ok_or_else(|| EngineError::Other("no edit points selected".into()))?;
    trim_all(s, ph - first)
}

/// `trim.toPlayhead {side: "previous"|"next", ripple}` (Q/W, ⌥Q/⌥W): remove the material between
/// the playhead and the previous (Q) or next (W) edit point on the targeted tracks. Ripple variants
/// extract the range (later material moves left, sync locks respected); the others lift it.
pub fn to_playhead(s: &mut Session, p: &Value) -> Result<Value> {
    let next = p.get("side").and_then(Value::as_str) == Some("next");
    let ripple = p.get("ripple").and_then(Value::as_bool).unwrap_or(true);
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let targeted = s.targeting().targeted;
    let tracks: Vec<TrackId> = seq
        .video_tracks
        .iter()
        .chain(seq.audio_tracks.iter())
        .filter(|t| targeted.contains(&t.id) && !t.locked && t.items.iter().any(|i| i.start < ph && ph < i.end()))
        .map(|t| t.id)
        .collect();
    if tracks.is_empty() {
        return Err(EngineError::Other("no clip under the playhead on the targeted tracks".into()));
    }
    // nearest edit point on those tracks
    let edits =
        seq.video_tracks.iter().chain(seq.audio_tracks.iter()).filter(|t| tracks.contains(&t.id)).flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()]));
    let edge = if next { edits.filter(|e| *e > ph).min() } else { edits.filter(|e| *e < ph).max() };
    let edge = edge.ok_or_else(|| EngineError::Other("no edit point".into()))?;
    let range = if next { TimeRange::from_bounds(ph, edge) } else { TimeRange::from_bounds(edge, ph) };
    let label = match (next, ripple) {
        (true, true) => "Ripple Trim Next Edit to Playhead",
        (false, true) => "Ripple Trim Previous Edit to Playhead",
        (true, false) => "Trim Next Edit to Playhead",
        (false, false) => "Trim Previous Edit to Playhead",
    };
    s.edit_sequence(label, |q, ctx, _| {
        if ripple {
            filmcraft_edit::extract(q, &tracks, range, ctx);
        } else {
            filmcraft_edit::lift(q, &tracks, range, ctx);
        }
        Ok(())
    })?;
    if !next && ripple {
        // the head before the playhead was removed; park on the new edit (as Premiere does)
        s.set_playhead(edge);
    }
    Ok(json!({"range": [range.start.0, range.end().0]}))
}
