//! Native macOS menu bar (muda), generated from the command registry — like Premiere, menus live in
//! the system menu bar. Items send command ids to the app's command inbox.

use std::sync::mpsc::{Receiver, channel};

use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::{MENUS, MenuItem as Item, menu_items};
use muda::accelerator::Accelerator;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

fn accel(s: &str) -> Option<Accelerator> {
    // Only shortcuts with a modifier become native key equivalents (single keys stay in-app so
    // typing in fields keeps working).
    if !(s.contains("Cmd+") || s.contains("Ctrl+") || s.contains("Alt+")) {
        return None;
    }
    let mapped =
        s.replace("Cmd+", "CmdOrCtrl+").replace(";", "Semicolon").replace("'", "Quote").replace("/", "Slash").replace("=", "Equal").replace("\\", "Backslash");
    mapped.parse().ok()
}

/// Updates native key equivalents when the active keyboard shortcuts change.
pub type ShortcutUpdater = Box<dyn FnMut(&[Item])>;

pub fn install(app: &FilmcraftApp, ctx: egui::Context) -> (Receiver<String>, ShortcutUpdater) {
    let items = menu_items(app);
    let mut native: Vec<(String, MenuItem)> = Vec::new();
    let bar = Menu::new();
    let app_menu = Submenu::new("FilmCraft", true);
    let _ = app_menu.append_items(&[
        &MenuItem::with_id("app.about", "About FilmCraft", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("app.preferences.autoSave", "Settings…", true, None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);
    let _ = bar.append(&app_menu);
    for top in MENUS {
        let sub = Submenu::new(top, true);
        let mine: Vec<&Item> = items.iter().filter(|i| i.path.first().map(String::as_str) == Some(top)).collect();
        let mut subs: Vec<(String, Submenu)> = Vec::new();
        for it in mine {
            let mi = MenuItem::with_id(it.id.clone(), &it.label, true, it.shortcut.as_deref().and_then(accel));
            native.push((it.id.clone(), mi.clone()));
            if let Some(name) = it.path.get(1) {
                if let Some((_, s)) = subs.iter().find(|(n, _)| n == name) {
                    let _ = s.append(&mi);
                } else {
                    let s = Submenu::new(name, true);
                    let _ = s.append(&mi);
                    let _ = sub.append(&s);
                    subs.push((name.clone(), s));
                }
            } else {
                let _ = sub.append(&mi);
            }
        }
        let _ = bar.append(&sub);
    }
    bar.init_for_nsapp();
    if let Some(help) = bar.items().last().and_then(|i| i.as_submenu().cloned()) {
        help.set_as_help_menu_for_nsapp();
    }
    Box::leak(Box::new(bar));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        while let Ok(ev) = MenuEvent::receiver().recv() {
            if tx.send(ev.id().0.clone()).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    let update: ShortcutUpdater = Box::new(move |items: &[Item]| {
        for (id, mi) in &native {
            if let Some(it) = items.iter().find(|i| &i.id == id) {
                let _ = mi.set_accelerator(it.shortcut.as_deref().and_then(accel));
            }
        }
    });
    (rx, update)
}
