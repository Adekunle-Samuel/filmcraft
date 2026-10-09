//! Bake a Lumetri grade into a 3D LUT (`lumetri.bakeLut`).
//!
//! The whole lattice is laid out as one image (`size²` × `size` pixels, red fastest, then green,
//! then blue: the `.cube` order) and graded by the real Lumetri implementation
//! ([`crate::effects::apply`]), so the LUT reproduces exactly what the renderer does at the
//! lattice points.
//!
//! - **Input:** SDR Rec. 709, display-referred. A lattice value is FilmCraft's SDR grading signal
//!   (the sRGB-encoded display value, 0…1), the same signal Lumetri's Input LUT and Creative Look
//!   slots read, so the baked file can be loaded back into either slot. HDR working spaces are
//!   not represented.
//! - **Per-pixel sections only:** Basic Correction (white balance, tone, saturation, Input LUT),
//!   Creative (look / Look LUT, faded film, vibrance, split toning), Curves, Color Wheels and the
//!   HSL Secondary key and correction. Anything that depends on a pixel's neighbourhood or its
//!   position is switched off for the bake and reported in the warnings: the Vignette, Creative
//!   Sharpen, the HSL Secondary Denoise / Blur refinement and masks.
//! - Animated parameters are baked at their value at the given time.

use filmcraft_color::{GradeSpace, Lut3d, WorkingSpace};
use filmcraft_project::{EffectInstance, Param, ParamValue, Project};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, apply};

/// The lattice sizes offered (points per axis).
pub const SIZES: [usize; 3] = [17, 33, 65];

/// What [`bake_lumetri`] made.
#[derive(Clone, Debug)]
pub struct Baked {
    pub lut: Lut3d,
    /// Parts of the grade the LUT does not contain.
    pub warnings: Vec<String>,
}

fn on(e: &EffectInstance, id: &str) -> bool {
    e.param(id).and_then(|p| p.value.as_bool()).unwrap_or(true)
}

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    let p = e.params.entry(id.to_string()).or_insert_with(|| Param::new(v.clone()));
    p.keyframes.clear();
    p.value = v;
}

/// A copy of a Lumetri instance with its spatial parts switched off (and why, in `warnings`).
pub fn per_pixel_lumetri(e: &EffectInstance, t: Tick, warnings: &mut Vec<String>) -> EffectInstance {
    let mut e = e.clone();
    if on(&e, "vignette_on") && e.f64_at("vignette_amount", t).abs() > 1e-6 {
        warnings.push("Vignette skipped: it depends on the pixel position, which a LUT cannot hold".into());
    }
    set(&mut e, "vignette_on", ParamValue::Bool(false));
    if on(&e, "creative_on") && e.f64_at("sharpen", t).abs() > 1e-6 {
        warnings.push("Creative ▸ Sharpen skipped: it reads neighbouring pixels".into());
    }
    set(&mut e, "sharpen", ParamValue::Float(0.0));
    let hsl_on = e.param("hsl_on").and_then(|p| p.value.as_bool()).unwrap_or(false);
    if hsl_on && (e.f64_at("hsl_denoise", t).abs() > 1e-6 || e.f64_at("hsl_blur", t).abs() > 1e-6) {
        warnings.push("HSL Secondary ▸ Refine (Denoise / Blur) skipped: it filters the key spatially; the key itself is baked".into());
    }
    set(&mut e, "hsl_denoise", ParamValue::Float(0.0));
    set(&mut e, "hsl_blur", ParamValue::Float(0.0));
    if !e.masks.is_empty() {
        warnings.push("Masks skipped: the grade is baked as if it covered the whole frame".into());
        e.masks.clear();
    }
    if e.is_animated() {
        warnings.push(format!("Animated parameters were baked at their values at media time {} ticks", t.0));
    }
    e
}

/// Bake `effects` (Lumetri instances, applied in order; disabled ones are skipped) into a
/// `size`³ LUT on the SDR grading signal. `project` resolves `lib:` LUT references.
pub fn bake_lumetri(project: Option<&Project>, effects: &[EffectInstance], t: Tick, size: usize) -> Result<Baked, String> {
    if !SIZES.contains(&size) {
        return Err(format!("LUT size must be one of 17, 33 or 65 (got {size})"));
    }
    let mut warnings = Vec::new();
    let mut grades = Vec::new();
    for e in effects {
        if e.effect != "lumetri" {
            warnings.push(format!("`{}` is not a Lumetri Color effect and was not baked", e.effect));
            continue;
        }
        if e.enabled {
            grades.push(per_pixel_lumetri(e, t, &mut warnings));
        }
    }
    let gs = GradeSpace::Sdr;
    let n = (size - 1) as f32;
    let w = size * size;
    let mut img = Image::new(w, size);
    for (i, px) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (r, g, b) = (i % size, (i / size) % size, i / w);
        let lin = gs.decode([r as f32 / n, g as f32 / n, b as f32 / n]);
        px.copy_from_slice(&[lin[0], lin[1], lin[2], 1.0]);
    }
    let cx = FxCtx { t, px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project, env: None, working: WorkingSpace::Rec709 };
    for e in &grades {
        apply(&mut img, e, &cx);
    }
    let out: Vec<[f32; 3]> = img
        .px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = if p[3] > 1e-6 { p[3] } else { 1.0 };
            gs.encode([p[0] / a, p[1] / a, p[2] / a]).map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
        })
        .collect();
    let index = |c: [f32; 3]| {
        let k = |v: f32| ((v * n).round().max(0.0) as usize).min(size - 1);
        k(c[0]) + k(c[1]) * size + k(c[2]) * w
    };
    let lut = Lut3d::from_fn(size, |c| out.get(index(c)).copied().unwrap_or(c));
    Ok(Baked { lut, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ProjectLut, find_effect};

    fn lumetri() -> EffectInstance {
        find_effect("lumetri").unwrap().instance()
    }

    #[test]
    fn identity_grade_bakes_an_identity_lut() {
        for size in SIZES {
            let b = bake_lumetri(None, &[lumetri()], Tick::ZERO, size).unwrap();
            assert!(b.warnings.is_empty(), "{:?}", b.warnings);
            let id = Lut3d::identity(size);
            let err = b.lut.data.iter().zip(&id.data).flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs())).fold(0f32, f32::max);
            assert!(err < 1e-3, "size {size}: max error {err}");
        }
        // no grade at all is an identity too
        assert!(bake_lumetri(None, &[], Tick::ZERO, 17).is_ok());
    }

    #[test]
    fn baked_grade_through_the_lut_path_matches_the_direct_render() {
        let mut g = lumetri();
        set(&mut g, "exposure", ParamValue::Float(0.6));
        set(&mut g, "saturation", ParamValue::Float(140.0));
        set(&mut g, "contrast", ParamValue::Float(20.0));
        set(&mut g, "wheel_midtones", ParamValue::Vec2(filmcraft_geom::Vec2::new(0.1, -0.05)));
        let b = bake_lumetri(None, std::slice::from_ref(&g), Tick::ZERO, 33).unwrap();
        // the LUT as a project LUT in a fresh Lumetri's Input LUT slot
        let mut p = Project::new("t");
        p.luts.push(ProjectLut { id: "baked".into(), name: "baked".into(), source_path: None, format: "cube".into(), text: b.lut.to_cube().into() });
        let mut via = lumetri();
        set(&mut via, "input_lut", ParamValue::Text("lib:baked".into()));
        let mut src = Image::new(64, 64);
        for (i, px) in src.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i % 64) as f32 / 63.0, (i / 64) as f32 / 63.0);
            let c = [x * x, y, (1.0 - x) * (0.3 + 0.7 * y)];
            px.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
        let cx = FxCtx { t: Tick::ZERO, px_scale: 1.0, seconds: 0.0, timecode: "", clip_name: "", project: Some(&p), env: None, working: WorkingSpace::Rec709 };
        let (mut direct, mut lut) = (src.clone(), src.clone());
        apply(&mut direct, &g, &cx);
        apply(&mut lut, &via, &cx);
        let enc = |v: f32| filmcraft_color::linear_to_srgb(v.clamp(0.0, 1.0));
        let err = direct.px.iter().zip(&lut.px).map(|(a, b)| (enc(*a) - enc(*b)).abs()).fold(0f32, f32::max);
        assert!(err < 0.02, "max display error {err}");
        // and it is not an identity
        let id_err = direct.px.iter().zip(&src.px).map(|(a, b)| (enc(*a) - enc(*b)).abs()).fold(0f32, f32::max);
        assert!(id_err > 0.1);
    }

    #[test]
    fn spatial_sections_are_skipped_with_warnings_and_bad_sizes_refused() {
        let mut g = lumetri();
        set(&mut g, "vignette_amount", ParamValue::Float(-2.0));
        set(&mut g, "sharpen", ParamValue::Float(50.0));
        set(&mut g, "hsl_on", ParamValue::Bool(true));
        set(&mut g, "hsl_blur", ParamValue::Float(30.0));
        let b = bake_lumetri(
            None,
            &[g, find_effect("gaussian_blur").map(|d| d.instance()).unwrap_or_else(|| EffectInstance { effect: "x".into(), ..lumetri() })],
            Tick::ZERO,
            17,
        )
        .unwrap();
        assert_eq!(b.warnings.len(), 4, "{:?}", b.warnings);
        for bad in [0, 1, 2, 16, 34, 1_000_000, usize::MAX] {
            assert!(bake_lumetri(None, &[lumetri()], Tick::ZERO, bad).is_err());
        }
    }
}
