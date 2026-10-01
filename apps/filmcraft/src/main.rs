//! FilmCraft desktop app.
//!
//! Usage: `filmcraft [--control <port>] [--demo|--empty] [--recover|--no-recover] [--data-dir <dir>]
//! [project.fcproj | media files…]`
//!
//! `--control <port>` (or `FILMCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server;
//! see `filmcraft_ui_egui::control` for the methods.
//!
//! Auto-save and the crash-recovery journal run in every session (data in `--data-dir`, else
//! `FILMCRAFT_DATA_DIR`, else the per-user application data folder). If a previous session died
//! with unsaved changes the app asks to recover them; `--recover` recovers the newest without
//! asking, `--no-recover` starts without asking (the changes stay available via File ▸ Recover
//! Unsaved Changes…).

mod audio;
mod control_server;
#[cfg(target_os = "macos")]
mod native_menu;

use chrono::TimeZone;
use filmcraft_engine::Session;
use filmcraft_engine::autosave::{AutosaveConfig, default_data_dir};
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn main() -> eframe::Result {
    let mut control_port: Option<u16> = std::env::var("FILMCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok());
    let mut files = Vec::new();
    let mut demo = true;
    let mut recover: Option<bool> = None;
    let mut data_dir = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = args.next().and_then(|p| p.parse().ok()),
            "--demo" => demo = true,
            "--empty" => demo = false,
            "--recover" => recover = Some(true),
            "--no-recover" => recover = Some(false),
            "--data-dir" => data_dir = args.next().map(std::path::PathBuf::from),
            "--version" => {
                println!("filmcraft {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => files.push(a),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FilmCraft")
            .with_inner_size([1600.0, 980.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true)
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };
    eframe::run_native(
        "FilmCraft",
        options,
        Box::new(move |cc| {
            let mut session = Session::default();
            if let Some(dir) = data_dir.clone().or_else(default_data_dir) {
                let mut cfg = AutosaveConfig::new(dir);
                cfg.local_offset = local_offset;
                if let Err(e) = session.start_autosave(cfg) {
                    eprintln!("filmcraft: auto-save and crash recovery unavailable: {e}");
                }
            }
            let project = files.iter().find(|f| f.ends_with(".fcproj")).cloned();
            if let Some(p) = project {
                if let Err(e) = session.execute("file.open", json!({"path": p})) {
                    eprintln!("filmcraft: {e}");
                }
            } else if demo {
                let _ = session.execute("file.openDemoProject", json!({}));
            }
            let media: Vec<String> = files.iter().filter(|f| !f.ends_with(".fcproj")).cloned().collect();
            if !media.is_empty() {
                let _ = session.execute("file.import", json!({"paths": media}));
            }
            if recover == Some(true) && !session.recovery_candidates().is_empty() {
                let id = session.recovery_candidates()[0].id.clone();
                match session.execute("file.recover", json!({"id": id})) {
                    Ok(r) => eprintln!("filmcraft: recovered {r}"),
                    Err(e) => eprintln!("filmcraft: recovery failed: {e}"),
                }
            }
            let mut app = FilmcraftApp::new(session);
            if recover == Some(false) {
                app.dialog = None;
            }
            app.integrated_titlebar = cfg!(target_os = "macos");
            if let Some(rs) = cc.wgpu_render_state.clone()
                && std::env::var_os("FILMCRAFT_CPU_COMPOSITE").is_none()
            {
                app.set_wgpu(rs);
            }
            if let Some(out) = audio::CpalOut::new() {
                app.audio = Some(Box::new(out));
            }
            app.hooks.pick_files = Some(Box::new(|exts: &[&str]| {
                rfd::FileDialog::new().add_filter("Media", exts).pick_files().unwrap_or_default().into_iter().map(|p| p.to_string_lossy().to_string()).collect()
            }));
            app.hooks.pick_save = Some(Box::new(|name: &str| {
                rfd::FileDialog::new().add_filter("FilmCraft Project", &["fcproj"]).set_file_name(name).save_file().map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_save_as = Some(Box::new(|filter: &str, exts: &[&str], name: &str| {
                rfd::FileDialog::new().add_filter(filter, exts).set_file_name(name).save_file().map(|p| p.to_string_lossy().to_string())
            }));
            app.hooks.pick_open_project =
                Some(Box::new(|| rfd::FileDialog::new().add_filter("FilmCraft Project", &["fcproj"]).pick_file().map(|p| p.to_string_lossy().to_string())));
            #[cfg(target_os = "macos")]
            {
                let rx = native_menu::install(&app, cc.egui_ctx.clone());
                app.command_inbox = Some(rx);
                app.ui.show_menu_bar = false;
            }
            if let Some(port) = control_port {
                let rx = control_server::start(port, cc.egui_ctx.clone());
                app = app.with_control(rx);
            }
            Ok(Box::new(app))
        }),
    )
}

/// Local UTC offset at a unix time (auto-save file names and recovery times use local time).
fn local_offset(unix: i64) -> i32 {
    chrono::Local.timestamp_opt(unix, 0).single().map(|d| d.offset().local_minus_utc()).unwrap_or(0)
}
