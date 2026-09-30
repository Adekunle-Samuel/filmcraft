//! Design tokens. Every widget reads colours/sizes from [`Tokens`] so themes apply everywhere.
//!
//! The default theme follows the look of Premiere Pro's current dark UI (values measured from
//! black-box screenshots, see `plan/premiere/02-ui-ux.md`); fonts are Inter + JetBrains Mono (OFL).

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// Premiere-style darkest (default).
    #[default]
    Dark,
    /// Slightly lighter grey panels (Premiere's brightness slider mid position).
    Medium,
    Light,
}

impl ThemeKind {
    pub fn from_name(s: &str) -> Option<ThemeKind> {
        match s.to_ascii_lowercase().as_str() {
            "dark" | "darkest" => Some(ThemeKind::Dark),
            "medium" | "grey" | "gray" => Some(ThemeKind::Medium),
            "light" => Some(ThemeKind::Light),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Space between panels / app background.
    pub app_bg: Color32,
    /// Header bar (top).
    pub header_bg: Color32,
    /// Panel body.
    pub panel_bg: Color32,
    /// Panel tab strip.
    pub tab_bg: Color32,
    pub tab_text: Color32,
    pub tab_text_active: Color32,
    /// Blue focus rectangle around the active panel.
    pub focus: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub icon_active: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub field_bg: Color32,
    pub field_border: Color32,
    pub separator: Color32,
    pub row_alt: Color32,
    pub row_selected: Color32,
    /// Scrubby (hot-text) numeric value colour.
    pub hot_text: Color32,
    // timeline
    pub tl_bg: Color32,
    pub tl_track_bg: Color32,
    pub tl_track_bg_alt: Color32,
    pub tl_header_bg: Color32,
    pub tl_ruler_bg: Color32,
    pub tl_ruler_tick: Color32,
    pub tl_ruler_text: Color32,
    pub playhead: Color32,
    pub in_out_shade: Color32,
    pub clip_selected_border: Color32,
    pub render_red: Color32,
    pub render_yellow: Color32,
    pub render_green: Color32,
    pub monitor_bg: Color32,
    pub timecode: Color32,
    pub danger: Color32,
    pub radius: f32,
    pub radius_sm: f32,
    pub gap: f32,
    pub tab_h: f32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        let dark = Tokens {
            kind,
            app_bg: Color32::from_rgb(10, 10, 10),
            header_bg: Color32::from_rgb(19, 19, 19),
            panel_bg: Color32::from_rgb(29, 29, 29),
            tab_bg: Color32::from_rgb(29, 29, 29),
            tab_text: Color32::from_rgb(160, 160, 160),
            tab_text_active: Color32::from_rgb(236, 236, 236),
            focus: Color32::from_rgb(57, 138, 243),
            accent: Color32::from_rgb(57, 138, 243),
            accent_hover: Color32::from_rgb(88, 160, 250),
            text: Color32::from_rgb(214, 214, 214),
            text_dim: Color32::from_rgb(160, 160, 160),
            text_faint: Color32::from_rgb(110, 110, 110),
            icon: Color32::from_rgb(190, 190, 190),
            icon_active: Color32::from_rgb(57, 138, 243),
            hover: Color32::from_rgb(52, 52, 52),
            pressed: Color32::from_rgb(64, 64, 64),
            field_bg: Color32::from_rgb(20, 20, 20),
            field_border: Color32::from_rgb(58, 58, 58),
            separator: Color32::from_rgb(44, 44, 44),
            row_alt: Color32::from_rgb(33, 33, 33),
            row_selected: Color32::from_rgb(52, 76, 110),
            hot_text: Color32::from_rgb(76, 158, 255),
            tl_bg: Color32::from_rgb(24, 24, 24),
            tl_track_bg: Color32::from_rgb(29, 29, 29),
            tl_track_bg_alt: Color32::from_rgb(33, 33, 33),
            tl_header_bg: Color32::from_rgb(36, 36, 36),
            tl_ruler_bg: Color32::from_rgb(29, 29, 29),
            tl_ruler_tick: Color32::from_rgb(92, 92, 92),
            tl_ruler_text: Color32::from_rgb(150, 150, 150),
            playhead: Color32::from_rgb(57, 138, 243),
            in_out_shade: Color32::from_rgba_unmultiplied(120, 120, 120, 40),
            clip_selected_border: Color32::from_rgb(240, 240, 240),
            render_red: Color32::from_rgb(210, 50, 45),
            render_yellow: Color32::from_rgb(222, 196, 40),
            render_green: Color32::from_rgb(64, 180, 70),
            monitor_bg: Color32::from_rgb(0, 0, 0),
            timecode: Color32::from_rgb(76, 158, 255),
            danger: Color32::from_rgb(230, 80, 80),
            radius: 4.0,
            radius_sm: 3.0,
            gap: 3.0,
            tab_h: 28.0,
        };
        match kind {
            ThemeKind::Dark => dark,
            ThemeKind::Medium => Tokens {
                app_bg: Color32::from_rgb(22, 22, 22),
                header_bg: Color32::from_rgb(34, 34, 34),
                panel_bg: Color32::from_rgb(46, 46, 46),
                tab_bg: Color32::from_rgb(46, 46, 46),
                field_bg: Color32::from_rgb(34, 34, 34),
                tl_bg: Color32::from_rgb(40, 40, 40),
                tl_track_bg: Color32::from_rgb(46, 46, 46),
                tl_track_bg_alt: Color32::from_rgb(50, 50, 50),
                tl_header_bg: Color32::from_rgb(54, 54, 54),
                tl_ruler_bg: Color32::from_rgb(46, 46, 46),
                row_alt: Color32::from_rgb(50, 50, 50),
                hover: Color32::from_rgb(64, 64, 64),
                ..dark
            },
            ThemeKind::Light => Tokens {
                app_bg: Color32::from_rgb(180, 180, 180),
                header_bg: Color32::from_rgb(214, 214, 214),
                panel_bg: Color32::from_rgb(232, 232, 232),
                tab_bg: Color32::from_rgb(232, 232, 232),
                tab_text: Color32::from_rgb(90, 90, 90),
                tab_text_active: Color32::from_rgb(20, 20, 20),
                text: Color32::from_rgb(34, 34, 34),
                text_dim: Color32::from_rgb(90, 90, 90),
                text_faint: Color32::from_rgb(140, 140, 140),
                icon: Color32::from_rgb(60, 60, 60),
                hover: Color32::from_rgb(210, 210, 210),
                pressed: Color32::from_rgb(196, 196, 196),
                field_bg: Color32::from_rgb(250, 250, 250),
                field_border: Color32::from_rgb(170, 170, 170),
                separator: Color32::from_rgb(200, 200, 200),
                row_alt: Color32::from_rgb(224, 224, 224),
                row_selected: Color32::from_rgb(170, 200, 240),
                tl_bg: Color32::from_rgb(210, 210, 210),
                tl_track_bg: Color32::from_rgb(222, 222, 222),
                tl_track_bg_alt: Color32::from_rgb(228, 228, 228),
                tl_header_bg: Color32::from_rgb(214, 214, 214),
                tl_ruler_bg: Color32::from_rgb(226, 226, 226),
                tl_ruler_text: Color32::from_rgb(80, 80, 80),
                ..dark
            },
        }
    }

    /// Mono font for timecode.
    pub fn mono(size: f32) -> FontId {
        FontId::new(size, FontFamily::Monospace)
    }
    pub fn ui(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }
    pub fn semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name("semibold".into()))
    }
}

/// Install fonts (Inter, Inter SemiBold, JetBrains Mono) and egui visuals.
pub fn install(ctx: &egui::Context, t: &Tokens) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("inter-medium".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Medium.ttf"))));
    fonts.font_data.insert("inter-semibold".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"))));
    fonts.font_data.insert("jbmono".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"))));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "jbmono".into());
    fonts.families.insert(FontFamily::Name("semibold".into()), vec!["inter-semibold".into(), "inter".into()]);
    fonts.families.insert(FontFamily::Name("medium".into()), vec!["inter-medium".into(), "inter".into()]);
    ctx.set_fonts(fonts);
    apply_visuals(ctx, t);
}

pub fn apply_visuals(ctx: &egui::Context, t: &Tokens) {
    let mut v = if t.kind == ThemeKind::Light { Visuals::light() } else { Visuals::dark() };
    v.panel_fill = t.panel_bg;
    v.window_fill = t.panel_bg;
    v.extreme_bg_color = t.field_bg;
    v.faint_bg_color = t.row_alt;
    v.override_text_color = Some(t.text);
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = t.accent;
    v.window_stroke = Stroke::new(1.0, t.field_border);
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(5);
    v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(140) };
    v.window_shadow = v.popup_shadow;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::same(t.radius_sm as u8);
    }
    v.widgets.noninteractive.bg_fill = t.panel_bg;
    v.widgets.noninteractive.weak_bg_fill = t.panel_bg;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.inactive.bg_fill = t.field_bg;
    v.widgets.inactive.weak_bg_fill = t.field_bg;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, t.field_border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.hovered.bg_fill = t.hover;
    v.widgets.hovered.weak_bg_fill = t.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.field_border);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, t.tab_text_active);
    v.widgets.active.bg_fill = t.pressed;
    v.widgets.active.weak_bg_fill = t.pressed;
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.tab_text_active);
    v.widgets.open.bg_fill = t.hover;
    v.widgets.open.weak_bg_fill = t.hover;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 20.0;
        s.spacing.menu_margin = egui::Margin::same(4);
        s.text_styles.insert(TextStyle::Body, FontId::new(12.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(12.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(15.0, FontFamily::Name("semibold".into())));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace));
        s.animation_time = 0.12;
    });
}
