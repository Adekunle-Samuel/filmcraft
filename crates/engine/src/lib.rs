//! The FilmCraft engine façade.
//!
//! Every user-visible action is a command with a stable id (`sequence.addEdit`,
//! `timeline.trim`, `markers.markIn`…) and JSON parameters, dispatched through
//! [`Session::execute`]. The egui UI, the CLI, the control channel and the MCP server all use this
//! one entry point; that is what makes the UI swappable and the whole app agent-drivable.
//!
//! The project is an `Arc<Project>` edited copy-on-write; undo keeps whole-project snapshots (cheap
//! thanks to structural sharing of untouched items).

pub mod commands;
pub mod demo;
pub mod media_pool;

use std::sync::Arc;

use filmcraft_edit::EditCtx;
use filmcraft_project::{ClipId, ItemId, Project, Sequence, TrackId, TrackKind};
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use commands::{CommandSpec, command_specs, find as find_command};
pub use filmcraft_export as export;
pub use filmcraft_project as project;
pub use filmcraft_render as render;
pub use filmcraft_time as time;
pub use media_pool::MediaPool;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    #[error("command `{0}` is not available right now: {1}")]
    Disabled(String, String),
    #[error("invalid parameters for `{cmd}`: {msg}")]
    BadParams { cmd: String, msg: String },
    #[error("no active sequence")]
    NoSequence,
    #[error("edit failed: {0}")]
    Edit(#[from] filmcraft_edit::EditError),
    #[error("media: {0}")]
    Media(#[from] filmcraft_media::MediaError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Host services (file access, clipboard…) injected by the frontend.
pub trait Services: Send + Sync {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>>;
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()>;
}

/// Native filesystem services.
pub struct FsServices;
impl Services for FsServices {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        // Atomic save: write a sibling temp file then rename over the target.
        let tmp = format!("{path}.fcsave~");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }
}

/// Undo history of whole-project snapshots.
#[derive(Clone, Default)]
pub struct History {
    pub undo: Vec<(String, Arc<Project>)>,
    pub redo: Vec<(String, Arc<Project>)>,
    /// Labels of all applied states, oldest first (History panel).
    pub limit: usize,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Source patching / track targeting (the track header "V1/A1" source buttons + target toggles).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Targeting {
    /// Timeline tracks targeted for navigation / paste / match frame.
    pub targeted: Vec<TrackId>,
    /// Destination track for the source's video (source patch), None = unpatched.
    pub video_dest: Option<TrackId>,
    pub audio_dest: Option<TrackId>,
}

/// Editing state that commands depend on (not project data, but headless-relevant).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EditorState {
    pub active_sequence: Option<ItemId>,
    /// Playhead per sequence.
    pub playheads: std::collections::BTreeMap<ItemId, Tick>,
    /// Item loaded in the Source monitor and its playhead (media time).
    pub source_item: Option<ItemId>,
    pub source_playhead: Tick,
    /// Selected timeline items.
    pub selection: Vec<ClipId>,
    /// Selected project panel items.
    pub project_selection: Vec<ItemId>,
    pub targeting: std::collections::BTreeMap<ItemId, Targeting>,
    pub snapping: bool,
    pub linked_selection: bool,
    /// Open sequences (timeline tabs), in tab order.
    pub open_sequences: Vec<ItemId>,
    /// Timeline clipboard (serialized track items with their relative track index).
    #[serde(skip)]
    pub clipboard: Vec<(TrackKind, usize, filmcraft_project::TrackItem)>,
    pub default_video_transition: String,
    pub default_audio_transition: String,
}

/// Events for frontends (drained each frame).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Event {
    ProjectChanged { revision: u64 },
    Toast { message: String, error: bool },
    OpenSequence(ItemId),
    OpenSource(ItemId),
}

pub struct Session {
    pub project: Arc<Project>,
    pub history: History,
    pub revision: u64,
    pub saved_revision: u64,
    pub path: Option<String>,
    pub state: EditorState,
    pub media: Arc<MediaPool>,
    pub services: Arc<dyn Services>,
    pub events: Vec<Event>,
    /// Commands executed (for macros/debugging): (id, params).
    pub journal: Vec<(String, Value)>,
    /// Background jobs (exports, …).
    pub jobs: Vec<Job>,
}

/// A background job with shared progress.
#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub label: String,
    pub progress: Arc<filmcraft_export::Progress>,
    pub result: Arc<std::sync::Mutex<Option<std::result::Result<filmcraft_export::Report, String>>>>,
}

impl Job {
    pub fn to_json(&self) -> Value {
        use std::sync::atomic::Ordering;
        let res = self.result.lock().unwrap_or_else(|e| e.into_inner()).clone();
        serde_json::json!({
            "id": self.id,
            "label": self.label,
            "progress": self.progress.fraction(),
            "done": self.progress.done.load(Ordering::Relaxed),
            "total": self.progress.total.load(Ordering::Relaxed),
            "status": self.progress.status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            "finished": res.is_some(),
            "result": match res { Some(Ok(r)) => serde_json::to_value(r).unwrap_or_default(), Some(Err(e)) => serde_json::json!({"error": e}), None => Value::Null },
        })
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new(Arc::new(FsServices))
    }
}

impl Session {
    pub fn new(services: Arc<dyn Services>) -> Self {
        Self {
            project: Arc::new(Project::new("Untitled")),
            history: History { limit: 200, ..Default::default() },
            revision: 1,
            saved_revision: 1,
            path: None,
            state: EditorState {
                snapping: true,
                linked_selection: true,
                default_video_transition: "cross_dissolve".into(),
                default_audio_transition: "constant_power".into(),
                ..Default::default()
            },
            media: Arc::new(MediaPool::default()),
            services,
            events: Vec::new(),
            journal: Vec::new(),
            jobs: Vec::new(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Run a command by id.
    pub fn execute(&mut self, id: &str, params: Value) -> Result<Value> {
        let spec = commands::find(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        (spec.enabled)(self).map_err(|why| EngineError::Disabled(id.to_string(), why))?;
        let r = (spec.run)(self, &params);
        if r.is_ok() && spec.journal {
            self.journal.push((id.to_string(), params));
            if self.journal.len() > 10_000 {
                self.journal.drain(..1000);
            }
        }
        r
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        commands::find(id).is_some_and(|c| (c.enabled)(self).is_ok())
    }

    /// Apply an undoable project edit. The closure gets a mutable copy; on error nothing changes.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut Project, &mut EditorState) -> Result<R>) -> Result<R> {
        let mut p = (*self.project).clone();
        let mut st = self.state.clone();
        let r = f(&mut p, &mut st)?;
        let old = std::mem::replace(&mut self.project, Arc::new(p));
        self.history.undo.push((label.to_string(), old));
        if self.history.undo.len() > self.history.limit {
            self.history.undo.remove(0);
        }
        self.history.redo.clear();
        self.state = st;
        self.bump();
        Ok(r)
    }

    /// Edit the active sequence with the edit-algebra context.
    pub fn edit_sequence<R>(&mut self, label: &str, f: impl FnOnce(&mut Sequence, &mut EditCtx, &mut EditorState) -> Result<R>) -> Result<R> {
        let seq_id = self.state.active_sequence.ok_or(EngineError::NoSequence)?;
        let media = self.media.clone();
        self.edit(label, move |p, st| {
            let project_snapshot = p.clone();
            let durations = move |id: ItemId| -> Option<Tick> { media_duration(&project_snapshot, &media, id) };
            let min = p.sequence(seq_id).map(|s| s.settings.frame_rate.frame_duration()).unwrap_or(Tick(1));
            let mut next = p.next_id;
            let r = {
                let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
                let mut ctx = EditCtx { next_id: &mut next, media_duration: &durations, min_duration: min };
                f(seq, &mut ctx, st)?
            };
            p.next_id = next;
            if let Some(s) = p.sequence(seq_id) {
                s.check().map_err(EngineError::Other)?;
            }
            Ok(r)
        })
    }

    pub fn undo(&mut self) -> Option<String> {
        let (label, prev) = self.history.undo.pop()?;
        let cur = std::mem::replace(&mut self.project, prev);
        self.history.redo.push((label.clone(), cur));
        self.fix_state();
        self.bump();
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let (label, next) = self.history.redo.pop()?;
        let cur = std::mem::replace(&mut self.project, next);
        self.history.undo.push((label.clone(), cur));
        self.fix_state();
        self.bump();
        Some(label)
    }

    /// Drop dangling references after undo/redo/delete.
    pub fn fix_state(&mut self) {
        let p = self.project.clone();
        if let Some(s) = self.state.active_sequence
            && p.sequence(s).is_none()
        {
            self.state.active_sequence = p.sequences().next().map(|i| i.id);
        }
        self.state.open_sequences.retain(|s| p.sequence(*s).is_some());
        if let Some(s) = self.state.source_item
            && p.item(s).is_none()
        {
            self.state.source_item = None;
        }
        if let Some(seq) = self.state.active_sequence.and_then(|s| p.sequence(s)) {
            self.state.selection.retain(|c| seq.find_item(*c).is_some());
        } else {
            self.state.selection.clear();
        }
        self.state.project_selection.retain(|i| p.item(*i).is_some());
    }

    fn bump(&mut self) {
        self.revision += 1;
        self.events.push(Event::ProjectChanged { revision: self.revision });
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.events.push(Event::Toast { message: msg.into(), error: false });
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    // ---- accessors ----

    pub fn active_sequence(&self) -> Option<&Sequence> {
        self.state.active_sequence.and_then(|s| self.project.sequence(s))
    }

    pub fn playhead(&self) -> Tick {
        self.state.active_sequence.and_then(|s| self.state.playheads.get(&s).copied()).unwrap_or_default()
    }

    pub fn set_playhead(&mut self, t: Tick) {
        if let Some(s) = self.state.active_sequence {
            let rate = self.active_sequence().map(|s| s.settings.frame_rate).unwrap_or_default();
            self.state.playheads.insert(s, rate.snap(t.max(Tick::ZERO)));
        }
    }

    pub fn sequence_rate(&self) -> FrameRate {
        self.active_sequence().map(|s| s.settings.frame_rate).unwrap_or_default()
    }

    pub fn targeting(&self) -> Targeting {
        let Some(sid) = self.state.active_sequence else { return Targeting::default() };
        if let Some(t) = self.state.targeting.get(&sid) {
            return t.clone();
        }
        // Default: V1/A1 patched and all tracks targeted.
        let seq = self.active_sequence();
        Targeting {
            targeted: seq.map(|s| s.all_tracks().map(|t| t.id).collect()).unwrap_or_default(),
            video_dest: seq.and_then(|s| s.video_tracks.first().map(|t| t.id)),
            audio_dest: seq.and_then(|s| s.audio_tracks.first().map(|t| t.id)),
        }
    }

    /// The media source for an item (creating it on first use).
    pub fn source(&self, item: ItemId) -> Option<filmcraft_media::SharedSource> {
        self.media.source_for(&self.project, item, &*self.services)
    }

    /// Render the active sequence at the playhead (CPU reference path).
    pub fn render_program(&self, scale: f32) -> Option<filmcraft_render::Image> {
        let seq = self.state.active_sequence?;
        let provider = self.media.provider(self.project.clone(), self.services.clone());
        Some(filmcraft_render::render_sequence(&self.project, seq, self.playhead(), filmcraft_render::RenderOptions { scale, ..Default::default() }, &provider))
    }
}

/// Media duration of an item (None for stills/adjustment layers = unlimited handles).
pub fn media_duration(p: &Project, _pool: &MediaPool, id: ItemId) -> Option<Tick> {
    let it = p.item(id)?;
    match &it.kind {
        filmcraft_project::ItemKind::Media(m) => match m.info.kind {
            filmcraft_media::MediaKind::Still | filmcraft_media::MediaKind::Synthetic => None,
            _ => Some(m.info.duration),
        },
        filmcraft_project::ItemKind::Sequence(s) => Some(s.duration()),
        filmcraft_project::ItemKind::Subclip { range, .. } => Some(range.end()),
        filmcraft_project::ItemKind::AdjustmentLayer { .. } => None,
    }
}

#[cfg(test)]
mod tests;
