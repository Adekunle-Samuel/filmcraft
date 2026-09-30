//! Export mode (header "Export"): destination list, settings and a live preview. The encode
//! pipeline lands in M6; the page already shows the sequence preview and settings summary.

use egui::{Align2, Color32, Rect, pos2, vec2};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let left = Rect::from_min_size(rect.min, vec2(220.0, rect.height()));
    let right = Rect::from_min_max(pos2(rect.max.x - 360.0, rect.min.y), rect.max);
    let mid = Rect::from_min_max(pos2(left.max.x + 4.0, rect.min.y), pos2(right.min.x - 4.0, rect.max.y));
    for r in [left, mid, right] {
        ui.painter().rect_filled(r, t.radius, t.panel_bg);
    }
    ui.painter().text(left.min + vec2(14.0, 20.0), Align2::LEFT_CENTER, "Destinations", Tokens::semibold(13.0), t.text);
    for (i, d) in ["Media File", "YouTube", "Vimeo", "TikTok", "Instagram", "FTP"].iter().enumerate() {
        let r = Rect::from_min_size(left.min + vec2(8.0, 40.0 + i as f32 * 30.0), vec2(left.width() - 16.0, 26.0));
        if i == 0 {
            ui.painter().rect_filled(r, 4.0, t.row_selected);
        }
        ui.painter().text(pos2(r.min.x + 10.0, r.center().y), Align2::LEFT_CENTER, *d, Tokens::ui(12.5), t.text);
    }
    // preview
    if let Some(seq_id) = app.session.state.active_sequence {
        let q = app.session.active_sequence().expect("seq").clone();
        let rate = q.settings.frame_rate;
        let frame = rate.frame_at(app.session.playhead());
        let key = FrameKey { target: Target::Sequence(seq_id), frame, size: 500, revision: app.session.revision };
        let project = app.session.project.clone();
        app.frames.request(key, rate.tick_of(frame), 0.5, &project, 0);
        let pic = crate::panels::monitor::fit(mid.shrink(24.0), q.settings.width as f32, q.settings.height as f32);
        ui.painter().rect_filled(pic, 0.0, Color32::BLACK);
        if let Some(img) = app.frames.get(&key) {
            let ctx = ui.ctx().clone();
            let tex = app.texture_for(&ctx, "export-preview", key, &img);
            ui.painter().image(tex, pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // settings summary
        let lines = [
            ("File name", format!("{}.mp4", app.session.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default())),
            ("Preset", "Match Source - Adaptive High Bitrate".into()),
            ("Format", "H.264".into()),
            ("Video", format!("{}x{} · {} fps · VBR 1 pass, target 20 Mbps", q.settings.width, q.settings.height, q.settings.frame_rate.label())),
            ("Audio", format!("AAC · {} Hz · Stereo · 320 kbps", q.settings.sample_rate)),
            ("Range", "Entire Source".into()),
        ];
        let mut y = right.min.y + 22.0;
        ui.painter().text(pos2(right.min.x + 16.0, y), Align2::LEFT_CENTER, "Settings", Tokens::semibold(13.0), t.text);
        y += 28.0;
        for (k, v) in lines {
            ui.painter().text(pos2(right.min.x + 16.0, y), Align2::LEFT_CENTER, k, Tokens::ui(11.5), t.text_dim);
            ui.painter().text(pos2(right.min.x + 16.0, y + 16.0), Align2::LEFT_CENTER, v, Tokens::ui(12.5), t.text);
            y += 42.0;
        }
        let b = Rect::from_min_size(pos2(right.max.x - 120.0, right.max.y - 44.0), vec2(104.0, 30.0));
        ui.painter().rect_filled(b, 15.0, t.accent);
        ui.painter().text(b.center(), Align2::CENTER_CENTER, "Export", Tokens::semibold(13.0), Color32::WHITE);
        app.auto.add("export.button", b, "Export");
    } else {
        crate::dock::placeholder(ui, mid, &t, "Open a sequence to export");
    }
}
