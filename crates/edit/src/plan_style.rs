//! The presentation parts of an edit plan that are pure data: the output frame of an
//! [`Aspect`], the caption style object (`captions.style`) mapped onto a caption track's
//! [`CaptionStyle`], and caption text case.
//!
//! `captions.style` is written by an assistant, so it is hostile input: it is read key by key,
//! unknown keys and bad values become warnings (the rest still applies), strings are capped and
//! every number is checked and clamped. Nothing here fails or panics.

use filmcraft_project::{CaptionAlign, CaptionAnchor, CaptionStyle, MAX_FRAME_PIXELS, MAX_FRAME_SIDE};
use serde_json::Value;

use crate::plan::Aspect;

/// Most keys of a style object that are looked at (the rest are reported once).
pub const MAX_STYLE_KEYS: usize = 64;
/// Longest string value in a style object (characters).
pub const MAX_STYLE_TEXT: usize = 200;
/// Longest font name (characters).
pub const MAX_FONT_CHARS: usize = 100;
/// Most characters of a key or value echoed in a warning.
const ECHO: usize = 40;

impl Aspect {
    /// Width : height.
    pub fn ratio(self) -> (u32, u32) {
        match self {
            Aspect::Wide => (16, 9),
            Aspect::Vertical => (9, 16),
            Aspect::Square => (1, 1),
            Aspect::Portrait => (4, 5),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Aspect::Wide => "16:9",
            Aspect::Vertical => "9:16",
            Aspect::Square => "1:1",
            Aspect::Portrait => "4:5",
        }
    }

    /// The frame size of this aspect made from a `w`×`h` frame: the short side is kept (1920×1080
    /// gives 1080×1920 for 9:16, 1080×1080 for 1:1, 1080×1350 for 4:5), both sides even, scaled
    /// down to the frame-size limits when needed.
    pub fn frame_for(self, w: u32, h: u32) -> (u32, u32) {
        let (rw, rh) = self.ratio();
        let short = u64::from(w.min(h).max(2));
        let (fw, fh) = if rw >= rh { (short * u64::from(rw) / u64::from(rh), short) } else { (short, short * u64::from(rh) / u64::from(rw)) };
        let (mut fw, mut fh) = (fw.max(2), fh.max(2));
        // keep within the limits, keeping the aspect
        let side = u64::from(MAX_FRAME_SIDE);
        let longest = fw.max(fh);
        if longest > side {
            fw = fw * side / longest;
            fh = fh * side / longest;
        }
        while fw.saturating_mul(fh) > MAX_FRAME_PIXELS && fw > 2 && fh > 2 {
            fw = fw * 9 / 10;
            fh = fh * 9 / 10;
        }
        let even = |x: u64| -> u32 { u32::try_from((x / 2 * 2).max(2)).unwrap_or(MAX_FRAME_SIDE) };
        (even(fw), even(fh))
    }
}

/// Characters per caption line that fit a `w`×`h` frame at a caption size of `size` pixels per
/// 1080 lines (about 0.55 em per character over 90 % of the width), between 8 and 42.
pub fn caption_chars_for(w: u32, h: u32, size: f32) -> usize {
    let (w, h, size) = (f64::from(w.max(1)), f64::from(h.max(1)), f64::from(size).clamp(4.0, 400.0));
    let px_per_char = 0.55 * size * h / 1080.0;
    let n = 0.9 * w / px_per_char.max(1e-3);
    if n.is_finite() { (n.floor() as usize).clamp(8, 42) } else { 42 }
}

/// Text case for captions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextCase {
    Upper,
    Lower,
    /// First letter of every word upper case.
    Title,
}

/// `text` in `case`.
pub fn apply_case(text: &str, case: TextCase) -> String {
    match case {
        TextCase::Upper => text.to_uppercase(),
        TextCase::Lower => text.to_lowercase(),
        TextCase::Title => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() {
                        start = true;
                    } else if c.is_alphabetic() {
                        start = false;
                    }
                }
            }
            out
        }
    }
}

/// A `captions.style` object read onto a caption track style.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionLook {
    pub style: CaptionStyle,
    pub case: Option<TextCase>,
    /// Unknown keys and values that could not be used (each names its key).
    pub warnings: Vec<String>,
}

fn echo(s: &str) -> String {
    let t: String = s.chars().take(ECHO).collect();
    if t.chars().count() < s.chars().count() { format!("{t}…") } else { t }
}

fn short_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\u{201c}{}\u{201d}", echo(s)),
        Value::Array(_) => "an array".into(),
        Value::Object(_) => "an object".into(),
        o => echo(&o.to_string()),
    }
}

/// A colour: `"#rrggbb"`, `"#rrggbbaa"`, `"#rgb"`, a few names, or `[r, g, b(, a)]` (0–255).
pub fn parse_color(v: &Value) -> Option<[u8; 4]> {
    if let Some(a) = v.as_array() {
        if !(3..=4).contains(&a.len()) {
            return None;
        }
        let mut c = [255u8; 4];
        for (slot, x) in c.iter_mut().zip(a) {
            let f = x.as_f64().filter(|f| f.is_finite() && (0.0..=255.0).contains(f))?;
            *slot = f.round() as u8;
        }
        return Some(c);
    }
    let s = v.as_str()?.trim();
    if s.chars().count() > 32 {
        return None;
    }
    let named = match s.to_ascii_lowercase().as_str() {
        "white" => Some([255, 255, 255, 255]),
        "black" => Some([0, 0, 0, 255]),
        "yellow" => Some([255, 221, 0, 255]),
        "red" => Some([230, 30, 30, 255]),
        "green" => Some([40, 200, 80, 255]),
        "blue" => Some([40, 110, 240, 255]),
        "transparent" | "none" => Some([0, 0, 0, 0]),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let h = s.strip_prefix('#').unwrap_or(s);
    if !h.is_ascii() {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let n = |i: usize| u8::from_str_radix(h.get(i..i + 1)?, 16).ok().map(|x| x * 17);
    match h.len() {
        3 => Some([n(0)?, n(1)?, n(2)?, 255]),
        6 => Some([b(0)?, b(2)?, b(4)?, 255]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|f| f.is_finite())
}

/// Read a `captions.style` object onto `base`. Keys (camelCase; a few aliases):
///
/// - `size`: a fraction of the frame height (0.01–0.3, e.g. 0.05), or, above 1, pixels at 1080
///   lines (`sizePx` is always pixels at 1080 lines);
/// - `font`; `color` / `textColor`; `background` / `box` (bool, or a colour that turns it on);
///   `backgroundColor` / `boxColor`; `outline` / `outlineWidth` (pixels at 1080 lines, or a
///   bool); `outlineColor`;
/// - `position` / `anchor` (`top` / `middle` / `center` / `bottom`), `align` / `alignment`
///   (`left` / `center` / `right`), `margin` (fraction of the frame height, 0–0.45),
///   `lineSpacing` (0.8–4);
/// - `case` (`upper` / `lower` / `title` / `none`).
///
/// Anything else is a warning; `null` values are ignored. Not an object: a warning, `base` kept.
pub fn caption_look(v: &Value, base: &CaptionStyle) -> CaptionLook {
    let mut y = base.clone();
    let mut case = None;
    let mut w: Vec<String> = Vec::new();
    let Some(obj) = v.as_object() else {
        if !v.is_null() {
            w.push(format!("captions.style: must be an object, not {}; ignored", short_value(v)));
        }
        return CaptionLook { style: y, case, warnings: w };
    };
    if obj.len() > MAX_STYLE_KEYS {
        w.push(format!("captions.style: {} keys; only the first {MAX_STYLE_KEYS} were read", obj.len()));
    }
    for (k, v) in obj.iter().take(MAX_STYLE_KEYS) {
        if v.is_null() {
            continue;
        }
        let key = echo(k);
        if let Some(s) = v.as_str()
            && s.chars().count() > MAX_STYLE_TEXT
        {
            w.push(format!("captions.style.{key}: longer than {MAX_STYLE_TEXT} characters; ignored"));
            continue;
        }
        let bad = |w: &mut Vec<String>, what: &str| w.push(format!("captions.style.{key}: {} is not {what}; ignored", short_value(v)));
        match k.as_str() {
            "size" | "fontSize" => match num(v) {
                Some(f) if f > 0.0 && f <= 1.0 => y.size = ((f * 1080.0) as f32).clamp(4.0, 400.0),
                Some(f) if f > 1.0 && f <= 400.0 => y.size = (f as f32).clamp(4.0, 400.0),
                _ => bad(&mut w, "a fraction of the frame height (0–1) or pixels at 1080 lines (up to 400)"),
            },
            "sizePx" => match num(v) {
                Some(f) if f > 0.0 && f <= 400.0 => y.size = (f as f32).clamp(4.0, 400.0),
                _ => bad(&mut w, "a size in pixels at 1080 lines (up to 400)"),
            },
            "font" | "fontFamily" => match v.as_str().map(str::trim) {
                Some(f) if !f.is_empty() && f.chars().count() <= MAX_FONT_CHARS => y.font = f.to_string(),
                _ => bad(&mut w, "a font name"),
            },
            "color" | "textColor" => match parse_color(v) {
                Some(c) => y.color = c,
                None => bad(&mut w, "a colour (#rrggbb, #rrggbbaa or [r,g,b])"),
            },
            "background" | "box" => match (v.as_bool(), parse_color(v)) {
                (Some(b), _) => y.background = b,
                (None, Some(c)) => {
                    y.background = c[3] > 0;
                    y.background_color = c;
                }
                _ => bad(&mut w, "a bool or a colour"),
            },
            "backgroundColor" | "boxColor" => match parse_color(v) {
                Some(c) => {
                    y.background_color = c;
                    y.background = c[3] > 0;
                }
                None => bad(&mut w, "a colour"),
            },
            "outline" | "outlineWidth" | "stroke" => match (v.as_bool(), num(v)) {
                (Some(b), _) => y.outline = if b { 4.0 } else { 0.0 },
                (None, Some(f)) if (0.0..=40.0).contains(&f) => y.outline = f as f32,
                _ => bad(&mut w, "an outline width in pixels at 1080 lines (0–40) or a bool"),
            },
            "outlineColor" | "strokeColor" => match parse_color(v) {
                Some(c) => {
                    y.outline_color = c;
                    if y.outline <= 0.0 {
                        y.outline = 4.0;
                    }
                }
                None => bad(&mut w, "a colour"),
            },
            "position" | "anchor" | "verticalPosition" => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("top") => y.anchor = CaptionAnchor::Top,
                Some("middle" | "center" | "centre") => y.anchor = CaptionAnchor::Middle,
                Some("bottom") => y.anchor = CaptionAnchor::Bottom,
                _ => bad(&mut w, "top, middle or bottom"),
            },
            "align" | "alignment" | "textAlign" => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("left") => y.align = CaptionAlign::Left,
                Some("center" | "centre" | "middle") => y.align = CaptionAlign::Center,
                Some("right") => y.align = CaptionAlign::Right,
                _ => bad(&mut w, "left, center or right"),
            },
            "margin" => match num(v) {
                Some(f) if (0.0..=0.45).contains(&f) => y.margin = f as f32,
                _ => bad(&mut w, "a fraction of the frame height (0–0.45)"),
            },
            "lineSpacing" => match num(v) {
                Some(f) if (0.8..=4.0).contains(&f) => y.line_spacing = f as f32,
                _ => bad(&mut w, "a line spacing (0.8–4)"),
            },
            "case" | "textCase" | "textTransform" => match v.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("upper" | "uppercase" | "allcaps" | "caps") => case = Some(TextCase::Upper),
                Some("lower" | "lowercase") => case = Some(TextCase::Lower),
                Some("title" | "titlecase" | "capitalize") => case = Some(TextCase::Title),
                Some("none" | "as-is" | "asis" | "sentence" | "normal") => case = None,
                _ => bad(&mut w, "upper, lower, title or none"),
            },
            other => w.push(format!("captions.style.{}: unknown key; ignored", echo(other))),
        }
    }
    CaptionLook { style: y, case, warnings: w }
}

#[cfg(test)]
#[path = "plan_style_tests.rs"]
mod tests;
