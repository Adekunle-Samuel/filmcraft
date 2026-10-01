//! Menus and UI-level commands. The menu bar is generated from the engine registry plus the UI
//! command table below (commands that only affect the frontend: tools, playback, zoom, panels).
//! `invoke` is the single entry point used by menus, shortcuts and the control channel.

use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::state::{Mode, PlaybackRes, Tool};

pub struct UiCommand {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: &'static [&'static str],
    pub shortcut: Option<&'static str>,
}

macro_rules! uic {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr) => {
        UiCommand { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc }
    };
}

pub const UI_COMMANDS: &[UiCommand] = &[
    uic!("playback.toggle", "Play/Stop", [], Some("Space")),
    uic!("playback.forward", "Shuttle Right", [], Some("L")),
    uic!("playback.stop", "Shuttle Stop", [], Some("K")),
    uic!("playback.reverse", "Shuttle Left", [], Some("J")),
    uic!("playback.slowForward", "Shuttle Slow Right", [], None),
    uic!("playback.slowReverse", "Shuttle Slow Left", [], None),
    uic!("playback.playAround", "Play Around", [], None),
    uic!("playback.inToOut", "Play In to Out", [], Some("Shift+Space")),
    uic!("playback.loop", "Loop", [], None),
    uic!("view.zoomIn", "Zoom In", ["View"], Some("=")),
    uic!("view.zoomOut", "Zoom Out", ["View"], Some("-")),
    uic!("view.zoomToSequence", "Zoom to Sequence", ["View"], Some("\\")),
    uic!("view.playbackRes.full", "Full", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.half", "1/2", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.quarter", "1/4", ["View", "Playback Resolution"], None),
    uic!("view.playbackRes.eighth", "1/8", ["View", "Playback Resolution"], None),
    uic!("view.safeMargins", "Safe Margins", ["View"], None),
    uic!("view.theme.dark", "Darkest", ["View", "Appearance"], None),
    uic!("view.theme.medium", "Medium", ["View", "Appearance"], None),
    uic!("view.theme.light", "Light", ["View", "Appearance"], None),
    uic!("window.workspace.editing", "Editing", ["Window", "Workspaces"], Some("Alt+Shift+1")),
    uic!("window.workspace.assembly", "Assembly", ["Window", "Workspaces"], Some("Alt+Shift+2")),
    uic!("window.workspace.color", "Color", ["Window", "Workspaces"], Some("Alt+Shift+3")),
    uic!("window.workspace.effects", "Effects", ["Window", "Workspaces"], Some("Alt+Shift+4")),
    uic!("window.workspace.audio", "Audio", ["Window", "Workspaces"], Some("Alt+Shift+5")),
    uic!("window.workspace.captionsandgraphics", "Captions and Graphics", ["Window", "Workspaces"], Some("Alt+Shift+6")),
    uic!("window.workspace.allpanels", "All Panels", ["Window", "Workspaces"], None),
    uic!("window.workspace.reset", "Reset to Saved Layout", ["Window", "Workspaces"], Some("Alt+Shift+0")),
    uic!("tool.selection", "Selection Tool", [], Some("V")),
    uic!("tool.trackSelectForward", "Track Select Forward Tool", [], Some("A")),
    uic!("tool.trackSelectBackward", "Track Select Backward Tool", [], Some("Shift+A")),
    uic!("tool.ripple", "Ripple Edit Tool", [], Some("B")),
    uic!("tool.rolling", "Rolling Edit Tool", [], Some("N")),
    uic!("tool.rateStretch", "Rate Stretch Tool", [], Some("R")),
    uic!("tool.razor", "Razor Tool", [], Some("C")),
    uic!("tool.slip", "Slip Tool", [], Some("Y")),
    uic!("tool.slide", "Slide Tool", [], Some("U")),
    uic!("tool.pen", "Pen Tool", [], Some("P")),
    uic!("tool.hand", "Hand Tool", [], Some("H")),
    uic!("tool.zoom", "Zoom Tool", [], Some("Z")),
    uic!("tool.type", "Type Tool", [], Some("T")),
    uic!("mode.import", "Import", [], None),
    uic!("mode.edit", "Edit", [], None),
    uic!("mode.export", "Export", ["File", "Export"], Some("Cmd+M")),
    uic!("app.about", "About FilmCraft", ["Help"], None),
    uic!("app.keyboardShortcuts", "Keyboard Shortcuts…", ["Edit"], Some("Cmd+Alt+K")),
    uic!("app.preferences.autoSave", "Auto Save…", ["Edit", "Preferences"], Some("Cmd+,")),
];

pub fn panel_command_id(p: PanelKind) -> String {
    format!("window.panel.{}", p.id())
}

/// Execute a UI or engine command by id.
pub fn invoke(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if let Some(rest) = id.strip_prefix("window.panel.") {
        let p = PanelKind::from_name(rest).ok_or_else(|| format!("unknown panel `{rest}`"))?;
        app.show_panel(p);
        return Ok(Value::Null);
    }
    if let Some(ws) = id.strip_prefix("window.workspace.") {
        if ws == "reset" {
            let name = app.ui.workspace.clone();
            app.set_workspace(&name);
            return Ok(Value::Null);
        }
        let name = crate::dock::WORKSPACES.iter().find(|w| w.to_ascii_lowercase().replace(' ', "") == ws).ok_or_else(|| format!("unknown workspace `{ws}`"))?;
        app.set_workspace(name);
        return Ok(json!({"workspace": name}));
    }
    if let Some(t) = id.strip_prefix("tool.") {
        let tool = Tool::from_name(t).ok_or_else(|| format!("unknown tool `{t}`"))?;
        app.ui.tool = tool;
        return Ok(json!({"tool": tool}));
    }
    if let Some(r) = crate::panels::trim_monitor::route_transport(app, ctx, id) {
        return r;
    }
    if let Some(r) = crate::panels::media_dialogs::route(app, id, &params) {
        if let Err(e) = &r {
            app.ui.status = e.clone();
        }
        return r;
    }
    match id {
        "playback.slowForward" | "playback.slowReverse" => {
            app.play(if id == "playback.slowForward" { 0.25 } else { -0.25 });
            return Ok(json!({"speed": app.playback.speed}));
        }
        "playback.toggle" => {
            app.toggle_play(1.0);
            return Ok(json!({"playing": app.playback.playing}));
        }
        "playback.forward" => {
            let s = if app.playback.playing && app.playback.speed > 0.0 { (app.playback.speed * 2.0).min(8.0) } else { 1.0 };
            app.play(s);
            return Ok(json!({"speed": app.playback.speed}));
        }
        "playback.reverse" => {
            let s = if app.playback.playing && app.playback.speed < 0.0 { (app.playback.speed * 2.0).max(-8.0) } else { -1.0 };
            app.play(s);
            return Ok(json!({"speed": app.playback.speed}));
        }
        "playback.stop" => {
            app.stop();
            return Ok(Value::Null);
        }
        "playback.inToOut" => {
            if let Some(i) = app.session.active_sequence().and_then(|q| q.mark_in) {
                app.session.set_playhead(i);
            }
            app.play(1.0);
            return Ok(Value::Null);
        }
        "playback.loop" => {
            app.playback.looping = !app.playback.looping;
            return Ok(json!({"loop": app.playback.looping}));
        }
        "view.zoomIn" | "view.zoomOut" => {
            let f = if id == "view.zoomIn" { 1.6 } else { 1.0 / 1.6 };
            let ph = app.session.playhead().seconds();
            crate::panels::timeline::zoom_about(&mut app.ui.timeline, f, ph, app.last_timeline_width);
            return Ok(Value::Null);
        }
        "view.zoomToSequence" => {
            app.ui.timeline.fit_pending = true;
            return Ok(Value::Null);
        }
        "view.safeMargins" => {
            app.ui.program.safe_margins = !app.ui.program.safe_margins;
            return Ok(Value::Null);
        }
        "mode.import" => {
            app.ui.mode = Mode::Import;
            return Ok(Value::Null);
        }
        "mode.edit" => {
            app.ui.mode = Mode::Edit;
            return Ok(Value::Null);
        }
        "mode.export" => {
            app.ui.mode = Mode::Export;
            return Ok(Value::Null);
        }
        "app.about" => {
            app.dialog = Some(crate::Dialog::About);
            return Ok(Value::Null);
        }
        "help.shortcuts" | "app.keyboardShortcuts" => {
            crate::panels::shortcuts_dialog::open(app);
            return Ok(Value::Null);
        }
        "app.preferences" | "app.preferences.autoSave" => {
            app.file_dialogs.prefs_draft = Some(app.session.prefs.auto_save.clone());
            app.dialog = Some(crate::Dialog::Preferences);
            return Ok(Value::Null);
        }
        // Audio Gain from the menu or G opens the dialog; with params it applies directly.
        "clip.audioGain" if params.as_object().is_none_or(|m| m.is_empty()) => {
            filmcraft_engine::find_command("clip.audioGain").map_or(Ok(()), |c| (c.enabled)(&app.session))?;
            if app.ui.audio_gain.mode.is_empty() {
                app.ui.audio_gain.mode = "adjust".into();
            }
            app.dialog = Some(crate::Dialog::AudioGain);
            return Ok(json!({"dialog": "audioGain"}));
        }
        // From menus/shortcuts (no params) these ask first; agents pass params to act directly.
        "file.revert" if params.as_object().is_none_or(|m| m.is_empty()) && app.session.is_dirty() => {
            if app.session.path.is_none() {
                return Err("the project has not been saved yet".into());
            }
            app.dialog = Some(crate::Dialog::RevertConfirm);
            return Ok(json!({"dialog": "revert"}));
        }
        "file.recover" if params.as_object().is_none_or(|m| m.is_empty()) => {
            if app.session.recovery_candidates().is_empty() {
                return Err("there are no unsaved changes to recover".into());
            }
            app.file_dialogs.recovery_choice = 0;
            app.dialog = Some(crate::Dialog::Recovery);
            return Ok(json!({"dialog": "recovery"}));
        }
        _ => {}
    }
    if let Some(r) = id.strip_prefix("view.playbackRes.") {
        let res = match r {
            "full" => PlaybackRes::Full,
            "half" => PlaybackRes::Half,
            "quarter" => PlaybackRes::Quarter,
            "eighth" => PlaybackRes::Eighth,
            _ => PlaybackRes::Sixteenth,
        };
        app.ui.program.res = res;
        return Ok(Value::Null);
    }
    if let Some(th) = id.strip_prefix("view.theme.") {
        let k = crate::theme::ThemeKind::from_name(th).ok_or("unknown theme")?;
        app.set_theme(ctx, k);
        return Ok(Value::Null);
    }
    // File dialogs for commands that need a path.
    if (id == "file.import" && params.get("paths").is_none() && params.get("path").is_none())
        || (id == "file.saveAs" && params.get("path").is_none())
        || (id == "file.saveCopy" && params.get("path").is_none())
        || (id == "file.open" && params.get("path").is_none())
        || (id == "file.save" && params.get("path").is_none() && app.session.path.is_none())
        || (matches!(id, "captions.import" | "captions.export") && params.get("path").is_none())
    {
        return app.file_dialog(id, &params);
    }
    let r = app.session.execute(id, params).map_err(|e| e.to_string());
    if let Err(e) = &r {
        app.ui.status = e.clone();
    }
    r
}

/// A menu tree entry for display / `ui.menu.list`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub path: Vec<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
}

pub const MENUS: [&str; 9] = ["File", "Edit", "Clip", "Sequence", "Markers", "Graphics and Titles", "View", "Window", "Help"];

pub fn menu_items(app: &FilmcraftApp) -> Vec<MenuItem> {
    menu_items_for(&app.session)
}

/// Menu entries with their live shortcuts and enablement.
pub fn menu_items_for(session: &filmcraft_engine::Session) -> Vec<MenuItem> {
    let mut out = Vec::new();
    for c in filmcraft_engine::command_specs() {
        if c.menu.is_empty() {
            continue;
        }
        out.push(MenuItem {
            id: c.id.into(),
            label: c.label.into(),
            path: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: session.shortcuts.primary(c.id),
            enabled: session.is_enabled(c.id),
        });
    }
    for c in UI_COMMANDS {
        if c.menu.is_empty() {
            continue;
        }
        out.push(MenuItem {
            id: c.id.into(),
            label: c.label.into(),
            path: c.menu.iter().map(|s| s.to_string()).collect(),
            shortcut: session.shortcuts.primary(c.id),
            enabled: true,
        });
    }
    for p in PanelKind::ALL {
        out.push(MenuItem {
            id: panel_command_id(p),
            label: p.title().into(),
            path: vec!["Window".into()],
            shortcut: session.shortcuts.primary(&panel_command_id(p)),
            enabled: true,
        });
    }
    out
}

/// Shortcut text for menus in this OS's notation (`⇧⌘K` on macOS, `Ctrl+Shift+K` elsewhere).
pub fn shortcut_text(s: &str) -> String {
    use filmcraft_engine::shortcuts::{Chord, Platform};
    Chord::parse(s).map(|c| c.display(Platform::current())).unwrap_or_else(|_| s.to_string())
}

/// Parse "Cmd+Shift+K" into modifiers + key.
pub fn parse_shortcut(s: &str) -> Option<(egui::Modifiers, egui::Key)> {
    let mut m = egui::Modifiers::NONE;
    let mut key = None;
    let parts: Vec<&str> = if s == "+" { vec!["+"] } else { s.split('+').collect() };
    for p in parts {
        match p {
            "Cmd" => m.command = true,
            "Shift" => m.shift = true,
            "Alt" => m.alt = true,
            "Ctrl" => m.ctrl = true,
            k => {
                key = match k {
                    ";" => Some(egui::Key::Semicolon),
                    "'" => Some(egui::Key::Quote),
                    "," => Some(egui::Key::Comma),
                    "." => Some(egui::Key::Period),
                    "/" => Some(egui::Key::Slash),
                    "\\" => Some(egui::Key::Backslash),
                    "=" => Some(egui::Key::Equals),
                    "-" => Some(egui::Key::Minus),
                    "`" => Some(egui::Key::Backtick),
                    _ => egui::Key::from_name(k),
                }
            }
        }
    }
    key.map(|k| (m, k))
}

/// One active key binding for the input loop: (modifiers, key, command id, panel or None).
pub type KeyBinding = (egui::Modifiers, egui::Key, String, Option<String>);

/// The active key bindings (from the engine's shortcut set), most specific first so Shift+I
/// doesn't also fire I.
pub fn bindings(app: &FilmcraftApp) -> Vec<KeyBinding> {
    let mut v: Vec<KeyBinding> =
        app.session.shortcuts.bindings.iter().filter_map(|b| parse_shortcut(&b.keys).map(|(m, k)| (m, k, b.command.clone(), b.panel.clone()))).collect();
    v.sort_by_key(|(m, ..)| std::cmp::Reverse(m.command as u8 + m.shift as u8 + m.alt as u8 + m.ctrl as u8));
    v
}

/// Frontend-owned commands, registered with the engine's shortcut set so they can be listed,
/// rebound and resolved alongside engine commands (the `shortcuts.` commands).
pub fn external_commands() -> Vec<filmcraft_engine::shortcuts::CommandInfo> {
    use filmcraft_engine::shortcuts::CommandInfo;
    let mut v: Vec<CommandInfo> = UI_COMMANDS.iter().map(|c| CommandInfo::new(c.id, c.label, c.menu, c.shortcut)).collect();
    for p in PanelKind::ALL {
        v.push(CommandInfo::new(&panel_command_id(p), p.title(), &["Window"], p.window_shortcut()));
    }
    v
}

/// Draw the in-window menu bar.
pub fn menu_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let items = menu_items(app);
    let ctx = ui.ctx().clone();
    let mut clicked: Option<String> = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for top in MENUS {
            let mine: Vec<&MenuItem> = items.iter().filter(|i| i.path.first().map(String::as_str) == Some(top)).collect();
            ui.menu_button(top, |ui| {
                ui.set_min_width(260.0);
                if mine.is_empty() {
                    ui.add_enabled(false, egui::Button::new("(empty)"));
                }
                let mut subs: Vec<&str> = Vec::new();
                for it in &mine {
                    if it.path.len() > 1 {
                        let sub = it.path[1].as_str();
                        if !subs.contains(&sub) {
                            subs.push(sub);
                            ui.menu_button(sub, |ui| {
                                ui.set_min_width(220.0);
                                for s in mine.iter().filter(|x| x.path.get(1).map(String::as_str) == Some(sub)) {
                                    if menu_entry(ui, s) {
                                        clicked = Some(s.id.clone());
                                        ui.close();
                                    }
                                }
                            });
                        }
                    } else if menu_entry(ui, it) {
                        clicked = Some(it.id.clone());
                        ui.close();
                    }
                }
            });
        }
    });
    if let Some(id) = clicked {
        let _ = invoke(app, &ctx, &id, json!({}));
    }
}

fn menu_entry(ui: &mut egui::Ui, it: &MenuItem) -> bool {
    let mut b = egui::Button::new(&it.label);
    if let Some(s) = &it.shortcut {
        b = b.shortcut_text(shortcut_text(s));
    }
    ui.add_enabled(it.enabled, b).clicked()
}
