//! Project panel: bins and items in List or Icon view, search, and the bottom action bar.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{Bin, BinEntry, ItemId, ItemKind};
use filmcraft_time::{TimeDisplay, format_time};
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::ProjectView;
use crate::theme::Tokens;

const ROW_H: f32 = 22.0;

fn item_icon(k: &ItemKind) -> Icon {
    match k {
        ItemKind::Media(m) => match m.info.kind {
            filmcraft_media::MediaKind::AudioOnly => Icon::Audio,
            filmcraft_media::MediaKind::Still | filmcraft_media::MediaKind::ImageSequence => Icon::Image,
            _ => Icon::Film,
        },
        ItemKind::Sequence(_) => Icon::Sequence,
        ItemKind::Subclip { .. } => Icon::Film,
        ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => Icon::Adjust,
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    // header: project file name + item count, search
    let head = Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(rect.width() - 16.0, 22.0));
    let n_items = app.session.project.items.len();
    ui.painter().text(pos2(head.min.x, head.center().y), Align2::LEFT_CENTER, format!("{}.fcproj", app.session.project.name), Tokens::ui(11.5), t.text_dim);
    ui.painter().text(pos2(head.max.x, head.center().y), Align2::RIGHT_CENTER, format!("{n_items} items"), Tokens::ui(11.0), t.text_faint);
    let search_rect = Rect::from_min_size(pos2(rect.min.x + 8.0, head.max.y + 4.0), vec2(rect.width() - 16.0, 22.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(search_rect));
    let mut q = app.ui.project_search.clone();
    crate::widgets::search_field(&mut child, &mut q, "Search", search_rect.width(), &t);
    app.ui.project_search = q;
    app.auto.add("project.search", search_rect, "Search");
    let bottom_h = 30.0;
    let body = Rect::from_min_max(pos2(rect.min.x, search_rect.max.y + 6.0), pos2(rect.max.x, rect.max.y - bottom_h));
    let mut actions: Vec<(String, serde_json::Value)> = Vec::new();
    let root = app.session.project.root.clone();
    let filter = app.ui.project_search.to_ascii_lowercase();
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("project-body"));
    body_ui.set_clip_rect(body);
    egui::ScrollArea::vertical().id_salt("project-scroll").auto_shrink([false, false]).show(&mut body_ui, |ui| {
        match app.ui.project_view {
            ProjectView::List => {
                // column header
                let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
                let cols = columns(hr);
                ui.painter().text(pos2(cols[0] + 26.0, hr.center().y), Align2::LEFT_CENTER, "Name", Tokens::ui(11.0), t.text_dim);
                for (i, name) in ["Frame Rate", "Media Start", "Media Duration"].iter().enumerate() {
                    if cols[i + 1] < hr.max.x - 20.0 {
                        ui.painter().text(pos2(cols[i + 1], hr.center().y), Align2::LEFT_CENTER, *name, Tokens::ui(11.0), t.text_dim);
                    }
                }
                ui.painter().line_segment([hr.left_bottom(), hr.right_bottom()], Stroke::new(1.0, t.separator));
                let mut row = 0usize;
                list_bin(app, ui, &root, 0, &filter, &mut row, &mut actions, true);
            }
            _ => icon_view(app, ui, &root, &filter, &mut actions),
        }
        // empty space: click deselects; right-click new items
        let rest = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(Rect::from_min_size(rest.min, vec2(rest.width(), rest.height().max(40.0))), Sense::click());
        if resp.clicked() {
            actions.push(("project.select".into(), json!({"items": []})));
        }
        if resp.double_clicked() {
            actions.push(("file.import".into(), json!({})));
        }
        resp.context_menu(|ui| {
            for (label, cmd) in [
                ("New Bin", "file.newBin"),
                ("New Sequence…", "file.newSequence"),
                ("Import…", "file.import"),
                ("Bars and Tone", "file.newBarsAndTone"),
                ("Color Matte", "file.newColorMatte"),
                ("Adjustment Layer", "file.newAdjustmentLayer"),
                ("Universal Counting Leader", "file.newCountingLeader"),
            ] {
                if ui.button(label).clicked() {
                    actions.push((cmd.into(), json!({})));
                    ui.close();
                }
            }
        });
    });
    // bottom bar
    let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - bottom_h), rect.max);
    ui.painter().line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0, t.separator));
    let mut x = bar.min.x + 6.0;
    for (icon, view, tip) in [
        (Icon::ListView, ProjectView::List, "List View"),
        (Icon::IconView, ProjectView::Icon, "Icon View"),
        (Icon::Freeform, ProjectView::Freeform, "Freeform View"),
    ] {
        let r = Rect::from_min_size(pos2(x, bar.min.y + 4.0), vec2(24.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(("pv", tip)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("project.view.{view:?}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, if app.ui.project_view == view { t.icon_active } else { t.icon });
        if resp.clicked() {
            app.ui.project_view = view;
        }
        x += 26.0;
    }
    // icon size slider
    let sr = Rect::from_min_size(pos2(x + 6.0, bar.min.y + 7.0), vec2((bar.width() - 260.0).clamp(40.0, 140.0), 16.0));
    let mut sz = app.ui.icon_size;
    ui.put(sr, egui::Slider::new(&mut sz, 60.0..=240.0).show_value(false));
    app.ui.icon_size = sz;
    let mut rx = bar.max.x - 6.0;
    for (icon, cmd, tip) in [
        (Icon::Trash, "project.delete", "Clear (Delete)"),
        (Icon::NewItem, "new-item", "New Item"),
        (Icon::Folder, "file.newBin", "New Bin"),
        (Icon::Search, "find", "Find"),
    ] {
        let r = Rect::from_min_size(pos2(rx - 24.0, bar.min.y + 4.0), vec2(24.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(("pb", cmd)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("project.button.{cmd}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, t.icon);
        if cmd == "new-item" {
            egui::Popup::menu(&resp).show(|ui| {
                for (label, c) in [
                    ("Sequence…", "file.newSequence"),
                    ("Adjustment Layer…", "file.newAdjustmentLayer"),
                    ("Bars and Tone…", "file.newBarsAndTone"),
                    ("Black Video…", "file.newBlackVideo"),
                    ("Color Matte…", "file.newColorMatte"),
                    ("Universal Counting Leader…", "file.newCountingLeader"),
                    ("Transparent Video…", "file.newTransparentVideo"),
                    ("Demo Footage", "file.importDemoFootage"),
                ] {
                    if ui.button(label).clicked() {
                        actions.push((c.into(), json!({})));
                    }
                }
            });
        } else if resp.clicked() && cmd != "find" {
            actions.push((cmd.into(), json!({})));
        }
        rx -= 26.0;
    }
    let ctx = ui.ctx().clone();
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, &ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}

fn columns(r: Rect) -> [f32; 4] {
    let w = r.width();
    let name_w = (w * 0.46).max(160.0);
    [r.min.x, r.min.x + name_w, r.min.x + name_w + 80.0, r.min.x + name_w + 180.0]
}

fn matches(app: &FilmcraftApp, id: ItemId, filter: &str) -> bool {
    // graphic clip sources are internal (Premiere doesn't list graphics in the Project panel)
    app.session.project.item(id).is_some_and(|i| !matches!(i.kind, ItemKind::Graphic { .. }))
        && (filter.is_empty() || app.session.project.item(id).is_some_and(|i| i.name.to_ascii_lowercase().contains(filter)))
}

#[allow(clippy::too_many_arguments)]
fn list_bin(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    bin: &Bin,
    depth: usize,
    filter: &str,
    row: &mut usize,
    actions: &mut Vec<(String, serde_json::Value)>,
    _root: bool,
) {
    let t = app.tokens;
    for e in &bin.children {
        match e {
            BinEntry::Bin(b) => {
                let open = app.ui.expanded_bins.contains(&b.id.0) || !filter.is_empty();
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if *row % 2 == 1 {
                    ui.painter().rect_filled(r, 0.0, t.row_alt);
                }
                *row += 1;
                let x = r.min.x + 6.0 + depth as f32 * 14.0;
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(pos2(x + 5.0, r.center().y), vec2(10.0, 10.0)),
                    if open { Icon::ChevronDown } else { Icon::ChevronRight },
                    t.text_dim,
                );
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(pos2(x + 20.0, r.center().y), vec2(14.0, 14.0)),
                    Icon::Folder,
                    Color32::from_rgb(237, 150, 58),
                );
                ui.painter().text(pos2(x + 32.0, r.center().y), Align2::LEFT_CENTER, &b.name, Tokens::ui(12.0), t.text);
                app.auto.add(&format!("project.bin.{}", b.id.0), r, &b.name);
                if resp.clicked() {
                    if open {
                        app.ui.expanded_bins.retain(|x| *x != b.id.0);
                    } else {
                        app.ui.expanded_bins.push(b.id.0);
                    }
                }
                if open {
                    list_bin(app, ui, b, depth + 1, filter, row, actions, false);
                }
            }
            BinEntry::Item(id) => {
                if !matches(app, *id, filter) {
                    continue;
                }
                let Some(it) = app.session.project.item(*id).cloned() else { continue };
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                let selected = app.session.state.project_selection.contains(id);
                if selected {
                    ui.painter().rect_filled(r, 0.0, t.row_selected);
                } else if *row % 2 == 1 {
                    ui.painter().rect_filled(r, 0.0, t.row_alt);
                }
                *row += 1;
                let cols = columns(r);
                let x = r.min.x + 6.0 + depth as f32 * 14.0;
                // label swatch
                let lc = it.label.rgb();
                ui.painter().rect_filled(Rect::from_center_size(pos2(x + 5.0, r.center().y), vec2(8.0, 12.0)), 1.5, Color32::from_rgb(lc[0], lc[1], lc[2]));
                icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 20.0, r.center().y), vec2(14.0, 14.0)), item_icon(&it.kind), t.icon);
                let name_clip = ui.painter().with_clip_rect(Rect::from_min_max(r.min, pos2(cols[1] - 6.0, r.max.y)));
                name_clip.text(pos2(x + 32.0, r.center().y), Align2::LEFT_CENTER, &it.name, Tokens::ui(12.0), t.text);
                let rate = it.frame_rate();
                let has_v = it.has_video();
                let cells = [
                    if has_v { format!("{} fps", rate.label()) } else { String::new() },
                    format_time(filmcraft_time::Tick::ZERO, rate, false, TimeDisplay::Timecode, 48000),
                    format_time(it.duration(), rate, false, TimeDisplay::Timecode, 48000),
                ];
                for (i, c) in cells.iter().enumerate() {
                    if cols[i + 1] < r.max.x - 20.0 {
                        ui.painter().text(pos2(cols[i + 1], r.center().y), Align2::LEFT_CENTER, c, Tokens::ui(11.5), t.text_dim);
                    }
                }
                app.auto.add(&format!("project.item.{}", id.0), r, &it.name);
                item_interactions(app, ui, &resp, *id, &it.kind, actions);
            }
        }
    }
}

fn item_interactions(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    id: ItemId,
    kind: &ItemKind,
    actions: &mut Vec<(String, serde_json::Value)>,
) {
    if resp.clicked() {
        let mods = ui.input(|i| i.modifiers);
        let mut sel = app.session.state.project_selection.clone();
        if mods.command || mods.shift {
            if let Some(p) = sel.iter().position(|x| *x == id) {
                sel.remove(p);
            } else {
                sel.push(id);
            }
        } else {
            sel = vec![id];
        }
        actions.push(("project.select".into(), json!({"items": sel.iter().map(|i| i.0).collect::<Vec<_>>()})));
    }
    if resp.double_clicked() {
        let cmd = if matches!(kind, ItemKind::Sequence(_)) { "sequence.open" } else { "source.open" };
        actions.push((cmd.into(), json!({"item": id.0})));
    }
    if resp.drag_started() {
        crate::panels::start_drag_item(ui, id);
    }
    resp.context_menu(|ui| {
        if ui.button("Open in Source Monitor").clicked() {
            actions.push(("source.open".into(), json!({"item": id.0})));
            ui.close();
        }
        if ui.button("New Sequence From Clip").clicked() {
            actions.push(("file.newSequence".into(), json!({"fromItem": id.0})));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            actions.push(("project.select".into(), json!({"items": [id.0]})));
            actions.push(("edit.duplicate".into(), json!({})));
            ui.close();
        }
        if ui.button("Interpret Footage…").clicked() {
            actions.push(("clip.interpretFootage".into(), json!({"items": [id.0]})));
            ui.close();
        }
        ui.menu_button("Label", |ui| {
            for l in filmcraft_project::Label::ALL {
                if ui.button(l.name()).clicked() {
                    actions.push(("project.select".into(), json!({"items": [id.0]})));
                    actions.push(("edit.label".into(), json!({"label": l.name()})));
                    ui.close();
                }
            }
        });
        if ui.button("Clear").clicked() {
            actions.push(("project.delete".into(), json!({"items": [id.0]})));
            ui.close();
        }
    });
}

fn icon_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, root: &Bin, filter: &str, actions: &mut Vec<(String, serde_json::Value)>) {
    let t = app.tokens;
    let mut ids = Vec::new();
    root.all_items(&mut ids);
    let size = app.ui.icon_size;
    let th_h = size * 9.0 / 16.0;
    let cell = vec2(size + 12.0, th_h + 34.0);
    let per_row = ((ui.available_width() - 8.0) / cell.x).floor().max(1.0) as usize;
    let ctx = ui.ctx().clone();
    for chunk in ids.iter().filter(|i| matches(app, **i, filter)).copied().collect::<Vec<_>>().chunks(per_row) {
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell.y), Sense::hover());
        for (k, id) in chunk.iter().enumerate() {
            let Some(it) = app.session.project.item(*id).cloned() else { continue };
            let r = Rect::from_min_size(pos2(row.min.x + 6.0 + k as f32 * cell.x, row.min.y + 4.0), vec2(size, th_h));
            let resp = ui.interact(r.expand(2.0), egui::Id::new(("icon", id.0)), Sense::click_and_drag());
            ui.painter().rect_filled(r, 3.0, Color32::from_rgb(12, 12, 12));
            let hover_t = if resp.hovered() {
                // hover scrub: pointer x picks the time
                ctx.pointer_hover_pos().map(|p| ((p.x - r.min.x) / r.width()).clamp(0.0, 1.0) as f64).unwrap_or(0.3)
            } else {
                0.3
            };
            let dur = it.duration();
            let tt = filmcraft_time::Tick((dur.0 as f64 * hover_t) as i64);
            let q = filmcraft_time::Tick((tt.0 / (filmcraft_time::TICKS_PER_SECOND / 4)) * (filmcraft_time::TICKS_PER_SECOND / 4));
            if let Some((tex, sz)) = app.thumbnail(&ctx, *id, q, (size as u32).clamp(96, 320)) {
                let fitted = crate::panels::monitor::fit(r, sz.x, sz.y);
                ui.painter().image(tex, fitted, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(28.0, 28.0)), item_icon(&it.kind), t.text_faint);
            }
            let selected = app.session.state.project_selection.contains(id);
            if selected {
                ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
            }
            let lc = it.label.rgb();
            ui.painter().rect_filled(Rect::from_min_size(pos2(r.min.x, r.max.y + 6.0), vec2(8.0, 12.0)), 1.5, Color32::from_rgb(lc[0], lc[1], lc[2]));
            let secs = dur.seconds().max(0.0) as u64;
            let dtext =
                if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
            let dg = ui.painter().layout_no_wrap(dtext, Tokens::ui(11.0), t.text_dim);
            let dw = dg.size().x;
            ui.painter().galley(pos2(r.max.x - dw, r.max.y + 12.0 - dg.size().y / 2.0), dg, t.text_dim);
            let clip = ui.painter().with_clip_rect(Rect::from_min_max(pos2(r.min.x, r.max.y), pos2(r.max.x - dw - 8.0, r.max.y + 20.0)));
            clip.text(pos2(r.min.x + 12.0, r.max.y + 12.0), Align2::LEFT_CENTER, &it.name, Tokens::ui(11.5), t.text);
            app.auto.add(&format!("project.item.{}", id.0), r, &it.name);
            item_interactions(app, ui, &resp, *id, &it.kind, actions);
        }
    }
}
