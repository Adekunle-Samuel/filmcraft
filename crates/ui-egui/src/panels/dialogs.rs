//! Modal dialogs (About, Keyboard Shortcuts; Preferences/Recovery/Revert in `file_dialogs`).

use crate::{Dialog, FilmcraftApp};

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(d) = app.dialog else { return };
    if let Some(still_open) = crate::panels::file_dialogs::show(app, ctx, d) {
        if !still_open && app.dialog == Some(d) {
            app.dialog = None;
        }
        return;
    }
    let mut open = true;
    match d {
        Dialog::About => {
            egui::Window::new("About FilmCraft").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.heading("FilmCraft");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A clean-room, pure-Rust non-linear video editor.");
                ui.label("Fonts: Inter and JetBrains Mono (SIL OFL 1.1).");
            });
        }
        Dialog::Shortcuts => {
            egui::Window::new("Keyboard Shortcuts").open(&mut open).default_size([520.0, 520.0]).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                        for c in filmcraft_engine::command_specs().iter().filter(|c| c.shortcut.is_some()) {
                            ui.label(c.label);
                            ui.monospace(crate::menus::shortcut_text(c.shortcut.unwrap_or("")));
                            ui.end_row();
                        }
                        for c in crate::menus::UI_COMMANDS.iter().filter(|c| c.shortcut.is_some()) {
                            ui.label(c.label);
                            ui.monospace(crate::menus::shortcut_text(c.shortcut.unwrap_or("")));
                            ui.end_row();
                        }
                    });
                });
            });
        }
        Dialog::NewSequence | Dialog::Preferences | Dialog::Recovery | Dialog::RevertConfirm => {}
    }
    if !open {
        app.dialog = None;
    }
}
