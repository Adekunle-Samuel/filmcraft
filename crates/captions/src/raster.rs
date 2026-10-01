//! A small anti-aliased path rasteriser for glyph outlines.
//!
//! Curves are flattened to line segments; each pixel row is sampled at [`SUB`] sub-scanlines, and
//! on each sub-scanline the non-zero winding spans between edge crossings are accumulated with
//! exact horizontal coverage at the span ends. Original implementation for FilmCraft.

/// Vertical sub-samples per pixel row.
pub const SUB: usize = 5;

/// A flattened path: line segments `(x0, y0, x1, y1)` in pixel space, y down.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub segs: Vec<[f32; 4]>,
    start: (f32, f32),
    cur: (f32, f32),
    open: bool,
}

impl Path {
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = (x, y);
        self.cur = (x, y);
        self.open = true;
    }
    pub fn line_to(&mut self, x: f32, y: f32) {
        if (x, y) != self.cur {
            self.segs.push([self.cur.0, self.cur.1, x, y]);
        }
        self.cur = (x, y);
    }
    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let dd = ((x0 - 2.0 * cx + x).abs() + (y0 - 2.0 * cy + y).abs()).max(0.01);
        let n = ((dd * 2.0).sqrt().ceil() as usize).clamp(1, 32);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.line_to(u * u * x0 + 2.0 * u * t * cx + t * t * x, u * u * y0 + 2.0 * u * t * cy + t * t * y);
        }
    }
    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let dd = (x0 - 2.0 * c1x + c2x).abs() + (y0 - 2.0 * c1y + c2y).abs() + (c1x - 2.0 * c2x + x).abs() + (c1y - 2.0 * c2y + y).abs();
        let n = (((dd.max(0.01)) * 3.0).sqrt().ceil() as usize).clamp(1, 48);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.line_to(a * x0 + b * c1x + c * c2x + d * x, a * y0 + b * c1y + c * c2y + d * y);
        }
    }
    pub fn close(&mut self) {
        if self.open && self.cur != self.start {
            self.segs.push([self.cur.0, self.cur.1, self.start.0, self.start.1]);
        }
        self.cur = self.start;
        self.open = false;
    }
    /// Bounding box `(x0, y0, x1, y1)` of all segments.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut it = self.segs.iter();
        let f = it.next()?;
        let mut b = (f[0].min(f[2]), f[1].min(f[3]), f[0].max(f[2]), f[1].max(f[3]));
        for s in it {
            b = (b.0.min(s[0]).min(s[2]), b.1.min(s[1]).min(s[3]), b.2.max(s[0]).max(s[2]), b.3.max(s[1]).max(s[3]));
        }
        Some(b)
    }
}

/// Coverage mask (0..1), row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub w: usize,
    pub h: usize,
    pub a: Vec<f32>,
}

impl Mask {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, a: vec![0.0; w * h] }
    }
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.a[y * self.w + x]
    }
}

/// Fill `path` (non-zero winding) into a `w`×`h` mask, offsetting the path by `(dx, dy)`.
pub fn fill(path: &Path, w: usize, h: usize, dx: f32, dy: f32) -> Mask {
    let mut m = Mask::new(w, h);
    if w == 0 || h == 0 {
        return m;
    }
    // edges: (ytop, ybot, x at ytop, dx/dy, winding)
    let mut edges: Vec<(f32, f32, f32, f32, i32)> = path
        .segs
        .iter()
        .filter(|s| s[1] != s[3])
        .map(|s| {
            let (x0, y0, x1, y1) = (s[0] + dx, s[1] + dy, s[2] + dx, s[3] + dy);
            let (dir, xa, ya, xb, yb) = if y0 < y1 { (1, x0, y0, x1, y1) } else { (-1, x1, y1, x0, y0) };
            (ya, yb, xa, (xb - xa) / (yb - ya), dir)
        })
        .collect();
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut xs: Vec<(f32, i32)> = Vec::new();
    let mut row = vec![0.0f32; w + 1];
    let weight = 1.0 / SUB as f32;
    for y in 0..h {
        row.iter_mut().for_each(|v| *v = 0.0);
        let mut any = false;
        for k in 0..SUB {
            let sy = y as f32 + (k as f32 + 0.5) / SUB as f32;
            xs.clear();
            for e in &edges {
                if e.0 > sy {
                    break; // sorted by top
                }
                if sy < e.1 {
                    xs.push((e.2 + (sy - e.0) * e.3, e.4));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            let mut span_start = 0.0f32;
            for &(x, d) in &xs {
                let before = wind;
                wind += d;
                if before == 0 && wind != 0 {
                    span_start = x;
                } else if before != 0 && wind == 0 {
                    add_span(&mut row, span_start, x, weight, w);
                    any = true;
                }
            }
        }
        if any {
            let out = &mut m.a[y * w..(y + 1) * w];
            for (o, v) in out.iter_mut().zip(&row) {
                *o = v.min(1.0);
            }
        }
    }
    m
}

/// Add coverage `weight` over `[x0, x1)` with fractional ends.
fn add_span(row: &mut [f32], x0: f32, x1: f32, weight: f32, w: usize) {
    let x0 = x0.clamp(0.0, w as f32);
    let x1 = x1.clamp(0.0, w as f32);
    if x1 <= x0 {
        return;
    }
    let i0 = x0.floor() as usize;
    let i1 = x1.floor() as usize;
    if i0 == i1 {
        row[i0] += (x1 - x0) * weight;
        return;
    }
    row[i0] += (i0 as f32 + 1.0 - x0) * weight;
    for v in &mut row[i0 + 1..i1] {
        *v += weight;
    }
    if i1 < w {
        row[i1] += (x1 - i1 as f32) * weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Path {
        let mut p = Path::default();
        p.move_to(x0, y0);
        p.line_to(x1, y0);
        p.line_to(x1, y1);
        p.line_to(x0, y1);
        p.close();
        p
    }

    #[test]
    fn rectangle_coverage() {
        let m = fill(&rect(1.5, 2.0, 4.0, 5.0), 6, 6, 0.0, 0.0);
        assert!((m.get(1, 3) - 0.5).abs() < 1e-5);
        assert!((m.get(2, 3) - 1.0).abs() < 1e-5);
        assert_eq!(m.get(4, 3), 0.0);
        assert_eq!(m.get(2, 1), 0.0);
        assert_eq!(m.get(2, 5), 0.0);
        let total: f32 = m.a.iter().sum();
        assert!((total - 2.5 * 3.0).abs() < 1e-3, "{total}");
    }

    #[test]
    fn hole_with_opposite_winding() {
        let mut p = rect(0.0, 0.0, 10.0, 10.0);
        // inner square wound the other way
        p.move_to(3.0, 3.0);
        p.line_to(3.0, 7.0);
        p.line_to(7.0, 7.0);
        p.line_to(7.0, 3.0);
        p.close();
        let m = fill(&p, 10, 10, 0.0, 0.0);
        assert_eq!(m.get(5, 5), 0.0);
        assert!((m.get(1, 5) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn circle_area() {
        let mut p = Path::default();
        let (cx, cy, r) = (10.0f32, 10.0f32, 6.0f32);
        p.move_to(cx + r, cy);
        for i in 1..=64 {
            let a = i as f32 / 64.0 * std::f32::consts::TAU;
            p.line_to(cx + r * a.cos(), cy + r * a.sin());
        }
        p.close();
        let m = fill(&p, 20, 20, 0.0, 0.0);
        let total: f32 = m.a.iter().sum();
        let want = std::f32::consts::PI * r * r;
        assert!((total - want).abs() / want < 0.02, "{total} vs {want}");
    }
}
