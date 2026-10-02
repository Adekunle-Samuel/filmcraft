//! Audio Track Mixer and Audio Clip Mixer.
//!
//! Track Mixer strips (audio tracks, submixes, Mix): effect and send slots (behind the ▸
//! disclosure), input mapping, output assignment, pan knob, automation mode, M / S / R, fader with
//! a dB scale, stereo meters fed by the playing mix, the fader value and the strip name. Every
//! control goes through `mixer.*` commands: fader and knob drags are `mixer.touch` while held and
//! `mixer.release` when let go, so they are heard live during playback and recorded as automation
//! in Latch / Touch / Write mode.
//!
//! Automation ids: `mixer.<A1|S1|Mix>.<fader|value|pan|panValue|mode|mute|solo|record|output|input|
//! fx.<slot>|send.<slot>|meter|name>`, popup entries below them (`mixer.A1.mode.Touch`…), the
//! effects disclosure `mixer.showEffects`, and the footer transport `mixer.transport.<command>`.
//! The Clip Mixer uses `clipMixer.A1.<fader|value|pan|mute|solo|keyframe>`.

use std::collections::HashMap;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::mixer::strip_label;
use filmcraft_project::mixer::{FADER_MAX_DB, FADER_MIN_DB, LANE_MUTE, LANE_PAN, LANE_VOLUME, MASTER_STRIP};
use filmcraft_project::{AutomationMode, InputMap, Sequence, Track, TrackId};
use filmcraft_time::{TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

// ------------------------------------------------------------------------------------- fader law

/// Fader taper: (dB, position 0 … 1). Our own law: most of the travel around unity gain.
const TAPER: [(f64, f32); 10] =
    [(-96.0, 0.0), (-60.0, 0.05), (-40.0, 0.14), (-30.0, 0.24), (-20.0, 0.38), (-12.0, 0.52), (-6.0, 0.64), (0.0, 0.76), (6.0, 0.88), (15.0, 1.0)];
/// Labels on the fader scale.
const SCALE: [f64; 10] = [15.0, 6.0, 0.0, -6.0, -12.0, -20.0, -30.0, -40.0, -60.0, -96.0];
const STRIP_W: f32 = 98.0;
const SLOT_H: f32 = 15.0;
const RED: Color32 = Color32::from_rgb(0xd8, 0x50, 0x3f);
const MUTE_ON: Color32 = Color32::from_rgb(0x2d, 0x9d, 0x78);
const SOLO_ON: Color32 = Color32::from_rgb(0xf0, 0xf0, 0x4f);

/// Fader position (0 bottom … 1 top) of a dB value.
pub fn db_to_pos(db: f64) -> f32 {
    if db <= TAPER[0].0 {
        return 0.0;
    }
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if db <= d1 {
            return p0 + (p1 - p0) * ((db - d0) / (d1 - d0)) as f32;
        }
    }
    1.0
}

/// dB value of a fader position.
pub fn pos_to_db(p: f32) -> f64 {
    let p = p.clamp(0.0, 1.0);
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if p <= p1 {
            let db = d0 + (d1 - d0) * ((p - p0) / (p1 - p0)) as f64;
            return if db <= FADER_MIN_DB + 0.5 { FADER_MIN_DB } else { db };
        }
    }
    FADER_MAX_DB
}

fn db_text(db: f64) -> String {
    if db <= FADER_MIN_DB { "-∞".into() } else { format!("{db:.1}") }
}

// ------------------------------------------------------------------------------------- meters

/// Per-strip meter state, `strip id → [level L, level R, peak L, peak R]` (dBFS), fed by the playing
/// mix ([`filmcraft_render::mixer::LiveMix`]) with fast attack and a 20 dB/s fall.
pub fn poll_meters(app: &mut FilmcraftApp, ui: &egui::Ui) -> HashMap<u64, [f32; 4]> {
    let id = egui::Id::new("mixer-meters");
    let frame = ui.ctx().cumulative_frame_nr();
    let (last, mut st): (u64, HashMap<u64, [f32; 4]>) = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    if last == frame && frame != 0 {
        return st;
    }
    // without an audio device, the loudness feed is what mixes the programme (and posts meters)
    super::meters::feed_loudness(app);
    let fresh = app.session.previews.live.take_meters();
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    for v in st.values_mut() {
        v[0] = (v[0] - 20.0 * dt).max(-90.0);
        v[1] = (v[1] - 20.0 * dt).max(-90.0);
        v[2] = (v[2] - 6.0 * dt).max(-90.0);
        v[3] = (v[3] - 6.0 * dt).max(-90.0);
    }
    if app.playback.playing {
        for (k, p) in fresh {
            let e = st.entry(k.0).or_insert([-90.0; 4]);
            for c in 0..2 {
                let l = 20.0 * p[c].max(1e-6).log10();
                e[c] = e[c].max(l);
                e[c + 2] = e[c + 2].max(l);
            }
        }
        ui.ctx().request_repaint();
    }
    ui.data_mut(|d| d.insert_temp(id, (frame, st.clone())));
    st
}

fn draw_meters(ui: &egui::Ui, r: Rect, m: [f32; 4], t: &Tokens) {
    let p = ui.painter();
    p.rect_filled(r, 0.0, Color32::BLACK);
    let w = (r.width() - 2.0) / 2.0;
    for c in 0..2 {
        let br = Rect::from_min_size(pos2(r.min.x + c as f32 * (w + 2.0), r.min.y + 6.0), vec2(w, r.height() - 6.0));
        crate::widgets::meter_bar(p, br, m[c], m[c + 2], t);
        // clip light
        let lr = Rect::from_min_size(pos2(br.min.x, r.min.y), vec2(w, 4.0));
        p.rect_filled(lr, 0.0, if m[c + 2] >= -0.1 { RED } else { Color32::from_gray(0x30) });
    }
}

// ------------------------------------------------------------------------------------- helpers

struct Ctx<'a> {
    t: Tokens,
    seq: &'a Sequence,
    now: filmcraft_time::Tick,
    recording: bool,
    acts: Vec<(String, Value)>,
}

impl Ctx<'_> {
    fn cmd(&mut self, id: &str, p: Value) {
        self.acts.push((id.to_string(), p));
    }
}

/// The value a control shows: a held override, else the automation (or static value) at the playhead.
fn shown(app: &FilmcraftApp, tr: &Track, lane: &str, now: filmcraft_time::Tick) -> f64 {
    app.session.previews.live.get(tr.id, lane).map(|o| o.value).unwrap_or_else(|| tr.lane_value(lane, now))
}

fn small_text(ui: &egui::Ui, pos: egui::Pos2, align: Align2, s: &str, size: f32, c: Color32) {
    ui.painter().text(pos, align, s, Tokens::ui(size), c);
}

fn slot_box(ui: &mut egui::Ui, r: Rect, text: &str, filled: bool, dim: bool, t: &Tokens, id: egui::Id) -> egui::Response {
    let resp = ui.interact(r, id, Sense::click());
    ui.painter().rect_filled(r, 2.0, if resp.hovered() { t.hover } else { t.field_bg });
    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    let col = if !filled || dim { t.text_faint } else { t.text };
    let galley = ui.painter().layout_no_wrap(text.to_string(), Tokens::ui(10.0), col);
    let clip = ui.painter().with_clip_rect(r.shrink(1.0).intersect(ui.clip_rect()));
    clip.galley(pos2(r.min.x + 4.0, r.center().y - galley.size().y / 2.0), galley, col);
    icons::paint(ui.painter(), Rect::from_center_size(pos2(r.max.x - 7.0, r.center().y), vec2(7.0, 7.0)), Icon::ChevronDown, t.text_dim);
    resp
}

/// The DSP-backed audio effects that can go in an insert slot.
fn insert_effects() -> Vec<&'static filmcraft_project::EffectDef> {
    filmcraft_project::effect_defs()
        .iter()
        .filter(|d| d.kind == filmcraft_project::EffectKind::Audio && !d.intrinsic && filmcraft_render::audio_fx::supported(d.id))
        .collect()
}

/// Submixes a strip may route to (all of them for tracks; only later ones for submixes).
fn targets(seq: &Sequence, id: TrackId) -> Vec<&Track> {
    let me = seq.submix_tracks.iter().position(|t| t.id == id);
    seq.submix_tracks.iter().enumerate().filter(|(i, _)| me.is_none_or(|m| *i > m)).map(|(_, t)| t).collect()
}

// ------------------------------------------------------------------------------------- track mixer

pub fn track_mixer(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, "(no sequence)");
        return;
    };
    let meters = poll_meters(app, ui);
    let mut cx = Ctx { t, seq: &seq, now: app.session.playhead(), recording: app.session.mixrec.active(), acts: Vec::new() };
    let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 34.0), rect.max);
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + 2.0), pos2(rect.max.x, footer.min.y));
    // effects / sends disclosure at the left edge
    let arrow = Rect::from_min_size(pos2(body.min.x + 2.0, body.min.y + 4.0), vec2(14.0, 18.0));
    let aresp = ui.interact(arrow, egui::Id::new("mixer-fx-toggle"), Sense::click()).on_hover_text("Show/Hide Effects and Sends");
    icons::paint(ui.painter(), arrow.shrink(2.0), if app.ui.mixer_fx_open { Icon::ChevronDown } else { Icon::ChevronRight }, t.icon);
    app.auto.add("mixer.showEffects", arrow, "Show/Hide Effects and Sends");
    if aresp.clicked() {
        app.ui.mixer_fx_open = !app.ui.mixer_fx_open;
    }
    let strips = seq.strip_ids();
    let x0 = body.min.x + 18.0;
    let total = strips.len() as f32 * (STRIP_W + 2.0);
    let avail = body.max.x - x0 - 2.0;
    let max_off = (total - avail).max(0.0);
    let off_id = egui::Id::new("mixer-hscroll");
    let mut off: f32 = ui.data(|d| d.get_temp(off_id)).unwrap_or(0.0);
    if ui.rect_contains_pointer(body) {
        let sd = ui.input(|i| i.smooth_scroll_delta);
        off -= sd.x + if ui.input(|i| i.modifiers.shift) { sd.y } else { 0.0 };
    }
    off = off.clamp(0.0, max_off);
    let sb_h = if max_off > 0.0 { 12.0 } else { 0.0 };
    // strips never get shorter than their controls plus a usable fader: scroll vertically instead
    let need = 4.0 + if app.ui.mixer_fx_open { 10.0 * SLOT_H + 10.0 } else { 0.0 } + 112.0 + 26.0 + 24.0 + 6.0 + 170.0 + 42.0;
    let clip = Rect::from_min_max(pos2(x0, body.min.y), pos2(body.max.x - 8.0, body.max.y - sb_h));
    let voff_id = egui::Id::new("mixer-vscroll");
    let max_voff = (need - (clip.height() - 4.0)).max(0.0);
    let mut voff: f32 = ui.data(|d| d.get_temp(voff_id)).unwrap_or(0.0);
    if ui.rect_contains_pointer(body) && !ui.input(|i| i.modifiers.shift) {
        voff -= ui.input(|i| i.smooth_scroll_delta.y);
    }
    voff = voff.clamp(0.0, max_voff);
    if max_voff > 0.0 {
        let bar = Rect::from_min_max(pos2(body.max.x - 7.0, clip.min.y + 2.0), pos2(body.max.x - 2.0, clip.max.y - 2.0));
        ui.painter().rect_filled(bar, 3.0, t.field_bg);
        let kh = (bar.height() * clip.height() / (need + 4.0)).max(20.0);
        let ky = bar.min.y + (bar.height() - kh) * voff / max_voff;
        let knob = Rect::from_min_size(pos2(bar.min.x, ky), vec2(bar.width(), kh));
        let kr = ui.interact(knob, egui::Id::new("mixer-vscroll-knob"), Sense::drag());
        ui.painter().rect_filled(knob, 3.0, if kr.dragged() { t.accent } else { Color32::from_gray(0x5a) });
        if kr.dragged() {
            voff = (voff + kr.drag_delta().y * (need + 4.0) / bar.height()).clamp(0.0, max_voff);
        }
        app.auto.add("mixer.vscroll", knob, "Scroll strips vertically");
    }
    ui.data_mut(|d| d.insert_temp(voff_id, voff));
    if max_off > 0.0 {
        let bar = Rect::from_min_max(pos2(x0, body.max.y - 10.0), pos2(body.max.x - 4.0, body.max.y - 2.0));
        ui.painter().rect_filled(bar, 4.0, t.field_bg);
        let kw = (bar.width() * avail / total).max(20.0);
        let kx = bar.min.x + (bar.width() - kw) * off / max_off;
        let knob = Rect::from_min_size(pos2(kx, bar.min.y), vec2(kw, bar.height()));
        let kr = ui.interact(knob, egui::Id::new("mixer-hscroll-knob"), Sense::drag());
        ui.painter().rect_filled(knob, 4.0, if kr.dragged() { t.accent } else { Color32::from_gray(0x5a) });
        if kr.dragged() {
            off = (off + kr.drag_delta().x * total / bar.width()).clamp(0.0, max_off);
        }
        app.auto.add("mixer.scroll", knob, "Scroll strips");
    }
    ui.data_mut(|d| d.insert_temp(off_id, off));
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(clip));
    cui.set_clip_rect(clip.intersect(ui.clip_rect()));
    for (i, id) in strips.iter().enumerate() {
        let sx = x0 + i as f32 * (STRIP_W + 2.0) - off;
        if sx + STRIP_W < clip.min.x || sx > clip.max.x {
            continue;
        }
        let sr = Rect::from_min_size(pos2(sx, clip.min.y + 2.0 - voff), vec2(STRIP_W, (clip.height() - 4.0).max(need)));
        let m = meters.get(&id.0).copied().unwrap_or([-90.0; 4]);
        strip(app, &mut cui, &mut cx, *id, sr, m);
    }
    transport(app, ui, footer, &mut cx);
    let acts = std::mem::take(&mut cx.acts);
    run_actions(app, &ui.ctx().clone(), acts);
}

fn run_actions(app: &mut FilmcraftApp, ctx: &egui::Context, acts: Vec<(String, Value)>) {
    for (id, p) in acts {
        let r = if filmcraft_engine::find_command(&id).is_some() {
            app.session.execute(&id, p).map(|_| ()).map_err(|e| e.to_string())
        } else {
            crate::menus::invoke(app, ctx, &id, p).map(|_| ())
        };
        if let Err(e) = r {
            app.ui.status = e;
        }
    }
}

fn strip(app: &mut FilmcraftApp, ui: &mut egui::Ui, cx: &mut Ctx, id: TrackId, sr: Rect, meter: [f32; 4]) {
    let t = cx.t;
    let seq = cx.seq;
    let Some(tr) = seq.strip(id).map(|c| c.into_owned()) else { return };
    let label = strip_label(seq, id);
    let ap = format!("mixer.{label}");
    let is_master = id == MASTER_STRIP;
    let sid = json!(id.0);
    ui.painter().rect_filled(sr, 3.0, Color32::from_gray(0x24));
    ui.painter().line_segment([pos2(sr.max.x + 1.0, sr.min.y), pos2(sr.max.x + 1.0, sr.max.y)], Stroke::new(1.0, t.separator));
    let x = sr.min.x;
    let w = sr.width();
    let mut y = sr.min.y + 4.0;

    // ---- effect and send slots
    if app.ui.mixer_fx_open {
        let mut open_editor = None;
        for k in 0..filmcraft_project::mixer::MAX_INSERTS {
            let r = Rect::from_min_size(pos2(x + 4.0, y), vec2(w - 8.0, SLOT_H - 2.0));
            let fx = tr.effects.get(k);
            let text = match fx {
                Some(e) => format!("{}{}", if e.post_fader { "▸ " } else { "" }, e.def().map(|d| d.name).unwrap_or(&e.effect)),
                None => String::new(),
            };
            let resp = slot_box(ui, r, &text, fx.is_some(), fx.is_some_and(|e| !e.enabled), &t, egui::Id::new((&ap, "fx", k)));
            app.auto.add(&format!("{ap}.fx.{k}"), r, &if text.is_empty() { format!("Effect slot {}", k + 1) } else { text.clone() });
            let can_add = fx.is_none() && k == tr.effects.len();
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(180.0);
                match fx {
                    Some(e) => {
                        if crate::panels::audio_fx_editor::has_editor(&e.effect) {
                            let r0 = ui.button("Edit…");
                            app.auto.add(&format!("{ap}.fx.{k}.edit"), r0.rect, "Edit…");
                            if r0.clicked() {
                                open_editor = Some(crate::panels::audio_fx_editor::FxTarget::Insert { strip: id.0, slot: k });
                            }
                            ui.separator();
                        }
                        let on = e.enabled;
                        let r1 = ui.selectable_label(!on, "Bypass");
                        app.auto.add(&format!("{ap}.fx.{k}.bypass"), r1.rect, "Bypass");
                        if r1.clicked() {
                            cx.cmd("mixer.setInsert", json!({"strip": sid, "slot": k, "enabled": !on}));
                        }
                        let r2 = ui.selectable_label(e.post_fader, "Post-Fader");
                        app.auto.add(&format!("{ap}.fx.{k}.postFader"), r2.rect, "Post-Fader");
                        if r2.clicked() {
                            cx.cmd("mixer.setInsert", json!({"strip": sid, "slot": k, "postFader": !e.post_fader}));
                        }
                        ui.separator();
                        let r3 = ui.button("Remove Effect");
                        app.auto.add(&format!("{ap}.fx.{k}.remove"), r3.rect, "Remove Effect");
                        if r3.clicked() {
                            cx.cmd("mixer.removeInsert", json!({"strip": sid, "slot": k}));
                        }
                    }
                    None if can_add => {
                        // Premiere's effect-slot menu: one submenu per Audio Effects folder.
                        let fx = insert_effects();
                        let mut folders: Vec<&str> = Vec::new();
                        for d in &fx {
                            let f = d.category.get(1).copied().unwrap_or("");
                            if !folders.contains(&f) {
                                folders.push(f);
                            }
                        }
                        for folder in folders {
                            let items: Vec<_> = fx.iter().filter(|d| d.category.get(1).copied().unwrap_or("") == folder).collect();
                            let mut add = |ui: &mut egui::Ui| {
                                for d in &items {
                                    let r = ui.button(d.name);
                                    app.auto.add(&format!("{ap}.fx.{k}.{}", d.id), r.rect, d.name);
                                    if r.clicked() {
                                        cx.cmd("mixer.addInsert", json!({"strip": sid, "effect": d.id}));
                                        ui.close();
                                    }
                                }
                            };
                            if folder.is_empty() {
                                add(ui);
                            } else {
                                let mr = ui.menu_button(folder, |ui| add(ui));
                                app.auto.add(&format!("{ap}.fx.{k}.folder.{folder}"), mr.response.rect, folder);
                            }
                        }
                    }
                    None => {
                        ui.label("Fill the slots above first");
                    }
                }
            });
            if resp.double_clicked()
                && let Some(e) = fx
                && crate::panels::audio_fx_editor::has_editor(&e.effect)
            {
                open_editor = Some(crate::panels::audio_fx_editor::FxTarget::Insert { strip: id.0, slot: k });
            }
            y += SLOT_H;
        }
        if let Some(target) = open_editor {
            crate::panels::audio_fx_editor::open(app, target);
        }
        y += 4.0;
        for k in 0..filmcraft_project::mixer::MAX_SENDS {
            let r = Rect::from_min_size(pos2(x + 4.0, y), vec2(w - 8.0, SLOT_H - 2.0));
            let snd = tr.mixer.sends.get(k);
            let text = match snd {
                Some(s) => format!("{}{} {}", if s.pre_fader { "pre " } else { "" }, strip_label(seq, s.target), db_text(s.level_db)),
                None => String::new(),
            };
            if is_master {
                y += SLOT_H;
                continue;
            }
            let resp = slot_box(ui, r, &text, snd.is_some(), snd.is_some_and(|s| s.muted), &t, egui::Id::new((&ap, "send", k)));
            app.auto.add(&format!("{ap}.send.{k}"), r, &if text.is_empty() { format!("Send slot {}", k + 1) } else { text.clone() });
            let can_add = snd.is_none() && k == tr.mixer.sends.len();
            let tg = targets(seq, id);
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(180.0);
                match snd {
                    Some(s) => {
                        let mut lvl = s.level_db;
                        let sl = ui.add(egui::Slider::new(&mut lvl, FADER_MIN_DB..=FADER_MAX_DB).text("dB"));
                        app.auto.add(&format!("{ap}.send.{k}.level"), sl.rect, "Send level");
                        if sl.changed() {
                            cx.cmd("mixer.setSend", json!({"strip": sid, "send": k, "levelDb": lvl}));
                        }
                        let r1 = ui.selectable_label(s.pre_fader, "Pre-Fader");
                        app.auto.add(&format!("{ap}.send.{k}.preFader"), r1.rect, "Pre-Fader");
                        if r1.clicked() {
                            cx.cmd("mixer.setSend", json!({"strip": sid, "send": k, "preFader": !s.pre_fader}));
                        }
                        let r2 = ui.selectable_label(s.muted, "Mute Send");
                        app.auto.add(&format!("{ap}.send.{k}.mute"), r2.rect, "Mute Send");
                        if r2.clicked() {
                            cx.cmd("mixer.setSend", json!({"strip": sid, "send": k, "muted": !s.muted}));
                        }
                        ui.separator();
                        let r3 = ui.button("Remove Send");
                        app.auto.add(&format!("{ap}.send.{k}.remove"), r3.rect, "Remove Send");
                        if r3.clicked() {
                            cx.cmd("mixer.removeSend", json!({"strip": sid, "send": k}));
                        }
                    }
                    None if can_add => {
                        for s in &tg {
                            let lbl = format!("{} ({})", strip_label(seq, s.id), s.name);
                            let r = ui.button(&lbl);
                            app.auto.add(&format!("{ap}.send.{k}.{}", strip_label(seq, s.id)), r.rect, &lbl);
                            if r.clicked() {
                                cx.cmd("mixer.addSend", json!({"strip": sid, "target": s.id.0}));
                            }
                        }
                        if seq.submix_tracks.iter().all(|s| s.id != id) {
                            let r = ui.button("New Submix");
                            app.auto.add(&format!("{ap}.send.{k}.newSubmix"), r.rect, "New Submix");
                            if r.clicked() {
                                let n = seq.submix_tracks.len();
                                cx.cmd("mixer.addSubmix", json!({}));
                                cx.cmd("mixer.addSend", json!({"strip": sid, "target": format!("S{}", n + 1)}));
                            }
                        }
                    }
                    None => {
                        ui.label("Fill the slots above first");
                    }
                }
            });
            y += SLOT_H;
        }
        y += 6.0;
    }

    // ---- input / output
    if !is_master {
        let ir = Rect::from_min_size(pos2(x + 4.0, y), vec2(w - 8.0, 20.0));
        let iresp = crate::widgets::dropdown_text(ui, ir, tr.mixer.input_map.label(), &t, egui::Id::new((&ap, "input")));
        app.auto.add(&format!("{ap}.input"), ir, "Input channel mapping");
        egui::Popup::menu(&iresp).show(|ui| {
            for m in InputMap::ALL {
                let r = ui.selectable_label(m == tr.mixer.input_map, m.label());
                app.auto.add(&format!("{ap}.input.{}", m.label()), r.rect, m.label());
                if r.clicked() {
                    cx.cmd("mixer.setStrip", json!({"strip": sid, "inputMap": m.label()}));
                }
            }
        });
        y += 24.0;
        let or = Rect::from_min_size(pos2(x + 4.0, y), vec2(w - 8.0, 20.0));
        let out_label = tr.mixer.output.map(|o| seq.mix_track(o).map(|s| s.name.clone()).unwrap_or_else(|| "Mix".into())).unwrap_or_else(|| "Mix".into());
        let oresp = crate::widgets::dropdown_text(ui, or, &out_label, &t, egui::Id::new((&ap, "output")));
        app.auto.add(&format!("{ap}.output"), or, "Track output assignment");
        let tg = targets(seq, id);
        egui::Popup::menu(&oresp).show(|ui| {
            let r = ui.selectable_label(tr.mixer.output.is_none(), "Mix");
            app.auto.add(&format!("{ap}.output.Mix"), r.rect, "Mix");
            if r.clicked() {
                cx.cmd("mixer.setStrip", json!({"strip": sid, "output": "Mix"}));
            }
            for s in &tg {
                let r = ui.selectable_label(tr.mixer.output == Some(s.id), &s.name);
                app.auto.add(&format!("{ap}.output.{}", strip_label(seq, s.id)), r.rect, &s.name);
                if r.clicked() {
                    cx.cmd("mixer.setStrip", json!({"strip": sid, "output": s.id.0}));
                }
            }
        });
        y += 26.0;
        // ---- pan knob
        let pan = shown(app, &tr, LANE_PAN, cx.now);
        let kc = pos2(sr.center().x, y + 20.0);
        let kr = Rect::from_center_size(kc, vec2(38.0, 38.0));
        let kresp = ui.interact(kr, egui::Id::new((&ap, "pan")), Sense::click_and_drag()).on_hover_text("Pan / balance (drag; double-click: centre)");
        let ring = if kresp.hovered() || kresp.dragged() { t.tab_text_active } else { Color32::from_gray(0xb0) };
        ui.painter().circle_stroke(kc, 16.0, Stroke::new(2.0, ring));
        let ang = (pan / 100.0) as f32 * 135f32.to_radians();
        ui.painter().line_segment([kc, kc + vec2(ang.sin(), -ang.cos()) * 14.0], Stroke::new(2.0, ring));
        small_text(ui, pos2(kc.x - 22.0, kc.y + 17.0), Align2::CENTER_CENTER, "L", 11.0, t.hot_text);
        small_text(ui, pos2(kc.x + 22.0, kc.y + 17.0), Align2::CENTER_CENTER, "R", 11.0, t.hot_text);
        app.auto.add(&format!("{ap}.pan"), kr, "Pan");
        if kresp.dragged() {
            let d = kresp.drag_delta();
            let np = (pan + (d.x - d.y) as f64).clamp(-100.0, 100.0);
            if np != pan {
                cx.cmd("mixer.touch", json!({"strip": sid, "lane": LANE_PAN, "value": np, "time": cx.now.0}));
            }
        }
        if kresp.drag_stopped() {
            cx.cmd("mixer.release", json!({"strip": sid, "lane": LANE_PAN, "time": cx.now.0}));
        }
        if kresp.double_clicked() {
            cx.cmd("mixer.setValue", json!({"strip": sid, "lane": LANE_PAN, "value": 0.0}));
        }
        let vr = Rect::from_center_size(pos2(kc.x, y + 48.0), vec2(56.0, 16.0));
        let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(vr).layout(egui::Layout::top_down(egui::Align::Center)));
        let (vresp, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new((&ap, "panValue")), pan, 1.0, (-100.0, 100.0), 1, "", &t);
        app.auto.add(&format!("{ap}.panValue"), vresp.rect, &format!("{pan:.1}"));
        if let Some(v) = nv {
            cx.cmd("mixer.setValue", json!({"strip": sid, "lane": LANE_PAN, "value": v}));
        }
        y += 62.0;
    } else {
        y += 112.0;
    }

    // ---- automation mode
    let mr = Rect::from_min_size(pos2(x + 4.0, y), vec2(w - 8.0, 20.0));
    let mode = tr.mixer.mode;
    let mresp = crate::widgets::dropdown_text(ui, mr, mode.label(), &t, egui::Id::new((&ap, "mode")));
    if cx.recording && mode.writes() {
        ui.painter().rect_stroke(mr, 4.0, Stroke::new(1.0, RED), StrokeKind::Inside);
    }
    app.auto.add(&format!("{ap}.mode"), mr, &format!("Automation mode: {}", mode.label()));
    egui::Popup::menu(&mresp).show(|ui| {
        for m in AutomationMode::ALL {
            let r = ui.selectable_label(m == mode, m.label());
            app.auto.add(&format!("{ap}.mode.{}", m.label()), r.rect, m.label());
            if r.clicked() {
                cx.cmd("mixer.setStrip", json!({"strip": sid, "mode": m.label()}));
            }
        }
    });
    y += 26.0;

    // ---- M / S / R
    if !is_master {
        let muted = shown(app, &tr, LANE_MUTE, cx.now) >= 0.5;
        let bw = 18.0;
        let bx = sr.center().x - bw * 1.5 - 6.0;
        let b = |i: f32| Rect::from_min_size(pos2(bx + i * (bw + 6.0), y), vec2(bw, 16.0));
        let m_r = crate::widgets::letter_toggle(ui, b(0.0), "M", muted, MUTE_ON, &t, egui::Id::new((&ap, "M")));
        app.auto.add(&format!("{ap}.mute"), b(0.0), "Mute Track");
        if m_r.clicked() {
            if cx.recording && mode.writes() {
                let v = if muted { 0.0 } else { 1.0 };
                cx.cmd("mixer.touch", json!({"strip": sid, "lane": LANE_MUTE, "value": v, "time": cx.now.0}));
                cx.cmd("mixer.release", json!({"strip": sid, "lane": LANE_MUTE, "time": cx.now.0}));
            } else {
                cx.cmd("mixer.setStrip", json!({"strip": sid, "muted": !tr.muted}));
            }
        }
        let s_r = crate::widgets::letter_toggle(ui, b(1.0), "S", tr.solo, SOLO_ON, &t, egui::Id::new((&ap, "S")));
        app.auto.add(&format!("{ap}.solo"), b(1.0), "Solo Track");
        if s_r.clicked() {
            cx.cmd("mixer.setStrip", json!({"strip": sid, "solo": !tr.solo}));
        }
        let is_sub = seq.submix_tracks.iter().any(|s| s.id == id);
        if is_sub {
            let r_r = crate::widgets::letter_toggle(ui, b(2.0), "◆", tr.mixer.solo_safe, Color32::from_gray(0xc8), &t, egui::Id::new((&ap, "safe")))
                .on_hover_text("Solo safe");
            app.auto.add(&format!("{ap}.soloSafe"), b(2.0), "Solo Safe");
            if r_r.clicked() {
                cx.cmd("mixer.setStrip", json!({"strip": sid, "soloSafe": !tr.mixer.solo_safe}));
            }
        } else {
            let r_r = crate::widgets::letter_toggle(ui, b(2.0), "R", tr.mixer.record_arm, RED, &t, egui::Id::new((&ap, "R")))
                .on_hover_text("Enable track for recording");
            app.auto.add(&format!("{ap}.record"), b(2.0), "Enable Track for Recording");
            if r_r.clicked() {
                cx.cmd("mixer.setStrip", json!({"strip": sid, "recordArm": !tr.mixer.record_arm}));
            }
        }
    }
    y += 24.0;

    // ---- fader + meters
    let name_h = 20.0;
    let value_h = 18.0;
    let area = Rect::from_min_max(pos2(x, y + 6.0), pos2(sr.max.x, sr.max.y - name_h - value_h - 4.0));
    if area.height() > 40.0 {
        let track = Rect::from_min_max(pos2(x + 33.0, area.min.y + 8.0), pos2(x + 37.0, area.max.y - 8.0));
        let ypos = |db: f64| track.max.y - db_to_pos(db) * track.height();
        let tall = track.height() > 150.0;
        // scale
        small_text(ui, pos2(x + 26.0, area.min.y - 2.0), Align2::RIGHT_CENTER, "dB", 9.0, t.text_dim);
        for d in SCALE.iter().copied().filter(|d| tall || [15.0, 0.0, -12.0, -30.0, -96.0].contains(d)) {
            let yy = ypos(d);
            let s = if d <= FADER_MIN_DB { "-∞".to_string() } else { format!("{}", d as i32) };
            small_text(ui, pos2(x + 26.0, yy), Align2::RIGHT_CENTER, &s, 9.0, t.text_dim);
            ui.painter().line_segment([pos2(x + 28.0, yy), pos2(x + 31.0, yy)], Stroke::new(1.0, t.text_faint));
        }
        ui.painter().rect_filled(track, 2.0, Color32::from_gray(0x10));
        ui.painter().rect_stroke(track, 2.0, Stroke::new(1.0, Color32::from_gray(0x50)), StrokeKind::Inside);
        let vol = shown(app, &tr, LANE_VOLUME, cx.now);
        let cy = ypos(vol);
        let cap = Rect::from_center_size(pos2(track.center().x, cy), vec2(18.0, 26.0));
        let fresp = ui.interact(cap.expand(3.0), egui::Id::new((&ap, "fader")), Sense::click_and_drag()).on_hover_text("Volume (drag; double-click: 0 dB)");
        let held = app.session.previews.live.get(id, LANE_VOLUME).is_some();
        let col = if fresp.dragged() || held { t.accent } else { Color32::from_gray(0xe0) };
        ui.painter().rect_filled(cap, 3.0, Color32::from_gray(0x2a));
        ui.painter().rect_stroke(cap, 3.0, Stroke::new(2.0, col), StrokeKind::Inside);
        ui.painter().line_segment([pos2(cap.min.x + 5.0, cy), pos2(cap.max.x - 5.0, cy)], Stroke::new(2.0, col));
        app.auto.add(&format!("{ap}.fader"), cap, &format!("Volume {}", db_text(vol)));
        if fresp.dragged() {
            let dy = fresp.drag_delta().y;
            if dy != 0.0 {
                let fine = if ui.input(|i| i.modifiers.command) { 0.1 } else { 1.0 };
                let nv = pos_to_db(db_to_pos(vol) - dy * fine / track.height());
                cx.cmd("mixer.touch", json!({"strip": sid, "lane": LANE_VOLUME, "value": nv, "time": cx.now.0}));
            } else if fresp.drag_started() {
                cx.cmd("mixer.touch", json!({"strip": sid, "lane": LANE_VOLUME, "value": vol, "time": cx.now.0}));
            }
        }
        if fresp.drag_stopped() {
            cx.cmd("mixer.release", json!({"strip": sid, "lane": LANE_VOLUME, "time": cx.now.0}));
        }
        if fresp.double_clicked() {
            cx.cmd("mixer.setValue", json!({"strip": sid, "lane": LANE_VOLUME, "value": 0.0}));
        }
        // meters with their own scale (0 … −54 dB)
        let mrect = Rect::from_min_max(pos2(x + 46.0, area.min.y), pos2(x + 70.0, area.max.y));
        draw_meters(ui, mrect, meter, &t);
        app.auto.add(&format!("{ap}.meter"), mrect, &format!("Meter {:.1} / {:.1} dB", meter[0], meter[1]));
        let bar_top = mrect.min.y + 6.0;
        let bar_h = mrect.height() - 6.0;
        let mut d = 0;
        let step = if tall { 6 } else { 18 };
        while d >= -54 {
            let yy = bar_top + bar_h * (-d as f32 / 60.0);
            small_text(ui, pos2(x + 91.0, yy), Align2::RIGHT_CENTER, &format!("{d}"), 8.5, t.text_dim);
            ui.painter().line_segment([pos2(x + 71.0, yy), pos2(x + 74.0, yy)], Stroke::new(1.0, t.text_faint));
            d -= step;
        }
        small_text(ui, pos2(x + 91.0, mrect.max.y + 6.0), Align2::RIGHT_CENTER, "dB", 8.5, t.text_dim);
        // fader value (hot text)
        let vr = Rect::from_min_size(pos2(x + 4.0, sr.max.y - name_h - value_h), vec2(56.0, value_h));
        let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(vr));
        let (vresp, nv) = crate::widgets::hot_number(&mut vui, egui::Id::new((&ap, "value")), vol.max(-96.0), 0.1, (FADER_MIN_DB, FADER_MAX_DB), 1, "", &t);
        app.auto.add(&format!("{ap}.value"), vresp.rect, &db_text(vol));
        if let Some(v) = nv {
            cx.cmd("mixer.setValue", json!({"strip": sid, "lane": LANE_VOLUME, "value": v}));
        }
    }
    // ---- name
    let nr = Rect::from_min_max(pos2(x, sr.max.y - name_h), sr.max);
    ui.painter().line_segment([pos2(nr.min.x + 4.0, nr.min.y), pos2(nr.max.x - 4.0, nr.min.y)], Stroke::new(1.0, t.separator));
    if is_master {
        small_text(ui, nr.center(), Align2::CENTER_CENTER, "Mix", 12.0, t.text);
    } else {
        small_text(ui, pos2(nr.min.x + 6.0, nr.center().y), Align2::LEFT_CENTER, &label, 12.0, t.text);
        let name = ui.painter().with_clip_rect(Rect::from_min_max(pos2(nr.min.x + 26.0, nr.min.y), nr.max).intersect(ui.clip_rect()));
        name.text(pos2(nr.min.x + 30.0, nr.center().y), Align2::LEFT_CENTER, &tr.name, Tokens::ui(12.0), t.text);
    }
    app.auto.add(&format!("{ap}.name"), nr, &tr.name);
}

fn transport(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, cx: &mut Ctx) {
    let t = cx.t;
    let seq = cx.seq;
    ui.painter().line_segment([pos2(row.min.x, row.min.y + 0.5), pos2(row.max.x, row.min.y + 0.5)], Stroke::new(1.0, t.separator));
    let tc = format_time(cx.now, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    ui.painter().text(pos2(row.min.x + 12.0, row.center().y), Align2::LEFT_CENTER, tc, Tokens::mono(13.0), t.timecode);
    let dur = format_time(seq.duration(), seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    ui.painter().text(pos2(row.max.x - 12.0, row.center().y), Align2::RIGHT_CENTER, dur, Tokens::mono(12.0), t.text_dim);
    let playing = app.playback.playing;
    let buttons: [(Icon, &str, &str); 6] = [
        (Icon::GoToIn, "markers.goToIn", "Go to In Point"),
        (Icon::GoToOut, "markers.goToOut", "Go to Out Point"),
        (if playing { Icon::Pause } else { Icon::Play }, "playback.toggle", "Play-Stop Toggle (Space)"),
        (Icon::Play, "playback.inToOut", "Play In to Out"),
        (Icon::Loop, "playback.loop", "Loop"),
        (Icon::Mic, "record", "Record (voice-over recording is not available yet)"),
    ];
    let bw = 30.0;
    let mut x = row.center().x - bw * buttons.len() as f32 / 2.0;
    for (icon, cmd, tip) in buttons {
        let r = Rect::from_min_size(pos2(x, row.min.y + 4.0), vec2(bw - 2.0, 26.0));
        let resp = ui.interact(r, egui::Id::new(("mixer-transport", cmd)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("mixer.transport.{cmd}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        let on = cmd == "playback.loop" && app.playback.looping;
        let col = if on {
            t.accent
        } else if resp.hovered() {
            t.tab_text_active
        } else {
            t.icon
        };
        if cmd == "record" {
            ui.painter().circle_filled(r.center(), 6.0, if cx.recording { RED } else { RED.gamma_multiply(0.55) });
        } else {
            icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(14.0, 14.0)), icon, col);
            if cmd == "playback.inToOut" {
                // braces around the play triangle
                let c = r.center();
                for s in [-1.0f32, 1.0] {
                    ui.painter().line_segment([pos2(c.x + s * 10.0, c.y - 6.0), pos2(c.x + s * 10.0, c.y + 6.0)], Stroke::new(1.5, col));
                }
            }
        }
        if resp.clicked() && cmd != "record" {
            cx.cmd(cmd, json!({}));
        }
        x += bw;
    }
}

// ------------------------------------------------------------------------------------- clip mixer

/// Audio Clip Mixer: one strip per audio track, controlling the clip under the playhead (clip
/// Volume and Panner). With the keyframe button on, changes add keyframes at the playhead.
pub fn clip_mixer(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, "(no sequence)");
        return;
    };
    let meters = poll_meters(app, ui);
    let now = app.session.playhead();
    let mut acts: Vec<(String, Value)> = Vec::new();
    let kf_id = egui::Id::new("clip-mixer-keyframes");
    let mut write_kf: HashMap<u64, bool> = ui.data(|d| d.get_temp(kf_id)).unwrap_or_default();
    for (i, tr) in seq.audio_tracks.iter().enumerate() {
        let x = rect.min.x + 6.0 + i as f32 * (STRIP_W + 2.0);
        if x + STRIP_W > rect.max.x {
            break;
        }
        let sr = Rect::from_min_max(pos2(x, rect.min.y + 4.0), pos2(x + STRIP_W, rect.max.y - 4.0));
        let label = format!("A{}", i + 1);
        let ap = format!("clipMixer.{label}");
        ui.painter().rect_filled(sr, 3.0, Color32::from_gray(0x24));
        let clip = tr.item_at(now).filter(|c| c.enabled);
        let active = clip.is_some();
        let dimc = |c: Color32| if active { c } else { c.gamma_multiply(0.4) };
        let mt = clip.map(|c| c.source_time_at(now));
        let vol = clip.and_then(|c| c.effect("volume")).map(|e| e.f64_at("level", mt.unwrap_or_default())).unwrap_or(0.0);
        let pan = clip.and_then(|c| c.effect("panner")).map(|e| e.f64_at("balance", mt.unwrap_or_default())).unwrap_or(0.0);
        let kf = *write_kf.get(&tr.id.0).unwrap_or(&false);
        let set = |acts: &mut Vec<(String, Value)>, effect: &str, param: &str, v: f64, begin: bool| {
            if let Some(c) = clip {
                acts.push(("clipMixer.set".into(), json!({"clip": c.id.0, "effect": effect, "param": param, "value": v, "keyframe": kf, "begin": begin})));
            }
        };
        // pan knob
        let kc = pos2(sr.center().x, sr.min.y + 26.0);
        let kr = Rect::from_center_size(kc, vec2(34.0, 34.0));
        let kresp = ui.interact(kr, egui::Id::new((&ap, "pan")), Sense::click_and_drag());
        ui.painter().circle_stroke(kc, 14.0, Stroke::new(2.0, dimc(Color32::from_gray(0xb0))));
        let ang = (pan / 100.0) as f32 * 135f32.to_radians();
        ui.painter().line_segment([kc, kc + vec2(ang.sin(), -ang.cos()) * 12.0], Stroke::new(2.0, dimc(Color32::from_gray(0xb0))));
        small_text(ui, pos2(kc.x - 18.0, kc.y + 16.0), Align2::CENTER_CENTER, "L", 11.0, dimc(t.hot_text));
        small_text(ui, pos2(kc.x + 18.0, kc.y + 16.0), Align2::CENTER_CENTER, "R", 11.0, dimc(t.hot_text));
        small_text(ui, pos2(kc.x, kc.y + 32.0), Align2::CENTER_CENTER, &format!("{pan:.1}"), 11.5, dimc(t.hot_text));
        app.auto.add(&format!("{ap}.pan"), kr, "Clip pan");
        if kresp.dragged() && active {
            let d = kresp.drag_delta();
            set(&mut acts, "panner", "balance", (pan + (d.x - d.y) as f64).clamp(-100.0, 100.0), kresp.drag_started());
        }
        if kresp.double_clicked() {
            set(&mut acts, "panner", "balance", 0.0, true);
        }
        // M S ◇
        let y = sr.min.y + 72.0;
        let b = |k: f32| Rect::from_min_size(pos2(sr.center().x - 33.0 + k * 24.0, y), vec2(18.0, 16.0));
        let mr = crate::widgets::letter_toggle(ui, b(0.0), "M", tr.muted, MUTE_ON, &t, egui::Id::new((&ap, "M")));
        app.auto.add(&format!("{ap}.mute"), b(0.0), "Mute Track");
        if mr.clicked() {
            acts.push(("mixer.setStrip".into(), json!({"strip": tr.id.0, "muted": !tr.muted})));
        }
        let so = crate::widgets::letter_toggle(ui, b(1.0), "S", tr.solo, SOLO_ON, &t, egui::Id::new((&ap, "S")));
        app.auto.add(&format!("{ap}.solo"), b(1.0), "Solo Track");
        if so.clicked() {
            acts.push(("mixer.setStrip".into(), json!({"strip": tr.id.0, "solo": !tr.solo})));
        }
        let kresp =
            crate::widgets::icon_toggle(ui, b(2.0), Icon::Keyframe, kf, &t, egui::Id::new((&ap, "kf")), Some(t.accent)).on_hover_text("Write keyframes");
        app.auto.add(&format!("{ap}.keyframe"), b(2.0), "Write Keyframes");
        if kresp.clicked() {
            write_kf.insert(tr.id.0, !kf);
        }
        // fader + meter
        let area = Rect::from_min_max(pos2(sr.min.x, y + 30.0), pos2(sr.max.x, sr.max.y - 46.0));
        if area.height() > 40.0 {
            let track = Rect::from_min_max(pos2(sr.min.x + 33.0, area.min.y + 8.0), pos2(sr.min.x + 37.0, area.max.y - 8.0));
            let ypos = |db: f64| track.max.y - db_to_pos(db) * track.height();
            for d in SCALE {
                let s = if d <= FADER_MIN_DB { "-∞".to_string() } else { format!("{}", d as i32) };
                small_text(ui, pos2(sr.min.x + 26.0, ypos(d)), Align2::RIGHT_CENTER, &s, 9.0, t.text_dim);
            }
            ui.painter().rect_filled(track, 2.0, Color32::from_gray(0x10));
            let cy = ypos(vol);
            let cap = Rect::from_center_size(pos2(track.center().x, cy), vec2(18.0, 26.0));
            let fresp = ui.interact(cap.expand(3.0), egui::Id::new((&ap, "fader")), Sense::click_and_drag());
            ui.painter().rect_filled(cap, 3.0, Color32::from_gray(0x2a));
            ui.painter().rect_stroke(cap, 3.0, Stroke::new(2.0, dimc(Color32::from_gray(0xe0))), StrokeKind::Inside);
            ui.painter().line_segment([pos2(cap.min.x + 5.0, cy), pos2(cap.max.x - 5.0, cy)], Stroke::new(2.0, dimc(Color32::from_gray(0xe0))));
            app.auto.add(&format!("{ap}.fader"), cap, &format!("Clip volume {}", db_text(vol)));
            if fresp.dragged() && active && fresp.drag_delta().y != 0.0 {
                set(&mut acts, "volume", "level", pos_to_db(db_to_pos(vol) - fresp.drag_delta().y / track.height()), fresp.drag_started());
            }
            if fresp.double_clicked() {
                set(&mut acts, "volume", "level", 0.0, true);
            }
            let mrect = Rect::from_min_max(pos2(sr.min.x + 46.0, area.min.y), pos2(sr.max.x - 6.0, area.max.y));
            draw_meters(ui, mrect, meters.get(&tr.id.0).copied().unwrap_or([-90.0; 4]), &t);
        }
        small_text(ui, pos2(sr.min.x + 8.0, sr.max.y - 34.0), Align2::LEFT_CENTER, &db_text(vol), 11.5, dimc(t.hot_text));
        app.auto.add(&format!("{ap}.value"), Rect::from_min_size(pos2(sr.min.x + 4.0, sr.max.y - 42.0), vec2(50.0, 16.0)), &db_text(vol));
        small_text(ui, pos2(sr.min.x + 6.0, sr.max.y - 10.0), Align2::LEFT_CENTER, &label, 12.0, t.text);
        small_text(ui, pos2(sr.min.x + 30.0, sr.max.y - 10.0), Align2::LEFT_CENTER, &tr.name, 12.0, t.text);
    }
    ui.data_mut(|d| d.insert_temp(kf_id, write_kf));
    run_actions(app, &ui.ctx().clone(), acts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taper_roundtrips_and_is_monotonic() {
        let mut last = f64::NEG_INFINITY;
        for i in 0..=100 {
            let p = i as f32 / 100.0;
            let db = pos_to_db(p);
            assert!(db >= last - 1e-9);
            last = db;
            if db > FADER_MIN_DB {
                assert!((db_to_pos(db) - p).abs() < 1e-4, "{p} → {db}");
            }
        }
        assert_eq!(db_to_pos(0.0), 0.76);
        assert_eq!(pos_to_db(1.0), 15.0);
        assert_eq!(pos_to_db(0.0), FADER_MIN_DB);
    }
}
