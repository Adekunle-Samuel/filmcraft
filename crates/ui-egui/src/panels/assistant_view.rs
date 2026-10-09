//! Drawing the Assistant panel (first cut): the chat as plain rows, pending approvals with Allow /
//! Deny, and a composer. Logic lives in [`super::assistant`].

use egui::{Rect, RichText, pos2, vec2};

use super::assistant::{self as logic, ChatItem, NOT_AVAILABLE};
use crate::FilmcraftApp;
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    if !logic::available(app) {
        crate::dock::placeholder(ui, rect, &t, NOT_AVAILABLE);
        app.auto.add("assistant.unavailable", rect, NOT_AVAILABLE);
        return;
    }
    if rect.height() < 80.0 {
        return;
    }
    let composer = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.max.y - 70.0), pos2(rect.max.x - 8.0, rect.max.y - 6.0));
    let list = Rect::from_min_max(rect.min + vec2(8.0, 6.0), pos2(rect.max.x - 8.0, composer.min.y - 6.0));
    let mut approve: Option<(u64, bool)> = None;
    let mut reg: Vec<(String, Rect, String)> = Vec::new();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt("assistant-chat"));
    let rt = &app.assistant;
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(&mut child, |ui| {
        for (n, item) in rt.items.iter().enumerate() {
            let (text, color) = match item {
                ChatItem::User(s) => (format!("You: {s}"), t.text),
                ChatItem::Assistant(s) => (s.clone(), t.text),
                ChatItem::Thinking(s) => (format!("thinking… {s}"), t.text_faint),
                ChatItem::Notice { text, error } => (text.clone(), if *error { t.render_red } else { t.text_dim }),
                ChatItem::Tool(c) => (format!("[{}] {}", c.name, c.summary), t.text_dim),
            };
            let r = ui.add(egui::Label::new(RichText::new(text).color(color).size(12.5)).wrap()).rect;
            reg.push((format!("assistant.msg.{n}"), r, String::new()));
        }
        for a in &rt.approvals {
            ui.label(RichText::new(format!("Allow {}? {}", a.call.name, a.reason)).color(t.text));
            ui.horizontal(|ui| {
                let y = ui.button("Allow");
                let d = ui.button("Deny");
                reg.push((format!("assistant.approval.{}.allow", a.k), y.rect, "Allow".into()));
                reg.push((format!("assistant.approval.{}.deny", a.k), d.rect, "Deny".into()));
                if y.clicked() {
                    approve = Some((a.k, true));
                }
                if d.clicked() {
                    approve = Some((a.k, false));
                }
            });
        }
    });
    let field = Rect::from_min_max(composer.min, pos2(composer.max.x - 80.0, composer.max.y));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(field).id_salt("assistant-composer"));
    let resp = c.add_sized(field.size(), egui::TextEdit::multiline(&mut app.ui.panels.assistant.draft).font(Tokens::ui(13.0)));
    reg.push(("assistant.input".into(), resp.rect, "Message".into()));
    let br = Rect::from_min_size(pos2(field.max.x + 6.0, field.min.y), vec2(72.0, 26.0));
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(br).id_salt("assistant-send"));
    let running = app.assistant.running();
    let clicked = b.add_sized(br.size(), egui::Button::new(if running { "Cancel" } else { "Send" }));
    reg.push((if running { "assistant.cancel" } else { "assistant.send" }.into(), clicked.rect, String::new()));
    for (id, r, label) in reg {
        app.auto.add(&id, r, &label);
    }
    let ctx = ui.ctx().clone();
    let r = if let Some((k, allow)) = approve {
        logic::approve(app, k, allow)
    } else if clicked.clicked() && running {
        logic::cancel(app);
        Ok(())
    } else if clicked.clicked() {
        let text = app.ui.panels.assistant.draft.clone();
        logic::send(app, &ctx, &text).map(|()| app.ui.panels.assistant.draft.clear())
    } else {
        Ok(())
    };
    if let Err(e) = r {
        app.assistant.error = Some(e);
    }
}
