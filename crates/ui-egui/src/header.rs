//! The header bar: app mark + menus on the left, Import / Edit / Export mode tabs in the centre,
//! project title, workspace switcher and quick actions on the right.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.header_bg);
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, Color32::from_rgb(0, 0, 0)));
    // Window drag area (integrated title bar on macOS).
    let drag = ui.interact(rect, egui::Id::new("header-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
    let mut x = rect.min.x + if app.integrated_titlebar { 78.0 } else { 10.0 };
    // App mark: a small film-frame glyph in the accent colour.
    let mark = Rect::from_center_size(pos2(x + 11.0, rect.center().y), vec2(22.0, 22.0));
    p.rect_filled(mark, 5.0, Color32::from_rgb(38, 28, 90));
    p.rect_stroke(mark, 5.0, Stroke::new(1.0, Color32::from_rgb(120, 104, 255)), egui::StrokeKind::Inside);
    p.text(mark.center(), Align2::CENTER_CENTER, "Fc", Tokens::semibold(11.0), Color32::from_rgb(200, 190, 255));
    app.auto.add("header.home", mark, "Home");
    x = mark.max.x + 8.0;
    // Menu bar
    let menu_rect = Rect::from_min_max(pos2(x, rect.min.y + 6.0), pos2(x + 560.0, rect.max.y - 6.0));
    let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(menu_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    mu.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    mu.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    crate::menus::menu_bar(app, &mut mu);
    // Mode tabs centred
    let modes = [(Mode::Import, "Import"), (Mode::Edit, "Edit"), (Mode::Export, "Export")];
    let tab_w = 74.0;
    let total = tab_w * 3.0;
    let mut tx = rect.center().x - total / 2.0;
    for (m, label) in modes {
        let r = Rect::from_min_size(pos2(tx, rect.min.y + 6.0), vec2(tab_w, rect.height() - 12.0));
        let resp = ui.interact(r, egui::Id::new(("mode", label)), Sense::click());
        app.auto.add(&format!("header.mode.{}", label.to_ascii_lowercase()), r, label);
        let active = app.ui.mode == m;
        if resp.hovered() && !active {
            p.rect_filled(r, 5.0, t.hover);
        }
        p.text(
            r.center() - vec2(0.0, 1.0),
            Align2::CENTER_CENTER,
            label,
            if active { Tokens::semibold(13.0) } else { Tokens::ui(13.0) },
            if active { t.tab_text_active } else { t.tab_text },
        );
        if active {
            let u = Rect::from_center_size(pos2(r.center().x, r.max.y - 1.0), vec2(label.len() as f32 * 7.5, 2.0));
            p.rect_filled(u, 1.0, t.tab_text_active);
        }
        if resp.clicked() {
            app.ui.mode = m;
        }
        tx += tab_w;
    }
    // Right side: title, workspaces, quick export, fullscreen
    let mut rx = rect.max.x - 12.0;
    let btn = |ui: &mut egui::Ui, rx: &mut f32, icon: Icon, id: &str, tip: &str, app: &mut FilmcraftApp| -> bool {
        let r = Rect::from_center_size(pos2(*rx - 14.0, rect.center().y), vec2(28.0, 28.0));
        *rx -= 32.0;
        let resp = ui.interact(r, egui::Id::new(("hdr", id)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("header.{id}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 5.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(7.0), icon, t.icon);
        resp.clicked()
    };
    if btn(ui, &mut rx, Icon::Fullscreen, "fullscreen", "Full screen", app) {
        let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }
    if btn(ui, &mut rx, Icon::Bell, "notifications", "Progress & notifications", app) {
        app.ui.status = "No background jobs running".into();
    }
    if btn(ui, &mut rx, Icon::Export, "quickExport", "Quick Export", app) {
        app.ui.mode = Mode::Export;
    }
    let ws_clicked = btn(ui, &mut rx, Icon::Workspaces, "workspaces", "Workspaces", app);
    let ws_rect = Rect::from_center_size(pos2(rx + 18.0, rect.center().y), vec2(28.0, 28.0));
    let popup_id = egui::Id::new("workspaces-popup");
    if ws_clicked {
        ui.ctx().data_mut(|d| d.insert_temp(popup_id, true));
    }
    let open = ui.ctx().data(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
    if open {
        let area = egui::Area::new(popup_id.with("area")).order(egui::Order::Foreground).fixed_pos(pos2(ws_rect.max.x - 200.0, ws_rect.max.y + 4.0)).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(200.0);
                    ui.label(egui::RichText::new("Workspaces").strong());
                    ui.separator();
                    for w in crate::dock::WORKSPACES {
                        let sel = app.ui.workspace == w;
                        if ui.selectable_label(sel, w).clicked() {
                            app.set_workspace(w);
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                        }
                    }
                    ui.separator();
                    if ui.button("Reset to Saved Layout").clicked() {
                        let n = app.ui.workspace.clone();
                        app.set_workspace(&n);
                        ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                    }
                });
            },
        );
        if area.response.clicked_elsewhere() && !ws_clicked {
            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
        }
    }
    // Project title (with unsaved dot)
    let title = format!("{}{}", app.session.project.name, if app.session.is_dirty() { " •" } else { "" });
    let right_start = rx - 8.0;
    p.text(pos2(right_start, rect.center().y), Align2::RIGHT_CENTER, title, Tokens::ui(12.5), t.text_dim);
}
