//! Audio Meters (stereo peak meters with dB scale and peak hold) and a basic Audio Track Mixer.
//! Levels are measured from the mixed sequence audio around the playhead.

use egui::{Align2, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

fn levels(app: &FilmcraftApp) -> [f32; 2] {
    let Some(seq) = app.session.active_sequence() else { return [-90.0; 2] };
    if !app.playback.playing {
        return [-90.0; 2];
    }
    let sr = seq.settings.sample_rate as i64;
    let s0 = app.session.playhead().to_units_floor(sr);
    let provider = app.session.media.provider(app.session.project.clone(), app.session.services.clone());
    let Some(seq_id) = app.session.state.active_sequence else { return [-90.0; 2] };
    let buf = app.session.previews.mix(&app.session.project, seq_id, s0, 1024, &provider);
    let p = buf.peaks();
    [20.0 * p.first().copied().unwrap_or(0.0).max(1e-6).log10(), 20.0 * p.get(1).copied().unwrap_or(0.0).max(1e-6).log10()]
}

/// Feed the loudness meter with exactly the programme audio played since the last frame
/// (consecutive, non-overlapping blocks, so gating and integration are correct). Seeks reset it.
fn feed_loudness(app: &mut FilmcraftApp) {
    if !app.playback.playing {
        return;
    }
    let Some(seq) = app.session.active_sequence() else { return };
    let sr = seq.settings.sample_rate as i64;
    let now = app.session.playhead().to_units_floor(sr);
    let fresh = match &app.loudness {
        Some((_, next)) => now < *next || now - *next > sr,
        None => true,
    };
    if fresh {
        app.loudness = Some((filmcraft_audio_dsp::LoudnessMeter::new(sr as f64, 2), now));
        return;
    }
    let next = app.loudness.as_ref().map_or(now, |l| l.1);
    if now <= next {
        return;
    }
    let provider = app.session.media.provider(app.session.project.clone(), app.session.services.clone());
    let Some(seq_id) = app.session.state.active_sequence else { return };
    let buf = app.session.previews.mix(&app.session.project, seq_id, next, (now - next) as usize, &provider);
    if let Some((m, n)) = app.loudness.as_mut() {
        let chans: Vec<&[f32]> = buf.channels.iter().take(2).map(Vec::as_slice).collect();
        if chans.len() == 2 {
            m.process(&chans);
        } else if let Some(c) = chans.first() {
            m.process(&[c, c]);
        }
        *n = now;
    }
}

fn lufs_text(v: f64) -> String {
    if v.is_finite() && v > -70.0 { format!("{v:.1}") } else { "—".into() }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    feed_loudness(app);
    let lv = levels(app);
    let id = egui::Id::new("meter-state");
    let now = ui.input(|i| i.time);
    let mut st: [f32; 4] = ui.data(|d| d.get_temp(id)).unwrap_or([-90.0; 4]);
    for c in 0..2 {
        // fast attack, 20 dB/s release; peak hold decays after 1.5 s
        st[c] = if lv[c] > st[c] { lv[c] } else { (st[c] - 20.0 * ui.input(|i| i.stable_dt)).max(-90.0) };
        st[c + 2] = if lv[c] > st[c + 2] { lv[c] } else { st[c + 2] - 6.0 * ui.input(|i| i.stable_dt) };
    }
    ui.data_mut(|d| d.insert_temp(id, st));
    let _ = now;
    // Premiere: black meter area, scale 0 … −57 dB in 3 dB steps on the right, "dB" at the foot.
    // Loudness readout (BS.1770 / EBU R128) under the bars.
    let lufs_h = if rect.height() > 260.0 { 64.0 } else { 0.0 };
    let area = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.min.y + 8.0), pos2(rect.max.x - 26.0, rect.max.y - 30.0 - lufs_h));
    if lufs_h > 0.0 {
        let (m, s, i, tp) = app.loudness.as_ref().map_or((f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY), |(l, _)| {
            (l.momentary(), l.short_term(), l.integrated(), l.true_peak_dbtp())
        });
        let lr = Rect::from_min_max(pos2(rect.min.x + 4.0, rect.max.y - lufs_h), pos2(rect.max.x - 4.0, rect.max.y - 4.0));
        ui.painter().rect_filled(lr, 2.0, egui::Color32::from_rgb(0x14, 0x14, 0x14));
        let rows = [("M", lufs_text(m)), ("S", lufs_text(s)), ("I", lufs_text(i)), ("TP", lufs_text(tp))];
        for (k, (label, val)) in rows.iter().enumerate() {
            let y = lr.min.y + 8.0 + k as f32 * 14.0;
            ui.painter().text(pos2(lr.min.x + 6.0, y), Align2::LEFT_CENTER, *label, Tokens::ui(9.5), t.text_dim);
            let hot = *label == "I" || (*label == "TP" && tp > -1.0);
            let col = if *label == "TP" && tp > -1.0 {
                egui::Color32::from_rgb(0xe0, 0x50, 0x40)
            } else if hot {
                t.hot_text
            } else {
                t.text
            };
            ui.painter().text(
                pos2(lr.max.x - 6.0, y),
                Align2::RIGHT_CENTER,
                format!("{val} {}", if *label == "TP" { "dBTP" } else { "LUFS" }),
                Tokens::mono(9.5),
                col,
            );
        }
        app.auto.add("audioMeters.loudness", lr, &format!("M {} S {} I {} LUFS TP {} dBTP", lufs_text(m), lufs_text(s), lufs_text(i), lufs_text(tp)));
    }
    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.min.x + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 4.0, area.max.y + 4.0)), 0.0, egui::Color32::BLACK);
    let w = ((area.width() - 4.0) / 2.0).max(3.0);
    for c in 0..2 {
        let r = Rect::from_min_size(pos2(area.min.x + c as f32 * (w + 4.0), area.min.y), vec2(w, area.height()));
        crate::widgets::meter_bar(ui.painter(), r, st[c], st[c + 2], &t);
    }
    let mut db = 0;
    while db >= -57 {
        let y = area.min.y + area.height() * (-db as f32 / 60.0);
        ui.painter().text(pos2(rect.max.x - 6.0, y), Align2::RIGHT_CENTER, format!("{db}"), Tokens::ui(8.5), t.text_dim);
        db -= 3;
    }
    ui.painter().text(pos2(rect.max.x - 6.0, area.max.y + 10.0), Align2::RIGHT_CENTER, "dB", Tokens::ui(8.5), t.text_dim);
    for c in 0..2 {
        let r = Rect::from_center_size(pos2(area.min.x + c as f32 * (w + 4.0) + w / 2.0, area.max.y + 16.0), vec2(14.0, 14.0));
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "S", Tokens::ui(10.0), t.text_dim);
    }
    if app.playback.playing {
        ui.ctx().request_repaint();
    }
    app.auto.add("audioMeters", area, "Audio Meters");
}

pub fn track_mixer(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, "(no sequence)");
        return;
    };
    let strip_w = 86.0;
    let mut actions = Vec::new();
    for (i, tr) in seq.audio_tracks.iter().enumerate().chain(std::iter::empty()) {
        let x = rect.min.x + 8.0 + i as f32 * (strip_w + 4.0);
        if x + strip_w > rect.max.x {
            break;
        }
        let strip = Rect::from_min_max(pos2(x, rect.min.y + 6.0), pos2(x + strip_w, rect.max.y - 6.0));
        ui.painter().rect_filled(strip, 4.0, t.tl_header_bg);
        ui.painter().text(pos2(strip.center().x, strip.min.y + 12.0), Align2::CENTER_CENTER, format!("A{}", i + 1), Tokens::semibold(11.0), t.text);
        // pan knob as hot number
        let mut pan_ui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(strip.min.x + 20.0, strip.min.y + 26.0), vec2(60.0, 18.0))));
        let (_, np) = crate::widgets::hot_number(&mut pan_ui, egui::Id::new(("pan", tr.id.0)), tr.pan, 1.0, (-100.0, 100.0), 0, "", &t);
        if let Some(np) = np {
            actions.push(json!({"track": tr.id.0, "pan": np}));
        }
        // fader
        let fader = Rect::from_min_max(pos2(strip.center().x - 3.0, strip.min.y + 54.0), pos2(strip.center().x + 3.0, strip.max.y - 40.0));
        ui.painter().rect_filled(fader, 3.0, t.field_bg);
        let norm = ((tr.volume_db + 60.0) / 66.0).clamp(0.0, 1.0) as f32;
        let ky = fader.max.y - norm * fader.height();
        let knob = Rect::from_center_size(pos2(fader.center().x, ky), vec2(26.0, 10.0));
        let resp = ui.interact(knob.expand(4.0), egui::Id::new(("fader", tr.id.0)), Sense::drag());
        ui.painter().rect_filled(knob, 2.0, if resp.dragged() { t.accent } else { egui::Color32::from_rgb(160, 160, 160) });
        if resp.dragged() {
            let nn = (norm - resp.drag_delta().y / fader.height()).clamp(0.0, 1.0);
            actions.push(json!({"track": tr.id.0, "volumeDb": nn as f64 * 66.0 - 60.0}));
        }
        app.auto.add(&format!("mixer.A{}.fader", i + 1), knob, "volume");
        ui.painter().text(pos2(strip.center().x, strip.max.y - 26.0), Align2::CENTER_CENTER, format!("{:.1}", tr.volume_db), Tokens::ui(11.0), t.hot_text);
        ui.painter().text(pos2(strip.center().x, strip.max.y - 10.0), Align2::CENTER_CENTER, &tr.name, Tokens::ui(10.0), t.text_dim);
    }
    for a in actions {
        let _ = app.session.execute("timeline.setTrack", a);
    }
}
