//! Preferences ▸ Auto Save, the crash-recovery prompt and the Revert confirmation.
//!
//! Automation ids: `prefs.autoSave.<key>` (controls), `prefs.category.autoSave`, `prefs.reset`,
//! `prefs.cancel`, `prefs.ok`; `recovery.item.<n>`, `recovery.recover`, `recovery.discard`,
//! `recovery.later`; `revert.yes`, `revert.no`.

use egui::{Align2, Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, vec2};
use filmcraft_engine::autosave::AutoSavePrefs;
use serde_json::json;

use crate::theme::Tokens;
use crate::{Dialog, FilmcraftApp};

/// Dialog-local state (the Preferences draft is applied on OK).
#[derive(Default)]
pub struct FileDialogState {
    pub prefs_draft: Option<AutoSavePrefs>,
    pub recovery_choice: usize,
}

fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

fn button(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let text = RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text });
    let b = egui::Button::new(text)
        .min_size(vec2(88.0, 30.0))
        .corner_radius(CornerRadius::same(15))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b);
    app.auto.add(id, r.rect, label);
    r.clicked()
}

/// A Premiere-style group box: 1-px rounded border with the title inset in the top edge.
fn group(ui: &mut Ui, t: &Tokens, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(8.0);
    let r = Frame::new()
        .stroke(Stroke::new(1.0, t.field_border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin { left: 14, right: 14, top: 18, bottom: 12 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            add(ui);
        });
    let rect = r.response.rect;
    let galley = ui.painter().layout_no_wrap(title.to_string(), Tokens::ui(12.5), t.text_dim);
    let pos = rect.left_top() + vec2(14.0, -galley.size().y / 2.0);
    ui.painter().rect_filled(egui::Rect::from_min_size(pos - vec2(5.0, 0.0), galley.size() + vec2(10.0, 0.0)), 0.0, t.panel_bg);
    ui.painter().galley(pos, galley, t.text_dim);
    ui.add_space(6.0);
}

fn checkbox(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, value: &mut bool, label: &str) {
    let r = ui.checkbox(value, RichText::new(label).size(13.0));
    app.auto.add(id, r.rect, label);
}

fn number(app: &mut FilmcraftApp, ui: &mut Ui, id: &str, label: &str, value: &mut u32, range: (u32, u32), unit: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(230.0, 24.0), egui::Label::new(RichText::new(label).size(13.0)));
        let r = ui.add_sized(vec2(90.0, 26.0), egui::DragValue::new(value).range(range.0..=range.1).speed(0.2));
        app.auto.add(id, r.rect, label);
        if !unit.is_empty() {
            ui.label(RichText::new(unit).size(13.0));
        }
    });
}

pub fn show_preferences(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let mut draft = app.file_dialogs.prefs_draft.clone().unwrap_or_else(|| app.session.prefs.auto_save.clone());
    let mut close = false;
    let status = app.session.execute("file.autoSaveStatus", json!({})).unwrap_or_default();
    let resp = egui::Modal::new(egui::Id::new("prefs-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(780.0);
        // title bar
        let (bar, _) = ui.allocate_exact_size(vec2(780.0, 34.0), egui::Sense::hover());
        ui.painter().rect_filled(bar, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, t.header_bg);
        ui.painter().text(bar.center(), Align2::CENTER_CENTER, "Preferences", Tokens::semibold(13.0), t.text_dim);
        ui.horizontal_top(|ui| {
            ui.add_space(16.0);
            // category list
            ui.vertical(|ui| {
                ui.add_space(14.0);
                let (list, _) = ui.allocate_exact_size(vec2(190.0, 420.0), egui::Sense::hover());
                ui.painter().rect(list, 2.0, t.app_bg, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
                let row = egui::Rect::from_min_size(list.min + vec2(2.0, 4.0), vec2(186.0, 24.0));
                ui.painter().rect_filled(row, 0.0, t.row_selected);
                ui.painter().text(row.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "Auto Save", Tokens::ui(13.0), t.text);
                app.auto.add("prefs.category.autoSave", row, "Auto Save");
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.set_width(540.0);
                ui.add_space(14.0);
                group(ui, &t, "Local Projects", |ui| {
                    checkbox(app, ui, "prefs.autoSave.enabled", &mut draft.enabled, "Automatically save projects");
                    ui.add_enabled_ui(draft.enabled, |ui| {
                        number(app, ui, "prefs.autoSave.intervalMinutes", "Automatically Save Every:", &mut draft.interval_minutes, (1, 1440), "minute(s)");
                        number(app, ui, "prefs.autoSave.maxVersions", "Maximum Project Versions:", &mut draft.max_versions, (1, 1000), "");
                        checkbox(app, ui, "prefs.autoSave.saveCurrentProject", &mut draft.save_current_project, "Auto Save also saves the current project(s)");
                    });
                    let dir = status["autoSaveDir"].as_str().unwrap_or("Auto-Save folder next to the project");
                    ui.label(RichText::new(format!("Auto-saves go to: {dir}")).size(11.5).color(t.text_dim));
                });
                group(ui, &t, "Crash Recovery", |ui| {
                    checkbox(app, ui, "prefs.autoSave.recoveryJournal", &mut draft.recovery_journal, "Keep a recovery copy of unsaved changes");
                    ui.add_enabled_ui(draft.recovery_journal, |ui| {
                        number(
                            app,
                            ui,
                            "prefs.autoSave.recoveryIntervalSeconds",
                            "Update it at least every:",
                            &mut draft.recovery_interval_seconds,
                            (1, 600),
                            "second(s)",
                        );
                    });
                    ui.label(
                        RichText::new("Changes are copied in the background a moment after each edit. If FilmCraft quits unexpectedly, they are offered the next time it starts.")
                            .size(11.5)
                            .color(t.text_dim),
                    );
                    if let Some(dir) = status["sessionDir"].as_str() {
                        ui.label(RichText::new(format!("Location: {dir}")).size(11.5).color(t.text_dim));
                    }
                    let last = match (status["lastAutoSaveAt"].as_str(), status["lastJournalAt"].as_str()) {
                        (Some(a), Some(j)) => format!("Last auto-save {a} · last recovery copy {j}"),
                        (Some(a), None) => format!("Last auto-save {a}"),
                        (None, Some(j)) => format!("Last recovery copy {j}"),
                        (None, None) => String::new(),
                    };
                    if !last.is_empty() {
                        ui.label(RichText::new(last).size(11.5).color(t.text_dim));
                    }
                });
            });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            if button(app, ui, "prefs.reset", "Reset…", false) {
                draft = AutoSavePrefs::default();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(16.0);
                if button(app, ui, "prefs.ok", "OK", true) {
                    let values = serde_json::to_value(&draft).unwrap_or_default();
                    let map: serde_json::Map<String, serde_json::Value> =
                        values.as_object().map(|m| m.iter().map(|(k, v)| (format!("autoSave.{k}"), v.clone())).collect()).unwrap_or_default();
                    if let Err(e) = app.session.execute("prefs.set", json!({ "values": map })) {
                        app.ui.status = e.to_string();
                    }
                    close = true;
                }
                if button(app, ui, "prefs.cancel", "Cancel", false) {
                    close = true;
                }
            });
        });
        ui.add_space(14.0);
    });
    if resp.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    }
    app.file_dialogs.prefs_draft = if close { None } else { Some(draft) };
    !close
}

pub fn show_recovery(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let list = app.session.execute("file.recoveryList", json!({})).unwrap_or_default();
    let items = list.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return false;
    }
    let mut choice = app.file_dialogs.recovery_choice.min(items.len() - 1);
    let mut open = true;
    egui::Modal::new(egui::Id::new("recovery-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(520.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new("Recover Unsaved Changes").size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            let c = &items[choice];
            let name = c["projectName"].as_str().unwrap_or("Untitled");
            let when = c["savedAt"].as_str().unwrap_or("");
            let why = if c["cleanExit"].as_bool() == Some(true) {
                format!("FilmCraft was closed while “{name}” had unsaved changes.")
            } else {
                format!("FilmCraft quit unexpectedly while “{name}” had unsaved changes.")
            };
            ui.label(RichText::new(why).size(13.5).color(t.text));
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Recover unsaved changes from {when}?")).size(13.5).color(t.text));
            if let Some(p) = c["projectPath"].as_str() {
                ui.label(RichText::new(p).size(11.5).color(t.text_dim));
            } else {
                ui.label(RichText::new("The project had not been saved yet.").size(11.5).color(t.text_dim));
            }
            if items.len() > 1 {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{} sessions have unsaved changes:", items.len())).size(12.0).color(t.text_dim));
                for (i, it) in items.iter().enumerate() {
                    let label = format!("{} — {}", it["projectName"].as_str().unwrap_or("Untitled"), it["savedAt"].as_str().unwrap_or(""));
                    let r = ui.radio(choice == i, RichText::new(&label).size(12.5));
                    app.auto.add(&format!("recovery.item.{i}"), r.rect, &label);
                    if r.clicked() {
                        choice = i;
                    }
                }
            }
            ui.add_space(18.0);
            let id = items[choice]["id"].as_str().unwrap_or("").to_string();
            ui.horizontal(|ui| {
                if button(app, ui, "recovery.discard", "Discard", false) {
                    if let Err(e) = app.session.execute("file.discardRecovery", json!({ "id": id })) {
                        app.ui.status = e.to_string();
                    }
                    choice = 0;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(app, ui, "recovery.recover", "Recover", true) {
                        match app.session.execute("file.recover", json!({ "id": id })) {
                            Ok(_) => open = false,
                            Err(e) => app.ui.status = e.to_string(),
                        }
                    }
                    if button(app, ui, "recovery.later", "Not Now", false) {
                        open = false;
                    }
                });
            });
        });
    });
    app.file_dialogs.recovery_choice = choice;
    open && !app.session.recovery_candidates().is_empty()
}

pub fn show_revert(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let t = app.tokens;
    let file = app.session.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut open = true;
    let r = egui::Modal::new(egui::Id::new("revert-modal")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(460.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new("Revert").size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Are you sure you want to discard your changes to '{file}'?")).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "revert.yes", "Yes", true) {
                    if let Err(e) = app.session.execute("file.revert", json!({})) {
                        app.ui.status = e.to_string();
                    }
                    open = false;
                }
                if button(app, ui, "revert.no", "No", false) {
                    open = false;
                }
            });
        });
    });
    open && !r.should_close()
}

/// Draw the dialog if it is one of ours; returns Some(still open).
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, d: Dialog) -> Option<bool> {
    match d {
        Dialog::Preferences => Some(show_preferences(app, ctx)),
        Dialog::Recovery => Some(show_recovery(app, ctx)),
        Dialog::RevertConfirm => Some(show_revert(app, ctx)),
        _ => None,
    }
}
