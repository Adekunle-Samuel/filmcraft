//! Drawing the Assistant panel: header, chat (bubbles, streamed text, thinking notes, tool cards,
//! plan cards, approval cards), composer and usage line.
//! Every interactive widget registers an `assistant.*` automation id. Logic lives in
//! [`super::assistant`].

use egui::text::LayoutJob;
use egui::{Align, Align2, Color32, FontId, Key, Modifiers, Rect, RichText, Sense, Stroke, TextFormat, pos2, vec2};
use serde_json::Value;

use super::assistant::{self as logic, ChatItem, NOT_AVAILABLE, PlanStatus, ToolCard};
use super::assistant_host::pretty_tool_name;
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Something the user did this frame, applied after drawing.
enum Action {
    Send,
    Cancel,
    New,
    ToggleSettings,
    Approve(u64, bool),
    Plan(usize, bool),
    ToggleRaw(String),
    Suggest(&'static str),
}

const HEADER_H: f32 = 30.0;
const SUGGESTIONS: [&str; 3] =
    ["Remove the silences and ums from this interview", "Cut this down to about 3 minutes and add captions", "Give me an overview of the project"];

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let mut actions = Vec::new();
    header(app, ui, rect, &mut actions);
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + HEADER_H), rect.max);
    if body.height() < 20.0 || body.width() < 60.0 {
        return;
    }
    if !logic::available(app) {
        let r = Rect::from_center_size(body.center(), vec2(body.width() - 24.0, 60.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(Align::Center)));
        child.label(RichText::new(NOT_AVAILABLE).color(t.text_dim).size(12.5));
        app.auto.add("assistant.unavailable", r, NOT_AVAILABLE);
    } else if app.ui.panels.assistant.show_settings || !app.ui.panels.assistant.settings.consented {
        crate::dock::placeholder(ui, body, &t, "Assistant settings and consent arrive with A3.3");
    } else {
        chat(app, ui, body, &mut actions);
    }
    apply(app, ui.ctx(), actions);
}

fn apply(app: &mut FilmcraftApp, ctx: &egui::Context, actions: Vec<Action>) {
    for a in actions {
        let r = match a {
            Action::Send => {
                let text = app.ui.panels.assistant.draft.clone();
                logic::send(app, ctx, &text).map(|()| app.ui.panels.assistant.draft.clear())
            }
            Action::Cancel => {
                logic::cancel(app);
                Ok(())
            }
            Action::New => {
                logic::reset(app);
                Ok(())
            }
            Action::ToggleSettings => {
                let s = &mut app.ui.panels.assistant.show_settings;
                *s = !*s;
                Ok(())
            }
            Action::Approve(k, allow) => logic::approve(app, k, allow),
            Action::Plan(k, apply) => logic::plan_action(app, ctx, k, apply),
            Action::ToggleRaw(id) => {
                for i in &mut app.assistant.items {
                    if let ChatItem::Tool(c) = i
                        && c.id == id
                    {
                        c.expanded = !c.expanded;
                    }
                }
                Ok(())
            }
            Action::Suggest(s) => {
                app.ui.panels.assistant.draft = s.to_string();
                Ok(())
            }
        };
        if let Err(e) = r {
            app.assistant.error = Some(e);
        }
    }
}

fn icon_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon: Icon, id: &str, label: &str, on: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::new(("assistant-btn", id)), Sense::click()).on_hover_text(label);
    if resp.hovered() || on {
        ui.painter().rect_filled(r, 3.0, if on { t.pressed } else { t.hover });
    }
    icons::paint(ui.painter(), r.shrink(5.0), icon, if on { t.icon_active } else { t.icon });
    app.auto.add(id, r, label);
    resp.clicked()
}

fn header(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, actions: &mut Vec<Action>) {
    let t = app.tokens;
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
    ui.painter().line_segment([pos2(bar.min.x, bar.max.y - 0.5), pos2(bar.max.x, bar.max.y - 0.5)], Stroke::new(1.0, t.separator));
    let icon_r = Rect::from_min_size(pos2(bar.min.x + 10.0, bar.center().y - 8.0), vec2(16.0, 16.0));
    icons::paint(ui.painter(), icon_r, Icon::Sparkle, t.hot_text);
    let s = &app.ui.panels.assistant.settings;
    let sub = if logic::available(app) { format!("{} · {}", s.model, s.host()) } else { String::new() };
    let title = ui.painter().text(pos2(icon_r.max.x + 6.0, bar.center().y), Align2::LEFT_CENTER, "Assistant", Tokens::semibold(12.5), t.text);
    let right = bar.max.x - 8.0 - 2.0 * 26.0;
    if !sub.is_empty() && right - title.max.x > 60.0 {
        let galley = ui.painter().layout(sub, Tokens::ui(11.0), t.text_faint, right - title.max.x - 16.0);
        let row = galley.rows.first().map(|r| r.text()).unwrap_or_default();
        let max_chars = ((right - title.max.x - 16.0) / 6.0).max(4.0) as usize;
        let shown: String =
            if row.chars().count() > max_chars { format!("{}…", row.chars().take(max_chars.saturating_sub(1)).collect::<String>()) } else { row };
        ui.painter().text(pos2(title.max.x + 8.0, bar.center().y), Align2::LEFT_CENTER, shown, Tokens::ui(11.0), t.text_faint);
    }
    let gear = Rect::from_min_size(pos2(bar.max.x - 8.0 - 24.0, bar.center().y - 12.0), vec2(24.0, 24.0));
    let settings_open = app.ui.panels.assistant.show_settings;
    if icon_button(app, ui, gear, Icon::Gear, "assistant.settings", "Assistant settings", settings_open) {
        actions.push(Action::ToggleSettings);
    }
    if logic::available(app) {
        let new = gear.translate(vec2(-26.0, 0.0));
        if icon_button(app, ui, new, Icon::Plus, "assistant.new", "New conversation", false) {
            actions.push(Action::New);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// chat

fn chat(app: &mut FilmcraftApp, ui: &mut egui::Ui, body: Rect, actions: &mut Vec<Action>) {
    let t = app.tokens;
    let composer_h = 104.0f32.min(body.height() * 0.6);
    let composer = Rect::from_min_max(pos2(body.min.x, body.max.y - composer_h), body.max);
    let list = Rect::from_min_max(body.min, pos2(body.max.x, composer.min.y));
    let mut reg: Vec<(String, Rect, String)> = Vec::new();

    let scroll_now = std::mem::take(&mut app.ui.panels.assistant.scroll_to_bottom);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink2(vec2(8.0, 6.0))).id_salt("assistant-chat"));
    let rt = &app.assistant;
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).id_salt("assistant-scroll").show(&mut child, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        if rt.items.is_empty() && rt.approvals.is_empty() {
            empty_state(ui, &t, &mut reg, actions);
        }
        let mut plan_k = 0usize;
        for (n, item) in rt.items.iter().enumerate() {
            let r = match item {
                ChatItem::User(text) => user_bubble(ui, &t, text),
                ChatItem::Assistant(text) => {
                    let job = markdown_job(text, ui.available_width(), &t);
                    ui.add(egui::Label::new(job).wrap().selectable(true)).rect
                }
                ChatItem::Thinking(text) => {
                    let short: String = text.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
                    ui.add(egui::Label::new(RichText::new(format!("thinking… {}", short.trim())).italics().color(t.text_faint).size(11.5)).wrap()).rect
                }
                ChatItem::Notice { text, error } => {
                    ui.add(egui::Label::new(RichText::new(text).color(if *error { t.render_red } else { t.text_dim }).size(12.0)).wrap()).rect
                }
                ChatItem::Tool(card) => {
                    let k = card.plan.map(|_| {
                        plan_k += 1;
                        plan_k - 1
                    });
                    let r = tool_card(ui, &t, card, k, &mut reg, actions);
                    reg.push((format!("assistant.tool.{}", card.id), r, pretty_tool_name(&card.name)));
                    r
                }
            };
            let label: String = match item {
                ChatItem::User(s) | ChatItem::Assistant(s) | ChatItem::Thinking(s) => s.chars().take(60).collect(),
                ChatItem::Notice { text, .. } => text.chars().take(60).collect(),
                ChatItem::Tool(c) => pretty_tool_name(&c.name),
            };
            reg.push((format!("assistant.msg.{n}"), r, label));
        }
        for a in &rt.approvals {
            approval_card(ui, &t, a, &mut reg, actions);
        }
        if rt.running() && rt.approvals.is_empty() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0).color(t.text_dim));
                ui.label(RichText::new("Working…").color(t.text_dim).size(11.5));
            });
        }
        if let Some(e) = &rt.error {
            ui.add(egui::Label::new(RichText::new(e).color(t.render_red).size(12.0)).wrap());
        }
        if scroll_now {
            ui.scroll_to_cursor(Some(Align::BOTTOM));
        }
    });
    composer_ui(app, ui, composer, &mut reg, actions);
    for (id, r, label) in reg {
        app.auto.add(&id, r, &label);
    }
}

fn empty_state(ui: &mut egui::Ui, t: &Tokens, reg: &mut Vec<(String, Rect, String)>, actions: &mut Vec<Action>) {
    ui.add_space(12.0);
    ui.label(RichText::new("Ask for an edit").color(t.text).size(14.0).strong());
    ui.add(
        egui::Label::new(
            RichText::new("The Assistant reads your project, transcript and audio, proposes a plan you can review, and applies it as one undo step.")
                .color(t.text_dim)
                .size(12.0),
        )
        .wrap(),
    );
    ui.add_space(4.0);
    for (i, s) in SUGGESTIONS.iter().enumerate() {
        let resp =
            ui.add(egui::Button::new(RichText::new(*s).size(12.0).color(t.text)).fill(t.row_alt).stroke(Stroke::new(1.0, t.separator)).corner_radius(12.0));
        reg.push((format!("assistant.suggestion.{i}"), resp.rect, s.to_string()));
        if resp.clicked() {
            actions.push(Action::Suggest(s));
        }
    }
}

/// The assistant's text with the inline Markdown models use most: `**bold**` and `` `code` ``
/// (headings' `#` and list markers are left as typed).
pub fn markdown_job(text: &str, width: f32, t: &Tokens) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let plain = TextFormat::simple(Tokens::ui(13.0), t.text);
    let bold = TextFormat::simple(Tokens::semibold(13.0), t.text);
    let code = TextFormat { font_id: Tokens::mono(12.0), color: t.hot_text, background: t.field_bg, ..Default::default() };
    let (mut is_bold, mut is_code) = (false, false);
    let mut rest = text;
    while !rest.is_empty() {
        let next = if is_code {
            rest.find('`').map(|i| (i, 1))
        } else {
            [rest.find("**").map(|i| (i, 2)), rest.find('`').map(|i| (i, 1))].into_iter().flatten().min()
        };
        let Some((i, len)) = next else {
            job.append(
                rest,
                0.0,
                if is_code {
                    code.clone()
                } else if is_bold {
                    bold.clone()
                } else {
                    plain.clone()
                },
            );
            break;
        };
        let (before, after) = rest.split_at(i);
        if !before.is_empty() {
            job.append(
                before,
                0.0,
                if is_code {
                    code.clone()
                } else if is_bold {
                    bold.clone()
                } else {
                    plain.clone()
                },
            );
        }
        if len == 1 {
            is_code = !is_code;
        } else {
            is_bold = !is_bold;
        }
        rest = after.get(len..).unwrap_or("");
    }
    job
}

fn user_bubble(ui: &mut egui::Ui, t: &Tokens, text: &str) -> Rect {
    let max_w = (ui.available_width() * 0.85).max(80.0);
    ui.with_layout(egui::Layout::right_to_left(Align::TOP), |ui| {
        egui::Frame::NONE
            .fill(t.row_selected)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.add(egui::Label::new(RichText::new(text).color(t.text).size(13.0)).wrap().selectable(true));
            })
            .response
            .rect
    })
    .inner
}

/// One line of the input: `key: value, …` (strings quoted short, objects summarized).
fn input_summary(v: &Value) -> String {
    let Some(o) = v.as_object() else { return String::new() };
    let parts: Vec<String> = o
        .iter()
        .take(6)
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) => {
                    let one: String = s.chars().filter(|c| !c.is_control()).take(40).collect();
                    if s.chars().count() > 40 { format!("\"{one}…\"") } else { format!("\"{one}\"") }
                }
                Value::Array(a) => format!("[{}]", a.len()),
                Value::Object(m) => format!("{{{}}}", m.len()),
                other => other.to_string(),
            };
            format!("{k}: {val}")
        })
        .collect();
    let s = parts.join(", ");
    if s.chars().count() > 120 { format!("{}…", s.chars().take(119).collect::<String>()) } else { s }
}

fn pretty_json(v: &Value, max: usize) -> String {
    let s = serde_json::to_string_pretty(v).unwrap_or_default();
    if s.chars().count() > max { format!("{}\n… ({} characters)", s.chars().take(max).collect::<String>(), s.chars().count()) } else { s }
}

fn status_mark(ui: &egui::Ui, r: Rect, ok: bool, t: &Tokens) {
    let p = ui.painter();
    if ok {
        let c = t.render_green;
        p.line_segment([pos2(r.min.x + 2.0, r.center().y), pos2(r.min.x + 5.0, r.max.y - 2.5)], Stroke::new(1.6, c));
        p.line_segment([pos2(r.min.x + 5.0, r.max.y - 2.5), pos2(r.max.x - 1.5, r.min.y + 2.0)], Stroke::new(1.6, c));
    } else {
        let c = t.render_red;
        let s = r.shrink(2.5);
        p.line_segment([s.left_top(), s.right_bottom()], Stroke::new(1.6, c));
        p.line_segment([s.right_top(), s.left_bottom()], Stroke::new(1.6, c));
    }
}

fn tool_card(ui: &mut egui::Ui, t: &Tokens, c: &ToolCard, plan_k: Option<usize>, reg: &mut Vec<(String, Rect, String)>, actions: &mut Vec<Action>) -> Rect {
    let stroke = if c.ok == Some(false) { t.render_red.gamma_multiply(0.6) } else { t.separator };
    egui::Frame::NONE
        .fill(t.field_bg)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(9, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                match c.ok {
                    None => {
                        ui.put(r, egui::Spinner::new().size(12.0).color(t.text_dim));
                    }
                    Some(ok) => status_mark(ui, r, ok, t),
                }
                ui.label(RichText::new(pretty_tool_name(&c.name)).font(Tokens::semibold(12.5)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let resp = ui
                        .add(egui::Button::new(RichText::new(if c.expanded { "Hide details" } else { "Details" }).size(11.0).color(t.text_faint)).frame(false));
                    reg.push((format!("assistant.tool.{}.raw", c.id), resp.rect, "Details".into()));
                    if resp.clicked() {
                        actions.push(Action::ToggleRaw(c.id.clone()));
                    }
                });
            });
            let args = input_summary(&c.input);
            if !args.is_empty() {
                ui.add(egui::Label::new(RichText::new(args).font(Tokens::mono(11.0)).color(t.text_faint)).truncate());
            }
            if c.running
                && let Some(p) = &c.progress
            {
                let mut bar = egui::ProgressBar::new(p.fraction.unwrap_or(0.0)).desired_height(6.0).fill(t.accent);
                if p.fraction.is_none() {
                    bar = bar.animate(true);
                }
                ui.add(bar);
                if !p.status.is_empty() {
                    let pct = p.fraction.map(|f| format!(" · {:.0}%", f * 100.0)).unwrap_or_default();
                    ui.label(RichText::new(format!("{}{pct}", p.status)).size(11.0).color(t.text_dim));
                }
            }
            if !c.summary.is_empty() && !c.running {
                if c.ok == Some(false) {
                    ui.add(egui::Label::new(RichText::new(&c.summary).size(11.5).color(t.render_red)).wrap());
                } else {
                    // results are JSON for the model: one dim line here, the rest under Details
                    ui.add(egui::Label::new(RichText::new(&c.summary).font(Tokens::mono(10.5)).color(t.text_faint)).truncate());
                }
            }
            if let (Some(status), Some(k)) = (c.plan, plan_k) {
                plan_section(ui, t, c, status, k, reg, actions);
            }
            if c.expanded {
                ui.separator();
                ui.label(RichText::new("Input").size(11.0).color(t.text_faint));
                ui.add(egui::Label::new(RichText::new(pretty_json(&c.input, 4000)).font(Tokens::mono(10.5)).color(t.text_dim)).wrap());
                if let Some(r) = &c.result {
                    ui.label(RichText::new("Result").size(11.0).color(t.text_faint));
                    ui.add(egui::Label::new(RichText::new(pretty_json(r, 8000)).font(Tokens::mono(10.5)).color(t.text_dim)).wrap());
                }
            }
        })
        .response
        .rect
}

/// What a `propose_edit_plan` result says, read leniently (fields may be missing or renamed).
#[derive(Debug, Default, PartialEq)]
pub struct PlanView {
    pub before_s: Option<f64>,
    pub after_s: Option<f64>,
    /// (text, removed, reason)
    pub spans: Vec<(String, bool, Option<String>)>,
    pub warnings: Vec<String>,
    pub removed_count: Option<usize>,
}

fn num(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_f64)).filter(|f| f.is_finite())
}

impl PlanView {
    pub fn from_json(v: &Value) -> Self {
        // the preview may be the result itself or nested
        let root = ["preview", "result"].iter().find_map(|k| v.get(*k).filter(|x| x.is_object())).unwrap_or(v);
        let dur = root.get("duration").filter(|d| d.is_object());
        let before_s =
            num(root, &["durationBefore", "beforeSeconds", "duration_before", "before"]).or_else(|| dur.and_then(|d| num(d, &["before", "beforeSeconds"])));
        let after_s = num(root, &["durationAfter", "afterSeconds", "duration_after", "after"]).or_else(|| dur.and_then(|d| num(d, &["after", "afterSeconds"])));
        let warnings = root
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(20)
            .filter_map(|w| w.as_str().map(str::to_string).or_else(|| w.get("message").and_then(Value::as_str).map(str::to_string)))
            .collect();
        let mut spans = Vec::new();
        let diff = ["transcriptDiff", "diff", "spans", "words"].iter().find_map(|k| root.get(*k).and_then(Value::as_array));
        for s in diff.into_iter().flatten().take(2000) {
            let Some(text) = s.get("text").or_else(|| s.get("word")).and_then(Value::as_str) else { continue };
            let removed = s.get("removed").and_then(Value::as_bool).unwrap_or_else(|| {
                s.get("kind").or_else(|| s.get("op")).and_then(Value::as_str).is_some_and(|k| matches!(k, "remove" | "removed" | "cut" | "delete"))
            });
            let reason = s.get("reason").and_then(Value::as_str).map(str::to_string);
            spans.push((text.to_string(), removed, reason));
        }
        if spans.is_empty() {
            // a list of removals only
            let removals = ["removed", "removals", "cuts"].iter().find_map(|k| root.get(*k).and_then(Value::as_array));
            for s in removals.into_iter().flatten().take(500) {
                let text = s.get("text").and_then(Value::as_str).map(str::to_string).or_else(|| {
                    let a = num(s, &["start", "startSeconds", "from"])?;
                    let b = num(s, &["end", "endSeconds", "to"])?;
                    Some(format!("{a:.2}–{b:.2} s"))
                });
                if let Some(text) = text {
                    spans.push((text, true, s.get("reason").and_then(Value::as_str).map(str::to_string)));
                }
            }
        }
        let removed_count = ["removedCount", "cutCount", "ranges"]
            .iter()
            .find_map(|k| root.get(*k).and_then(|v| v.as_u64().or_else(|| v.as_array().map(|a| a.len() as u64))))
            .and_then(|n| usize::try_from(n).ok());
        PlanView { before_s, after_s, spans, warnings, removed_count }
    }
}

fn fmt_dur(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as u64, s - m * 60.0)
}

fn plan_section(ui: &mut egui::Ui, t: &Tokens, c: &ToolCard, status: PlanStatus, k: usize, reg: &mut Vec<(String, Rect, String)>, actions: &mut Vec<Action>) {
    let view = c.result.as_ref().map(PlanView::from_json).unwrap_or_default();
    ui.add_space(2.0);
    ui.separator();
    let mut head = "Edit plan".to_string();
    if let (Some(b), Some(a)) = (view.before_s, view.after_s) {
        head = format!("Edit plan · {} → {}", fmt_dur(b), fmt_dur(a));
        if b > 0.0 {
            head.push_str(&format!(" ({:+.0}%)", (a - b) / b * 100.0));
        }
    } else if let Some(n) = view.removed_count {
        head = format!("Edit plan · {n} cuts");
    }
    ui.label(RichText::new(head).font(Tokens::semibold(12.0)).color(t.text));
    if let (Some(b), Some(a)) = (view.before_s, view.after_s)
        && b > 0.0
    {
        // keep/remove bar against the duration
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, t.render_red.gamma_multiply(0.5));
        let keep = (a / b).clamp(0.0, 1.0) as f32;
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * keep, r.height())), 2.0, t.render_green);
    }
    if !view.spans.is_empty() {
        let mut job = LayoutJob::default();
        job.wrap.max_width = ui.available_width();
        let mut chars = 0usize;
        for (text, removed, reason) in &view.spans {
            if chars > 6000 {
                job.append(" …", 0.0, TextFormat::simple(FontId::proportional(12.0), t.text_faint));
                break;
            }
            chars += text.len();
            let fmt = if *removed {
                TextFormat { font_id: FontId::proportional(12.0), color: t.render_red, strikethrough: Stroke::new(1.0, t.render_red), ..Default::default() }
            } else {
                TextFormat::simple(FontId::proportional(12.0), t.text_dim)
            };
            job.append(text, 4.0, fmt);
            if *removed && let Some(r) = reason {
                job.append(&format!("({r})"), 2.0, TextFormat::simple(FontId::proportional(10.5), t.text_faint));
            }
        }
        ui.add(egui::Label::new(job).wrap());
    } else if c.result.is_none() {
        ui.label(RichText::new("The plan's details are not available.").size(11.5).color(t.text_faint));
    }
    for w in &view.warnings {
        ui.add(egui::Label::new(RichText::new(format!("⚠ {w}")).size(11.5).color(t.render_yellow)).wrap());
    }
    match status {
        PlanStatus::Proposed => {
            ui.horizontal(|ui| {
                let apply = ui.add(egui::Button::new(RichText::new("Apply").color(Color32::WHITE).size(12.0)).fill(t.accent).corner_radius(4.0));
                reg.push((format!("assistant.plan.{k}.apply"), apply.rect, "Apply plan".into()));
                let reject = ui.add(egui::Button::new(RichText::new("Reject").size(12.0)).corner_radius(4.0));
                reg.push((format!("assistant.plan.{k}.reject"), reject.rect, "Reject plan".into()));
                if apply.clicked() {
                    actions.push(Action::Plan(k, true));
                }
                if reject.clicked() {
                    actions.push(Action::Plan(k, false));
                }
            });
        }
        PlanStatus::Applying => {
            ui.label(RichText::new("Applying…").size(11.5).color(t.text_dim));
        }
        PlanStatus::Rejected => {
            ui.label(RichText::new("Rejected").size(11.5).color(t.text_faint));
        }
    }
}

fn approval_card(ui: &mut egui::Ui, t: &Tokens, a: &logic::PendingApproval, reg: &mut Vec<(String, Rect, String)>, actions: &mut Vec<Action>) {
    let r = egui::Frame::NONE
        .fill(t.row_alt)
        .stroke(Stroke::new(1.0, t.accent))
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("Allow {}?", pretty_tool_name(&a.call.name))).font(Tokens::semibold(12.5)).color(t.text));
            ui.add(egui::Label::new(RichText::new(&a.reason).size(12.0).color(t.text_dim)).wrap());
            let args = input_summary(&a.call.input);
            if !args.is_empty() {
                ui.add(egui::Label::new(RichText::new(args).font(Tokens::mono(11.0)).color(t.text_faint)).wrap());
            }
            ui.horizontal(|ui| {
                let allow = ui.add(egui::Button::new(RichText::new("Allow").color(Color32::WHITE).size(12.0)).fill(t.accent).corner_radius(4.0));
                let deny = ui.add(egui::Button::new(RichText::new("Deny").size(12.0)).corner_radius(4.0));
                reg.push((format!("assistant.approval.{}.allow", a.k), allow.rect, "Allow".into()));
                reg.push((format!("assistant.approval.{}.deny", a.k), deny.rect, "Deny".into()));
                if allow.clicked() {
                    actions.push(Action::Approve(a.k, true));
                }
                if deny.clicked() {
                    actions.push(Action::Approve(a.k, false));
                }
            });
        })
        .response
        .rect;
    reg.push((format!("assistant.approval.{}", a.k), r, pretty_tool_name(&a.call.name)));
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// `12.3k in · 1.2k out · 8.0k cache reads · ≈ $0.04`.
pub fn usage_line(u: &filmcraft_llm::Usage, cost: Option<f64>) -> String {
    let mut s = format!("{} in · {} out", fmt_tokens(u.input_tokens.saturating_add(u.cache_creation_input_tokens)), fmt_tokens(u.output_tokens));
    if u.cache_read_input_tokens > 0 {
        s.push_str(&format!(" · {} cache reads", fmt_tokens(u.cache_read_input_tokens)));
    }
    if let Some(c) = cost.filter(|c| c.is_finite()) {
        s.push_str(&format!(" · ≈ ${c:.2}"));
    }
    s
}

fn composer_ui(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, reg: &mut Vec<(String, Rect, String)>, actions: &mut Vec<Action>) {
    let t = app.tokens;
    ui.painter().line_segment([pos2(r.min.x, r.min.y + 0.5), pos2(r.max.x, r.min.y + 0.5)], Stroke::new(1.0, t.separator));
    let inner = r.shrink2(vec2(8.0, 6.0));
    let usage_h = 16.0;
    let btn_w = 72.0;
    let field = Rect::from_min_max(inner.min, pos2(inner.max.x - btn_w - 6.0, inner.max.y - usage_h - 4.0));
    let running = app.assistant.running();
    let te_id = egui::Id::new("assistant-input");
    if ui.memory(|m| m.has_focus(te_id)) && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) && !running {
        actions.push(Action::Send);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field).id_salt("assistant-composer"));
    let resp = child.add_sized(
        field.size(),
        egui::TextEdit::multiline(&mut app.ui.panels.assistant.draft)
            .id(te_id)
            .hint_text("Ask the Assistant…  (⌘↩ to send)")
            .font(Tokens::ui(13.0))
            .desired_rows(3),
    );
    reg.push(("assistant.input".into(), resp.rect, "Message".into()));
    let br = Rect::from_min_size(pos2(field.max.x + 6.0, field.min.y), vec2(btn_w, 26.0));
    let mut bchild = ui.new_child(egui::UiBuilder::new().max_rect(br).id_salt("assistant-send"));
    if running {
        let b = bchild.add_sized(br.size(), egui::Button::new(RichText::new("Cancel").size(12.0)).corner_radius(4.0));
        reg.push(("assistant.cancel".into(), b.rect, "Cancel".into()));
        if b.clicked() {
            actions.push(Action::Cancel);
        }
    } else {
        let can = !app.ui.panels.assistant.draft.trim().is_empty();
        let b = bchild
            .add_enabled(can, egui::Button::new(RichText::new("Send").color(Color32::WHITE).size(12.0)).fill(t.accent).corner_radius(4.0).min_size(br.size()));
        reg.push(("assistant.send".into(), b.rect, "Send".into()));
        if b.clicked() {
            actions.push(Action::Send);
        }
    }
    let usage = usage_line(&app.assistant.usage, app.assistant.cost_usd);
    let ur = Rect::from_min_max(pos2(inner.min.x, inner.max.y - usage_h), inner.max);
    ui.painter().text(pos2(ur.min.x, ur.center().y), Align2::LEFT_CENTER, &usage, Tokens::ui(11.0), t.text_faint);
    reg.push(("assistant.usage".into(), ur, usage));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_view_reads_what_it_can() {
        let v = PlanView::from_json(&json!({
            "preview": {
                "durationBefore": 120.0, "durationAfter": 90.0,
                "transcriptDiff": [{"text": "So"}, {"text": "um", "removed": true, "reason": "filler"}, {"word": "hello", "kind": "keep"}, {"bad": 1}],
                "warnings": ["short clip", {"message": "gap"}, 3],
            }
        }));
        assert_eq!((v.before_s, v.after_s), (Some(120.0), Some(90.0)));
        assert_eq!(v.spans, vec![("So".into(), false, None), ("um".into(), true, Some("filler".into())), ("hello".into(), false, None)]);
        assert_eq!(v.warnings, ["short clip", "gap"]);
        let r =
            PlanView::from_json(&json!({"cuts": [{"start": 1.0, "end": 2.5, "reason": "silence"}, {"start": "x"}], "duration": {"before": 10, "after": 8}}));
        assert_eq!(r.spans, vec![("1.00–2.50 s".into(), true, Some("silence".into()))]);
        assert_eq!((r.before_s, r.after_s), (Some(10.0), Some(8.0)));
        assert_eq!(PlanView::from_json(&json!("junk")), PlanView::default());
        assert_eq!(PlanView::from_json(&json!({"durationBefore": f64::MAX})).before_s, Some(f64::MAX));
    }

    #[test]
    fn inline_markdown_never_loses_text() {
        let t = Tokens::for_kind(crate::theme::ThemeKind::Dark);
        let j = markdown_job("A **bold** and `code` here", 300.0, &t);
        assert_eq!(j.text, "A bold and code here");
        assert_eq!(j.sections.len(), 5);
        for s in ["", "**", "`", "** unclosed", "a`b**c`d", "é**ü**"] {
            let j = markdown_job(s, 100.0, &t);
            assert!(j.text.len() <= s.len(), "{s}");
        }
    }

    #[test]
    fn summaries_and_usage() {
        assert_eq!(input_summary(&json!({"a": "x", "b": [1, 2], "c": {"d": 1}, "e": 2})), "a: \"x\", b: [2], c: {1}, e: 2");
        assert_eq!(input_summary(&json!(null)), "");
        assert!(input_summary(&json!({"p": "y".repeat(500)})).chars().count() <= 120);
        let u = filmcraft_llm::Usage { input_tokens: 12_300, output_tokens: 900, cache_read_input_tokens: 8000, cache_creation_input_tokens: 0 };
        assert_eq!(usage_line(&u, Some(0.0421)), "12.3k in · 900 out · 8.0k cache reads · ≈ $0.04");
        assert_eq!(usage_line(&Default::default(), None), "0 in · 0 out");
        assert_eq!(fmt_dur(125.0), "2:05.0");
    }
}
