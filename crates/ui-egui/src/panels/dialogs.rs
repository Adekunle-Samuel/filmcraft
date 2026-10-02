//! Modal dialogs (About; Keyboard Shortcuts in `shortcuts_dialog`; Preferences/Recovery/Revert in
//! `file_dialogs`).

use crate::{Dialog, FilmcraftApp};

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    crate::panels::media_dialogs::show(app, ctx);
    crate::panels::color_dialogs::show(app, ctx);
    crate::panels::clip_dialogs::show(app, ctx);
    crate::panels::presets::save_dialog(app, ctx);
    crate::panels::multicam::show_dialog(app, ctx);
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
            if !crate::panels::shortcuts_dialog::show(app, ctx) && app.dialog == Some(d) {
                app.dialog = None;
            }
            return;
        }
        Dialog::AudioGain => {
            if !audio_gain(app, ctx) {
                app.dialog = None;
            }
            return;
        }
        Dialog::NewSequence | Dialog::Preferences | Dialog::Recovery | Dialog::RevertConfirm => {}
    }
    if !open {
        app.dialog = None;
    }
}

/// Clip ▸ Audio Gain…: Set Gain to / Adjust Gain by / Normalize Max Peak to / Normalize All Peaks to,
/// with the selection's peak amplitude. Returns whether the dialog stays open.
///
/// Automation ids: `audioGain.<set|adjust|normalizeMax|normalizeAll>` (radio buttons),
/// `audioGain.<mode>.value` (dB fields), `audioGain.peak`, `audioGain.ok`, `audioGain.cancel`.
fn audio_gain(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let peak_id = egui::Id::new("audio-gain-peak");
    let rev = app.session.revision;
    let cached: Option<(u64, Option<f64>)> = ctx.data(|d| d.get_temp(peak_id));
    let peak = match cached {
        Some((r, p)) if r == rev => p,
        _ => {
            let p = app.session.execute("clip.audioPeak", serde_json::json!({})).ok().and_then(|v| v["peakDb"].as_f64());
            ctx.data_mut(|d| d.insert_temp(peak_id, (rev, p)));
            p
        }
    };
    let mut draft = app.ui.audio_gain.clone();
    let mut keep = true;
    let mut apply = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new("Audio Gain").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.add_space(6.0);
        egui::Grid::new("audio-gain").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            let rows: [(&str, &str, &mut f64); 4] = [
                ("set", "Set Gain to:", &mut draft.set_db),
                ("adjust", "Adjust Gain by:", &mut draft.adjust_db),
                ("normalizeMax", "Normalize Max Peak to:", &mut draft.max_peak_db),
                ("normalizeAll", "Normalize All Peaks to:", &mut draft.all_peaks_db),
            ];
            for (id, label, v) in rows {
                let r = ui.radio(draft.mode == id, label);
                elems.push((format!("audioGain.{id}"), r.rect, label.to_string()));
                if r.clicked() {
                    draft.mode = id.to_string();
                }
                let f = ui.add_enabled(draft.mode == id, egui::DragValue::new(v).speed(0.1).range(-96.0..=96.0).suffix(" dB"));
                elems.push((format!("audioGain.{id}.value"), f.rect, format!("{v:.1} dB")));
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let pk_text = format!("Peak Amplitude: {}", peak.map(|p| format!("{p:.1} dB")).unwrap_or_else(|| "—".into()));
        let pk = ui.label(&pk_text);
        elems.push(("audioGain.peak".into(), pk.rect, pk_text));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let c = ui.button("Cancel");
            elems.push(("audioGain.cancel".into(), c.rect, "Cancel".into()));
            if c.clicked() {
                keep = false;
            }
            let o = ui.add(egui::Button::new(egui::RichText::new("OK").color(egui::Color32::WHITE)).fill(app.tokens.accent));
            elems.push(("audioGain.ok".into(), o.rect, "OK".into()));
            if o.clicked() {
                apply = true;
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep = false;
    }
    if apply {
        let db = match draft.mode.as_str() {
            "set" => draft.set_db,
            "normalizeMax" => draft.max_peak_db,
            "normalizeAll" => draft.all_peaks_db,
            _ => draft.adjust_db,
        };
        if let Err(e) = app.session.execute("clip.audioGain", serde_json::json!({"mode": draft.mode, "db": db})) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.audio_gain = draft;
    keep
}
