//! Video transitions: combine the outgoing (A) and incoming (B) layers at progress `p` ∈ [0,1].

use filmcraft_geom::Affine;
use filmcraft_project::{EffectInstance, ParamValue};
use rayon::prelude::*;

use crate::image::Image;

fn dir(e: &EffectInstance) -> u32 {
    match e.param("direction").map(|p| &p.value) {
        Some(ParamValue::Choice(c)) => *c,
        _ => 3,
    }
}

/// Mask-based mix: out = A·(1-m) + B·m with m = f(x, y) ∈ [0,1].
fn masked(a: &Image, b: &Image, m: impl Fn(f32, f32) -> f32 + Sync) -> Image {
    let (w, h) = (a.w, a.h);
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let k = m(u, v).clamp(0.0, 1.0);
            let i = (y * w + x) * 4;
            for c in 0..4 {
                row[x * 4 + c] = a.px[i + c] * (1.0 - k) + b.px[i + c] * k;
            }
        }
    });
    out
}

fn over(dst: &mut Image, src: &Image) {
    crate::blend::composite(dst, src, 1.0, crate::blend::Blend::Normal);
}

fn shifted(img: &Image, dx: f64, dy: f64) -> Image {
    img.transformed(img.w, img.h, &Affine::translate(dx, dy))
}

/// Soft edge width in uv units.
const EDGE: f32 = 0.004;

fn step(edge: f32, x: f32) -> f32 {
    ((x - edge) / EDGE + 0.5).clamp(0.0, 1.0)
}

pub fn apply(e: &EffectInstance, a: &Image, b: &Image, p: f32) -> Image {
    let (w, h) = (a.w as f64, a.h as f64);
    let aspect = (a.w as f32) / (a.h as f32).max(1.0);
    let pd = p as f64;
    match e.effect.as_str() {
        "cross_dissolve" | "non_additive_dissolve" | "morph_cut" => masked(a, b, |_, _| p),
        "film_dissolve" => {
            // blend in a gamma-2.2-like space for a filmic, less-dippy dissolve
            let mut out = Image::new(a.w, a.h);
            out.px.par_iter_mut().zip(a.px.par_iter().zip(b.px.par_iter())).for_each(|(o, (x, y))| {
                let g = 1.0 / 2.2;
                let v = x.max(0.0).powf(g) * (1.0 - p) + y.max(0.0).powf(g) * p;
                *o = v.powf(2.2);
            });
            out
        }
        "additive_dissolve" => {
            let (ka, kb) = ((2.0 * (1.0 - p)).min(1.0), (2.0 * p).min(1.0));
            let mut out = Image::new(a.w, a.h);
            out.px.par_iter_mut().zip(a.px.par_iter().zip(b.px.par_iter())).for_each(|(o, (x, y))| *o = (x * ka + y * kb).min(1.0));
            out
        }
        "dip_to_black" | "dip_to_white" => {
            let col = if e.effect == "dip_to_black" { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] };
            let mut out = Image::filled(a.w, a.h, col);
            let (src, k) = if p < 0.5 { (a, 1.0 - p * 2.0) } else { (b, (p - 0.5) * 2.0) };
            let mut s = src.clone();
            s.scale_alpha(k);
            over(&mut out, &s);
            out
        }
        "wipe" => {
            let d = dir(e);
            masked(a, b, move |u, v| match d {
                0 => 1.0 - step(p, v), // from north: B grows from top
                1 => step(1.0 - p, u),
                2 => step(1.0 - p, v),
                _ => 1.0 - step(p, u),
            })
        }
        "barn_doors" => masked(a, b, |u, _| 1.0 - step(p * 0.5, (u - 0.5).abs())),
        "clock_wipe" | "radial_wipe" => masked(a, b, |u, v| {
            let ang = ((u - 0.5) * aspect).atan2(-(v - 0.5)) / std::f32::consts::TAU;
            let ang = ang.rem_euclid(1.0);
            1.0 - step(p, ang)
        }),
        "iris_round" => {
            let r = p * 0.8;
            masked(a, b, move |u, v| 1.0 - step(r, (((u - 0.5) * aspect).powi(2) + (v - 0.5).powi(2)).sqrt()))
        }
        "iris_box" => masked(a, b, |u, v| 1.0 - step(p * 0.5, (u - 0.5).abs().max((v - 0.5).abs()))),
        "iris_diamond" => masked(a, b, |u, v| 1.0 - step(p, (u - 0.5).abs() + (v - 0.5).abs())),
        "iris_cross" => masked(a, b, |u, v| 1.0 - step(p * 0.5, (u - 0.5).abs().min((v - 0.5).abs()) * 2.0 - p * 0.0).min(1.0)),
        "venetian_blinds" => masked(a, b, |_, v| 1.0 - step(p, (v * 10.0).fract())),
        "checker_wipe" => masked(a, b, |u, v| {
            let cxv = (u * 8.0).floor() as i32 + (v * 8.0).floor() as i32;
            let off = if cxv % 2 == 0 { 0.0 } else { 0.5 };
            1.0 - step((p * 2.0 - off).clamp(0.0, 1.0), (u * 8.0).fract())
        }),
        "gradient_wipe" => {
            let soft = e.f64_at("softness", filmcraft_time::Tick::ZERO) as f32 / 100.0 + 0.02;
            masked(a, b, move |u, v| {
                let g = (u * 0.7 + v * 0.3).clamp(0.0, 1.0);
                ((p * (1.0 + soft) - g) / soft).clamp(0.0, 1.0)
            })
        }
        "inset" => masked(a, b, |u, v| if u < p && v < p { 1.0 } else { 0.0 }),
        "center_split" => {
            let mut out = b.clone();
            let off_x = w * 0.5 * pd;
            let off_y = h * 0.5 * pd;
            let quads = [(-off_x, -off_y, 0.0, 0.0), (off_x, -off_y, 0.5, 0.0), (-off_x, off_y, 0.0, 0.5), (off_x, off_y, 0.5, 0.5)];
            for (dx, dy, qx, qy) in quads {
                let mut q = masked(a, &Image::new(a.w, a.h), move |u, v| if u >= qx && u < qx + 0.5 && v >= qy && v < qy + 0.5 { 0.0 } else { 1.0 });
                q = shifted(&q, dx, dy);
                over(&mut out, &q);
            }
            out
        }
        "split" => {
            let mut out = b.clone();
            let left = masked(a, &Image::new(a.w, a.h), |u, _| if u < 0.5 { 0.0 } else { 1.0 });
            let right = masked(a, &Image::new(a.w, a.h), |u, _| if u >= 0.5 { 0.0 } else { 1.0 });
            over(&mut out, &shifted(&left, -w * 0.5 * pd, 0.0));
            over(&mut out, &shifted(&right, w * 0.5 * pd, 0.0));
            out
        }
        "band_slide" => {
            let mut out = a.clone();
            let bands = 6;
            let bb = masked(&Image::new(a.w, a.h), b, |_, _| 1.0);
            for i in 0..bands {
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                let band = masked(&Image::new(a.w, a.h), &bb, move |_, v| if (v * bands as f32).floor() as i32 == i { 1.0 } else { 0.0 });
                over(&mut out, &shifted(&band, sign * w * (1.0 - pd), 0.0));
            }
            out
        }
        "push" | "whip" => {
            let (dx, dy) = match dir(e) {
                0 => (0.0, h),
                1 => (-w, 0.0),
                2 => (0.0, -h),
                _ => (w, 0.0),
            };
            let ease = if e.effect == "whip" { ease_in_out(pd) } else { pd };
            let mut out = shifted(a, dx * ease, dy * ease);
            over(&mut out, &shifted(b, dx * (ease - 1.0), dy * (ease - 1.0)));
            if e.effect == "whip" {
                let s = (1.0 - (2.0 * pd - 1.0).abs()) as f32 * 40.0 * (a.w as f32 / 1920.0);
                if s > 0.5 {
                    crate::effects::gaussian(&mut out, s, 0.0, true);
                }
            }
            out
        }
        "slide" => {
            let (dx, dy) = match dir(e) {
                0 => (0.0, -h),
                1 => (w, 0.0),
                2 => (0.0, h),
                _ => (-w, 0.0),
            };
            let mut out = a.clone();
            over(&mut out, &shifted(b, dx * (1.0 - pd), dy * (1.0 - pd)));
            out
        }
        "cross_zoom" => {
            let z = 1.0 + 3.0 * (if pd < 0.5 { pd * 2.0 } else { (1.0 - pd) * 2.0 });
            let src = if pd < 0.5 { a } else { b };
            let m = Affine::translate(w / 2.0, h / 2.0).then_apply(&Affine::scale(z, z)).then_apply(&Affine::translate(-w / 2.0, -h / 2.0));
            let mut out = src.transformed(a.w, a.h, &m);
            let blur = (z as f32 - 1.0) * 6.0 * (a.w as f32 / 1920.0);
            if blur > 0.3 {
                crate::effects::gaussian(&mut out, blur, blur, true);
            }
            out
        }
        "flip_over" | "cube_spin" => {
            // Horizontal squeeze approximating a 3D rotation (A shrinks, then B grows).
            let (src, k) = if pd < 0.5 { (a, 1.0 - pd * 2.0) } else { (b, (pd - 0.5) * 2.0) };
            let k = k.max(0.001);
            let cxp = if e.effect == "cube_spin" { if pd < 0.5 { 0.0 } else { w } } else { w / 2.0 };
            let m = Affine::translate(cxp, 0.0).then_apply(&Affine::scale(k, 1.0)).then_apply(&Affine::translate(-cxp, 0.0));
            if e.effect == "cube_spin" {
                let ma = Affine::scale(1.0 - pd, 1.0);
                let mb = Affine::translate(w * (1.0 - pd), 0.0).then_apply(&Affine::scale(pd.max(0.001), 1.0));
                let mut out = a.transformed(a.w, a.h, &ma);
                over(&mut out, &b.transformed(a.w, a.h, &mb));
                return out;
            }
            src.transformed(a.w, a.h, &m)
        }
        "page_peel" | "page_turn" => {
            // Diagonal fold line sweeping from the bottom-right corner, with a shaded curl band.
            let mut out = masked(a, b, |u, v| step(1.0 - p * 1.1, 1.0 - (u + v) * 0.5 + 0.0) * 0.0 + if (u + v) * 0.5 > 1.0 - p { 1.0 } else { 0.0 });
            let w_ = a.w;
            out.px.par_chunks_mut(w_ * 4).enumerate().for_each(|(y, row)| {
                for x in 0..w_ {
                    let u = x as f32 / a.w as f32;
                    let v = y as f32 / a.h as f32;
                    let d = (u + v) * 0.5 - (1.0 - p);
                    if d < 0.0 && d > -0.06 {
                        let shade = 0.75 + 0.25 * (-d / 0.06);
                        for c in 0..3 {
                            row[x * 4 + c] *= shade;
                        }
                    }
                }
            });
            out
        }
        _ => masked(a, b, |_, _| p),
    }
}

fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Audio crossfade gains (out, in) for progress p.
pub fn audio_gains(kind: &str, p: f32) -> (f32, f32) {
    match kind {
        "constant_gain" => (1.0 - p, p),
        "exponential_fade" => ((1.0 - p).powi(3), 1.0 - (1.0 - p).powi(3)),
        _ => ((p * std::f32::consts::FRAC_PI_2).cos(), (p * std::f32::consts::FRAC_PI_2).sin()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        let a = Image::filled(16, 8, [1.0, 0.0, 0.0, 1.0]);
        let b = Image::filled(16, 8, [0.0, 0.0, 1.0, 1.0]);
        for def in filmcraft_project::effect_defs().iter().filter(|d| d.kind == filmcraft_project::EffectKind::VideoTransition) {
            let e = def.instance();
            let end = apply(&e, &a, &b, 1.0);
            let c = end.get(8, 4);
            assert!(c.iter().all(|v| v.is_finite()), "{}", def.id);
            if !matches!(
                def.id,
                "center_split" | "band_slide" | "page_peel" | "page_turn" | "cube_spin" | "iris_cross" | "inset" | "checker_wipe" | "venetian_blinds"
            ) {
                assert!(c[2] > 0.9 && c[0] < 0.1, "{} at p=1 shows B: {c:?}", def.id);
            }
            let start = apply(&e, &a, &b, 0.0);
            assert!(start.px.iter().all(|v| v.is_finite()));
        }
    }
}
