//! FilmCraft desktop app.
//!
//! Usage: `filmcraft [--control <port>] [--demo] [project.fcproj | media files…]`
//!
//! `--control <port>` (or `FILMCRAFT_CONTROL_PORT`) starts a localhost JSON-lines control server;
//! see `filmcraft_ui_egui::control` for the methods.

mod audio;
mod control_server;
#[cfg(target_os = "macos")]
mod native_menu;

use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn main() -> eframe::Result {
    let mut control_port: Option<u16> = std::env::var("FILMCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok());
    let mut files = Vec::new();
    let mut demo = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => control_port = args.next().and_then(|p| p.parse().ok()),
            "--demo" => demo = true,
            "--empty" => demo = false,
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
            let mut app = FilmcraftApp::new(session);
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
