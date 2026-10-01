//! Source and Program monitors.
//!
//! Frames come from the background [`FrameServer`](crate::frames::FrameServer) at the *smaller* of
//! the playback resolution and the on-screen resolution (so a small monitor never pays for 4K).
//! While playing we prefetch the next frames in priority order; while scrubbing the exact frame is
//! requested first and the nearest cached frame is shown until it arrives (no black flashes).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::ItemKind;
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::json;

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::state::PlaybackRes;
use crate::theme::Tokens;

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Source,
    Program,
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, which: Which) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let controls_h = 28.0 + 24.0 + 36.0;
    let video_area = Rect::from_min_max(rect.min + vec2(4.0, 4.0), pos2(rect.max.x - 4.0, rect.max.y - controls_h));
    let (target, frame_size, rate, time, duration, drop_frame, mark_in, mark_out, name) = match which {
        Which::Program => {
            let Some(seq_id) = app.session.state.active_sequence else {
                crate::dock::placeholder(ui, rect, &t, "(no sequences)");
                return;
            };
            let q = app.session.active_sequence().expect("active");
            (
                Target::Sequence(seq_id),
                (q.settings.width, q.settings.height),
                q.settings.frame_rate,
                app.session.playhead(),
                q.duration(),
                q.settings.drop_frame,
                q.mark_in,
                q.mark_out,
                app.session.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default(),
            )
        }
        Which::Source => {
            let Some(item) = app.session.state.source_item else {
                crate::dock::placeholder(ui, rect, &t, "(no clips)");
                return;
            };
            let Some(pi) = app.session.project.item(item) else { return };
            let size = match &pi.kind {
                ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)).unwrap_or((0, 0)),
                ItemKind::Sequence(s) => (s.settings.width, s.settings.height),
                _ => (1920, 1080),
            };
            let (mi, mo) = match &pi.kind {
                ItemKind::Media(m) => (m.mark_in, m.mark_out),
                ItemKind::Sequence(s) => (s.mark_in, s.mark_out),
                _ => (None, None),
            };
            (Target::Item(item), size, pi.frame_rate(), app.session.state.source_playhead, pi.duration(), false, mi, mo, pi.name.clone())
        }
    };
    let prefix = if which == Which::Program { "program" } else { "source" };
    // Multi-Camera view: the angle grid on the left, the program on the right
    let multicam = which == Which::Program && app.ui.program.multicam;
    let (grid_area, video_area) = if multicam {
        let mid = video_area.center().x;
        (Some(Rect::from_min_max(video_area.min, pos2(mid - 2.0, video_area.max.y))), Rect::from_min_max(pos2(mid + 2.0, video_area.min.y), video_area.max))
    } else {
        (None, video_area)
    };
    if let Some(g) = grid_area {
        crate::panels::multicam::grid(app, ui, g);
    }
    ui.painter().rect_filled(video_area, 0.0, t.panel_bg);
    // ---- picture
    let has_video = frame_size.0 > 0;
    let pic = if has_video { fit(video_area, frame_size.0 as f32, frame_size.1 as f32) } else { video_area };
    ui.painter().rect_filled(pic, 0.0, t.monitor_bg);
    let res = if which == Which::Program { app.ui.program.res } else { app.ui.source.res };
    if has_video {
        let ppp = ctx.pixels_per_point();
        let screen_scale = (pic.width() * ppp / frame_size.0 as f32).min(1.0);
        let scale = quantize_scale(res.scale().min(screen_scale.max(1.0 / 32.0)));
        let frame = rate.frame_at(time);
        let rev = match target {
            Target::Item(i) => app.item_revision(i),
            _ => app.session.revision,
        };
        let size_key = (scale * 1000.0) as u32;
        let use_gpu = which == Which::Program && app.gpu.is_some();
        let target = match (use_gpu, target) {
            (true, Target::Sequence(s)) => Target::SequencePlan(s),
            (_, t) => t,
        };
        let key = FrameKey { target, frame, size: size_key, revision: rev };
        let project = app.session.project.clone();
        let playing = which == Which::Program && app.playback.playing;
        if playing {
            let preroll = app.playback.preroll.is_some();
            app.frames.schedule_playback(key, rate, scale, &project, app.playback.speed, preroll);
            if preroll {
                app.playback.preroll_ready = app.frames.preroll_ready(key, app.playback.speed, rate.frame_at(duration) - 1);
            }
        } else {
            app.frames.request(key, rate.tick_of(frame), scale, &project, 0);
        }
        let tex_name = format!("monitor-{prefix}");
        let (shown, exact) = if use_gpu {
            let exact = app.frames.get_plan(&key).map(|p| (key, p));
            let is_exact = exact.is_some();
            let tex = match exact.or_else(|| app.frames.nearest_plan(key, 6)) {
                Some((k, plan)) => app.gpu_present(k, &plan).map(|(id, _)| id),
                None => app.gpu.as_ref().and_then(|g| g.texture),
            };
            (tex, is_exact)
        } else if let Some(img) = app.frames.get(&key) {
            (Some(app.texture_for(&ctx, &tex_name, key, &img)), true)
        } else {
            let tex = match app.frames.nearest(target, frame, size_key, rev, 6) {
                Some(img) => Some(app.texture_for(&ctx, &tex_name, FrameKey { frame: frame - 1, ..key }, &img)),
                None => app.texture_existing(&tex_name).map(|(id, _)| id),
            };
            (tex, false)
        };
        if playing {
            if std::mem::take(&mut app.playback.hidden) {
                app.playback.meter.resync(frame, exact);
            } else {
                app.playback.meter.refresh(frame, exact);
            }
        }
        if let Some(tex) = shown {
            ui.painter().image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        let mv = if which == Which::Program { &app.ui.program } else { &app.ui.source };
        if mv.safe_margins {
            for (f, c) in [(0.9, Color32::from_white_alpha(120)), (0.8, Color32::from_white_alpha(90))] {
                let r = Rect::from_center_size(pic.center(), pic.size() * f);
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, c), StrokeKind::Middle);
            }
            let c = pic.center();
            ui.painter().line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], Stroke::new(1.0, Color32::from_white_alpha(120)));
            ui.painter().line_segment([c - vec2(0.0, 8.0), c + vec2(0.0, 8.0)], Stroke::new(1.0, Color32::from_white_alpha(120)));
        }
    } else if which == Which::Source {
        // audio-only source: draw a waveform placeholder
        ui.painter().text(pic.center(), Align2::CENTER_CENTER, format!("♪  {name}"), Tokens::ui(14.0), t.text_dim);
    }
    // Dropped-frame indicator (Program only)
    if which == Which::Program && app.playback.playing {
        let c = if app.playback.meter.counts().1 > 0 { t.render_yellow } else { t.render_green };
        ui.painter().circle_filled(pos2(video_area.min.x + 10.0, video_area.min.y + 10.0), 4.0, c);
    }
    // Click/drag in the picture: the Hand tool pans, otherwise focus.
    let pic_resp = ui.interact(pic, egui::Id::new((prefix, "pic")), Sense::click());
    app.auto.add(&format!("{prefix}.picture"), pic, "picture");
    if which == Which::Program && has_video {
        crate::panels::graphics::monitor_overlay(app, ui, pic, frame_size);
        crate::panels::masks::monitor_overlay(app, ui, pic, frame_size);
    }
    if pic_resp.double_clicked() && which == Which::Source {
        // (Premiere opens the clip's settings; we show info)
        app.ui.status = name.clone();
    }

    // ---- controls row: timecode | zoom | res | wrench | duration
    let row1 = Rect::from_min_size(pos2(rect.min.x + 14.0, video_area.max.y + 2.0), vec2(rect.width() - 28.0, 26.0));
    let tc = format_time(time, rate, drop_frame, TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(row1.min.x, row1.center().y), Align2::LEFT_CENTER, &tc, Tokens::semibold(15.0), t.hot_text);
    app.auto.add(&format!("{prefix}.timecode"), Rect::from_min_size(row1.min, vec2(110.0, row1.height())), &tc);
    let dur_tc = format_time(
        mark_out.map(|o| o + rate.frame_duration()).unwrap_or(duration) - mark_in.unwrap_or(Tick::ZERO),
        rate,
        drop_frame,
        TimeDisplay::Timecode,
        48000,
    );
    ui.painter().text(pos2(row1.max.x, row1.center().y), Align2::RIGHT_CENTER, &dur_tc, Tokens::semibold(15.0), t.text_dim);
    // zoom + resolution dropdowns centred-ish
    let zr = Rect::from_min_size(pos2(row1.min.x + 116.0, row1.min.y), vec2(70.0, 24.0));
    let zoom_label = match if which == Which::Program { app.ui.program.zoom } else { app.ui.source.zoom } {
        None => "Fit".to_string(),
        Some(z) => format!("{}%", (z * 100.0) as i32),
    };
    crate::widgets::dropdown_text(ui, zr, &zoom_label, &t, egui::Id::new((prefix, "zoom")));
    let rr = Rect::from_min_size(pos2(row1.max.x - 196.0, row1.min.y), vec2(62.0, 24.0));
    let rresp = crate::widgets::dropdown_text(ui, rr, res.label(), &t, egui::Id::new((prefix, "res")));
    app.auto.add(&format!("{prefix}.resolution"), rr, "Select Playback Resolution");
    egui::Popup::menu(&rresp).show(|ui| {
        for r in PlaybackRes::ALL {
            if ui.selectable_label(r == res, r.label()).clicked() {
                if which == Which::Program {
                    app.ui.program.res = r;
                } else {
                    app.ui.source.res = r;
                }
            }
        }
    });
    let wr = Rect::from_min_size(pos2(rr.max.x + 6.0, row1.min.y + 1.0), vec2(22.0, 22.0));
    let wresp = ui.interact(wr, egui::Id::new((prefix, "wrench")), Sense::click());
    icons::paint(ui.painter(), wr.shrink(4.0), Icon::Wrench, if wresp.hovered() { t.tab_text_active } else { t.icon });
    app.auto.add(&format!("{prefix}.settings"), wr, "Settings");
    egui::Popup::menu(&wresp).show(|ui| {
        let mv = if which == Which::Program { &mut app.ui.program } else { &mut app.ui.source };
        ui.checkbox(&mut mv.safe_margins, "Safe Margins");
        ui.checkbox(&mut mv.show_transport, "Show Transport Controls");
        if which == Which::Program {
            ui.separator();
            ui.checkbox(&mut app.ui.show_scopes, "Lumetri Scopes");
            ui.checkbox(&mut app.ui.program.multicam, "Multi-Camera");
            ui.checkbox(&mut app.playback.looping, "Loop");
            ui.separator();
            let mut follows = app.session.state.multicam_audio_follows_video;
            if ui.checkbox(&mut follows, "Multi-Camera Audio Follows Video").changed() {
                let _ = app.session.execute("multicam.audioFollowsVideo", json!({"enabled": follows}));
            }
            ui.checkbox(&mut app.ui.multicam_record, "Multi-Camera Record");
        }
    });

    // ---- mini timeline / scrub bar
    let bar = Rect::from_min_size(pos2(rect.min.x + 14.0, row1.max.y + 2.0), vec2(rect.width() - 28.0, 22.0));
    mini_timeline(app, ui, bar, which, time, duration, rate, mark_in, mark_out);

    // ---- transport buttons
    let row3 = Rect::from_min_size(pos2(rect.min.x, bar.max.y + 4.0), vec2(rect.width(), 32.0));
    transport(app, ui, row3, which);
}

pub fn quantize_scale(s: f32) -> f32 {
    // Buckets keep the cache effective while resizing the panel.
    let buckets = [1.0 / 32.0, 1.0 / 16.0, 1.0 / 8.0, 0.1875, 0.25, 0.375, 0.5, 0.75, 1.0];
    *buckets.iter().find(|b| **b >= s - 1e-4).unwrap_or(&1.0)
}

pub fn fit(area: Rect, w: f32, h: f32) -> Rect {
    let s = (area.width() / w).min(area.height() / h);
    Rect::from_center_size(area.center(), vec2(w * s, h * s))
}

#[allow(clippy::too_many_arguments)]
fn mini_timeline(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    bar: Rect,
    which: Which,
    time: Tick,
    duration: Tick,
    rate: filmcraft_time::FrameRate,
    mark_in: Option<Tick>,
    mark_out: Option<Tick>,
) {
    let t = app.tokens;
    let p = ui.painter();
    let dur = duration.0.max(1) as f64;
    let xof = |tk: Tick| bar.min.x + ((tk.0 as f64 / dur) as f32).clamp(0.0, 1.0) * bar.width();
    // ticks: minor 4 pt, major 10 pt, ~20 pt spacing
    let n = ((bar.width() / 20.0) as i32).max(2);
    for i in 0..=n {
        let x = bar.min.x + bar.width() * i as f32 / n as f32;
        let h = if i % 5 == 0 { 8.0 } else { 3.5 };
        p.line_segment([pos2(x, bar.max.y - h), pos2(x, bar.max.y)], Stroke::new(1.0, t.text_faint));
    }
    if mark_in.is_some() || mark_out.is_some() {
        let a = xof(mark_in.unwrap_or(Tick::ZERO));
        let b = xof(mark_out.map(|o| o + rate.frame_duration()).unwrap_or(duration));
        p.rect_filled(Rect::from_min_max(pos2(a, bar.min.y + 8.0), pos2(b, bar.max.y)), 0.0, Color32::from_rgb(0x5c, 0x5c, 0x5c));
    }
    if which == Which::Program
        && let Some(q) = app.session.active_sequence()
    {
        for m in &q.markers {
            let x = xof(m.start);
            let c = m.color.marker_rgb();
            let c = Color32::from_rgb(c[0], c[1], c[2]);
            let y = bar.min.y;
            p.add(egui::Shape::convex_polygon(
                vec![pos2(x - 3.5, y), pos2(x + 3.5, y), pos2(x + 3.5, y + 7.0), pos2(x, y + 10.0), pos2(x - 3.5, y + 7.0)],
                c,
                Stroke::NONE,
            ));
        }
    }
    let x = xof(time);
    // playhead: blue triangle over a line
    let hy = bar.max.y - 11.0;
    p.add(egui::Shape::convex_polygon(
        vec![pos2(x - 5.5, hy), pos2(x + 5.5, hy), pos2(x + 5.5, hy + 5.0), pos2(x, hy + 9.0), pos2(x - 5.5, hy + 5.0)],
        t.playhead,
        Stroke::NONE,
    ));
    p.line_segment([pos2(x, hy + 8.0), pos2(x, bar.max.y)], Stroke::new(1.0, t.playhead));
    let resp = ui.interact(bar, egui::Id::new((which as u8, "scrub")), Sense::click_and_drag());
    app.auto.add(if which == Which::Program { "program.scrubBar" } else { "source.scrubBar" }, bar, "scrub bar");
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - bar.min.x) / bar.width()).clamp(0.0, 1.0) as f64;
        let tk = rate.snap(Tick((f * dur) as i64));
        match which {
            Which::Program => {
                app.stop();
                app.session.set_playhead(tk);
            }
            Which::Source => {
                let _ = app.session.execute("source.setPlayhead", json!({"time": tk.0}));
            }
        }
    }
}

fn transport(app: &mut FilmcraftApp, ui: &mut egui::Ui, row: Rect, which: Which) {
    let t = app.tokens;
    let src = which == Which::Source;
    let buttons: Vec<(Icon, &str, &str)> = if src {
        vec![
            (Icon::Marker, "markers.add", "Add Marker (M)"),
            (Icon::MarkIn, "src.markIn", "Mark In (I)"),
            (Icon::MarkOut, "src.markOut", "Mark Out (O)"),
            (Icon::GoToIn, "src.goIn", "Go to In (Shift+I)"),
            (Icon::StepBack, "src.stepBack", "Step Back 1 Frame (Left)"),
            (Icon::Play, "src.play", "Play-Stop Toggle (Space)"),
            (Icon::StepFwd, "src.stepFwd", "Step Forward 1 Frame (Right)"),
            (Icon::GoToOut, "src.goOut", "Go to Out (Shift+O)"),
            (Icon::Insert, "source.insert", "Insert (,)"),
            (Icon::Overwrite, "source.overwrite", "Overwrite (.)"),
            (Icon::Camera, "exportFrame", "Export Frame (Shift+E)"),
            (Icon::Proxy, "media.toggleProxies", "Toggle Proxies"),
        ]
    } else {
        vec![
            (Icon::Marker, "markers.add", "Add Marker (M)"),
            (Icon::MarkIn, "markers.markIn", "Mark In (I)"),
            (Icon::MarkOut, "markers.markOut", "Mark Out (O)"),
            (Icon::GoToIn, "markers.goToIn", "Go to In (Shift+I)"),
            (Icon::StepBack, "playhead.stepBack", "Step Back 1 Frame (Left)"),
            (if app.playback.playing { Icon::Pause } else { Icon::Play }, "playback.toggle", "Play-Stop Toggle (Space)"),
            (Icon::StepFwd, "playhead.stepForward", "Step Forward 1 Frame (Right)"),
            (Icon::GoToOut, "markers.goToOut", "Go to Out (Shift+O)"),
            (Icon::Lift, "sequence.lift", "Lift (;)"),
            (Icon::Extract, "sequence.extract", "Extract (')"),
            (Icon::Camera, "exportFrame", "Export Frame (Shift+E)"),
            (Icon::Proxy, "media.toggleProxies", "Toggle Proxies"),
        ]
    };
    let bw = 30.0;
    let total = buttons.len() as f32 * bw;
    let mut x = row.center().x - total / 2.0;
    let ctx = ui.ctx().clone();
    let prefix = if src { "source" } else { "program" };
    for (icon, cmd, tip) in buttons {
        let r = Rect::from_min_size(pos2(x, row.min.y + 2.0), vec2(bw - 2.0, 26.0));
        let resp = ui.interact(r, egui::Id::new((prefix, cmd)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("{prefix}.transport.{cmd}"), r, tip);
        let is_play = cmd.ends_with("play") || cmd == "playback.toggle";
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        let sz = if is_play { 16.0 } else { 14.0 };
        let on = cmd == "media.toggleProxies" && app.session.media.use_proxies();
        let col = if on {
            t.accent
        } else if resp.hovered() {
            t.tab_text_active
        } else {
            t.icon
        };
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(sz, sz)), icon, col);
        if resp.clicked() {
            let r = match cmd {
                "src.markIn" => app.session.execute("markers.markIn", json!({"target": "source"})).map_err(|e| e.to_string()),
                "src.markOut" => app.session.execute("markers.markOut", json!({"target": "source"})).map_err(|e| e.to_string()),
                "src.goIn" | "src.goOut" | "src.stepBack" | "src.stepFwd" => {
                    source_nav(app, cmd);
                    Ok(serde_json::Value::Null)
                }
                "src.play" => Ok(serde_json::Value::Null),
                "exportFrame" => {
                    app.ui.status = "Export Frame: use Export mode (M6)".into();
                    Ok(serde_json::Value::Null)
                }
                c => crate::menus::invoke(app, &ctx, c, json!({})),
            };
            if let Err(e) = r {
                app.ui.status = e;
            }
        }
        x += bw;
    }
    // button editor "+"
    let r = Rect::from_min_size(pos2(row.max.x - 30.0, row.min.y + 4.0), vec2(22.0, 22.0));
    let resp = ui.interact(r, egui::Id::new((prefix, "btn-editor")), Sense::click()).on_hover_text("Button Editor");
    icons::paint(ui.painter(), r.shrink(5.0), Icon::Plus, if resp.hovered() { t.tab_text_active } else { t.text_dim });
}

fn source_nav(app: &mut FilmcraftApp, cmd: &str) {
    let Some(item) = app.session.state.source_item else { return };
    let Some(pi) = app.session.project.item(item) else { return };
    let rate = pi.frame_rate();
    let (mi, mo) = match &pi.kind {
        ItemKind::Media(m) => (m.mark_in, m.mark_out),
        _ => (None, None),
    };
    let cur = app.session.state.source_playhead;
    let t = match cmd {
        "src.goIn" => mi.unwrap_or(Tick::ZERO),
        "src.goOut" => mo.unwrap_or(pi.duration() - rate.frame_duration()),
        "src.stepBack" => cur - rate.frame_duration(),
        _ => cur + rate.frame_duration(),
    };
    let _ = app.session.execute("source.setPlayhead", json!({"time": t.0}));
}
