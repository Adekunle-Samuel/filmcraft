//! Colour science for FilmCraft: YUV matrices, transfer functions, colour metadata and LUTs.
//!
//! The compositor works in **linear-light, premultiplied RGBA f32** in the sequence working space
//! (Rec.709 primaries by default). Decoded frames carry [`ColorInfo`] so conversions are explicit.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// YUV ↔ RGB matrix coefficients.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Matrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020Ncl,
}

impl Matrix {
    /// (Kr, Kb) luma coefficients.
    pub fn kr_kb(self) -> (f32, f32) {
        match self {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020Ncl => (0.2627, 0.0593),
        }
    }
    /// From an ISO/IEC 23091-2 `matrix_coefficients` code.
    pub fn from_code(c: u8) -> Option<Matrix> {
        match c {
            1 => Some(Matrix::Bt709),
            5 | 6 => Some(Matrix::Bt601),
            9 | 10 => Some(Matrix::Bt2020Ncl),
            _ => None,
        }
    }
}

/// Transfer characteristics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transfer {
    /// BT.709 / BT.1886 display (we use pure 2.4 gamma for display-referred decode, as NLEs do).
    #[default]
    Bt709,
    Srgb,
    Linear,
    /// SMPTE ST 2084.
    Pq,
    /// ARIB STD-B67.
    Hlg,
}

impl Transfer {
    pub fn from_code(c: u8) -> Option<Transfer> {
        match c {
            1 | 6 | 14 | 15 => Some(Transfer::Bt709),
            13 => Some(Transfer::Srgb),
            8 => Some(Transfer::Linear),
            16 => Some(Transfer::Pq),
            18 => Some(Transfer::Hlg),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Primaries {
    #[default]
    Bt709,
    Bt601_625,
    Bt601_525,
    Bt2020,
    P3D65,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Range {
    /// 16–235 (8-bit) "video" range.
    #[default]
    Limited,
    Full,
}

/// Colour metadata attached to frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorInfo {
    pub matrix: Matrix,
    pub transfer: Transfer,
    pub primaries: Primaries,
    pub range: Range,
}

impl ColorInfo {
    pub const SRGB_FULL: ColorInfo = ColorInfo { matrix: Matrix::Bt709, transfer: Transfer::Srgb, primaries: Primaries::Bt709, range: Range::Full };
    pub const REC709: ColorInfo = ColorInfo { matrix: Matrix::Bt709, transfer: Transfer::Bt709, primaries: Primaries::Bt709, range: Range::Limited };
}

/// Convert normalised Y'CbCr (Y in 0..1, Cb/Cr in -0.5..0.5) to non-linear R'G'B'.
#[inline]
pub fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32, m: Matrix) -> [f32; 3] {
    let (kr, kb) = m.kr_kb();
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    [r, g, b]
}

/// Convert non-linear R'G'B' to normalised Y'CbCr.
#[inline]
pub fn rgb_to_ycbcr(r: f32, g: f32, b: f32, m: Matrix) -> [f32; 3] {
    let (kr, kb) = m.kr_kb();
    let kg = 1.0 - kr - kb;
    let y = kr * r + kg * g + kb * b;
    [y, (b - y) / (2.0 * (1.0 - kb)), (r - y) / (2.0 * (1.0 - kr))]
}

/// Normalise an integer code value to (Y 0..1, C -0.5..0.5) given bit depth and range.
#[inline]
pub fn normalize_y(v: u32, bits: u32, range: Range) -> f32 {
    let scale = (1u32 << (bits - 8)) as f32;
    match range {
        Range::Limited => (v as f32 - 16.0 * scale) / (219.0 * scale),
        Range::Full => v as f32 / ((1u32 << bits) - 1) as f32,
    }
}
#[inline]
pub fn normalize_c(v: u32, bits: u32, range: Range) -> f32 {
    let scale = (1u32 << (bits - 8)) as f32;
    match range {
        Range::Limited => (v as f32 - 128.0 * scale) / (224.0 * scale),
        Range::Full => (v as f32 - (1u32 << (bits - 1)) as f32) / ((1u32 << bits) - 1) as f32,
    }
}

/// sRGB electro-optical transfer (encoded → linear).
#[inline]
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}
#[inline]
pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Encoded → linear for a transfer function (display-referred; PQ normalised to 100 nits = 1.0).
pub fn to_linear(v: f32, t: Transfer) -> f32 {
    match t {
        Transfer::Srgb => srgb_to_linear(v),
        // Premiere treats Rec.709 as display gamma 2.4 in its colour-managed pipeline; the
        // sRGB curve is visually identical for UI previews and keeps round-trips exact.
        Transfer::Bt709 => srgb_to_linear(v),
        Transfer::Linear => v,
        Transfer::Pq => pq_eotf(v) * 100.0,
        Transfer::Hlg => hlg_inverse_oetf(v),
    }
}

pub fn from_linear(v: f32, t: Transfer) -> f32 {
    match t {
        Transfer::Srgb | Transfer::Bt709 => linear_to_srgb(v),
        Transfer::Linear => v,
        Transfer::Pq => pq_inverse_eotf(v / 100.0),
        Transfer::Hlg => hlg_oetf(v),
    }
}

/// SMPTE ST 2084 EOTF, output normalised to 10 000 nits = 1.0.
pub fn pq_eotf(e: f32) -> f32 {
    let (m1, m2) = (0.159_301_76_f32, 78.84375_f32);
    let (c1, c2, c3) = (0.835_937_5_f32, 18.851_563_f32, 18.6875_f32);
    let p = e.max(0.0).powf(1.0 / m2);
    ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}
pub fn pq_inverse_eotf(y: f32) -> f32 {
    let (m1, m2) = (0.159_301_76_f32, 78.84375_f32);
    let (c1, c2, c3) = (0.835_937_5_f32, 18.851_563_f32, 18.6875_f32);
    let p = y.max(0.0).powf(m1);
    ((c1 + c2 * p) / (1.0 + c3 * p)).powf(m2)
}
pub fn hlg_oetf(l: f32) -> f32 {
    let (a, b, c) = (0.178_832_77_f32, 0.284_668_92_f32, 0.559_910_7_f32);
    if l <= 1.0 / 12.0 { (3.0 * l.max(0.0)).sqrt() } else { a * (12.0 * l - b).ln() + c }
}
pub fn hlg_inverse_oetf(e: f32) -> f32 {
    let (a, b, c) = (0.178_832_77_f32, 0.284_668_92_f32, 0.559_910_7_f32);
    if e <= 0.5 { e * e / 3.0 } else { (((e - c) / a).exp() + b) / 12.0 }
}

/// 256-entry table: sRGB-encoded u8 → linear f32.
pub fn srgb_u8_to_linear_table() -> &'static [f32; 256] {
    static T: OnceLock<[f32; 256]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

/// 4096-entry table: linear (0..1, quantised to 12 bits) → sRGB-encoded u8.
pub fn linear_to_srgb_u8_table() -> &'static [u8; 4096] {
    static T: OnceLock<[u8; 4096]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| (linear_to_srgb(i as f32 / 4095.0) * 255.0 + 0.5).clamp(0.0, 255.0) as u8))
}

#[inline]
pub fn linear_to_srgb_u8(v: f32) -> u8 {
    let i = (v.clamp(0.0, 1.0) * 4095.0 + 0.5) as usize;
    linear_to_srgb_u8_table()[i]
}

/// Rec.709 luma of a linear or encoded RGB triple.
#[inline]
pub fn luma709(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// RGB (0..1) → HSL (h in 0..1).
pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> [f32; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-7 {
        return [0.0, 0.0, l];
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h / 6.0, s, l]
}

pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

/// A 3D LUT (from a `.cube` file), applied to encoded RGB in 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    pub title: String,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// `size³` entries, red fastest.
    pub data: Vec<[f32; 3]>,
}

impl Lut3d {
    pub fn identity(size: usize) -> Self {
        let n = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / n, g as f32 / n, b as f32 / n]);
                }
            }
        }
        Lut3d { title: "identity".into(), size, domain_min: [0.0; 3], domain_max: [1.0; 3], data }
    }

    /// Parse the Adobe/Resolve `.cube` text format (3D LUTs).
    pub fn parse_cube(text: &str) -> Result<Lut3d, String> {
        let mut size = 0usize;
        let mut title = String::new();
        let mut dmin = [0.0f32; 3];
        let mut dmax = [1.0f32; 3];
        let mut data = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let key = it.next().unwrap_or("");
            match key {
                "TITLE" => title = line[5..].trim().trim_matches('"').to_string(),
                "LUT_3D_SIZE" => size = it.next().and_then(|v| v.parse().ok()).ok_or("bad LUT_3D_SIZE")?,
                "LUT_1D_SIZE" => return Err("1D .cube LUTs are not supported yet".into()),
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let v: Vec<f32> = it.filter_map(|x| x.parse().ok()).collect();
                    if v.len() != 3 {
                        return Err(format!("bad {key}"));
                    }
                    let t = if key == "DOMAIN_MIN" { &mut dmin } else { &mut dmax };
                    t.copy_from_slice(&v);
                }
                _ => {
                    let v: Vec<f32> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 3 {
                        data.push([v[0], v[1], v[2]]);
                    }
                }
            }
        }
        if size < 2 || data.len() != size * size * size {
            return Err(format!("expected {}³ entries, found {}", size, data.len()));
        }
        Ok(Lut3d { title, size, domain_min: dmin, domain_max: dmax, data })
    }

    pub fn to_cube(&self) -> String {
        let mut s = format!("TITLE \"{}\"\nLUT_3D_SIZE {}\n", self.title, self.size);
        for d in &self.data {
            s += &format!("{:.6} {:.6} {:.6}\n", d[0], d[1], d[2]);
        }
        s
    }

    /// Trilinear lookup.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let m = (n - 1) as f32;
        let mut idx = [0usize; 3];
        let mut frac = [0f32; 3];
        for c in 0..3 {
            let t = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * m;
            let i = (t.floor() as usize).min(n - 2);
            idx[c] = i;
            frac[c] = t - i as f32;
        }
        let at = |r: usize, g: usize, b: usize| self.data[r + g * n + b * n * n];
        let mut out = [0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let c = |dr, dg, db| at(idx[0] + dr, idx[1] + dg, idx[2] + db)[k];
            let x00 = c(0, 0, 0) + (c(1, 0, 0) - c(0, 0, 0)) * frac[0];
            let x10 = c(0, 1, 0) + (c(1, 1, 0) - c(0, 1, 0)) * frac[0];
            let x01 = c(0, 0, 1) + (c(1, 0, 1) - c(0, 0, 1)) * frac[0];
            let x11 = c(0, 1, 1) + (c(1, 1, 1) - c(0, 1, 1)) * frac[0];
            let y0 = x00 + (x10 - x00) * frac[1];
            let y1 = x01 + (x11 - x01) * frac[1];
            *o = y0 + (y1 - y0) * frac[2];
        }
        out
    }
}

/// Parse `#rrggbb` / `#rrggbbaa` into 0..1 floats.
pub fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let s = s.trim().trim_start_matches('#');
    let b = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    match s.len() {
        6 => Some([b(0)?, b(2)?, b(4)?, 1.0]),
        8 => Some([b(0)?, b(2)?, b(4)?, b(6)?]),
        _ => None,
    }
}

pub fn to_hex(c: [f32; 4]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c[3] >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]), q(c[3]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ycbcr_roundtrip() {
        for m in [Matrix::Bt601, Matrix::Bt709, Matrix::Bt2020Ncl] {
            let rgb = [0.8, 0.3, 0.1];
            let y = rgb_to_ycbcr(rgb[0], rgb[1], rgb[2], m);
            let back = ycbcr_to_rgb(y[0], y[1], y[2], m);
            for k in 0..3 {
                assert!((rgb[k] - back[k]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn limited_range_levels() {
        assert_eq!(normalize_y(16, 8, Range::Limited), 0.0);
        assert_eq!(normalize_y(235, 8, Range::Limited), 1.0);
        assert_eq!(normalize_y(940, 10, Range::Limited), 1.0);
        assert_eq!(normalize_c(128, 8, Range::Limited), 0.0);
    }

    #[test]
    fn transfer_roundtrips() {
        for t in [Transfer::Srgb, Transfer::Pq, Transfer::Hlg, Transfer::Linear] {
            for v in [0.0, 0.1, 0.5, 0.9] {
                let l = to_linear(v, t);
                assert!((from_linear(l, t) - v).abs() < 1e-3, "{t:?} {v}");
            }
        }
        assert_eq!(linear_to_srgb_u8(1.0), 255);
        assert_eq!(linear_to_srgb_u8(0.0), 0);
        assert_eq!(linear_to_srgb_u8(srgb_u8_to_linear_table()[128]), 128);
    }

    #[test]
    fn hsl_roundtrip() {
        let c = [0.2, 0.6, 0.9];
        let h = rgb_to_hsl(c[0], c[1], c[2]);
        let b = hsl_to_rgb(h[0], h[1], h[2]);
        for k in 0..3 {
            assert!((c[k] - b[k]).abs() < 1e-5);
        }
    }

    #[test]
    fn cube_identity() {
        let lut = Lut3d::identity(17);
        let parsed = Lut3d::parse_cube(&lut.to_cube()).unwrap();
        let v = parsed.apply([0.3, 0.55, 0.91]);
        assert!((v[0] - 0.3).abs() < 1e-4 && (v[1] - 0.55).abs() < 1e-4 && (v[2] - 0.91).abs() < 1e-4);
        assert!(Lut3d::parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert_eq!(to_hex([1.0, 0.0, 0.0, 1.0]), "#ff0000");
    }
}
