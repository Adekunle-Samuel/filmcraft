//! The caption font: Inter SemiBold (SIL OFL 1.1, `assets/fonts/Inter-SemiBold.ttf`, attributed in
//! ATTRIBUTION.md), read with `skrifa` and rasterised by [`crate::raster`]. Glyph masks are
//! cached by (char, size, sub-pixel offset).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

use crate::raster::{Mask, Path, fill};

static INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf");

fn font() -> Option<FontRef<'static>> {
    FontRef::new(INTER_SEMIBOLD).ok()
}

/// Vertical metrics at a pixel size: (ascent, descent), both positive.
pub fn metrics(px: f32) -> (f32, f32) {
    match font() {
        Some(f) => {
            let m = f.metrics(Size::new(px), LocationRef::default());
            (m.ascent, -m.descent)
        }
        None => (px * 0.8, px * 0.2),
    }
}

/// Horizontal advance of a character at a pixel size.
pub fn advance(c: char, px: f32) -> f32 {
    let Some(f) = font() else { return px * 0.5 };
    let gid = f.charmap().map(c).or_else(|| f.charmap().map('?'));
    gid.and_then(|g| f.glyph_metrics(Size::new(px), LocationRef::default()).advance_width(g)).unwrap_or(px * 0.5)
}

/// Width of a single line of text.
pub fn measure(text: &str, px: f32) -> f32 {
    text.chars().map(|c| advance(c, px)).sum()
}

struct Pen {
    p: Path,
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.p.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.p.line_to(x, -y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.p.quad_to(cx0, -cy0, x, -y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.p.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }
    fn close(&mut self) {
        self.p.close();
    }
}

/// A rasterised glyph: coverage mask placed with its top-left at `(left, top)` relative to the
/// pen position on the baseline.
#[derive(Debug)]
pub struct Glyph {
    pub mask: Mask,
    pub left: i32,
    pub top: i32,
}

type Key = (char, u32, u8);

fn cache() -> &'static Mutex<HashMap<Key, Option<Arc<Glyph>>>> {
    static C: OnceLock<Mutex<HashMap<Key, Option<Arc<Glyph>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Rasterise `c` at `px` with the pen at fractional x offset `frac` (0..1, quantised to ¼ px).
pub fn glyph(c: char, px: f32, frac: f32) -> Option<Arc<Glyph>> {
    let size_q = (px * 4.0).round().max(1.0) as u32;
    let frac_q = ((frac.rem_euclid(1.0) * 4.0).round() as u8) % 4;
    let key = (c, size_q, frac_q);
    if let Some(g) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return g.clone();
    }
    let g = render_glyph(c, size_q as f32 / 4.0, frac_q as f32 / 4.0).map(Arc::new);
    let mut m = cache().lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 8192 {
        m.clear();
    }
    m.insert(key, g.clone());
    g
}

fn render_glyph(c: char, px: f32, frac: f32) -> Option<Glyph> {
    if c.is_whitespace() {
        return None;
    }
    let f = font()?;
    let gid = f.charmap().map(c).or_else(|| f.charmap().map('?'))?;
    let outline = f.outline_glyphs().get(gid)?;
    let mut pen = Pen { p: Path::default() };
    outline.draw(DrawSettings::unhinted(Size::new(px), LocationRef::default()), &mut pen).ok()?;
    pen.p.close();
    let (x0, y0, x1, y1) = pen.p.bounds()?;
    let left = (x0 + frac).floor() as i32;
    let top = y0.floor() as i32;
    let w = ((x1 + frac).ceil() as i32 - left).max(1) as usize;
    let h = (y1.ceil() as i32 - top).max(1) as usize;
    let mask = fill(&pen.p, w, h, frac - left as f32, -top as f32);
    Some(Glyph { mask, left, top })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inter_loads_and_draws() {
        let (a, d) = metrics(100.0);
        assert!(a > 80.0 && a < 110.0, "ascent {a}");
        assert!(d > 10.0 && d < 40.0, "descent {d}");
        assert!(advance('W', 100.0) > advance('i', 100.0));
        let g = glyph('H', 40.0, 0.0).unwrap();
        // the H has two stems: the middle row has ink on both sides and a gap between them
        let y = g.mask.h / 4;
        let row: Vec<f32> = (0..g.mask.w).map(|x| g.mask.get(x, y)).collect();
        assert!(row[1] > 0.9 && row[g.mask.w - 2] > 0.9, "{row:?}");
        assert!(row[g.mask.w / 2] < 0.1, "{row:?}");
        assert!(g.top < 0, "glyph rises above the baseline");
        assert!(glyph(' ', 40.0, 0.0).is_none());
        assert!(measure("Hello", 30.0) > 50.0);
    }
}
