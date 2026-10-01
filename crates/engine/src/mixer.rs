//! Audio Track Mixer commands and the automation recorder.
//!
//! Strips are addressed by `strip` (or `track`): `"A1"` … (audio tracks), `"S1"` … (submixes),
//! `"Mix"` (the master), a track id, or a strip name. Lanes are `volume`, `pan`, `mute`,
//! `send.<i>.level` and `fx.<slot>.<param>` (see [`filmcraft_project::mixer`]).
//!
//! **Recording.** `mixer.recordStart` begins an automation pass at the playhead (the UI calls it
//! when playback starts). While it runs, `mixer.touch` / `mixer.release` are the fader gestures:
//! the audio follows the control immediately (through [`filmcraft_render::mixer::LiveMix`]) and the
//! gesture is recorded as a stream of (time, value) points. `mixer.recordStop` (playback stops)
//! thins every stream into keyframes and writes them as one undoable step, following the strip's
//! automation mode:
//!
//! | mode | records | after release |
//! |---|---|---|
//! | Off / Read | nothing (Read plays the automation) | the control returns to the automation |
//! | Latch | from the first touch | keeps writing the last value until playback stops |
//! | Touch | while touched | ramps back to the existing automation over the automatch time |
//! | Write | every control from the start of playback | keeps writing the last value until playback stops |
//!
//! With "Switch to Touch after Write" (on by default) a Write strip changes to Touch after the
//! pass. Without a recording pass, `mixer.touch` holds the control live and `mixer.release` commits
//! it once (a static value, or a keyframe at the playhead when the lane is automated).

use serde_json::{Value, json};

use filmcraft_project::mixer::{
    self as pm, FADER_MAX_DB, FADER_MIN_DB, LANE_MUTE, LANE_PAN, LANE_VOLUME, MASTER_STRIP, lane_info, parse_fx_lane, parse_send_lane, thin_points, write_lane,
};
use filmcraft_project::{AudioChannels, AutomationMode, InputMap, ItemId, Param, ParamValue, Sequence, Track, TrackId, TrackKind, TrackSend, find_effect};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

// ------------------------------------------------------------------------------------- recorder

/// A held value ends this long before the next move (one sample at 48 kHz).
const HOLD_EPS: Tick = Tick(TICKS_PER_SECOND / 48_000);

/// One recorded control gesture.
#[derive(Clone, Debug)]
pub struct Gesture {
    pub strip: TrackId,
    pub lane: String,
    pub mode: AutomationMode,
    /// Recorded (time, value) points.
    pub points: Vec<(Tick, f64)>,
    pub touching: bool,
    /// Touch: released at.
    pub released: Option<Tick>,
    last: (Tick, f64),
}

impl Gesture {
    fn new(strip: TrackId, lane: &str, mode: AutomationMode, t: Tick, v: f64) -> Self {
        Gesture { strip, lane: lane.to_string(), mode, points: vec![(t, v)], touching: true, released: None, last: (t, v) }
    }
    /// A new value at `t`. Repeats of the same value extend the hold instead of adding points, so a
    /// fader held still records a flat line, not a ramp to the next move.
    fn push(&mut self, t: Tick, v: f64) {
        if t < self.last.0 {
            return;
        }
        if v == self.last.1 {
            self.last.0 = t;
            return;
        }
        if self.points.last().is_some_and(|p| self.last.0 > p.0) {
            self.points.push(self.last);
        }
        self.points.push((t, v));
        self.last = (t, v);
    }
    fn flush(&mut self) {
        if self.points.last().is_some_and(|p| self.last.0 > p.0) {
            self.points.push(self.last);
        }
    }
    pub fn value(&self) -> f64 {
        self.last.1
    }
}

/// State of the current automation pass.
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    /// Sequence being recorded (None = no pass running).
    pub seq: Option<ItemId>,
    pub start: Tick,
    pub gestures: Vec<Gesture>,
}

impl Recorder {
    pub fn active(&self) -> bool {
        self.seq.is_some()
    }
    fn find(&mut self, strip: TrackId, lane: &str) -> Option<&mut Gesture> {
        self.gestures.iter_mut().find(|g| g.strip == strip && g.lane == lane)
    }
}

/// Lanes a Write pass records on every strip.
fn write_lanes(strip: TrackId) -> &'static [&'static str] {
    if strip == MASTER_STRIP { &[LANE_VOLUME] } else { &[LANE_VOLUME, LANE_PAN, LANE_MUTE] }
}

/// The lane value as automation plays it (no live overrides).
fn underlying(tr: &Track, lane: &str, t: Tick) -> f64 {
    let v = tr.lane_value(lane, t);
    if lane_info(lane).is_some_and(|i| i.hold) {
        // hold lanes: last keyframe at or before t
        if let Some(p) = tr.lane(lane).filter(|p| p.is_animated() && tr.mixer.mode.reads()) {
            let i = p.keyframes.partition_point(|k| k.time <= t);
            return p.keyframes[i.saturating_sub(1)].value.as_f64().unwrap_or(v);
        }
    }
    v
}

fn clamp_lane(lane: &str, v: f64) -> f64 {
    match lane_info(lane) {
        Some(i) if i.hold => {
            if v >= 0.5 {
                1.0
            } else {
                0.0
            }
        }
        Some(i) => v.clamp(i.min, i.max),
        None => v,
    }
}

fn seq_of(s: &Session, id: ItemId) -> Result<&Sequence> {
    s.project.sequence(id).ok_or(EngineError::NoSequence)
}

/// Start an automation pass at `t` (playback start).
pub fn record_start(s: &mut Session, t: Tick) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let seq = seq_of(s, seq_id)?;
    let mut gestures = Vec::new();
    for id in seq.strip_ids() {
        let Some(tr) = seq.strip(id) else { continue };
        if tr.mixer.mode != AutomationMode::Write {
            continue;
        }
        for lane in write_lanes(id) {
            let v = underlying(&tr, lane, t);
            let mut g = Gesture::new(id, lane, AutomationMode::Write, t, v);
            g.touching = false;
            gestures.push(g);
        }
    }
    s.previews.live.clear_all();
    for g in &gestures {
        s.previews.live.hold(g.strip, &g.lane, g.value());
    }
    let n = gestures.len();
    s.mixrec = Recorder { seq: Some(seq_id), start: t, gestures };
    Ok(json!({"recording": true, "writing": n}))
}

/// A control moved to `v` at `t`.
pub fn touch(s: &mut Session, strip: TrackId, lane: &str, v: f64, t: Tick) -> Result<Value> {
    let v = clamp_lane(lane, v);
    let seq_id = s.mixrec.seq.or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
    let mode = seq_of(s, seq_id)?.strip(strip).ok_or_else(|| bad("mixer.touch", "no such strip"))?.mixer.mode;
    s.previews.live.hold(strip, lane, v);
    if !s.mixrec.active() || !mode.writes() {
        return Ok(json!({"recording": false, "value": v}));
    }
    match s.mixrec.find(strip, lane) {
        Some(g) => {
            if !g.touching {
                if g.mode == AutomationMode::Touch {
                    // a new touch after a release: the old ramp is overwritten from here
                    g.released = None;
                } else if t > g.last.0 {
                    // Write / Latch held the last value until this touch
                    g.last.0 = (t - HOLD_EPS).max(g.last.0);
                }
            }
            g.touching = true;
            g.push(t, v);
        }
        None => s.mixrec.gestures.push(Gesture::new(strip, lane, mode, t, v)),
    }
    Ok(json!({"recording": true, "value": v}))
}

/// A control let go at `t`.
pub fn release(s: &mut Session, strip: TrackId, lane: &str, t: Tick) -> Result<Value> {
    let automatch = Tick::from_seconds_f64(s.prefs.audio.automatch_time);
    if s.mixrec.active() {
        if let Some(g) = s.mixrec.find(strip, lane) {
            g.touching = false;
            g.push(t, g.value());
            if g.mode == AutomationMode::Touch {
                g.released = Some(t);
                s.previews.live.release(strip, lane, t, automatch);
            }
            // Latch / Write keep holding the last value until playback stops.
            return Ok(json!({"recording": true}));
        }
        s.previews.live.clear(strip, lane);
        return Ok(json!({"recording": false}));
    }
    // no pass: commit the held value once
    let held = s.previews.live.get(strip, lane);
    s.previews.live.clear(strip, lane);
    match held {
        Some(o) => {
            set_value(s, strip, lane, o.value, t)?;
            Ok(json!({"recording": false, "value": o.value}))
        }
        None => Ok(json!({"recording": false})),
    }
}

/// End the pass at `t`: write every gesture as keyframes (one undo step).
pub fn record_stop(s: &mut Session, t_stop: Tick) -> Result<Value> {
    let rec = std::mem::take(&mut s.mixrec);
    let Some(seq_id) = rec.seq.filter(|_| !rec.gestures.is_empty()) else {
        s.previews.live.clear_all();
        return Ok(json!({"recording": false, "lanes": 0}));
    };
    let automatch = Tick::from_seconds_f64(s.prefs.audio.automatch_time);
    let prefs = s.prefs.audio.clone();
    let rate = seq_of(s, seq_id)?.settings.sample_rate.max(1) as i64;
    let eps = Tick::from_units(1, rate).max(Tick(1));
    let min_interval = if prefs.minimum_time_interval_thinning { Tick(prefs.minimum_time_ms as i64 * TICKS_PER_SECOND / 1000) } else { Tick::ZERO };
    let mut written = Vec::new();
    let mut keyframes = 0usize;
    let result = s.edit("Write Automation", |p, _| {
        let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        for mut g in rec.gestures {
            if g.points.is_empty() {
                continue;
            }
            let Some(tr) = seq.strip(g.strip).map(|c| c.into_owned()) else { continue };
            let hold = lane_info(&g.lane).is_some_and(|i| i.hold);
            let held = g.value();
            let end = match g.mode {
                AutomationMode::Touch => g.released.unwrap_or(t_stop).min(t_stop.max(g.points[0].0)),
                _ => t_stop,
            };
            g.push(end.max(g.last.0), held);
            g.flush();
            let mut pts = g.points.clone();
            if g.mode == AutomationMode::Touch && automatch.0 > 0 && !hold {
                let back = end + automatch;
                pts.push((back, underlying(&tr, &g.lane, back)));
            }
            let t0 = pts[0].0;
            let t1 = pts.last().map(|p| p.0).unwrap_or(t0);
            let tol = if hold {
                0.0
            } else if prefs.linear_keyframe_thinning {
                if g.lane == LANE_PAN { 0.25 } else { 0.05 }
            } else {
                0.0
            };
            let thinned = thin_points(&pts, tol, min_interval, hold);
            let before = (t0.0 > 0).then(|| underlying(&tr, &g.lane, t0 - eps));
            let after = tr.lane(&g.lane).filter(|p| p.keyframes.iter().any(|k| k.time > t1)).map(|_| underlying(&tr, &g.lane, t1 + eps));
            keyframes += thinned.len();
            let lane = g.lane.clone();
            seq.with_strip_mut(g.strip, |tr| {
                if let Some(param) = tr.lane_mut(&lane) {
                    write_lane(param, t0, t1, &thinned, before, after, eps, hold);
                }
            });
            written.push(json!({"strip": g.strip.0, "lane": lane, "from": t0.0, "to": t1.0, "keyframes": thinned.len(), "points": pts.len()}));
        }
        if prefs.switch_to_touch_after_write {
            for id in seq.strip_ids() {
                seq.with_strip_mut(id, |tr| {
                    if tr.mixer.mode == AutomationMode::Write {
                        tr.mixer.mode = AutomationMode::Touch;
                    }
                });
            }
        }
        Ok(())
    });
    s.previews.live.clear_all();
    s.previews.live.publish_project(s.project.clone());
    result?;
    Ok(json!({"recording": false, "lanes": written.len(), "keyframes": keyframes, "written": written}))
}

// ------------------------------------------------------------------------------------- helpers

fn active_seq(s: &Session) -> Result<&Sequence> {
    s.active_sequence().ok_or(EngineError::NoSequence)
}

/// Parse a strip reference: id, "A1", "S1", "Mix", or a strip name.
pub fn strip_ref(seq: &Sequence, v: &Value) -> Option<TrackId> {
    if let Some(id) = v.as_u64() {
        let id = TrackId(id);
        return seq.strip(id).map(|_| id);
    }
    let name = v.as_str()?.trim();
    if name.eq_ignore_ascii_case("mix") || name.eq_ignore_ascii_case("master") {
        return Some(MASTER_STRIP);
    }
    let idx = |pre: &str| {
        name.strip_prefix(pre).or_else(|| name.strip_prefix(&pre.to_lowercase())).and_then(|n| n.parse::<usize>().ok()).and_then(|n| n.checked_sub(1))
    };
    if let Some(i) = idx("A") {
        return seq.audio_tracks.get(i).map(|t| t.id);
    }
    if let Some(i) = idx("S") {
        return seq.submix_tracks.get(i).map(|t| t.id);
    }
    seq.audio_tracks.iter().chain(&seq.submix_tracks).find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| t.id)
}

/// Display reference of a strip ("A1", "S2", "Mix").
pub fn strip_label(seq: &Sequence, id: TrackId) -> String {
    if id == MASTER_STRIP {
        return "Mix".into();
    }
    if let Some(i) = seq.audio_tracks.iter().position(|t| t.id == id) {
        return format!("A{}", i + 1);
    }
    if let Some(i) = seq.submix_tracks.iter().position(|t| t.id == id) {
        return format!("S{}", i + 1);
    }
    id.0.to_string()
}

fn strip_p(s: &Session, p: &Value, cmd: &str) -> Result<TrackId> {
    let seq = active_seq(s)?;
    let v = p.get("strip").or_else(|| p.get("track")).ok_or_else(|| bad(cmd, "need `strip` (\"A1\", \"S1\", \"Mix\" or an id)"))?;
    strip_ref(seq, v).ok_or_else(|| bad(cmd, format!("no strip {v}")))
}

fn lane_p(s: &Session, p: &Value, strip: TrackId, cmd: &str) -> Result<String> {
    let lane = str_p(p, "lane").unwrap_or(LANE_VOLUME).to_string();
    let seq = active_seq(s)?;
    let tr = seq.strip(strip).ok_or_else(|| bad(cmd, "no such strip"))?;
    let ok = match parse_fx_lane(&lane) {
        Some((slot, param)) => tr.effects.get(slot).and_then(|e| e.param(param)).is_some_and(|p| p.value.as_f64().is_some()),
        None => lane_info(&lane).is_some() && (parse_send_lane(&lane).is_none_or(|i| i < tr.mixer.sends.len())),
    };
    if !ok {
        return Err(bad(cmd, format!("no lane `{lane}` on this strip (volume, pan, mute, send.<i>.level, fx.<slot>.<param>)")));
    }
    if strip == MASTER_STRIP && (lane == LANE_PAN || lane == LANE_MUTE || parse_send_lane(&lane).is_some()) {
        return Err(bad(cmd, "the Mix track has volume and effect lanes only"));
    }
    Ok(lane)
}

fn value_p(p: &Value, lane: &str, cmd: &str) -> Result<f64> {
    let v = p.get("value").ok_or_else(|| bad(cmd, "need `value`"))?;
    let x = v.as_f64().or_else(|| v.as_bool().map(|b| if b { 1.0 } else { 0.0 })).ok_or_else(|| bad(cmd, "`value` must be a number"))?;
    Ok(clamp_lane(lane, x))
}

/// Set a control outside a recording pass: the static value, or a keyframe at `t` when the lane
/// is automated and the strip reads automation.
pub fn set_value(s: &mut Session, strip: TrackId, lane: &str, v: f64, t: Tick) -> Result<()> {
    let lane = lane.to_string();
    let hold = lane_info(&lane).is_some_and(|i| i.hold);
    s.edit_sequence("Track Mixer Adjust", move |q, _, _| {
        q.with_strip_mut(strip, |tr| {
            let animated = tr.lane(&lane).is_some_and(Param::is_animated) && tr.mixer.mode.reads();
            if animated {
                if let Some(p) = tr.lane_mut(&lane) {
                    set_keyframe(p, t, v, hold);
                }
            } else {
                tr.set_lane_static(&lane, v);
            }
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })
}

fn set_keyframe(p: &mut Param, t: Tick, v: f64, hold: bool) {
    p.put_keyframe(t, ParamValue::Float(v));
    if hold {
        for k in p.keyframes.iter_mut() {
            k.interp = filmcraft_project::Interpolation::Hold;
        }
    }
}

fn mode_p(p: &Value) -> Option<AutomationMode> {
    str_p(p, "mode").and_then(AutomationMode::from_name)
}

fn channels_name(c: AudioChannels) -> &'static str {
    match c {
        AudioChannels::Mono => "Mono",
        AudioChannels::Stereo => "Stereo",
        AudioChannels::Surround51 => "5.1",
        AudioChannels::Adaptive => "Adaptive",
    }
}

fn channels_from(s: &str) -> Option<AudioChannels> {
    match s.to_ascii_lowercase().as_str() {
        "mono" => Some(AudioChannels::Mono),
        "stereo" => Some(AudioChannels::Stereo),
        "5.1" | "surround51" => Some(AudioChannels::Surround51),
        "adaptive" => Some(AudioChannels::Adaptive),
        _ => None,
    }
}

/// JSON view of one strip at time `t`.
pub fn strip_json(s: &Session, seq: &Sequence, id: TrackId, t: Tick) -> Value {
    let Some(tr) = seq.strip(id) else { return Value::Null };
    let live = &s.previews.live;
    let shown = |lane: &str| live.get(id, lane).map(|o| o.value).unwrap_or_else(|| underlying(&tr, lane, t));
    let lanes: serde_json::Map<String, Value> = tr
        .automated_lanes()
        .into_iter()
        .map(|k| {
            let kf: Vec<Value> = tr.lane_keyframes(&k).iter().map(|kf| json!([kf.time.0, kf.value.as_f64()])).collect();
            (k, Value::Array(kf))
        })
        .collect();
    let kind = if id == MASTER_STRIP {
        "mix"
    } else if seq.submix_tracks.iter().any(|t| t.id == id) {
        "submix"
    } else {
        "audio"
    };
    json!({
        "id": id.0,
        "ref": strip_label(seq, id),
        "name": tr.name,
        "kind": kind,
        "channels": channels_name(tr.channels),
        "mode": tr.mixer.mode.label(),
        "volumeDb": tr.volume_db,
        "pan": tr.pan,
        "muted": tr.muted,
        "solo": tr.solo,
        "recordArm": tr.mixer.record_arm,
        "soloSafe": tr.mixer.solo_safe,
        "inputMap": tr.mixer.input_map.label(),
        "output": tr.mixer.output.map(|o| strip_label(seq, o)).unwrap_or_else(|| if id == MASTER_STRIP { String::new() } else { "Mix".into() }),
        "inserts": tr.effects.iter().enumerate().map(|(i, e)| json!({"slot": i, "effect": e.effect, "name": e.def().map(|d| d.name).unwrap_or(""), "enabled": e.enabled, "postFader": e.post_fader})).collect::<Vec<_>>(),
        "sends": tr.mixer.sends.iter().enumerate().map(|(i, sd)| json!({"send": i, "target": strip_label(seq, sd.target), "levelDb": sd.level_db, "pan": sd.pan, "preFader": sd.pre_fader, "muted": sd.muted})).collect::<Vec<_>>(),
        "at": {"time": t.0, "volumeDb": shown(LANE_VOLUME), "pan": shown(LANE_PAN), "mute": shown(LANE_MUTE) >= 0.5},
        "lanes": lanes,
    })
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let seq = active_seq(s)?;
    let strips: Vec<Value> = seq.strip_ids().into_iter().map(|id| strip_json(s, seq, id, t)).collect();
    Ok(json!({
        "strips": strips,
        "recording": s.mixrec.active(),
        "gestures": s.mixrec.gestures.iter().map(|g| json!({"strip": strip_label(seq, g.strip), "lane": g.lane, "mode": g.mode.label(), "touching": g.touching, "points": g.points.len(), "value": g.value()})).collect::<Vec<_>>(),
        "latencySamples": filmcraft_render::mixer::graph_latency(seq),
        "automatchTime": s.prefs.audio.automatch_time,
    }))
}

fn set_strip(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.setStrip")?;
    let seq = active_seq(s)?;
    let output = match p.get("output") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => {
            let o = strip_ref(seq, v).ok_or_else(|| bad("mixer.setStrip", format!("no output {v}")))?;
            if o == MASTER_STRIP {
                Some(None)
            } else if o == id || !seq.submix_tracks.iter().any(|t| t.id == o) {
                return Err(bad("mixer.setStrip", "output must be the Mix or a submix"));
            } else if seq
                .submix_tracks
                .iter()
                .position(|t| t.id == id)
                .is_some_and(|me| seq.submix_tracks.iter().position(|t| t.id == o).is_some_and(|it| it <= me))
            {
                return Err(bad("mixer.setStrip", "a submix can only feed a submix after it (no feedback)"));
            } else {
                Some(Some(o))
            }
        }
    };
    let p = p.clone();
    s.edit_sequence("Track Mixer Settings", move |q, _, _| {
        q.with_strip_mut(id, |t| {
            if let Some(v) = str_p(&p, "name") {
                t.name = v.to_string();
            }
            if let Some(v) = f64_p(&p, "volumeDb") {
                t.volume_db = v.clamp(FADER_MIN_DB, FADER_MAX_DB);
            }
            if let Some(v) = f64_p(&p, "pan") {
                t.pan = v.clamp(-100.0, 100.0);
            }
            if let Some(v) = bool_p(&p, "muted") {
                t.muted = v;
            }
            if let Some(v) = bool_p(&p, "solo") {
                t.solo = v;
            }
            if let Some(v) = bool_p(&p, "recordArm") {
                t.mixer.record_arm = v;
            }
            if let Some(v) = bool_p(&p, "soloSafe") {
                t.mixer.solo_safe = v;
            }
            if let Some(m) = mode_p(&p) {
                t.mixer.mode = m;
            }
            if let Some(m) = str_p(&p, "inputMap").and_then(InputMap::from_name) {
                t.mixer.input_map = m;
            }
            if let Some(c) = str_p(&p, "channels").and_then(channels_from) {
                t.channels = c;
            }
            if let Some(o) = output {
                t.mixer.output = o;
            }
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    let seq = active_seq(s)?;
    Ok(strip_json(s, seq, id, s.playhead()))
}

fn add_submix(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::to_string);
    let channels = str_p(p, "channels").and_then(channels_from).unwrap_or(AudioChannels::Stereo);
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let id = s.edit("Add Submix Track", |pr, _| {
        let id = TrackId(pr.alloc_id());
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let n = seq.submix_tracks.len() + 1;
        let mut t = Track::new(id, TrackKind::Audio, name.unwrap_or_else(|| format!("Submix {n}")));
        t.channels = channels;
        seq.submix_tracks.push(t);
        Ok(id)
    })?;
    let seq = active_seq(s)?;
    Ok(json!({"id": id.0, "ref": strip_label(seq, id)}))
}

fn delete_submix(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.deleteSubmix")?;
    if !active_seq(s)?.submix_tracks.iter().any(|t| t.id == id) {
        return Err(bad("mixer.deleteSubmix", "not a submix"));
    }
    s.edit_sequence("Delete Submix Track", move |q, _, _| {
        q.submix_tracks.retain(|t| t.id != id);
        // re-route everything that fed it to the Mix and drop sends to it
        for sid in q.strip_ids() {
            q.with_strip_mut(sid, |t| {
                if t.mixer.output == Some(id) {
                    t.mixer.output = None;
                }
                while let Some(i) = t.mixer.sends.iter().position(|sd| sd.target == id) {
                    t.remove_send(i);
                }
            });
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn slot_p(p: &Value, cmd: &str) -> Result<usize> {
    u64_p(p, "slot").map(|v| v as usize).ok_or_else(|| bad(cmd, "need `slot` (0-based)"))
}

fn add_insert(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.addInsert")?;
    let eid = str_p(p, "effect").ok_or_else(|| bad("mixer.addInsert", "need `effect` (an audio effect id)"))?;
    let def = find_effect(eid)
        .filter(|d| d.kind == filmcraft_project::EffectKind::Audio && !d.intrinsic)
        .ok_or_else(|| bad("mixer.addInsert", format!("`{eid}` is not an audio effect")))?;
    let mut inst = def.instance();
    inst.post_fader = bool_p(p, "postFader").unwrap_or(false);
    let slot = u64_p(p, "slot").map(|v| v as usize);
    let n = active_seq(s)?.strip(id).map(|t| t.effects.len()).unwrap_or(0);
    if n >= pm::MAX_INSERTS {
        return Err(bad("mixer.addInsert", format!("all {} effect slots are in use", pm::MAX_INSERTS)));
    }
    let at = slot.unwrap_or(n).min(n);
    s.edit_sequence(&format!("Add Track Effect {}", def.name), move |q, _, _| {
        q.with_strip_mut(id, |t| t.effects.insert(at, inst)).ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    Ok(json!({"slot": at}))
}

fn remove_insert(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.removeInsert")?;
    let slot = slot_p(p, "mixer.removeInsert")?;
    let ok = s.edit_sequence("Remove Track Effect", move |q, _, _| Ok(q.with_strip_mut(id, |t| t.remove_insert(slot)).unwrap_or(false)))?;
    if !ok {
        return Err(bad("mixer.removeInsert", "no effect in that slot"));
    }
    Ok(Value::Null)
}

fn set_insert(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.setInsert")?;
    let slot = slot_p(p, "mixer.setInsert")?;
    let p = p.clone();
    s.edit_sequence("Track Effect Settings", move |q, _, _| {
        q.with_strip_mut(id, |t| -> Result<()> {
            let e = t.effects.get_mut(slot).ok_or_else(|| bad("mixer.setInsert", "no effect in that slot"))?;
            if let Some(v) = bool_p(&p, "enabled") {
                e.enabled = v;
            }
            if let Some(v) = bool_p(&p, "postFader") {
                e.post_fader = v;
            }
            if let Some(obj) = p.get("params").and_then(Value::as_object) {
                for (k, v) in obj {
                    let par = e.param_mut(k).ok_or_else(|| bad("mixer.setInsert", format!("no parameter `{k}`")))?;
                    par.value = match (&par.value, v) {
                        (ParamValue::Bool(_), Value::Bool(b)) => ParamValue::Bool(*b),
                        (ParamValue::Choice(_), v) => ParamValue::Choice(v.as_u64().unwrap_or(0) as u32),
                        (_, v) => ParamValue::Float(v.as_f64().ok_or_else(|| bad("mixer.setInsert", format!("`{k}` needs a number")))?),
                    };
                }
            }
            Ok(())
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))?
    })?;
    Ok(Value::Null)
}

fn send_target(s: &Session, p: &Value, id: TrackId, cmd: &str) -> Result<TrackId> {
    let seq = active_seq(s)?;
    let v = p.get("target").ok_or_else(|| bad(cmd, "need `target` (a submix)"))?;
    let t = strip_ref(seq, v).filter(|t| seq.submix_tracks.iter().any(|x| x.id == *t)).ok_or_else(|| bad(cmd, "`target` must be a submix"))?;
    let pos = |x: TrackId| seq.submix_tracks.iter().position(|y| y.id == x);
    if t == id || pos(id).is_some_and(|me| pos(t).is_some_and(|it| it <= me)) {
        return Err(bad(cmd, "a submix can only send to a submix after it (no feedback)"));
    }
    Ok(t)
}

fn add_send(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.addSend")?;
    if id == MASTER_STRIP {
        return Err(bad("mixer.addSend", "the Mix track has no sends"));
    }
    let target = send_target(s, p, id, "mixer.addSend")?;
    let mut snd = TrackSend::new(target);
    snd.level_db = f64_p(p, "levelDb").unwrap_or(0.0).clamp(FADER_MIN_DB, FADER_MAX_DB);
    snd.pre_fader = bool_p(p, "preFader").unwrap_or(false);
    snd.pan = f64_p(p, "pan").unwrap_or(0.0).clamp(-100.0, 100.0);
    if active_seq(s)?.strip(id).is_some_and(|t| t.mixer.sends.len() >= pm::MAX_SENDS) {
        return Err(bad("mixer.addSend", format!("all {} send slots are in use", pm::MAX_SENDS)));
    }
    let i = s.edit_sequence("Add Send", move |q, _, _| {
        q.with_strip_mut(id, |t| {
            t.mixer.sends.push(snd);
            t.mixer.sends.len() - 1
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    Ok(json!({"send": i}))
}

fn set_send(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.setSend")?;
    let i = u64_p(p, "send").ok_or_else(|| bad("mixer.setSend", "need `send` (0-based)"))? as usize;
    let target = if p.get("target").is_some() { Some(send_target(s, p, id, "mixer.setSend")?) } else { None };
    let p = p.clone();
    s.edit_sequence("Send Settings", move |q, _, _| {
        q.with_strip_mut(id, |t| -> Result<()> {
            let sd = t.mixer.sends.get_mut(i).ok_or_else(|| bad("mixer.setSend", "no such send"))?;
            if let Some(v) = f64_p(&p, "levelDb") {
                sd.level_db = v.clamp(FADER_MIN_DB, FADER_MAX_DB);
            }
            if let Some(v) = f64_p(&p, "pan") {
                sd.pan = v.clamp(-100.0, 100.0);
            }
            if let Some(v) = bool_p(&p, "preFader") {
                sd.pre_fader = v;
            }
            if let Some(v) = bool_p(&p, "muted") {
                sd.muted = v;
            }
            if let Some(t) = target {
                sd.target = t;
            }
            Ok(())
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))?
    })?;
    Ok(Value::Null)
}

fn remove_send(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.removeSend")?;
    let i = u64_p(p, "send").ok_or_else(|| bad("mixer.removeSend", "need `send` (0-based)"))? as usize;
    let ok = s.edit_sequence("Remove Send", move |q, _, _| Ok(q.with_strip_mut(id, |t| t.remove_send(i)).unwrap_or(false)))?;
    if !ok {
        return Err(bad("mixer.removeSend", "no such send"));
    }
    Ok(Value::Null)
}

fn set_kf(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.setKeyframe")?;
    let lane = lane_p(s, p, id, "mixer.setKeyframe")?;
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let v = value_p(p, &lane, "mixer.setKeyframe")?;
    let hold = lane_info(&lane).is_some_and(|i| i.hold);
    s.edit_sequence("Add Track Keyframe", move |q, _, _| {
        q.with_strip_mut(id, |tr| {
            if let Some(par) = tr.lane_mut(&lane) {
                if par.keyframes.is_empty() {
                    // the first keyframe keeps the current value everywhere else
                    let cur = par.value.as_f64().unwrap_or(v);
                    par.value = ParamValue::Float(cur);
                }
                set_keyframe(par, t, v, hold);
            }
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    Ok(json!({"time": t.0, "value": v}))
}

fn delete_kf(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.deleteKeyframe")?;
    let lane = lane_p(s, p, id, "mixer.deleteKeyframe")?;
    let t = time_p(s, p, "").ok_or_else(|| bad("mixer.deleteKeyframe", "need `time`"))?;
    let ok = s.edit_sequence("Delete Track Keyframe", move |q, _, _| {
        Ok(q.with_strip_mut(id, |tr| tr.lane_mut(&lane).is_some_and(|par| par.remove_keyframe_at(t))).unwrap_or(false))
    })?;
    if !ok {
        return Err(bad("mixer.deleteKeyframe", "no keyframe at that time"));
    }
    Ok(Value::Null)
}

fn move_kf(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.moveKeyframe")?;
    let lane = lane_p(s, p, id, "mixer.moveKeyframe")?;
    let t = time_p(s, p, "").ok_or_else(|| bad("mixer.moveKeyframe", "need `time` (the keyframe)"))?;
    let nt = time_p(s, p, "new");
    let nv = p.get("value").map(|_| value_p(p, &lane, "mixer.moveKeyframe")).transpose()?;
    let ok = s.edit_sequence("Move Track Keyframe", move |q, _, _| {
        Ok(q.with_strip_mut(id, |tr| {
            let Some(par) = tr.lane_mut(&lane) else { return false };
            let Ok(i) = par.keyframes.binary_search_by_key(&t, |k| k.time) else { return false };
            let mut k = par.keyframes.remove(i);
            if let Some(nt) = nt {
                // keep keyframe order: clamp between the neighbours
                let lo = i.checked_sub(1).map(|j| par.keyframes[j].time + Tick(1)).unwrap_or(Tick::ZERO);
                let hi = par.keyframes.get(i).map(|n| n.time - Tick(1)).unwrap_or(Tick(i64::MAX));
                k.time = nt.clamp(lo, hi);
            }
            if let Some(v) = nv {
                k.value = ParamValue::Float(v);
            }
            let j = par.keyframes.partition_point(|x| x.time < k.time);
            par.keyframes.insert(j, k);
            true
        })
        .unwrap_or(false))
    })?;
    if !ok {
        return Err(bad("mixer.moveKeyframe", "no keyframe at that time"));
    }
    Ok(Value::Null)
}

fn clear_lane(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.clearLane")?;
    let lane = lane_p(s, p, id, "mixer.clearLane")?;
    let t = s.playhead();
    s.edit_sequence("Clear Track Keyframes", move |q, _, _| {
        q.with_strip_mut(id, |tr| {
            let v = tr.lane_value(&lane, t);
            if let Some(par) = tr.lane_mut(&lane) {
                par.keyframes.clear();
            }
            tr.set_lane_static(&lane, v);
            if parse_fx_lane(&lane).is_none() {
                tr.mixer.lanes.remove(&lane);
            }
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    Ok(Value::Null)
}

fn write_automation(s: &mut Session, p: &Value) -> Result<Value> {
    let id = strip_p(s, p, "mixer.writeAutomation")?;
    let lane = lane_p(s, p, id, "mixer.writeAutomation")?;
    let pts: Vec<(Tick, f64)> = p
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("mixer.writeAutomation", "need `points`: [[ticks, value], …]"))?
        .iter()
        .filter_map(|v| Some((Tick(v.get(0)?.as_i64()?), clamp_lane(&lane, v.get(1)?.as_f64()?))))
        .collect();
    if pts.is_empty() {
        return Err(bad("mixer.writeAutomation", "no points"));
    }
    let hold = lane_info(&lane).is_some_and(|i| i.hold);
    let tol = f64_p(p, "tolerance").unwrap_or(if hold { 0.0 } else { 0.05 });
    let thinned = thin_points(&pts, tol, Tick::ZERO, hold);
    let (t0, t1) = (thinned[0].0, thinned[thinned.len() - 1].0);
    let rate = active_seq(s)?.settings.sample_rate.max(1) as i64;
    let eps = Tick::from_units(1, rate).max(Tick(1));
    let n = thinned.len();
    s.edit_sequence("Write Automation", move |q, _, _| {
        q.with_strip_mut(id, |tr| {
            let before = (t0.0 > 0).then(|| underlying(tr, &lane, t0 - eps));
            let after = tr.lane(&lane).filter(|p| p.keyframes.iter().any(|k| k.time > t1)).map(|_| underlying(tr, &lane, t1 + eps));
            if let Some(par) = tr.lane_mut(&lane) {
                write_lane(par, t0, t1, &thinned, before, after, eps, hold);
            }
        })
        .ok_or_else(|| EngineError::Other("no such strip".into()))
    })?;
    Ok(json!({"keyframes": n, "points": pts.len()}))
}

/// Audio Clip Mixer: set the clip's Volume level / Panner balance (or any scalar clip effect
/// parameter) at the playhead. With `keyframe` (or when already animated) it writes a keyframe at the
/// playhead; a drag (same clip and parameter, no other edit between) is one undo step.
fn clip_set(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = filmcraft_project::ClipId(u64_p(p, "clip").ok_or_else(|| bad("clipMixer.set", "need `clip`"))?);
    let effect = str_p(p, "effect").unwrap_or("volume").to_string();
    let param = str_p(p, "param").unwrap_or("level").to_string();
    let v = f64_p(p, "value").ok_or_else(|| bad("clipMixer.set", "need `value`"))?;
    let kf = bool_p(p, "keyframe").unwrap_or(false);
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let key = format!("clipMixer:{}:{effect}:{param}", clip.0);
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    s.edit_merged("Audio Clip Mixer", &key, |pr, _| {
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let (_, it) = seq.find_item_mut(clip).ok_or_else(|| bad("clipMixer.set", "no such clip"))?;
        let mt = it.source_time_at(t.clamp(it.start, it.end()));
        let e = it.effect_mut(&effect).ok_or_else(|| bad("clipMixer.set", format!("the clip has no `{effect}` effect")))?;
        let par = e.param_mut(&param).ok_or_else(|| bad("clipMixer.set", format!("no parameter `{param}`")))?;
        let v = if param == "level" { v.clamp(FADER_MIN_DB, FADER_MAX_DB) } else { v.clamp(-100.0, 100.0) };
        if kf || par.is_animated() {
            par.put_keyframe(mt, ParamValue::Float(v));
        } else {
            par.value = ParamValue::Float(v);
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn not_recording(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.mixrec.active() { Err("an automation pass is already recording".into()) } else { Ok(()) }
}

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("mixer.inspect", "Inspect Audio Track Mixer", &[], None, r#"{"time":ticks?}"#, always, inspect, false),
        spec(
            "mixer.setStrip",
            "Track Mixer Settings",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"name":str?,"volumeDb":f64?,"pan":f64?,"muted":bool?,"solo":bool?,"recordArm":bool?,"soloSafe":bool?,"mode":"Off|Read|Latch|Touch|Write"?,"output":"Mix"|"S1"?,"inputMap":"Stereo|Left|Right|Swap|Mono"?,"channels":"Mono|Stereo|5.1"?}"#,
            has_seq,
            set_strip,
            true,
        ),
        spec(
            "mixer.setValue",
            "Set Mixer Control",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":"volume|pan|mute|send.<i>.level|fx.<slot>.<param>","value":f64,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let id = strip_p(s, p, "mixer.setValue")?;
                let lane = lane_p(s, p, id, "mixer.setValue")?;
                let v = value_p(p, &lane, "mixer.setValue")?;
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                if s.mixrec.active() {
                    touch(s, id, &lane, v, t)?;
                    return release(s, id, &lane, t);
                }
                set_value(s, id, &lane, v, t)?;
                Ok(json!({"value": v}))
            },
            true,
        ),
        spec(
            "mixer.touch",
            "Touch Mixer Control",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":"volume|pan|mute|send.<i>.level|fx.<slot>.<param>","value":f64,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let id = strip_p(s, p, "mixer.touch")?;
                let lane = lane_p(s, p, id, "mixer.touch")?;
                let v = value_p(p, &lane, "mixer.touch")?;
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                touch(s, id, &lane, v, t)
            },
            false,
        ),
        spec(
            "mixer.release",
            "Release Mixer Control",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let id = strip_p(s, p, "mixer.release")?;
                let lane = lane_p(s, p, id, "mixer.release")?;
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                release(s, id, &lane, t)
            },
            true,
        ),
        spec(
            "mixer.recordStart",
            "Start Automation Pass",
            &[],
            None,
            r#"{"time":ticks?}"#,
            not_recording,
            |s, p| {
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                record_start(s, t)
            },
            false,
        ),
        spec(
            "mixer.recordStop",
            "Write Automation",
            &[],
            None,
            r#"{"time":ticks?}"#,
            has_seq,
            |s, p| {
                let t = time_p(s, p, "").unwrap_or(s.playhead());
                record_stop(s, t)
            },
            true,
        ),
        spec(
            "clipMixer.set",
            "Audio Clip Mixer Adjust",
            &[],
            None,
            r#"{"clip":id,"effect":"volume"|"panner","param":"level"|"balance","value":f64,"keyframe":bool?,"time":ticks?,"begin":bool?}"#,
            has_seq,
            clip_set,
            true,
        ),
        spec("mixer.addSubmix", "Add Audio Submix Track", &["Sequence"], None, r#"{"name":str?,"channels":"Mono|Stereo|5.1"?}"#, has_seq, add_submix, true),
        spec("mixer.deleteSubmix", "Delete Submix Track", &[], None, r#"{"strip":"S1"|id}"#, has_seq, delete_submix, true),
        spec(
            "mixer.addInsert",
            "Add Track Effect",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"effect":str,"slot":n?,"postFader":bool?}"#,
            has_seq,
            add_insert,
            true,
        ),
        spec("mixer.removeInsert", "Remove Track Effect", &[], None, r#"{"strip":"A1"|"S1"|"Mix"|id,"slot":n}"#, has_seq, remove_insert, true),
        spec(
            "mixer.setInsert",
            "Track Effect Settings",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"slot":n,"enabled":bool?,"postFader":bool?,"params":{id:value}?}"#,
            has_seq,
            set_insert,
            true,
        ),
        spec(
            "mixer.addSend",
            "Add Send",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|id,"target":"S1"|id,"levelDb":f64?,"preFader":bool?,"pan":f64?}"#,
            has_seq,
            add_send,
            true,
        ),
        spec(
            "mixer.setSend",
            "Send Settings",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|id,"send":n,"levelDb":f64?,"pan":f64?,"preFader":bool?,"muted":bool?,"target":"S1"?}"#,
            has_seq,
            set_send,
            true,
        ),
        spec("mixer.removeSend", "Remove Send", &[], None, r#"{"strip":"A1"|"S1"|id,"send":n}"#, has_seq, remove_send, true),
        spec(
            "mixer.setKeyframe",
            "Add Track Keyframe",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?,"time":ticks?,"value":f64}"#,
            has_seq,
            set_kf,
            true,
        ),
        spec("mixer.deleteKeyframe", "Delete Track Keyframe", &[], None, r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?,"time":ticks}"#, has_seq, delete_kf, true),
        spec(
            "mixer.moveKeyframe",
            "Move Track Keyframe",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?,"time":ticks,"newTime":ticks?,"value":f64?}"#,
            has_seq,
            move_kf,
            true,
        ),
        spec("mixer.clearLane", "Clear Track Keyframes", &[], None, r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?}"#, has_seq, clear_lane, true),
        spec(
            "mixer.writeAutomation",
            "Write Automation Points",
            &[],
            None,
            r#"{"strip":"A1"|"S1"|"Mix"|id,"lane":str?,"points":[[ticks,value]],"tolerance":f64?}"#,
            has_seq,
            write_automation,
            true,
        ),
    ]
}
