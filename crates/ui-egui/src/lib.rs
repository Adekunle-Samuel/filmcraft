//! The FilmCraft egui frontend.
//!
//! Thin by design: all project changes go through `filmcraft_engine::Session::execute`; this crate
//! owns only presentation state ([`state::UiState`]), GPU textures, the playback clock and the
//! control-channel handlers. Swap it for another toolkit without touching the engine.

pub mod automation;
pub mod control;
pub mod dock;
pub mod frames;
pub mod header;
pub mod icons;
pub mod menus;
pub mod panels;
pub mod state;
pub mod theme;
pub mod widgets;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use egui::{Color32, TextureHandle, TextureOptions};
use filmcraft_engine::{Services, Session};
use filmcraft_time::Tick;
use serde_json::{Value, json};

pub use control::ControlRequest;
use dock::PanelKind;
use frames::{FrameKey, FrameServer, Target};
use state::UiState;
use theme::{ThemeKind, Tokens};

/// Audio output provided by the platform layer (cpal on desktop, WebAudio on web).
pub trait AudioOut {
    /// Start output; `fill(buffer, channels)` is called on the audio thread with interleaved f32.
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String>;
    fn stop(&mut self);
    /// Device sample rate.
    fn sample_rate(&self) -> u32;
    /// Frames played since `start` (the playback master clock), if the device reports it.
    fn played_frames(&self) -> Option<u64>;
}

/// Host hooks for native file dialogs etc.
#[derive(Default)]
pub struct HostHooks {
    pub pick_files: Option<Box<dyn FnMut(&[&str]) -> Vec<String>>>,
    pub pick_save: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    pub pick_open_project: Option<Box<dyn FnMut() -> Option<String>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    About,
    Shortcuts,
    NewSequence,
}

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    /// Wall-clock (egui time, s) and timeline tick when playback (re)started.
    anchor_time: f64,
    anchor_tick: Tick,
    /// Audio frames played at anchor (when the audio clock drives).
    pub audio_clock: bool,
    pub dropped: u64,
    pub shown: u64,
    last_frame: i64,
}

pub struct FilmcraftApp {
    pub session: Session,
    pub ui: UiState,
    pub tokens: Tokens,
    pub frames: Arc<FrameServer>,
    pub playback: Playback,
    pub audio: Option<Box<dyn AudioOut>>,
    pub hooks: HostHooks,
    pub dialog: Option<Dialog>,
    pub auto: automation::Registry,
    /// Named textures (monitors, thumbnails) with the key they show.
    textures: HashMap<String, (FrameKey, TextureHandle)>,
    control_rx: Option<Receiver<ControlRequest>>,
    /// Requests waiting for the UI to show an element: (request, give-up time).
    deferred: Vec<(ControlRequest, f64)>,
    last_ui_time: f64,
    pub(crate) synthetic: Vec<egui::Event>,
    pending_screenshots: Vec<(u64, Option<String>, Option<[f32; 4]>, Sender<Value>)>,
    queued_screenshots: Vec<(u64, f64, u32)>,
    input_waiters: Vec<Sender<Value>>,
    next_token: u64,
    styled: bool,
    fonts_ready: bool,
    pub integrated_titlebar: bool,
    pub last_timeline_width: f32,
    pub fps: f32,
    last_time: f64,
    bindings: Vec<(egui::Modifiers, egui::Key, String)>,
    pub toast: Option<(String, f64)>,
    pub tl: panels::timeline::TlState,
}

impl FilmcraftApp {
    pub fn new(session: Session) -> Self {
        let frames = Arc::new(FrameServer::new(
            session.media.clone(),
            session.services.clone(),
            std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3),
        ));
        Self {
            session,
            ui: UiState::default(),
            tokens: Tokens::for_kind(ThemeKind::Dark),
            frames,
            playback: Playback { speed: 1.0, ..Default::default() },
            audio: None,
            hooks: HostHooks::default(),
            dialog: None,
            auto: Default::default(),
            textures: HashMap::new(),
            control_rx: None,
            deferred: Vec::new(),
            last_ui_time: 0.0,
            synthetic: Vec::new(),
            pending_screenshots: Vec::new(),
            queued_screenshots: Vec::new(),
            input_waiters: Vec::new(),
            next_token: 1,
            styled: false,
            fonts_ready: false,
            integrated_titlebar: false,
            last_timeline_width: 1000.0,
            fps: 60.0,
            last_time: 0.0,
            bindings: menus::bindings(),
            toast: None,
            tl: Default::default(),
        }
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    pub fn services(&self) -> Arc<dyn Services> {
        self.session.services.clone()
    }

    /// Rebuild the frame server if the session's media pool was replaced (e.g. project opened).
    fn sync_pool(&mut self) {
        if !Arc::ptr_eq(&self.frames.pool, &self.session.media) {
            self.frames = Arc::new(FrameServer::new(
                self.session.media.clone(),
                self.session.services.clone(),
                std::thread::available_parallelism().map(|n| n.get().clamp(2, 6)).unwrap_or(3),
            ));
            self.textures.clear();
        }
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, k: ThemeKind) {
        self.tokens = Tokens::for_kind(k);
        theme::apply_visuals(ctx, &self.tokens);
        self.ui.dark = k != ThemeKind::Light;
    }

    pub fn set_workspace(&mut self, name: &str) {
        self.ui.workspace = name.to_string();
        self.ui.dock = dock::workspace(name);
        if name == "Color" {
            self.ui.show_scopes = false;
        }
    }

    pub fn show_panel(&mut self, p: PanelKind) {
        if !self.ui.dock.contains(p) {
            let near = match p {
                PanelKind::LumetriColor | PanelKind::EssentialGraphics | PanelKind::EssentialSound | PanelKind::Properties => PanelKind::Program,
                PanelKind::Source
                | PanelKind::EffectControls
                | PanelKind::AudioClipMixer
                | PanelKind::Metadata
                | PanelKind::LumetriScopes
                | PanelKind::AudioTrackMixer
                | PanelKind::Text => PanelKind::Source,
                _ => PanelKind::Project,
            };
            self.ui.dock.open_near(p, near);
        }
        self.ui.dock.activate(p);
        self.ui.focused = p;
    }

    pub fn status(&mut self, s: impl Into<String>) {
        self.ui.status = s.into();
    }

    // ---------------------------------------------------------------- playback

    pub fn toggle_play(&mut self, speed: f64) {
        if self.playback.playing {
            self.stop();
        } else {
            self.play(speed);
        }
    }

    pub fn play(&mut self, speed: f64) {
        if self.session.active_sequence().is_none() {
            return;
        }
        // restart from the end → from the start
        let dur = self.session.active_sequence().map(|q| q.duration()).unwrap_or_default();
        if speed > 0.0 && self.session.playhead() >= dur - self.session.sequence_rate().frame_duration() {
            self.session.set_playhead(Tick::ZERO);
        }
        self.playback.playing = true;
        self.playback.speed = speed;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.anchor_time = -1.0; // set on next frame
        self.playback.dropped = 0;
        self.playback.shown = 0;
        self.start_audio();
    }

    pub fn stop(&mut self) {
        self.playback.playing = false;
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
    }

    fn start_audio(&mut self) {
        let speed = self.playback.speed;
        if (speed - 1.0).abs() > 1e-9 {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            return;
        }
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let project = self.session.project.clone();
        let provider = self.session.media.provider(project.clone(), self.session.services.clone());
        let start_tick = self.session.playhead();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        let mut cursor = start_tick.to_units_floor(sr as i64);
        let fill = Box::new(move |buf: &mut [f32], ch: usize| {
            let Some(seq) = project.sequence(seq_id) else { return };
            let n = buf.len() / ch.max(1);
            // Mix at the sequence rate; convert when the device rate differs (nearest sample).
            let seq_sr = seq.settings.sample_rate;
            let mix = if seq_sr == sr {
                filmcraft_render::audio::mix_sequence(&project, seq, cursor, n, &provider)
            } else {
                let s0 = (cursor as i128 * seq_sr as i128 / sr as i128) as i64;
                let m = n * seq_sr as usize / sr as usize + 2;
                let b = filmcraft_render::audio::mix_sequence(&project, seq, s0, m, &provider);
                let mut out = filmcraft_frame::AudioBuffer::silence(sr, 2, n);
                for c in 0..2 {
                    for i in 0..n {
                        let j = (i * seq_sr as usize / sr as usize).min(m - 1);
                        out.channels[c][i] = b.channels[c][j];
                    }
                }
                out
            };
            for i in 0..n {
                for c in 0..ch {
                    buf[i * ch + c] = mix.channels[c.min(1)][i].clamp(-1.0, 1.0);
                }
            }
            cursor += n as i64;
        });
        match a.start(fill) {
            Ok(_) => self.playback.audio_clock = true,
            Err(e) => {
                log::warn!("audio output unavailable: {e}");
                self.playback.audio_clock = false;
            }
        }
    }

    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playback.playing {
            return;
        }
        let now = ctx.input(|i| i.time);
        if self.playback.anchor_time < 0.0 {
            self.playback.anchor_time = now;
        }
        let rate = self.session.sequence_rate();
        let elapsed = if self.playback.audio_clock {
            match self.audio.as_ref().and_then(|a| a.played_frames().map(|f| (f, a.sample_rate()))) {
                Some((f, sr)) => f as f64 / sr as f64,
                None => now - self.playback.anchor_time,
            }
        } else {
            now - self.playback.anchor_time
        };
        let t = self.playback.anchor_tick + Tick::from_seconds_f64(elapsed * self.playback.speed);
        let seq = self.session.active_sequence();
        let dur = seq.map(|q| q.duration()).unwrap_or_default();
        let (lo, hi) = if self.playback.looping {
            (seq.and_then(|q| q.mark_in).unwrap_or(Tick::ZERO), seq.and_then(|q| q.mark_out).map(|o| o + rate.frame_duration()).unwrap_or(dur))
        } else {
            (Tick::ZERO, dur)
        };
        if t >= hi && self.playback.speed > 0.0 {
            if self.playback.looping {
                self.session.set_playhead(lo);
                self.play(self.playback.speed);
            } else {
                self.session.set_playhead(hi - rate.frame_duration());
                self.stop();
            }
        } else if t <= Tick::ZERO && self.playback.speed < 0.0 {
            self.session.set_playhead(Tick::ZERO);
            self.stop();
        } else {
            self.session.set_playhead(t);
        }
        ctx.request_repaint();
    }

    // ---------------------------------------------------------------- textures

    /// Upload a rendered frame into a named texture (only when the key changed).
    pub fn texture_for(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                tex.set(egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    pub fn texture_existing(&self, name: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        self.textures.get(name).map(|(_, t)| (t.id(), t.size_vec2()))
    }

    /// Get a thumbnail texture for an item at a media time (requested at low priority).
    pub fn thumbnail(&mut self, ctx: &egui::Context, item: filmcraft_project::ItemId, t: Tick, width: u32) -> Option<(egui::TextureId, egui::Vec2)> {
        let pi = self.session.project.item(item)?;
        let src_w = match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref()?.width,
            filmcraft_project::ItemKind::Sequence(s) => s.settings.width,
            _ => return None,
        };
        let rate = pi.frame_rate();
        let frame = rate.frame_at(t);
        // Thumbnails ignore project revision (media content doesn't change); sequences use it.
        let rev = if matches!(pi.kind, filmcraft_project::ItemKind::Sequence(_)) { self.session.revision } else { 0 };
        let key = FrameKey { target: Target::Item(item), frame, size: width, revision: rev };
        let name = format!("thumb-{}-{}-{}", item.0, frame, width);
        if let Some(img) = self.frames.get(&key) {
            let id = self.texture_for(ctx, &name, key, &img);
            return Some((id, egui::vec2(img.w as f32, img.h as f32)));
        }
        let scale = width as f32 / src_w.max(1) as f32;
        let project = self.session.project.clone();
        self.frames.request(key, rate.tick_of(frame), scale, &project, 50);
        self.texture_existing(&name)
    }

    // ---------------------------------------------------------------- files

    pub fn file_dialog(&mut self, id: &str) -> Result<Value, String> {
        match id {
            "file.import" => {
                let exts: Vec<&str> = filmcraft_media::VIDEO_EXTENSIONS
                    .iter()
                    .chain(filmcraft_media::AUDIO_EXTENSIONS)
                    .chain(filmcraft_media::STILL_EXTENSIONS)
                    .copied()
                    .collect();
                let paths = self.hooks.pick_files.as_mut().map(|f| f(&exts)).unwrap_or_default();
                if paths.is_empty() {
                    return Ok(Value::Null);
                }
                let r = self.session.execute("file.import", json!({"paths": paths})).map_err(|e| e.to_string());
                if let Ok(v) = &r
                    && let Some(errs) = v.get("errors").and_then(Value::as_array)
                    && !errs.is_empty()
                {
                    self.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                }
                r
            }
            "file.saveAs" | "file.save" => {
                let suggested = format!("{}.fcproj", self.session.project.name);
                let Some(path) = self.hooks.pick_save.as_mut().and_then(|f| f(&suggested)) else { return Ok(Value::Null) };
                self.session.execute("file.saveAs", json!({"path": path})).map_err(|e| e.to_string())
            }
            "file.open" => {
                let Some(path) = self.hooks.pick_open_project.as_mut().and_then(|f| f()) else { return Ok(Value::Null) };
                self.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())
            }
            _ => Err(format!("no dialog for {id}")),
        }
    }

    /// Import dropped files.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let mut paths = Vec::new();
        for f in dropped {
            let p = f.path();
            if p.exists() {
                paths.push(p.to_string_lossy().to_string());
            }
        }
        if !paths.is_empty() {
            let _ = self.session.execute("file.import", json!({"paths": paths}));
        }
    }

    // ---------------------------------------------------------------- input

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut fire = Vec::new();
        ctx.input_mut(|i| {
            for (m, k, id) in &self.bindings {
                if i.consume_key(*m, *k) {
                    fire.push(id.clone());
                }
            }
        });
        for id in fire {
            // Mark In/Out in the Source monitor when it has focus.
            let params = if self.ui.focused == PanelKind::Source && matches!(id.as_str(), "markers.markIn" | "markers.markOut") {
                json!({"target": "source"})
            } else {
                json!({})
            };
            if let Err(e) = menus::invoke(self, ctx, &id, params) {
                self.ui.status = e;
            }
        }
    }

    // ---------------------------------------------------------------- control channel

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        let now = ctx.input(|i| i.time);
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut self.deferred);
        while let Ok(req) = rx.try_recv() {
            // UI requests need rendered frames: raise the window if `ui` hasn't run recently
            // (occluded macOS windows stop running `ui`).
            if req.method.starts_with("ui.") && now - self.last_ui_time > 0.25 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            reqs.push((req, now + 3.0));
        }
        for (req, deadline) in reqs {
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.deferred.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => {
                    let token = self.next_token;
                    self.next_token += 1;
                    let settle = ctx.input(|i| i.time) + 0.25;
                    self.queued_screenshots.push((token, settle, 0));
                    self.pending_screenshots.push((token, path, crop, reply));
                }
            }
        }
        self.control_rx = Some(rx);
    }

    fn issue_screenshots(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let mut any = false;
        self.queued_screenshots.retain_mut(|(token, at, frames)| {
            *frames += 1;
            any = true;
            if now >= *at && *frames >= 3 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(*token)));
                false
            } else {
                true
            }
        });
        if any || !self.pending_screenshots.is_empty() {
            ctx.request_repaint();
        }
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|(t, ..)| *t == token) {
                let (_, path, crop, reply) = self.pending_screenshots.remove(i);
                let r = control::save_screenshot(ctx, &image, path.as_deref(), crop);
                let _ = reply.send(r);
            }
        }
    }

    // ---------------------------------------------------------------- frame

    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.auto.begin_frame();
        self.frames.set_context(&ctx);
        self.sync_pool();
        for ev in self.session.drain_events() {
            match ev {
                filmcraft_engine::Event::OpenSequence(_) => {
                    self.ui.timeline.fit_pending = true;
                    self.ui.dock.activate(PanelKind::Timeline);
                }
                filmcraft_engine::Event::OpenSource(_) => {
                    self.ui.dock.activate(PanelKind::Source);
                }
                filmcraft_engine::Event::Toast { message, .. } => self.toast = Some((message, ctx.input(|i| i.time))),
                filmcraft_engine::Event::ProjectChanged { .. } => {}
            }
        }
        self.handle_drops(&ctx);
        self.handle_shortcuts(&ctx);
        self.advance_playback(&ctx);
        let t = self.tokens;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, t.app_bg);
        let header_h = 40.0;
        let header = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), header_h));
        header::show(self, ui, header);
        let body = egui::Rect::from_min_max(egui::pos2(full.min.x + 4.0, header.max.y + 1.0), egui::pos2(full.max.x - 4.0, full.max.y - 4.0));
        match self.ui.mode {
            state::Mode::Edit => self.dock_area(ui, body),
            state::Mode::Import => panels::import_mode::show(self, ui, body),
            state::Mode::Export => panels::export_mode::show(self, ui, body),
        }
        panels::dialogs::show(self, &ctx);
        if !self.ui.status.is_empty() {
            let r = egui::Rect::from_min_size(egui::pos2(full.center().x - 260.0, full.max.y - 34.0), egui::vec2(520.0, 26.0));
            ui.painter().rect_filled(r, 13.0, Color32::from_black_alpha(210));
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, &self.ui.status, Tokens::ui(12.0), t.text);
            let resp = ui.interact(r, egui::Id::new("status-toast"), egui::Sense::click());
            if resp.clicked() {
                self.ui.status.clear();
            }
        }
    }

    fn dock_area(&mut self, ui: &mut egui::Ui, body: egui::Rect) {
        let t = self.tokens;
        let mut dock = std::mem::replace(&mut self.ui.dock, dock::DockNode::Tabs { panels: vec![], active: 0 });
        let mut groups = Vec::new();
        dock::layout(ui, &mut dock, body, &t, "", &mut groups, &mut self.auto);
        let mut actions = Vec::new();
        for g in &groups {
            actions.extend(dock::draw_group_chrome(ui, g, self.ui.focused, &t, &mut self.auto));
        }
        self.ui.dock = dock;
        for g in &groups {
            let Some(p) = g.panels.get(g.active).copied() else { continue };
            self.auto.add(&format!("panel.{}", p.id()), g.content, p.title());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(g.content).id_salt(("panel", p.id())));
            child.set_clip_rect(g.content);
            panels::show(self, &mut child, p, g.content);
        }
        for a in actions {
            match a {
                dock::DockAction::Activate(p) => {
                    self.ui.dock.activate(p);
                }
                dock::DockAction::Focus(p) => self.ui.focused = p,
                dock::DockAction::Close(p) => self.ui.dock.close(p),
                dock::DockAction::PanelMenu(p, pos) => {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("panel-menu"), (p, pos)));
                }
            }
        }
        panels::panel_menu_popup(self, ui);
    }
}

impl eframe::App for FilmcraftApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if !self.synthetic.is_empty() {
            let n = self
                .synthetic
                .iter()
                .position(|e| matches!(e, egui::Event::PointerButton { pressed: false, .. } | egui::Event::Key { pressed: false, .. }))
                .map_or(self.synthetic.len(), |i| i + 1);
            raw_input.events.extend(self.synthetic.drain(..n));
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.styled {
            theme::install(ctx, &self.tokens);
            self.styled = true;
            ctx.request_repaint();
        } else {
            self.fonts_ready = true;
        }
        let now = ctx.input(|i| i.time);
        let dt = (now - self.last_time) as f32;
        if dt > 0.0 {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(480.0) * 0.1;
        }
        self.last_time = now;
        let had_synthetic = !self.synthetic.is_empty();
        self.drain_control(ctx);
        if !self.synthetic.is_empty() && !had_synthetic {
            // Occluded macOS windows stop running `ui`; raise the window so input is processed.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        self.issue_screenshots(ctx);
        self.collect_screenshots(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            ui.ctx().request_repaint();
            return;
        }
        self.frame(ui);
        let ctx = ui.ctx().clone();
        self.last_ui_time = ctx.input(|i| i.time);
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            ctx.request_repaint();
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        if self.frames.queue_len() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}
