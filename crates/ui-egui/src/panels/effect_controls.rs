//! Effect Controls: the selected clip's effects (fixed Motion/Opacity/Time Remapping first, as in
//! Premiere, then standard effects), generated from parameter schemas, with stopwatches and a
//! keyframe lane on the right. Also hosts the Lumetri Color panel body (same editor, grouped).

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ClipId, EffectInstance, ParamKind, ParamValue, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;

fn selected_clip(app: &FilmcraftApp) -> Option<(ClipId, TrackItem, filmcraft_project::TrackKind)> {
    let seq = app.session.active_sequence()?;
    let mut best = None;
    for c in &app.session.state.selection {
        if let Some((tid, it)) = seq.find_item(*c) {
            let kind = seq.track(tid)?.kind;
            if kind == filmcraft_project::TrackKind::Video {
                return Some((*c, it.clone(), kind));
            }
            best.get_or_insert((*c, it.clone(), kind));
        }
    }
    best
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it, kind)) = selected_clip(app) else {
        crate::dock::placeholder(ui, rect, &t, "(no clip selected)");
        return;
    };
    let seq = app.session.active_sequence().expect("seq").clone();
    let split = rect.min.x + (rect.width() * 0.58).max(260.0).min(rect.width() - 60.0);
    let head = Rect::from_min_size(rect.min + vec2(8.0, 2.0), vec2(split - rect.min.x - 8.0, 24.0));
    ui.painter().text(pos2(head.min.x, head.center().y), Align2::LEFT_CENTER, format!("Source · {}", it.name), Tokens::ui(11.5), t.text_dim);
    ui.painter().text(pos2(split - 6.0, head.center().y), Align2::RIGHT_CENTER, format!("{} · {}", seq_name(app), it.name), Tokens::ui(11.5), t.text);
    // keyframe lane header: mini ruler over the clip's duration
    let lane = Rect::from_min_max(pos2(split + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 6.0, rect.max.y - 26.0));
    ui.painter().rect_filled(lane, 0.0, t.tl_bg);
    let ph = app.session.playhead();
    let dur = it.duration.0.max(1) as f64;
    let lx = |tk: Tick| -> f32 { lane.min.x + (((tk - it.start).0 as f64 / dur) as f32).clamp(0.0, 1.0) * lane.width() };
    ui.painter().rect_filled(Rect::from_min_max(pos2(lane.min.x, lane.min.y + 2.0), pos2(lane.max.x, lane.min.y + 16.0)), 2.0, Color32::from_rgb(58, 58, 70));
    ui.painter().text(pos2(lane.min.x + 4.0, lane.min.y + 9.0), Align2::LEFT_CENTER, &it.name, Tokens::ui(10.0), t.text);
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y + 4.0), pos2(split, rect.max.y - 26.0));
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("ec-body"));
    bui.set_clip_rect(Rect::from_min_max(body.min, pos2(rect.max.x, body.max.y)));
    let mt_now = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let heading = if kind == filmcraft_project::TrackKind::Video { "Video" } else { "Audio" };
    bui.painter().text(pos2(body.min.x + 8.0, body.min.y + 8.0), Align2::LEFT_CENTER, heading, Tokens::semibold(11.5), t.text_dim);
    bui.add_space(18.0);
    for (idx, e) in it.effects.iter().enumerate() {
        let Some(def) = e.def() else { continue };
        let key = format!("{}:{}", clip.0, idx);
        let open = !app.ui.collapsed_fx.contains(&key);
        let (r, resp) = bui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::click());
        if resp.hovered() {
            bui.painter().rect_filled(r, 0.0, t.hover);
        }
        icons::paint(
            bui.painter(),
            Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(10.0, 10.0)),
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            t.text_dim,
        );
        // fx enable toggle
        let fxr = Rect::from_center_size(pos2(r.min.x + 28.0, r.center().y), vec2(18.0, 14.0));
        let fxresp = bui.interact(fxr, egui::Id::new(("fxen", clip.0, idx)), Sense::click());
        bui.painter().text(fxr.center(), Align2::CENTER_CENTER, "fx", Tokens::semibold(10.5), if e.enabled { t.text } else { t.text_faint });
        if !e.enabled {
            bui.painter().line_segment([fxr.left_bottom(), fxr.right_top()], Stroke::new(1.0, t.text_faint));
        }
        if fxresp.clicked() {
            actions.push(("effects.toggleEnabled".into(), json!({"clip": clip.0, "index": idx})));
        }
        bui.painter().text(pos2(r.min.x + 42.0, r.center().y), Align2::LEFT_CENTER, def.name, Tokens::ui(12.0), t.text);
        // reset button
        let rr = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(16.0, 16.0));
        let rresp = bui.interact(rr, egui::Id::new(("fxreset", clip.0, idx)), Sense::click()).on_hover_text("Reset Effect");
        icons::paint(bui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
        if rresp.clicked() {
            actions.push(("effects.reset".into(), json!({"clip": clip.0, "index": idx})));
        }
        app.auto.add(&format!("effectControls.effect.{}", e.effect), r, def.name);
        if resp.clicked() && !fxresp.clicked() && !rresp.clicked() {
            if open {
                app.ui.collapsed_fx.push(key.clone());
            } else {
                app.ui.collapsed_fx.retain(|k| *k != key);
            }
        }
        resp.context_menu(|ui| {
            if !def.intrinsic && ui.button("Clear").clicked() {
                actions.push(("effects.remove".into(), json!({"clip": clip.0, "index": idx})));
                ui.close();
            }
        });
        if !open {
            continue;
        }
        for pd in &def.params {
            param_row(app, &mut bui, body, clip, idx, e, pd, mt_now, &mut actions, &lane, &lx, &it);
        }
    }
    // playhead in lane
    let px = lx(ph);
    ui.painter().line_segment([pos2(px, lane.min.y), pos2(px, lane.max.y)], Stroke::new(1.0, t.playhead));
    // footer timecode
    let tc = filmcraft_time::format_time(ph, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(rect.min.x + 10.0, rect.max.y - 13.0), Align2::LEFT_CENTER, tc, Tokens::mono(13.0), t.timecode);
    for (cmd, p) in actions {
        if let Err(e) = app.session.execute(&cmd, p) {
            app.ui.status = e.to_string();
        }
    }
}

fn seq_name(app: &FilmcraftApp) -> String {
    app.session.state.active_sequence.and_then(|s| app.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn param_row(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    e: &EffectInstance,
    pd: &filmcraft_project::ParamDef,
    mt: Tick,
    actions: &mut Vec<(String, Value)>,
    lane: &Rect,
    lx: &dyn Fn(Tick) -> f32,
    it: &TrackItem,
) {
    let _ = lx;
    let t = app.tokens;
    let Some(param) = e.params.get(pd.id) else { return };
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::hover());
    let mut x = r.min.x + 26.0;
    if pd.animatable {
        let sw = Rect::from_center_size(pos2(x, r.center().y), vec2(14.0, 14.0));
        let resp = ui.interact(sw, egui::Id::new(("sw", clip.0, idx, pd.id)), Sense::click()).on_hover_text("Toggle animation");
        icons::paint(ui.painter(), sw, Icon::Stopwatch, if param.is_animated() { t.accent } else { t.text_dim });
        if resp.clicked() {
            actions.push(("effects.toggleAnimation".into(), json!({"clip": clip.0, "effect": idx, "param": pd.id})));
        }
        app.auto.add(&format!("effectControls.{}.{}.stopwatch", e.effect, pd.id), sw, "Toggle animation");
    }
    x += 14.0;
    ui.painter().text(pos2(x, r.center().y), Align2::LEFT_CENTER, pd.label, Tokens::ui(12.0), t.text);
    let vx = r.min.x + (r.width() * 0.5).max(150.0);
    let value = param.value_at(mt);
    let mut vui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(vx, r.min.y + 1.0), pos2(r.max.x - 26.0, r.max.y - 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let id = egui::Id::new(("pv", clip.0, idx, pd.id));
    let mut set: Option<Value> = None;
    match (&pd.kind, &value) {
        (ParamKind::Float { min, max, soft_min, soft_max, unit, decimals }, ParamValue::Float(v)) => {
            let speed = ((soft_max - soft_min) / 400.0).max(0.01);
            let (_, nv) = crate::widgets::hot_number(&mut vui, id, *v, speed, (*min, *max), *decimals as usize, unit, &t);
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Angle, ParamValue::Float(v)) => {
            let (_, nv) = crate::widgets::hot_number(&mut vui, id, *v, 0.5, (-36000.0, 36000.0), 1, "°", &t);
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Point, ParamValue::Vec2(p)) => {
            let (_, nx) = crate::widgets::hot_number(&mut vui, id.with("x"), p.x, 1.0, (-100_000.0, 100_000.0), 1, "", &t);
            let (_, ny) = crate::widgets::hot_number(&mut vui, id.with("y"), p.y, 1.0, (-100_000.0, 100_000.0), 1, "", &t);
            if nx.is_some() || ny.is_some() {
                set = Some(json!([nx.unwrap_or(p.x), ny.unwrap_or(p.y)]));
            }
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut v = *b;
            if vui.checkbox(&mut v, "").changed() {
                set = Some(json!(v));
            }
        }
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            let mut sel = *c as usize;
            egui::ComboBox::from_id_salt(id).selected_text(opts.get(sel).copied().unwrap_or("")).width(130.0).show_ui(&mut vui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    if ui.selectable_value(&mut sel, i, *o).changed() {
                        set = Some(json!(i));
                    }
                }
            });
        }
        (ParamKind::Color, ParamValue::Color(c)) => {
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            if egui::color_picker::color_edit_button_rgba(&mut vui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                set = Some(json!([rgba.r(), rgba.g(), rgba.b(), rgba.a()]));
            }
        }
        _ => {
            vui.label(format!("{value:?}"));
        }
    }
    if let Some(v) = set {
        actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": v})));
    }
    // keyframes in the lane
    if param.is_animated() {
        let y = r.center().y;
        for k in &param.keyframes {
            // media time → timeline
            let tl = it.start + Tick(((k.time - it.source_in).0 as f64 / it.speed.abs().max(1e-6)) as i64);
            let f = ((tl - it.start).0 as f64 / it.duration.0.max(1) as f64) as f32;
            if (0.0..=1.0).contains(&f) {
                let kx = lane.min.x + f * lane.width();
                icons::paint(ui.painter(), Rect::from_center_size(pos2(kx, y), vec2(10.0, 10.0)), Icon::Keyframe, Color32::from_rgb(200, 200, 200));
            }
        }
    }
}

/// Lumetri Color panel: edits (or adds) the Lumetri effect on the selected clip, grouped by section.
pub fn lumetri_panel(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it, _)) = selected_clip(app) else {
        crate::dock::placeholder(ui, rect, &t, "Select a clip to grade");
        return;
    };
    let idx = it.effects.iter().position(|e| e.effect == "lumetri");
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)).id_salt("lumetri"));
    bui.label(egui::RichText::new(format!("Master · {}", it.name)).color(t.text_dim));
    let Some(idx) = idx else {
        if bui.button("Add Lumetri Color to clip").clicked() {
            let _ = app.session.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}));
        }
        return;
    };
    let e = it.effects[idx].clone();
    let def = e.def().expect("lumetri def");
    let ph = app.session.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut bui, |ui| {
        let mut groups: Vec<&str> = Vec::new();
        for p in &def.params {
            if let Some(g) = p.group
                && !groups.contains(&g)
            {
                groups.push(g);
            }
        }
        for g in groups {
            let key = format!("lumetri:{g}");
            let open = !app.ui.collapsed_fx.contains(&key);
            let (resp, now_open) = crate::widgets::section_header(ui, egui::Id::new(&key), g, open, &t, true);
            app.auto.add(&format!("lumetri.section.{g}"), resp.rect, g);
            if now_open != open {
                if now_open {
                    app.ui.collapsed_fx.retain(|k| *k != key);
                } else {
                    app.ui.collapsed_fx.push(key.clone());
                }
            }
            if !now_open {
                continue;
            }
            for pd in def.params.iter().filter(|p| p.group == Some(g)) {
                let v = e.params.get(pd.id).map(|p| p.value_at(mt)).unwrap_or(pd.default.clone());
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.add_sized(vec2(110.0, 18.0), egui::Label::new(egui::RichText::new(pd.label).size(12.0)));
                    if let (ParamKind::Float { min, max, soft_min, soft_max, .. }, ParamValue::Float(x)) = (&pd.kind, &v) {
                        let mut val = *x;
                        let s = ui.add(egui::Slider::new(&mut val, *soft_min..=*soft_max).show_value(false));
                        let (_, nv) =
                            crate::widgets::hot_number(ui, egui::Id::new(("lum", pd.id)), val, (soft_max - soft_min) / 300.0, (*min, *max), 1, "", &t);
                        if s.changed() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": val}));
                        } else if let Some(nv) = nv {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": nv}));
                        }
                        if s.double_clicked() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": pd.default.as_f64().unwrap_or(0.0)}));
                        }
                    } else if let ParamValue::Color(c) = v {
                        let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                        if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": [rgba.r(), rgba.g(), rgba.b(), 1.0]}));
                        }
                    }
                });
            }
        }
    });
    for a in actions {
        if let Err(e) = app.session.execute("effects.setParam", a) {
            app.ui.status = e.to_string();
        }
    }
}
