//! The Timeline panel.
//!
//! Layout (Premiere's): big blue playhead timecode + toggles top-left, ruler with markers /
//! in-out / render bar top-right, video tracks (V1 lowest) above audio tracks (A1 highest), track
//! headers on the left, a zoom scroll bar at the bottom.
//!
//! Interaction is tool-driven; every gesture ends in exactly one engine command, so it is undoable,
//! journaled and reproducible over the control channel. Zoom and scroll are *animated* (critically
//! damped exponential easing, anchored under the cursor) so navigation feels fluid; all geometry is
//! drawn as GPU meshes by egui's wgpu backend and culled to the visible range.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{ClipId, ItemId, Sequence, TrackId, TrackItem, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{TimelineView, Tool};
use crate::theme::Tokens;

const TOP_H: f32 = 64.0; // timecode + ruler block
const RULER_H: f32 = 30.0;
const SCROLLBAR_H: f32 = 14.0;
const DIVIDER_H: f32 = 6.0;
const MASTER_H: f32 = 30.0;
const SNAP_PX: f32 = 9.0;

/// Transient interaction state.
#[derive(Default)]
pub struct TlState {
    pub drag: Option<Drag>,
    /// Last layout (for hit-testing from the control channel).
    pub layout: Option<Layout>,
    /// Snap indicator x (screen) this frame.
    snap_x: Option<f32>,
    pub peaks: Arc<Mutex<HashMap<ItemId, Arc<Vec<(f32, f32)>>>>>,
    peaks_pending: Arc<Mutex<Vec<ItemId>>>,
    zoom_anchor: Option<(f64, f32)>,
}

#[derive(Clone, Debug)]
pub enum Drag {
    Scrub,
    Move { clips: Vec<ClipId>, grab_tick: Tick, start_track: TrackId, offset: Tick, track_delta: i32 },
    Trim { clip: ClipId, edge: filmcraft_edit::Edge, mode: filmcraft_edit::TrimMode, delta: Tick },
    Roll { left: ClipId, right: ClipId, delta: Tick },
    Slip { clip: ClipId, delta: Tick },
    Slide { clip: ClipId, delta: Tick },
    Stretch { clip: ClipId, edge: filmcraft_edit::Edge, delta: Tick },
    Pan { last: Pos2 },
    Marquee { start: Pos2 },
    Divider,
    ZoomBar { grab: f32, mode: u8 },
}

/// Geometry of one visible track row.
#[derive(Clone, Debug)]
pub struct Row {
    pub track: TrackId,
    pub kind: TrackKind,
    pub index: usize,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub content: Rect,
    pub ruler: Rect,
    pub rows: Vec<Row>,
    pub pps: f64,
    pub scroll: f64,
}

impl Layout {
    pub fn x_of(&self, t: Tick) -> f32 {
        self.content.min.x + ((t.seconds() - self.scroll) * self.pps) as f32
    }
    pub fn tick_at(&self, x: f32) -> Tick {
        Tick::from_seconds_f64(self.scroll + (x - self.content.min.x) as f64 / self.pps)
    }
    pub fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.iter().find(|r| r.rect.min.y <= y && y < r.rect.max.y)
    }
}

/// Zoom about a time (keeps it under the same screen x).
pub fn zoom_about(v: &mut TimelineView, factor: f64, anchor_secs: f64, width: f32) {
    let old = v.target_pps;
    let new = (old * factor).clamp(0.05, 24_000.0);
    let x = ((anchor_secs - v.target_scroll) * old) as f32;
    let x = if (0.0..=width).contains(&x) { x } else { width / 2.0 };
    v.target_pps = new;
    v.target_scroll = (anchor_secs - x as f64 / new).max(0.0);
}

fn label_color(l: filmcraft_project::Label) -> Color32 {
    let c = l.rgb();
    Color32::from_rgb(c[0], c[1], c[2])
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let Some(seq_id) = app.session.state.active_sequence else {
        empty_state(app, ui, rect);
        return;
    };
    let seq = app.session.active_sequence().expect("active").clone();
    let rate = seq.settings.frame_rate;
    let dt = ctx.input(|i| i.stable_dt).min(0.05) as f64;
    let header_w = app.ui.timeline.header_w;
    let content = Rect::from_min_max(pos2(rect.min.x + header_w, rect.min.y + TOP_H), pos2(rect.max.x - 10.0, rect.max.y - SCROLLBAR_H));
    let ruler = Rect::from_min_max(pos2(content.min.x, rect.min.y + TOP_H - RULER_H), pos2(content.max.x, rect.min.y + TOP_H));
    app.last_timeline_width = content.width();
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.panel_bg);

    // ---- sequence tabs strip (top-left header block)
    // ---- animated zoom / scroll
    let dur_s = seq.duration().seconds().max(1.0);
    {
        let v = &mut app.ui.timeline;
        if v.fit_pending && content.width() > 50.0 {
            v.target_pps = (content.width() as f64 * 0.94 / dur_s).clamp(0.05, 24_000.0);
            v.target_scroll = 0.0;
            v.fit_pending = false;
        }
        let k = 1.0 - (-dt * 20.0).exp();
        let zooming = (v.pps - v.target_pps).abs() / v.target_pps > 0.001;
        v.pps += (v.target_pps - v.pps) * k;
        if let Some((anchor_t, anchor_x)) = app.tl.zoom_anchor.filter(|_| zooming) {
            v.scroll = (anchor_t - (anchor_x - content.min.x) as f64 / v.pps).max(0.0);
            v.target_scroll = (anchor_t - (anchor_x - content.min.x) as f64 / v.target_pps).max(0.0);
        } else {
            v.scroll += (v.target_scroll - v.scroll) * k;
            app.tl.zoom_anchor = None;
        }
        if (v.pps - v.target_pps).abs() > 1e-6 || (v.scroll - v.target_scroll).abs() > 1e-6 {
            ctx.request_repaint();
        }
        if (v.pps - v.target_pps).abs() / v.target_pps < 0.0005 {
            v.pps = v.target_pps;
        }
        if (v.scroll - v.target_scroll).abs() * v.pps < 0.05 {
            v.scroll = v.target_scroll;
        }
    }
    // follow playhead while playing (page scroll)
    if app.playback.playing && app.ui.timeline.follow {
        let v = &mut app.ui.timeline;
        let px = (app.session.playhead().seconds() - v.scroll) * v.pps;
        if px > content.width() as f64 * 0.95 || px < 0.0 {
            v.target_scroll = (app.session.playhead().seconds() - content.width() as f64 * 0.05 / v.pps).max(0.0);
            v.scroll = v.target_scroll;
        }
    }
    let pps = app.ui.timeline.pps;
    let scroll = app.ui.timeline.scroll;

    // ---- rows
    let tracks_area = Rect::from_min_max(content.min, content.max);
    let split_y = tracks_area.min.y + (tracks_area.height() - DIVIDER_H) * app.ui.timeline.split;
    let video_area = Rect::from_min_max(pos2(rect.min.x, tracks_area.min.y), pos2(content.max.x, split_y));
    let audio_area = Rect::from_min_max(pos2(rect.min.x, split_y + DIVIDER_H), pos2(content.max.x, tracks_area.max.y - MASTER_H));
    let vh = app.ui.timeline.video_track_h;
    let ah = app.ui.timeline.audio_track_h;
    let mut rows = Vec::new();
    // Video: V1 at the bottom of the video area, stacking upward; v_scroll shifts up.
    let nv = seq.video_tracks.len();
    let video_total = nv as f32 * vh;
    let v_off = (video_area.height() - video_total).max(0.0);
    let max_vs = (video_total - video_area.height()).max(0.0);
    app.ui.timeline.v_scroll = app.ui.timeline.v_scroll.clamp(0.0, max_vs);
    for (i, tr) in seq.video_tracks.iter().enumerate() {
        let top = video_area.min.y + v_off + (nv - 1 - i) as f32 * vh - (max_vs - app.ui.timeline.v_scroll);
        rows.push(Row { track: tr.id, kind: TrackKind::Video, index: i, rect: Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + vh)) });
    }
    let na = seq.audio_tracks.len();
    let max_as = (na as f32 * ah - audio_area.height()).max(0.0);
    app.ui.timeline.a_scroll = app.ui.timeline.a_scroll.clamp(0.0, max_as);
    for (i, tr) in seq.audio_tracks.iter().enumerate() {
        let top = audio_area.min.y + i as f32 * ah - app.ui.timeline.a_scroll;
        rows.push(Row { track: tr.id, kind: TrackKind::Audio, index: i, rect: Rect::from_min_max(pos2(content.min.x, top), pos2(content.max.x, top + ah)) });
    }
    let layout = Layout { content, ruler, rows: rows.clone(), pps, scroll };
    app.tl.layout = Some(layout.clone());

    // ---- backgrounds
    painter.rect_filled(Rect::from_min_max(pos2(content.min.x, content.min.y), content.max), 0.0, t.tl_bg);
    let vclip = Rect::from_min_max(pos2(rect.min.x, video_area.min.y), video_area.max);
    let aclip = Rect::from_min_max(pos2(rect.min.x, audio_area.min.y), audio_area.max);
    for r in &rows {
        let clip = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip);
        if row.height() <= 0.0 {
            continue;
        }
        let bg = if r.index % 2 == 0 { t.tl_track_bg } else { t.tl_track_bg_alt };
        painter.rect_filled(row, 0.0, bg);
        painter.line_segment([pos2(row.min.x, r.rect.max.y - 0.5), pos2(row.max.x, r.rect.max.y - 0.5)], Stroke::new(1.0, t.tl_bg));
    }
    // in/out shading across tracks
    if seq.mark_in.is_some() || seq.mark_out.is_some() {
        let a = layout.x_of(seq.mark_in.unwrap_or(Tick::ZERO)).max(content.min.x);
        let b = layout.x_of(seq.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(seq.duration())).min(content.max.x);
        if b > a {
            painter.rect_filled(Rect::from_min_max(pos2(a, content.min.y), pos2(b, content.max.y)), 0.0, t.in_out_shade);
        }
    }

    // ---- clips
    let visible = (layout.tick_at(content.min.x - 2.0), layout.tick_at(content.max.x + 2.0));
    let selection: Vec<ClipId> = app.session.state.selection.clone();
    let mut previews: HashMap<ClipId, (Tick, Tick, Option<TrackId>)> = HashMap::new(); // live drag preview: (start, dur, track)
    preview_drag(app, &seq, &layout, &mut previews);
    for r in &rows {
        let tr = seq.track(r.track).expect("track");
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let row = r.rect.intersect(clip_rect);
        if row.height() <= 0.0 {
            continue;
        }
        let p = painter.with_clip_rect(Rect::from_min_max(pos2(content.min.x, row.min.y), pos2(content.max.x, row.max.y)));
        for it in &tr.items {
            let (start, dur, moved_track) = previews.get(&it.id).copied().unwrap_or((it.start, it.duration, None));
            if moved_track.is_some_and(|m| m != r.track) {
                continue;
            }
            if start + dur < visible.0 || start > visible.1 {
                continue;
            }
            let x0 = layout.x_of(start);
            let x1 = layout.x_of(start + dur);
            let body = Rect::from_min_max(pos2(x0, r.rect.min.y + 1.0), pos2(x1.max(x0 + 1.0), r.rect.max.y - 1.0));
            draw_clip(app, &ctx, &p, body, it, r.kind, selection.contains(&it.id), &t, rate);
            app.auto.add(&format!("timeline.clip.{}", it.id.0), body.intersect(content), &it.name);
        }
        // items dragged onto this track from another
        for (cid, (start, dur, mt)) in &previews {
            if *mt == Some(r.track)
                && tr.item(*cid).is_none()
                && let Some((_, it)) = seq.find_item(*cid)
            {
                let body = Rect::from_min_max(pos2(layout.x_of(*start), r.rect.min.y + 1.0), pos2(layout.x_of(*start + *dur), r.rect.max.y - 1.0));
                draw_clip(app, &ctx, &p, body, it, r.kind, true, &t, rate);
            }
        }
        // transitions
        for trn in &tr.transitions {
            let x0 = layout.x_of(trn.start);
            let x1 = layout.x_of(trn.end());
            let tr_rect = Rect::from_min_max(pos2(x0, r.rect.min.y + 1.0), pos2(x1, r.rect.min.y + (r.rect.height() * 0.5).max(14.0)));
            draw_transition(&p, tr_rect, trn, &t);
            app.auto.add(&format!("timeline.transition.{}", trn.id.0), tr_rect, &trn.effect.effect);
        }
    }

    // ---- track headers
    draw_headers(app, ui, &seq, &rows, rect, vclip, aclip, &t);
    // divider between video and audio
    let div = Rect::from_min_max(pos2(rect.min.x, split_y), pos2(content.max.x, split_y + DIVIDER_H));
    painter.rect_filled(div, 0.0, t.app_bg);
    let div_resp = ui.interact(div, egui::Id::new("tl-divider"), Sense::drag());
    if div_resp.hovered() || div_resp.dragged() {
        ctx.set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if div_resp.dragged() {
        let f = app.ui.timeline.split + div_resp.drag_delta().y / (tracks_area.height() - DIVIDER_H).max(1.0);
        app.ui.timeline.split = f.clamp(0.1, 0.9);
    }
    // master track
    let master = Rect::from_min_max(pos2(rect.min.x, tracks_area.max.y - MASTER_H), pos2(content.max.x, tracks_area.max.y));
    painter.rect_filled(master, 0.0, t.tl_header_bg);
    painter.text(pos2(rect.min.x + 44.0, master.center().y), Align2::LEFT_CENTER, "Mix", Tokens::ui(11.5), t.text);
    painter.text(
        pos2(rect.min.x + header_w - 12.0, master.center().y),
        Align2::RIGHT_CENTER,
        format!("{:.1}", seq.master_volume_db),
        Tokens::ui(11.5),
        t.hot_text,
    );

    // ---- top block: timecode + toggles, ruler
    draw_top(app, ui, rect, &seq, &layout, &t, seq_id);

    // ---- playhead
    let ph = app.session.playhead();
    let px = layout.x_of(ph);
    if px >= content.min.x - 1.0 && px <= content.max.x + 1.0 {
        let head = [
            pos2(px - 6.0, ruler.min.y + 2.0),
            pos2(px + 6.0, ruler.min.y + 2.0),
            pos2(px + 6.0, ruler.max.y - 10.0),
            pos2(px, ruler.max.y - 4.0),
            pos2(px - 6.0, ruler.max.y - 10.0),
        ];
        painter.add(egui::Shape::convex_polygon(head.to_vec(), t.playhead, Stroke::NONE));
        painter.line_segment([pos2(px, ruler.max.y - 4.0), pos2(px, content.max.y)], Stroke::new(1.0, t.playhead));
    }
    // snap indicator
    if let Some(sx) = app.tl.snap_x.take() {
        painter.line_segment([pos2(sx, ruler.max.y), pos2(sx, content.max.y)], Stroke::new(1.0, Color32::from_rgb(250, 250, 250)));
        for y in [ruler.max.y, content.max.y] {
            let d = if y == ruler.max.y { 1.0 } else { -1.0 };
            painter.add(egui::Shape::convex_polygon(vec![pos2(sx - 4.0, y), pos2(sx + 4.0, y), pos2(sx, y + 5.0 * d)], Color32::WHITE, Stroke::NONE));
        }
    }

    // ---- scroll bars
    zoom_scrollbar(app, ui, Rect::from_min_max(pos2(content.min.x, rect.max.y - SCROLLBAR_H + 2.0), pos2(content.max.x, rect.max.y - 2.0)), dur_s, &t);
    vertical_scrollbar(
        ui,
        Rect::from_min_max(pos2(content.max.x + 2.0, video_area.min.y), pos2(rect.max.x - 1.0, video_area.max.y)),
        &mut app.ui.timeline.v_scroll,
        max_vs,
        true,
        &t,
        "vs",
    );
    vertical_scrollbar(
        ui,
        Rect::from_min_max(pos2(content.max.x + 2.0, audio_area.min.y), pos2(rect.max.x - 1.0, audio_area.max.y)),
        &mut app.ui.timeline.a_scroll,
        max_as,
        false,
        &t,
        "as",
    );

    // ---- interaction
    interact(app, ui, &seq, &layout, rect);
    let _ = (visible, TICKS_PER_SECOND);
}

fn empty_state(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, "Drop media here to create sequence.", Tokens::ui(13.0), t.text_dim);
    let b = Rect::from_center_size(rect.center() + vec2(0.0, 20.0), vec2(170.0, 26.0));
    let resp = ui.interact(b, egui::Id::new("tl-open-demo"), Sense::click());
    app.auto.add("timeline.openDemo", b, "Open Demo Project");
    ui.painter().rect_filled(b, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(b.center(), Align2::CENTER_CENTER, "Open Demo Project", Tokens::semibold(12.0), Color32::WHITE);
    if resp.clicked() {
        let _ = app.session.execute("file.openDemoProject", json!({}));
    }
    // accept drops from the project panel
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && ui.rect_contains_pointer(rect)
        && ui.input(|i| i.pointer.any_released())
    {
        let _ = app.session.execute("file.newSequence", json!({"fromItem": item.0}));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_clip(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    p: &egui::Painter,
    body: Rect,
    it: &TrackItem,
    kind: TrackKind,
    selected: bool,
    t: &Tokens,
    rate: FrameRate,
) {
    let base = label_color(it.label);
    let fill = if !it.enabled {
        Color32::from_rgb(70, 70, 70)
    } else if kind == TrackKind::Video {
        base.gamma_multiply(0.92)
    } else {
        base.gamma_multiply(0.8)
    };
    let r = 3.0;
    p.rect_filled(body, r, fill);
    let name_h = 16.0;
    let w = body.width();
    // thumbnails (video) under the name band
    if kind == TrackKind::Video && app.ui.timeline.show_thumbnails && body.height() > 30.0 && w > 24.0 && it.enabled {
        let th_rect = Rect::from_min_max(pos2(body.min.x + 1.0, body.min.y + name_h), pos2(body.max.x - 1.0, body.max.y - 1.0));
        let th_h = th_rect.height();
        let aspect = app.session.project.item(it.item).and_then(|pi| match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref().map(|v| v.width as f32 / v.height as f32),
            filmcraft_project::ItemKind::Sequence(s) => Some(s.settings.width as f32 / s.settings.height as f32),
            _ => None,
        });
        if let Some(aspect) = aspect {
            let th_w = th_h * aspect;
            let n = ((th_rect.width() / th_w).ceil() as usize).clamp(1, 60);
            let clip_p = p.with_clip_rect(th_rect.intersect(p.clip_rect()));
            for k in 0..n {
                let x = th_rect.min.x + k as f32 * th_w;
                if x > clip_p.clip_rect().max.x || x + th_w < clip_p.clip_rect().min.x {
                    continue;
                }
                let frac = (k as f64 * th_w as f64) / w.max(1.0) as f64;
                let tl = it.start + Tick((it.duration.0 as f64 * frac) as i64);
                let mt = it.source_time_at(tl);
                let mt = rate.snap(mt);
                // quantise thumbnail times to 1/2 s so the cache hits
                let q = Tick((mt.0 / (TICKS_PER_SECOND / 2)) * (TICKS_PER_SECOND / 2));
                if let Some((tex, _)) = app.thumbnail(ctx, it.item, q, 128) {
                    let tr = Rect::from_min_size(pos2(x, th_rect.min.y), vec2(th_w, th_h));
                    clip_p.image(tex, tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(235));
                }
            }
        }
    }
    // waveform (audio)
    if kind == TrackKind::Audio && app.ui.timeline.show_waveforms && w > 4.0 {
        draw_waveform(app, p, body, it, fill);
    }
    // name band
    let text_col = if it.enabled { Color32::from_rgb(20, 20, 24) } else { t.text_dim };
    let mut tx = body.min.x + 5.0;
    if it.has_standard_effects() || it.has_modified_intrinsics() {
        let fx = Rect::from_min_size(pos2(tx, body.min.y + 3.0), vec2(15.0, 10.0));
        if fx.max.x < body.max.x {
            p.rect_filled(fx, 2.0, if it.has_standard_effects() { Color32::from_rgb(236, 196, 58) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 70) });
            p.text(fx.center(), Align2::CENTER_CENTER, "fx", Tokens::semibold(8.5), Color32::from_rgb(30, 30, 30));
            tx += 18.0;
        }
    }
    if w > 30.0 {
        let clip_p = p.with_clip_rect(body.shrink2(vec2(3.0, 0.0)).intersect(p.clip_rect()));
        let mut label = it.name.clone();
        if (it.speed - 1.0).abs() > 1e-6 || it.reverse {
            label = format!("{label} [{}%]", ((if it.reverse { -1.0 } else { 1.0 }) * it.speed * 100.0).round());
        }
        clip_p.text(pos2(tx.max(clip_p.clip_rect().min.x + 4.0), body.min.y + 8.0), Align2::LEFT_CENTER, label, Tokens::ui(10.5), text_col);
    }
    if it.link.is_none() && kind == TrackKind::Video && w > 60.0 {
        // unlinked items show no link indicator; linked ones are default
    }
    if selected {
        p.rect_stroke(body, r, Stroke::new(1.5, t.clip_selected_border), StrokeKind::Inside);
    } else {
        p.rect_stroke(body, r, Stroke::new(1.0, Color32::from_black_alpha(90)), StrokeKind::Inside);
    }
}

fn draw_waveform(app: &mut FilmcraftApp, p: &egui::Painter, body: Rect, it: &TrackItem, fill: Color32) {
    let peaks = request_peaks(app, it.item);
    let Some(peaks) = peaks else { return };
    // peaks are per 256 samples at 48 kHz
    let spp = 256.0;
    let sr = 48_000.0;
    let area = body.shrink2(vec2(1.0, 3.0));
    let area = Rect::from_min_max(pos2(area.min.x, area.min.y + 12.0), area.max);
    if area.height() < 4.0 {
        return;
    }
    let clip = p.clip_rect().intersect(area);
    let mid = area.center().y;
    let amp = area.height() * 0.5;
    // Like Premiere, the waveform reflects clip gain (not the Volume effect); normalise so quiet
    // material stays readable.
    let peak = peaks.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs())).max(1e-4);
    let gain = filmcraft_render::audio::db_to_gain(it.gain_db) * (0.9 / peak).min(8.0);
    let col = fill.linear_multiply(0.45);
    let dark = Color32::from_rgba_unmultiplied(8, 40, 16, 150);
    let mut mesh = egui::Mesh::default();
    let x_start = clip.min.x.floor() as i32;
    let x_end = clip.max.x.ceil() as i32;
    let dur_px = body.width().max(1.0);
    for x in (x_start..x_end).step_by(1) {
        let f = (x as f32 - body.min.x) / dur_px;
        let tl = it.start + Tick((it.duration.0 as f64 * f as f64) as i64);
        let tl2 = it.start + Tick((it.duration.0 as f64 * ((x as f32 + 1.0 - body.min.x) / dur_px) as f64) as i64);
        let s0 = (it.source_time_at(tl).seconds() * sr / spp) as usize;
        let s1 = ((it.source_time_at(tl2).seconds() * sr / spp) as usize).max(s0 + 1);
        let mut lo = 0f32;
        let mut hi = 0f32;
        for (a, b) in peaks.iter().skip(s0).take(s1 - s0) {
            lo = lo.min(*a);
            hi = hi.max(*b);
        }
        let y0 = mid - (hi * gain).clamp(-1.0, 1.0) * amp;
        let y1 = mid - (lo * gain).clamp(-1.0, 1.0) * amp;
        let r = Rect::from_min_max(pos2(x as f32, y0.min(mid - 0.5)), pos2(x as f32 + 1.0, y1.max(mid + 0.5)));
        mesh.add_colored_rect(r, dark);
    }
    let _ = col;
    p.add(mesh);
}

fn request_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    if let Some(p) = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&item) {
        return Some(p.clone());
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&item) {
            return None;
        }
        pend.push(item);
    }
    let src = app.session.source(item)?;
    let peaks = app.tl.peaks.clone();
    let pending = app.tl.peaks_pending.clone();
    let dur = src.info().duration;
    let run = move || {
        let sr = 48_000u32;
        let total = dur.to_units_floor(sr as i64).max(0) as usize;
        let mut out = Vec::with_capacity(total / 256 + 1);
        let chunk = 48_000 * 4;
        let mut s = 0usize;
        while s < total {
            let n = chunk.min(total - s);
            match src.audio(s as i64, n, sr) {
                Ok(buf) => {
                    let ch = buf.channels.first().cloned().unwrap_or_default();
                    for c in ch.chunks(256) {
                        let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                        out.push((lo, hi));
                    }
                }
                Err(_) => break,
            }
            s += n;
        }
        peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(item, Arc::new(out));
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != item);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    None
}

fn draw_transition(p: &egui::Painter, r: Rect, trn: &filmcraft_project::Transition, _t: &Tokens) {
    let fill = Color32::from_rgba_unmultiplied(120, 120, 150, 200);
    p.rect_filled(r, 2.0, fill);
    // diagonal (Premiere shows a diagonal line across transitions)
    let clip = p.with_clip_rect(r.intersect(p.clip_rect()));
    clip.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, Color32::from_black_alpha(120)));
    p.rect_stroke(r, 2.0, Stroke::new(1.0, Color32::from_black_alpha(140)), StrokeKind::Inside);
    if r.width() > 50.0 {
        let name = trn.effect.def().map(|d| d.name).unwrap_or(&trn.effect.effect);
        clip.text(pos2(r.min.x + 4.0, r.center().y), Align2::LEFT_CENTER, name, Tokens::ui(9.5), Color32::from_rgb(20, 20, 24));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_headers(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, rows: &[Row], rect: Rect, vclip: Rect, aclip: Rect, t: &Tokens) {
    let hw = app.ui.timeline.header_w;
    let tg = app.session.targeting();
    let mut actions: Vec<(String, Value)> = Vec::new();
    for r in rows {
        let tr = seq.track(r.track).expect("track");
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let hrect = Rect::from_min_max(pos2(rect.min.x, r.rect.min.y), pos2(rect.min.x + hw - 2.0, r.rect.max.y));
        let visible = hrect.intersect(clip_rect);
        if visible.height() <= 0.0 {
            continue;
        }
        let p = ui.painter().with_clip_rect(visible);
        p.rect_filled(hrect.shrink2(vec2(0.0, 0.5)), 0.0, t.tl_header_bg);
        let label = format!("{}{}", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1);
        let cy = hrect.min.y + 13.0f32.min(hrect.height() / 2.0);
        // source patch (left): shows the source track when patched
        let patched = if r.kind == TrackKind::Video { tg.video_dest == Some(r.track) } else { tg.audio_dest == Some(r.track) };
        let pr = Rect::from_center_size(pos2(hrect.min.x + 14.0, cy), vec2(22.0, 17.0));
        let presp = ui.interact(pr.intersect(visible), egui::Id::new(("patch", r.track.0)), Sense::click());
        app.auto.add(&format!("timeline.track.{label}.sourcePatch"), pr, "Source patch");
        if patched {
            p.rect_filled(pr, 3.0, t.accent);
            p.text(pr.center(), Align2::CENTER_CENTER, label.clone(), Tokens::semibold(10.0), Color32::WHITE);
        } else {
            p.rect_stroke(pr, 3.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
        }
        if presp.clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "sourcePatch": !patched})));
        }
        // target toggle
        let targeted = tg.targeted.contains(&r.track);
        let trr = Rect::from_center_size(pos2(hrect.min.x + 40.0, cy), vec2(24.0, 17.0));
        let tresp = ui.interact(trr.intersect(visible), egui::Id::new(("target", r.track.0)), Sense::click());
        app.auto.add(&format!("timeline.track.{label}.target"), trr, "Toggle track targeting");
        p.rect_filled(trr, 3.0, if targeted { Color32::from_rgb(70, 70, 70) } else { Color32::TRANSPARENT });
        p.rect_stroke(trr, 3.0, Stroke::new(1.0, if targeted { t.accent } else { t.separator }), StrokeKind::Inside);
        p.text(trr.center(), Align2::CENTER_CENTER, label.clone(), Tokens::semibold(10.0), if targeted { t.tab_text_active } else { t.text_dim });
        if tresp.clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "targeted": !targeted})));
        }
        // sync lock, lock
        let mut x = hrect.min.x + 60.0;
        let mut toggle = |icon: Icon, on: bool, name: &str, key: &str, on_col: Option<Color32>, actions: &mut Vec<(String, Value)>, app: &mut FilmcraftApp| {
            let br = Rect::from_center_size(pos2(x + 9.0, cy), vec2(18.0, 18.0));
            let resp = crate::widgets::icon_toggle(ui, br.intersect(visible), icon, on, t, egui::Id::new((key, r.track.0)), on_col);
            app.auto.add(&format!("timeline.track.{label}.{key}"), br, name);
            if resp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, key: !on})));
            }
            x += 20.0;
        };
        toggle(Icon::SyncLock, tr.sync_lock, "Toggle Sync Lock", "syncLock", Some(t.icon), &mut actions, app);
        toggle(
            if tr.locked { Icon::Lock } else { Icon::Unlock },
            tr.locked,
            "Toggle Track Lock",
            "locked",
            Some(Color32::from_rgb(230, 190, 70)),
            &mut actions,
            app,
        );
        if r.kind == TrackKind::Video {
            toggle(if tr.enabled { Icon::Eye } else { Icon::EyeOff }, tr.enabled, "Toggle Track Output", "enabled", Some(t.icon), &mut actions, app);
        } else {
            let mr = Rect::from_center_size(pos2(x + 9.0, cy), vec2(17.0, 15.0));
            let mresp =
                crate::widgets::letter_toggle(ui, mr.intersect(visible), "M", tr.muted, Color32::from_rgb(70, 190, 110), t, egui::Id::new(("mute", r.track.0)));
            app.auto.add(&format!("timeline.track.{label}.muted"), mr, "Mute Track");
            if mresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "muted": !tr.muted})));
            }
            let sr = Rect::from_center_size(pos2(x + 29.0, cy), vec2(17.0, 15.0));
            let sresp =
                crate::widgets::letter_toggle(ui, sr.intersect(visible), "S", tr.solo, Color32::from_rgb(235, 200, 60), t, egui::Id::new(("solo", r.track.0)));
            app.auto.add(&format!("timeline.track.{label}.solo"), sr, "Solo Track");
            if sresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "solo": !tr.solo})));
            }
            let vr = Rect::from_center_size(pos2(x + 49.0, cy), vec2(17.0, 15.0));
            crate::widgets::icon_toggle(ui, vr.intersect(visible), Icon::Mic, false, t, egui::Id::new(("vo", r.track.0)), None);
            x += 62.0;
        }
        // name (when the track is tall enough) or label
        if hrect.height() >= 38.0 {
            p.text(pos2(hrect.min.x + 60.0, hrect.max.y - 11.0), Align2::LEFT_CENTER, &tr.name, Tokens::ui(10.5), t.text_dim);
        } else if x + 30.0 < hrect.max.x {
            p.text(pos2(hrect.max.x - 8.0, cy), Align2::RIGHT_CENTER, &tr.name, Tokens::ui(10.5), t.text_faint);
        }
        // resize track height by dragging the header's bottom edge
        let edge = Rect::from_min_max(pos2(hrect.min.x, hrect.max.y - 3.0), pos2(hrect.max.x, hrect.max.y + 2.0)).intersect(visible);
        if edge.height() > 0.0 {
            let eresp = ui.interact(edge, egui::Id::new(("trackh", r.track.0)), Sense::drag());
            if eresp.hovered() || eresp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if eresp.dragged() {
                let d = eresp.drag_delta().y * if r.kind == TrackKind::Video { -1.0 } else { 1.0 };
                let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
                *h = (*h + d).clamp(22.0, 220.0);
            }
        }
        // double-click header toggles tall/short
        let resp = ui.interact(hrect.intersect(visible), egui::Id::new(("hdr", r.track.0)), Sense::click());
        if resp.double_clicked() {
            let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
            *h = if *h < 60.0 { 96.0 } else { 36.0 };
        }
    }
    for (cmd, params) in actions {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
}

fn draw_top(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, seq: &Sequence, layout: &Layout, t: &Tokens, seq_id: ItemId) {
    let p = ui.painter().clone();
    let rate = seq.settings.frame_rate;
    // sequence tabs (open sequences)
    let tabs_rect = Rect::from_min_max(pos2(rect.min.x, rect.min.y), pos2(rect.max.x, rect.min.y + TOP_H - RULER_H));
    let _ = tabs_rect;
    // big timecode
    let hw = app.ui.timeline.header_w;
    let tc = format_time(app.session.playhead(), rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    let tc_rect = Rect::from_min_size(pos2(rect.min.x + 10.0, rect.min.y + 6.0), vec2(hw - 16.0, 24.0));
    p.text(pos2(tc_rect.min.x, tc_rect.center().y), Align2::LEFT_CENTER, &tc, Tokens::mono(18.0), t.timecode);
    app.auto.add("timeline.timecode", tc_rect, &tc);
    // toggle row
    let mut x = rect.min.x + 10.0;
    let y = rect.min.y + TOP_H - 16.0;
    let toggles: [(Icon, &str, bool, &str); 5] = [
        (Icon::Nest, "nest", true, "Insert and overwrite sequences as nests or individual clips"),
        (Icon::Magnet, "snap", app.session.state.snapping, "Snap in Timeline (S)"),
        (Icon::Link, "linked", app.session.state.linked_selection, "Linked Selection"),
        (Icon::Marker, "marker", false, "Add Marker (M)"),
        (Icon::Wrench, "settings", false, "Timeline Display Settings"),
    ];
    for (icon, key, on, tip) in toggles {
        let r = Rect::from_center_size(pos2(x + 11.0, y), vec2(22.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(("tl-toggle", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("timeline.toggle.{key}"), r, tip);
        if resp.hovered() {
            p.rect_filled(r, 3.0, t.hover);
        }
        let col = if on && matches!(key, "snap" | "linked") { t.icon_active } else { t.icon };
        icons::paint(&p, r.shrink(4.0), icon, col);
        if resp.clicked() {
            let _ = match key {
                "snap" => app.session.execute("sequence.snap", json!({})),
                "linked" => app.session.execute("sequence.linkedSelection", json!({})),
                "marker" => app.session.execute("markers.add", json!({})),
                _ => Ok(Value::Null),
            };
        }
        if key == "settings" {
            egui::Popup::menu(&resp).show(|ui| {
                ui.checkbox(&mut app.ui.timeline.show_thumbnails, "Show Video Thumbnails");
                ui.checkbox(&mut app.ui.timeline.show_waveforms, "Show Audio Waveform");
                ui.separator();
                if ui.button("Expand All Tracks").clicked() {
                    app.ui.timeline.video_track_h = 90.0;
                    app.ui.timeline.audio_track_h = 70.0;
                }
                if ui.button("Minimize All Tracks").clicked() {
                    app.ui.timeline.video_track_h = 26.0;
                    app.ui.timeline.audio_track_h = 26.0;
                }
            });
        }
        x += 26.0;
    }
    // ruler
    let ruler = layout.ruler;
    p.rect_filled(Rect::from_min_max(pos2(ruler.min.x, ruler.min.y - (TOP_H - RULER_H)), ruler.max), 0.0, t.panel_bg);
    p.rect_filled(ruler, 0.0, t.tl_ruler_bg);
    let pps = layout.pps;
    // choose a tick step in frames so labels are ≥ 90 px apart
    let fps = rate.as_f64();
    let base = rate.timecode_base();
    let steps_frames: Vec<i64> = vec![
        1,
        2,
        5,
        10,
        base / 2,
        base,
        base * 2,
        base * 5,
        base * 10,
        base * 15,
        base * 30,
        base * 60,
        base * 120,
        base * 300,
        base * 600,
        base * 1800,
        base * 3600,
    ];
    let frame_px = pps / fps;
    let label_step = *steps_frames.iter().find(|s| **s as f64 * frame_px >= 90.0).unwrap_or(&(base * 3600));
    let minor = *steps_frames.iter().find(|s| **s as f64 * frame_px >= 9.0).unwrap_or(&label_step);
    let f0 = rate.frame_at(layout.tick_at(ruler.min.x)).max(0);
    let f1 = rate.frame_at(layout.tick_at(ruler.max.x)) + 1;
    let mut f = (f0 / minor) * minor;
    let clip = p.with_clip_rect(ruler);
    while f <= f1 {
        let x = layout.x_of(rate.tick_of(f));
        let major = f % label_step == 0;
        let h = if major {
            10.0
        } else if f % (minor * 5) == 0 {
            6.0
        } else {
            3.5
        };
        clip.line_segment([pos2(x, ruler.max.y - h), pos2(x, ruler.max.y)], Stroke::new(1.0, t.tl_ruler_tick));
        if major {
            let label = format_time(rate.tick_of(f), rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
            clip.text(pos2(x + 3.0, ruler.min.y + 8.0), Align2::LEFT_CENTER, label, Tokens::ui(9.5), t.tl_ruler_text);
        }
        f += minor;
    }
    // in/out on ruler
    if seq.mark_in.is_some() || seq.mark_out.is_some() {
        let a = layout.x_of(seq.mark_in.unwrap_or(Tick::ZERO));
        let b = layout.x_of(seq.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(seq.duration()));
        clip.rect_filled(Rect::from_min_max(pos2(a, ruler.min.y + 14.0), pos2(b, ruler.max.y - 6.0)), 0.0, Color32::from_white_alpha(40));
        if seq.mark_in.is_some() {
            clip.line_segment([pos2(a, ruler.min.y + 12.0), pos2(a, ruler.max.y)], Stroke::new(1.5, Color32::from_white_alpha(170)));
        }
        if seq.mark_out.is_some() {
            clip.line_segment([pos2(b, ruler.min.y + 12.0), pos2(b, ruler.max.y)], Stroke::new(1.5, Color32::from_white_alpha(170)));
        }
    }
    // render bar: yellow where effects/transitions need GPU work, green where cached
    let rb = Rect::from_min_max(pos2(ruler.min.x, ruler.max.y - 3.0), pos2(ruler.max.x, ruler.max.y));
    for tr in &seq.video_tracks {
        for it in &tr.items {
            let heavy = it.has_standard_effects() || it.has_modified_intrinsics();
            let col = if heavy { t.render_yellow } else { continue };
            let r = Rect::from_min_max(pos2(layout.x_of(it.start), rb.min.y), pos2(layout.x_of(it.end()), rb.max.y)).intersect(ruler);
            clip.rect_filled(r, 0.0, col);
        }
        for trn in &tr.transitions {
            let r = Rect::from_min_max(pos2(layout.x_of(trn.start), rb.min.y), pos2(layout.x_of(trn.end()), rb.max.y)).intersect(ruler);
            clip.rect_filled(r, 0.0, t.render_yellow);
        }
    }
    // markers
    for m in &seq.markers {
        let x = layout.x_of(m.start);
        let c = label_color(m.color);
        let y0 = ruler.min.y + 14.0;
        let shape = vec![pos2(x - 5.0, y0), pos2(x + 5.0, y0), pos2(x + 5.0, y0 + 7.0), pos2(x, y0 + 11.0), pos2(x - 5.0, y0 + 7.0)];
        clip.add(egui::Shape::convex_polygon(shape, c, Stroke::NONE));
        if m.duration > Tick::ZERO {
            clip.rect_filled(Rect::from_min_max(pos2(x, y0), pos2(layout.x_of(m.start + m.duration), y0 + 7.0)), 0.0, c.gamma_multiply(0.6));
        }
        let mr = Rect::from_center_size(pos2(x, y0 + 5.0), vec2(10.0, 11.0));
        app.auto.add(&format!("timeline.marker.{}", m.id.0), mr, &m.name);
        if ui.rect_contains_pointer(mr) && !m.name.is_empty() {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new(("mk", m.id.0)), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(&m.name);
            });
        }
    }
    let _ = seq_id;
    app.auto.add("timeline.ruler", ruler, "time ruler");
}

fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, dur_s: f64, t: &Tokens) {
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.field_bg);
    let v = &mut app.ui.timeline;
    let total = (dur_s * 1.15).max(v.scroll + bar.width() as f64 / v.pps);
    let vis = bar.width() as f64 / v.pps;
    let a = bar.min.x + (v.scroll / total) as f32 * bar.width();
    let b = bar.min.x + ((v.scroll + vis) / total).min(1.0) as f32 * bar.width();
    let thumb = Rect::from_min_max(pos2(a, bar.min.y + 1.0), pos2(b.max(a + 16.0), bar.max.y - 1.0));
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { Color32::from_rgb(110, 110, 110) } else { Color32::from_rgb(85, 85, 85) });
    // end handles (circles) to zoom
    for x in [thumb.min.x + 5.0, thumb.max.x - 5.0] {
        p.circle_filled(pos2(x, thumb.center().y), 3.0, Color32::from_rgb(170, 170, 170));
    }
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    let resp = ui.interact(bar, egui::Id::new("tl-zoombar"), Sense::drag());
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let mode = if (pos.x - thumb.min.x).abs() < 8.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 8.0 {
            2
        } else {
            0
        };
        app.tl.drag = Some(Drag::ZoomBar { grab: pos.x, mode });
    }
    if let Some(Drag::ZoomBar { mode, .. }) = app.tl.drag.clone()
        && resp.dragged()
    {
        let dx = resp.drag_delta().x as f64 / bar.width() as f64 * total;
        let v = &mut app.ui.timeline;
        match mode {
            0 => {
                v.target_scroll = (v.target_scroll + dx).max(0.0);
                v.scroll = v.target_scroll;
            }
            1 => {
                let end = v.scroll + vis;
                let ns = (v.scroll + dx).clamp(0.0, end - 0.05);
                v.pps = bar.width() as f64 / (end - ns);
                v.target_pps = v.pps;
                v.scroll = ns;
                v.target_scroll = ns;
            }
            _ => {
                let ne = (v.scroll + vis + dx).max(v.scroll + 0.05);
                v.pps = bar.width() as f64 / (ne - v.scroll);
                v.target_pps = v.pps;
            }
        }
    }
    if resp.drag_stopped() {
        app.tl.drag = None;
    }
}

fn vertical_scrollbar(ui: &mut egui::Ui, bar: Rect, value: &mut f32, max: f32, invert: bool, t: &Tokens, id: &str) {
    if max <= 0.5 || bar.height() < 20.0 {
        return;
    }
    let p = ui.painter();
    let frac_vis = bar.height() / (bar.height() + max);
    let h = (bar.height() * frac_vis).max(18.0);
    let pos = if invert { 1.0 - *value / max } else { *value / max };
    let y = bar.min.y + (bar.height() - h) * pos;
    let thumb = Rect::from_min_size(pos2(bar.min.x + 1.0, y), vec2(bar.width() - 2.0, h));
    p.rect_filled(thumb, 3.0, Color32::from_rgb(80, 80, 80));
    let resp = ui.interact(bar, egui::Id::new(("tl-vs", id)), Sense::drag());
    if resp.dragged() {
        let d = resp.drag_delta().y / (bar.height() - h).max(1.0) * max;
        *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
    }
    let _ = t;
}

/// Snap `t` to nearby candidates (edits, playhead, markers, in/out). Returns snapped tick.
fn snap(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId]) -> Tick {
    if !app.session.state.snapping {
        return t;
    }
    let mut cands: Vec<Tick> = Vec::with_capacity(64);
    for tr in seq.all_tracks() {
        for it in &tr.items {
            if exclude.contains(&it.id) {
                continue;
            }
            cands.push(it.start);
            cands.push(it.end());
        }
    }
    cands.push(app.session.playhead());
    cands.extend(seq.markers.iter().map(|m| m.start));
    cands.extend(seq.mark_in);
    cands.extend(seq.mark_out);
    let x = layout.x_of(t);
    let mut best: Option<(f32, Tick)> = None;
    for c in cands {
        let d = (layout.x_of(c) - x).abs();
        if d < SNAP_PX && best.is_none_or(|b| d < b.0) {
            best = Some((d, c));
        }
    }
    match best {
        Some((_, c)) => {
            app.tl.snap_x = Some(layout.x_of(c));
            c
        }
        None => t,
    }
}

/// What is at a screen point.
#[derive(Clone, Debug)]
pub enum Hit {
    Ruler,
    Clip { track: TrackId, clip: ClipId, edge: Option<filmcraft_edit::Edge> },
    Transition { track: TrackId, id: filmcraft_project::TransitionId },
    Empty { track: TrackId },
    None,
}

pub fn hit(seq: &Sequence, layout: &Layout, pos: Pos2) -> Hit {
    if layout.ruler.contains(pos) {
        return Hit::Ruler;
    }
    if !layout.content.contains(pos) {
        return Hit::None;
    }
    let Some(row) = layout.row_at(pos.y) else { return Hit::None };
    let tr = seq.track(row.track).expect("track");
    for trn in &tr.transitions {
        let x0 = layout.x_of(trn.start);
        let x1 = layout.x_of(trn.end());
        if pos.x >= x0 && pos.x <= x1 && pos.y < row.rect.min.y + (row.rect.height() * 0.5).max(14.0) {
            return Hit::Transition { track: row.track, id: trn.id };
        }
    }
    let t = layout.tick_at(pos.x);
    let edge_px = 7.0f32;
    // prefer edges
    for it in &tr.items {
        let x0 = layout.x_of(it.start);
        let x1 = layout.x_of(it.end());
        let w = x1 - x0;
        let e = edge_px.min(w / 3.0);
        if (pos.x - x0).abs() <= e && pos.x >= x0 - e {
            return Hit::Clip { track: row.track, clip: it.id, edge: Some(filmcraft_edit::Edge::In) };
        }
        if (pos.x - x1).abs() <= e && pos.x <= x1 + e {
            return Hit::Clip { track: row.track, clip: it.id, edge: Some(filmcraft_edit::Edge::Out) };
        }
    }
    if let Some(it) = tr.item_at(t) {
        return Hit::Clip { track: row.track, clip: it.id, edge: None };
    }
    Hit::Empty { track: row.track }
}

pub fn hit_json(app: &FilmcraftApp, pos: Pos2) -> Value {
    let (Some(layout), Some(seq)) = (app.tl.layout.as_ref(), app.session.active_sequence()) else { return json!({"hit": "none"}) };
    let t = layout.tick_at(pos.x);
    match hit(seq, layout, pos) {
        Hit::Ruler => json!({"hit": "ruler", "time": t.0}),
        Hit::Clip { track, clip, edge } => json!({"hit": "clip", "track": track.0, "clip": clip.0, "edge": edge.map(|e| format!("{e:?}")), "time": t.0}),
        Hit::Transition { track, id } => json!({"hit": "transition", "track": track.0, "transition": id.0}),
        Hit::Empty { track } => json!({"hit": "empty", "track": track.0, "time": t.0}),
        Hit::None => json!({"hit": "none"}),
    }
}

/// Screen point of a clip (centre, or its in/out edge).
pub fn locate(app: &FilmcraftApp, clip: u64, edge: Option<&str>) -> Option<(f32, f32)> {
    let layout = app.tl.layout.as_ref()?;
    let seq = app.session.active_sequence()?;
    let (tid, it) = seq.find_item(ClipId(clip))?;
    let row = layout.rows.iter().find(|r| r.track == tid)?;
    let y = row.rect.center().y;
    let x = match edge {
        Some("in") => layout.x_of(it.start) + 2.0,
        Some("out") => layout.x_of(it.end()) - 2.0,
        _ => (layout.x_of(it.start) + layout.x_of(it.end())) / 2.0,
    };
    Some((x, y))
}

fn preview_drag(app: &FilmcraftApp, seq: &Sequence, _layout: &Layout, out: &mut HashMap<ClipId, (Tick, Tick, Option<TrackId>)>) {
    let Some(d) = &app.tl.drag else { return };
    match d {
        Drag::Move { clips, offset, track_delta, .. } => {
            for c in clips {
                if let Some((tid, it)) = seq.find_item(*c) {
                    let dest = shift_track(seq, tid, *track_delta);
                    out.insert(*c, ((it.start + *offset).max(Tick::ZERO), it.duration, dest));
                }
            }
        }
        Drag::Trim { clip, edge, delta, .. } | Drag::Stretch { clip, edge, delta } => {
            let ids = filmcraft_engine::commands::with_links(&app.session, &[*clip]);
            for c in ids {
                if let Some((_, it)) = seq.find_item(c) {
                    let v = match edge {
                        filmcraft_edit::Edge::In => (it.start + *delta, it.duration - *delta, None),
                        filmcraft_edit::Edge::Out => (it.start, it.duration + *delta, None),
                    };
                    out.insert(c, v);
                }
            }
        }
        Drag::Roll { left, right, delta } => {
            if let Some((_, l)) = seq.find_item(*left) {
                out.insert(*left, (l.start, l.duration + *delta, None));
            }
            if let Some((_, r)) = seq.find_item(*right) {
                out.insert(*right, (r.start + *delta, r.duration - *delta, None));
            }
        }
        Drag::Slide { clip, delta } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                out.insert(*clip, (it.start + *delta, it.duration, None));
            }
        }
        _ => {}
    }
}

fn shift_track(seq: &Sequence, tid: TrackId, delta: i32) -> Option<TrackId> {
    if delta == 0 {
        return Some(tid);
    }
    for tracks in [&seq.video_tracks, &seq.audio_tracks] {
        if let Some(i) = tracks.iter().position(|t| t.id == tid) {
            let ni = (i as i32 + delta).clamp(0, tracks.len() as i32 - 1) as usize;
            return Some(tracks[ni].id);
        }
    }
    Some(tid)
}

fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, rect: Rect) {
    let ctx = ui.ctx().clone();
    let area = Rect::from_min_max(pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
    let resp = ui.interact(area, egui::Id::new("timeline-area"), Sense::click_and_drag());
    let pos = resp.hover_pos().or(resp.interact_pointer_pos());
    let mods = ctx.input(|i| i.modifiers);
    let tool = app.ui.tool;
    let rate = seq.settings.frame_rate;

    // ---- wheel: horizontal scroll; Alt/Cmd+wheel zoom about cursor; Shift+wheel vertical
    if ui.rect_contains_pointer(rect) {
        let (scroll, zoom) = ctx.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        if let Some(p) = ctx.pointer_hover_pos() {
            if (zoom - 1.0).abs() > 1e-4 || ((mods.alt || mods.command) && scroll.y.abs() > 0.0) {
                let f = if (zoom - 1.0).abs() > 1e-4 { zoom as f64 } else { (1.0 + scroll.y as f64 * 0.01).clamp(0.5, 2.0) };
                let anchor_t = layout.tick_at(p.x).seconds();
                let v = &mut app.ui.timeline;
                v.target_pps = (v.target_pps * f).clamp(0.05, 24_000.0);
                app.tl.zoom_anchor = Some((anchor_t, p.x));
            } else if mods.shift && scroll.y.abs() > 0.0 {
                if layout.rows.iter().find(|r| r.kind == TrackKind::Video).is_some_and(|r| p.y < r.rect.max.y + 200.0) && p.y < layout.content.center().y {
                    app.ui.timeline.v_scroll -= scroll.y;
                } else {
                    app.ui.timeline.a_scroll -= scroll.y;
                }
            } else if scroll.x.abs() > 0.0 || scroll.y.abs() > 0.0 {
                let d = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
                let v = &mut app.ui.timeline;
                v.target_scroll = (v.target_scroll - d as f64 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        }
    }

    // ---- cursor feedback
    if app.tl.drag.is_none()
        && let Some(p) = pos
        && area.contains(p)
    {
        let h = hit(seq, layout, p);
        let cur = match (tool, &h) {
            (Tool::Selection, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeColumn,
            (Tool::Ripple | Tool::Rolling | Tool::RateStretch, Hit::Clip { edge: Some(_), .. }) => CursorIcon::ResizeColumn,
            (Tool::Razor, Hit::Clip { .. }) => CursorIcon::Crosshair,
            (Tool::Slip | Tool::Slide, Hit::Clip { .. }) => CursorIcon::ResizeHorizontal,
            (Tool::Hand, _) => CursorIcon::Grab,
            (Tool::Zoom, _) => CursorIcon::ZoomIn,
            _ => CursorIcon::Default,
        };
        ctx.set_cursor_icon(cur);
        // razor preview line
        if tool == Tool::Razor
            && let Hit::Clip { track, .. } = h
            && let Some(row) = layout.rows.iter().find(|r| r.track == track)
        {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x)), &[]);
            let x = layout.x_of(t);
            ui.painter().line_segment([pos2(x, row.rect.min.y), pos2(x, row.rect.max.y)], Stroke::new(1.0, Color32::WHITE));
        }
    }

    // ---- press
    if resp.drag_started() || (resp.clicked() && app.tl.drag.is_none()) {
        let Some(p) = resp.interact_pointer_pos() else { return };
        let h = hit(seq, layout, p);
        let t = layout.tick_at(p.x);
        let started = match (tool, h.clone()) {
            (_, Hit::Ruler) => Some(Drag::Scrub),
            (Tool::Hand, _) => Some(Drag::Pan { last: p }),
            (Tool::Zoom, _) => {
                if resp.clicked() {
                    let f = if mods.alt { 1.0 / 2.0 } else { 2.0 };
                    app.ui.timeline.target_pps = (app.ui.timeline.target_pps * f).clamp(0.05, 24_000.0);
                    app.tl.zoom_anchor = Some((t.seconds(), p.x));
                }
                None
            }
            (Tool::Razor, Hit::Clip { clip, .. }) => {
                if resp.clicked() || resp.drag_started() {
                    let tt = snap(app, seq, layout, rate.snap_nearest(t), &[]);
                    let r = if mods.shift {
                        app.session.execute("timeline.razor", json!({"time": tt.0}))
                    } else {
                        app.session.execute("timeline.razor", json!({"time": tt.0, "clip": clip.0}))
                    };
                    if let Err(e) = r {
                        app.ui.status = e.to_string();
                    }
                }
                None
            }
            (Tool::TrackSelectForward | Tool::TrackSelectBackward, Hit::Clip { track, .. } | Hit::Empty { track }) => {
                let fwd = tool == Tool::TrackSelectForward;
                let ids: Vec<u64> = seq
                    .all_tracks()
                    .filter(|tr| !mods.shift || tr.id == track)
                    .flat_map(|tr| tr.items.iter())
                    .filter(|i| if fwd { i.end() > t } else { i.start < t })
                    .map(|i| i.id.0)
                    .collect();
                let _ = app.session.execute("timeline.select", json!({"clips": ids}));
                None
            }
            (Tool::Selection | Tool::Ripple | Tool::Rolling | Tool::RateStretch, Hit::Clip { clip, edge: Some(edge), track }) => {
                let tr = seq.track(track).expect("track");
                let mode = if tool == Tool::Ripple || (tool == Tool::Selection && mods.command) {
                    filmcraft_edit::TrimMode::Ripple
                } else {
                    filmcraft_edit::TrimMode::Regular
                };
                if tool == Tool::Rolling || (tool == Tool::Selection && mods.command && mods.shift) {
                    // roll the cut between this and its neighbour
                    let it = tr.item(clip).expect("clip");
                    let (l, r) = match edge {
                        filmcraft_edit::Edge::Out => (Some(clip), tr.items.iter().find(|x| x.start == it.end()).map(|x| x.id)),
                        filmcraft_edit::Edge::In => (tr.items.iter().find(|x| x.end() == it.start).map(|x| x.id), Some(clip)),
                    };
                    match (l, r) {
                        (Some(left), Some(right)) => Some(Drag::Roll { left, right, delta: Tick::ZERO }),
                        _ => Some(Drag::Trim { clip, edge, mode, delta: Tick::ZERO }),
                    }
                } else if tool == Tool::RateStretch {
                    Some(Drag::Stretch { clip, edge, delta: Tick::ZERO })
                } else {
                    Some(Drag::Trim { clip, edge, mode, delta: Tick::ZERO })
                }
            }
            (Tool::Slip, Hit::Clip { clip, .. }) => Some(Drag::Slip { clip, delta: Tick::ZERO }),
            (Tool::Slide, Hit::Clip { clip, .. }) => Some(Drag::Slide { clip, delta: Tick::ZERO }),
            (_, Hit::Clip { clip, track, .. }) => {
                // select (shift toggles; alt selects one side of a link)
                let sel = &app.session.state.selection;
                if mods.shift {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0], "toggle": true}));
                } else if !sel.contains(&clip) {
                    if mods.alt {
                        app.session.state.selection = vec![clip];
                    } else {
                        let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                    }
                }
                if resp.drag_started() {
                    let clips = app.session.state.selection.clone();
                    Some(Drag::Move { clips, grab_tick: t, start_track: track, offset: Tick::ZERO, track_delta: 0 })
                } else {
                    None
                }
            }
            (_, Hit::Transition { .. }) => None,
            (_, Hit::Empty { .. }) => {
                if resp.drag_started() {
                    Some(Drag::Marquee { start: p })
                } else {
                    app.session.state.selection.clear();
                    None
                }
            }
            (_, Hit::None) => None,
        };
        if let Some(d) = started {
            if matches!(d, Drag::Scrub) {
                app.stop();
                let tt = rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO));
                app.session.set_playhead(tt);
            }
            if resp.drag_started() {
                app.tl.drag = Some(d);
            }
        }
    }

    // ---- drag
    if resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(d) = app.tl.drag.clone()
    {
        let t_here = layout.tick_at(p.x);
        let new = match d {
            Drag::Scrub => {
                let mut tt = rate.snap_nearest(t_here.max(Tick::ZERO));
                if mods.shift {
                    tt = snap(app, seq, layout, tt, &[]);
                }
                app.session.set_playhead(tt);
                Some(Drag::Scrub)
            }
            Drag::Pan { last } => {
                let dx = p.x - last.x;
                let v = &mut app.ui.timeline;
                v.target_scroll = (v.target_scroll - dx as f64 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
                app.ui.timeline.v_scroll += p.y - last.y;
                Some(Drag::Pan { last: p })
            }
            Drag::Move { clips, grab_tick, start_track, .. } => {
                let raw = rate.snap_nearest(t_here - grab_tick);
                // snap the moved block's start or end
                let first = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.start)).min().unwrap_or_default();
                let last = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.end())).max().unwrap_or_default();
                let s1 = snap(app, seq, layout, first + raw, &clips) - first;
                let offset = if s1 != raw { s1 } else { snap(app, seq, layout, last + raw, &clips) - last };
                let offset = offset.max(-first);
                let cur_row = layout.row_at(p.y);
                let start_row = layout.rows.iter().find(|r| r.track == start_track);
                let track_delta = match (cur_row, start_row) {
                    (Some(c), Some(s)) if c.kind == s.kind => c.index as i32 - s.index as i32,
                    _ => 0,
                };
                Some(Drag::Move { clips, grab_tick, start_track, offset, track_delta })
            }
            Drag::Trim { clip, edge, mode, .. } => {
                let (_, it) = seq.find_item(clip).expect("clip");
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[clip]);
                let delta = target - base;
                Some(Drag::Trim { clip, edge, mode, delta })
            }
            Drag::Stretch { clip, edge, .. } => {
                let (_, it) = seq.find_item(clip).expect("clip");
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[clip]);
                Some(Drag::Stretch { clip, edge, delta: target - base })
            }
            Drag::Roll { left, right, .. } => {
                let (_, l) = seq.find_item(left).expect("clip");
                let target = snap(app, seq, layout, rate.snap_nearest(t_here), &[left, right]);
                Some(Drag::Roll { left, right, delta: target - l.end() })
            }
            Drag::Slip { clip, .. } => {
                let start = resp.interact_pointer_pos().map(|_| ()).and(ctx.input(|i| i.pointer.press_origin()));
                let origin = start.map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slip { clip, delta: -(rate.snap_nearest(t_here - origin)) })
            }
            Drag::Slide { clip, .. } => {
                let origin = ctx.input(|i| i.pointer.press_origin()).map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slide { clip, delta: rate.snap_nearest(t_here - origin) })
            }
            Drag::Marquee { start } => {
                let r = Rect::from_two_pos(start, p);
                ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(18));
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(140)), StrokeKind::Inside);
                Some(Drag::Marquee { start })
            }
            other => Some(other),
        };
        app.tl.drag = new;
        // auto-scroll when dragging near the edges
        if !matches!(app.tl.drag, Some(Drag::Pan { .. }) | Some(Drag::ZoomBar { .. })) {
            let v = &mut app.ui.timeline;
            if p.x > layout.content.max.x - 20.0 {
                v.target_scroll += 6.0 / v.pps;
                v.scroll = v.target_scroll;
            } else if p.x < layout.content.min.x + 10.0 && v.scroll > 0.0 {
                v.target_scroll = (v.target_scroll - 6.0 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        }
    }

    // ---- release: commit exactly one command
    if resp.drag_stopped()
        && let Some(d) = app.tl.drag.take()
    {
        let r = match d {
            Drag::Move { clips, offset, track_delta, .. } if offset != Tick::ZERO || track_delta != 0 => {
                let moves: Vec<Value> = clips
                    .iter()
                    .filter_map(|c| seq.find_item(*c).map(|(tid, it)| json!({"clip": c.0, "track": shift_track(seq, tid, track_delta).unwrap_or(tid).0, "time": (it.start + offset).max(Tick::ZERO).0})))
                    .collect();
                Some(app.session.execute("timeline.move", json!({"moves": moves, "insert": mods.command})))
            }
            Drag::Trim { clip, edge, mode, delta } if delta != Tick::ZERO => Some(app.session.execute(
                "timeline.trim",
                json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "mode": if mode == filmcraft_edit::TrimMode::Ripple {"ripple"} else {"regular"}, "delta": delta.0}),
            )),
            Drag::Stretch { clip, edge, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.rateStretch", json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "delta": delta.0}))),
            Drag::Roll { left, right, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.roll", json!({"left": left.0, "right": right.0, "delta": delta.0}))),
            Drag::Slip { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slip", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Slide { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slide", json!({"clip": clip.0, "delta": delta.0}))),
            Drag::Marquee { start } => {
                if let Some(end) = resp.interact_pointer_pos() {
                    let r = Rect::from_two_pos(start, end);
                    let (a, b) = (layout.tick_at(r.min.x), layout.tick_at(r.max.x));
                    let ids: Vec<u64> = layout
                        .rows
                        .iter()
                        .filter(|row| row.rect.intersects(r))
                        .filter_map(|row| seq.track(row.track))
                        .flat_map(|tr| tr.items.iter().filter(|i| i.start < b && i.end() > a).map(|i| i.id.0))
                        .collect();
                    Some(app.session.execute("timeline.select", json!({"clips": ids, "add": mods.shift})))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(Err(e)) = r {
            app.ui.status = e.to_string();
        }
    }

    // ---- context menu on clips
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        let sel_n = app.session.state.selection.len();
        let items: [(&str, &str); 10] = [
            ("Cut", "edit.cut"),
            ("Copy", "edit.copy"),
            ("Clear", "edit.clear"),
            ("Ripple Delete", "edit.rippleDelete"),
            ("Enable", "clip.enable"),
            ("Unlink", "clip.link"),
            ("Group", "clip.group"),
            ("Speed/Duration…", "clip.speedDuration"),
            ("Nest…", "clip.nest"),
            ("Scale to Frame Size", "clip.scaleToFrameSize"),
        ];
        for (label, cmd) in items {
            if ui.add_enabled(sel_n > 0 && app.session.is_enabled(cmd), egui::Button::new(label)).clicked() {
                if cmd == "clip.speedDuration" {
                    app.dialog = None;
                    let _ = app.session.execute(cmd, json!({"speed": 50.0}));
                } else if let Err(e) = app.session.execute(cmd, json!({})) {
                    app.ui.status = e.to_string();
                }
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button("Label", |ui| {
            for l in filmcraft_project::Label::ALL {
                if ui.button(l.name()).clicked() {
                    let _ = app.session.execute("edit.label", json!({"label": l.name()}));
                    ui.close();
                }
            }
        });
    });

    // ---- drops: project items and effects
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
    {
        let row = layout.row_at(p.y).cloned();
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let dur = app.session.project.item(item).map(|i| i.duration()).filter(|d| d.0 > 0).unwrap_or(app.session.project.settings.default_still_duration);
        if let Some(row) = &row {
            let r = Rect::from_min_max(pos2(layout.x_of(t), row.rect.min.y + 1.0), pos2(layout.x_of(t + dur), row.rect.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            if mods.command {
                ui.painter().text(r.left_top() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, "Insert", Tokens::ui(10.0), Color32::WHITE);
            }
        }
        if ctx.input(|i| i.pointer.any_released())
            && let Some(row) = row
        {
            let (vt, at) = match row.kind {
                TrackKind::Video => (Some(row.track.0), seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0)),
                TrackKind::Audio => (seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0), Some(row.track.0)),
            };
            let r = app.session.execute("timeline.place", json!({"item": item.0, "track": vt, "audioTrack": at, "time": t.0, "insert": mods.command}));
            if let Err(e) = r {
                app.ui.status = e.to_string();
            }
            crate::panels::clear_drag(ui);
        }
    }
    if let Some(effect) = crate::panels::dragged_effect(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
        && let Hit::Clip { clip, .. } = hit(seq, layout, p)
        && let Some((tid, it)) = seq.find_item(clip)
        && let Some(row) = layout.rows.iter().find(|r| r.track == tid)
    {
        {
            let r = Rect::from_min_max(pos2(layout.x_of(it.start), row.rect.min.y), pos2(layout.x_of(it.end()), row.rect.max.y));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
            if ctx.input(|i| i.pointer.any_released()) {
                let is_transition = filmcraft_project::find_effect(&effect)
                    .is_some_and(|d| matches!(d.kind, filmcraft_project::EffectKind::VideoTransition | filmcraft_project::EffectKind::AudioTransition));
                let r = if is_transition {
                    let edge = if p.x - layout.x_of(it.start) < layout.x_of(it.end()) - p.x { "in" } else { "out" };
                    app.session.execute("effects.apply", json!({"effect": effect, "clip": clip.0, "edge": edge}))
                } else {
                    app.session.execute("effects.apply", json!({"effect": effect, "clips": [clip.0]}))
                };
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                } else {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                }
                crate::panels::clear_drag(ui);
            }
        }
    }
    app.auto.add("timeline.tracks", layout.content, "tracks");
}
