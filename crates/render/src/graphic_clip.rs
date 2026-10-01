//! Graphic clips: text and shape layers drawn as vectors straight into the output (no resampling),
//! with fill, up to two strokes (outer / centre / inner), a background box and a drop shadow.
//! Each layer is rasterised into a tight premultiplied linear-light image that is cached while
//! the layer and its transform stay the same.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::EffectInstance;
use filmcraft_project::graphic::{LayerContent, LayerSpec, ShapeProps, TextProps, eval_layer};
use filmcraft_text::raster::{Xform, apply, fill_into, xform_scale};
use filmcraft_text::{Align, Caps, Layout, Mask, ParagraphStyle, StrokeKind, TextStyle, layout, mask, render};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::graphics::premul_linear;
use crate::image::Image;

/// f64 affine → the text crate's f32 transform.
pub fn xform(a: &Affine) -> Xform {
    [a.a as f32, a.b as f32, a.c as f32, a.d as f32, a.e as f32, a.f as f32]
}

/// Layer pixels → graphic canvas pixels (position / anchor / scale / rotation).
pub fn layer_matrix(spec: &LayerSpec) -> Affine {
    let t = &spec.transform;
    Affine::motion(t.position, t.scale, t.rotation, t.anchor)
}

/// Text style + paragraph style of a text layer.
pub fn text_styles(t: &TextProps) -> (TextStyle, ParagraphStyle) {
    (
        TextStyle {
            family: if t.font.is_empty() { filmcraft_text::fonts::DEFAULT_FAMILY.into() } else { t.font.clone() },
            style: if t.style.is_empty() { "Regular".into() } else { t.style.clone() },
            size: t.size,
            tracking: t.tracking,
            kerning: t.kerning,
            ligatures: t.ligatures,
            baseline_shift: t.baseline_shift,
            faux_bold: t.faux_bold,
            faux_italic: t.faux_italic,
            caps: match t.caps {
                1 => Caps::All,
                2 => Caps::Small,
                _ => Caps::Normal,
            },
            underline: t.underline,
        },
        ParagraphStyle {
            align: match t.align {
                1 => Align::Center,
                2 => Align::Right,
                3 => Align::Justify,
                _ => Align::Left,
            },
            leading: t.leading,
            width: (t.box_width > 0.0).then_some(t.box_width),
            rtl: None,
        },
    )
}

/// The laid-out text of a text layer (cached by the text engine).
pub fn text_layout(t: &TextProps) -> Arc<Layout> {
    let (st, ps) = text_styles(t);
    layout(&t.text, &st, &ps)
}

/// A shape layer's outline in layer pixels (centred on the layer origin).
pub fn shape_path(s: &ShapeProps) -> filmcraft_text::Path {
    let (w, h) = (s.size.0 / 2.0, s.size.1 / 2.0);
    match s.shape {
        1 => filmcraft_text::Path::ellipse(0.0, 0.0, w, h),
        2 => {
            let n = s.sides.max(3);
            let pts: Vec<(f32, f32)> = (0..n)
                .map(|i| {
                    let a = std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                    (a.cos() * w, a.sin() * h)
                })
                .collect();
            filmcraft_text::Path::polygon(&pts)
        }
        3 if s.points.len() >= 3 => filmcraft_text::Path::polygon(&s.points.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>()),
        _ => filmcraft_text::Path::round_rect(-w, -h, w, h, s.corner_radius),
    }
}

/// Content bounds `[x0, y0, x1, y1]` of a layer in layer pixels (text box or shape box).
pub fn layer_local_bounds(spec: &LayerSpec) -> [f32; 4] {
    match &spec.content {
        LayerContent::Text(t) => text_layout(t).bounds,
        LayerContent::Shape(s) => match shape_path(s).bounds() {
            Some(b) => [b.0, b.1, b.2, b.3],
            None => [0.0; 4],
        },
    }
}

/// The four corners of a layer's content box in graphic canvas pixels (selection boxes, hit
/// testing, alignment), clockwise from the top-left.
pub fn layer_quad(spec: &LayerSpec) -> [Vec2; 4] {
    let b = layer_local_bounds(spec);
    let m = layer_matrix(spec);
    [Vec2::new(b[0] as f64, b[1] as f64), Vec2::new(b[2] as f64, b[1] as f64), Vec2::new(b[2] as f64, b[3] as f64), Vec2::new(b[0] as f64, b[3] as f64)]
        .map(|p| m.apply(p))
}

/// A rendered layer: premultiplied linear RGBA at device position (`x`, `y`).
#[derive(Debug)]
pub struct LayerRaster {
    pub x: i32,
    pub y: i32,
    pub img: Image,
}

fn stroke_kind(k: u32) -> StrokeKind {
    match k {
        1 => StrokeKind::Center,
        2 => StrokeKind::Inner,
        _ => StrokeKind::Outer,
    }
}

fn bbox_of(m: &Xform, b: [f32; 4], pad: f32) -> [f32; 4] {
    let pts = [(b[0] - pad, b[1] - pad), (b[2] + pad, b[1] - pad), (b[0] - pad, b[3] + pad), (b[2] + pad, b[3] + pad)];
    let mut o = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (x, y) in pts {
        let (a, c) = apply(m, x, y);
        o = [o[0].min(a), o[1].min(c), o[2].max(a), o[3].max(c)];
    }
    o
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Over-composite `c` (premultiplied) through `m` into `px` (premultiplied RGBA, same size).
fn over_mask(px: &mut [f32], m: &Mask, c: [f32; 4]) {
    if c[3] <= 0.0 || m.w == 0 {
        return;
    }
    px.par_chunks_mut(4 * m.w).zip(m.a.par_chunks(m.w)).for_each(|(row, mrow)| {
        for (p, &a) in row.chunks_exact_mut(4).zip(mrow) {
            if a > 0.0 {
                let k = 1.0 - c[3] * a;
                for i in 0..4 {
                    p[i] = c[i] * a + p[i] * k;
                }
            }
        }
    });
}

/// Rasterise one layer with layer→device transform `m`, clipped to a `cw`×`ch` canvas.
pub fn raster_layer(spec: &LayerSpec, m: &Xform, cw: usize, ch: usize) -> Option<LayerRaster> {
    if !spec.enabled || spec.transform.opacity <= 0.0 {
        return None;
    }
    let s = xform_scale(m).max(1e-6);
    let ap = &spec.appearance;
    let text = match &spec.content {
        LayerContent::Text(t) => {
            let l = text_layout(t);
            if l.glyphs.is_empty() && l.underlines.is_empty() && ap.background.is_none() {
                return None;
            }
            Some(l)
        }
        LayerContent::Shape(_) => None,
    };
    let lb = match &text {
        Some(l) => l.bounds,
        None => layer_local_bounds(spec),
    };
    let shape = match &spec.content {
        LayerContent::Shape(sh) => Some(shape_path(sh).transformed(m)),
        _ => None,
    };
    let stroke_out = ap
        .strokes
        .iter()
        .map(|(_, w, k)| {
            if *k == 2 {
                0.0
            } else if *k == 1 {
                w / 2.0
            } else {
                *w
            }
        })
        .fold(0.0f32, f32::max);
    let overhang = text.as_ref().map_or(0.0, |l| l.glyphs.iter().map(|g| g.size).fold(0.0f32, f32::max) * 0.35);
    let mut bb = bbox_of(m, lb, overhang + stroke_out + 2.0 / s);
    if let Some((_, pad, _)) = ap.background {
        bb = union(bb, bbox_of(m, lb, pad + 1.0 / s));
    }
    let mut shadow_dev = (0.0f32, 0.0f32);
    if let Some(sh) = &ap.shadow {
        // the offset is in layer space, so it turns and scales with the layer
        shadow_dev = (m[0] * sh.offset.0 + m[2] * sh.offset.1, m[1] * sh.offset.0 + m[3] * sh.offset.1);
        let grow = (sh.size + sh.blur) * s + 2.0;
        bb = union(bb, [bb[0] + shadow_dev.0 - grow, bb[1] + shadow_dev.1 - grow, bb[2] + shadow_dev.0 + grow, bb[3] + shadow_dev.1 + grow]);
    }
    // clip to the canvas, keeping enough margin for blur to read content just outside it
    let margin = ap.shadow.as_ref().map_or(2.0, |sh| (sh.blur + sh.size) * s + shadow_dev.0.abs().max(shadow_dev.1.abs()) + 2.0);
    let x0 = bb[0].floor().max(-margin) as i32;
    let y0 = bb[1].floor().max(-margin) as i32;
    let x1 = bb[2].ceil().min(cw as f32 + margin) as i32;
    let y1 = bb[3].ceil().min(ch as f32 + margin) as i32;
    if x1 <= x0 || y1 <= y0 || x1 <= 0 || y1 <= 0 || x0 >= cw as i32 || y0 >= ch as i32 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if w * h > 64 << 20 {
        return None;
    }
    let mut cover = Mask::new(w, h);
    if let Some(l) = &text {
        render::draw(l, m, &mut cover, (x0, y0));
    }
    if let Some(p) = &shape {
        fill_into(p, &mut cover, -x0 as f32, -y0 as f32);
    }
    let strokes: Vec<(Mask, [f32; 4])> = ap.strokes.iter().map(|(c, sw, k)| (mask::stroke(&cover, sw * s, stroke_kind(*k)), *c)).collect();
    let bg = ap.background.map(|(c, pad, r)| {
        let p = filmcraft_text::Path::round_rect(lb[0] - pad, lb[1] - pad, lb[2] + pad, lb[3] + pad, r).transformed(m);
        let mut bm = Mask::new(w, h);
        fill_into(&p, &mut bm, -x0 as f32, -y0 as f32);
        (bm, c)
    });
    let mut px = vec![0.0f32; w * h * 4];
    if let Some(sh) = &ap.shadow {
        let mut u = if ap.fill.is_some() { cover.clone() } else { Mask::new(w, h) };
        for (sm, _) in &strokes {
            u.max_with(sm);
        }
        if let Some((bm, _)) = &bg {
            u.max_with(bm);
        }
        if sh.size > 0.0 {
            u = mask::stroke(&u, sh.size * s, StrokeKind::Outer);
        }
        let u = mask::offset(&u, shadow_dev.0.round() as i32, shadow_dev.1.round() as i32);
        let u = mask::blur(&u, sh.blur * s / 3.0);
        over_mask(&mut px, &u, premul_linear(sh.color));
    }
    if let Some((bm, c)) = &bg {
        over_mask(&mut px, bm, premul_linear(*c));
    }
    // outer strokes sit under the fill; centre and inner strokes are drawn over it
    let outer = |k: u32| k != 1 && k != 2;
    for ((sm, c), (_, _, k)) in strokes.iter().zip(&ap.strokes).rev() {
        if outer(*k) {
            over_mask(&mut px, sm, premul_linear(*c));
        }
    }
    if let Some(c) = ap.fill {
        over_mask(&mut px, &cover, premul_linear(c));
    }
    for ((sm, c), (_, _, k)) in strokes.iter().zip(&ap.strokes).rev() {
        if !outer(*k) {
            over_mask(&mut px, sm, premul_linear(*c));
        }
    }
    let op = spec.transform.opacity;
    if op < 1.0 {
        px.iter_mut().for_each(|v| *v *= op);
    }
    Some(LayerRaster { x: x0, y: y0, img: Image { w, h, px } })
}

type Cache = Mutex<HashMap<u64, (u64, Option<Arc<LayerRaster>>)>>;

fn raster_cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// [`raster_layer`] through a small cache (static titles re-composite without re-rasterising).
pub fn raster_layer_cached(spec: &LayerSpec, m: &Xform, cw: usize, ch: usize) -> Option<Arc<LayerRaster>> {
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    format!("{spec:?}").hash(&mut hs);
    m.iter().for_each(|v| v.to_bits().hash(&mut hs));
    (cw, ch).hash(&mut hs);
    let key = hs.finish();
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Some(e) = raster_cache().lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key) {
        e.0 = now;
        return e.1.clone();
    }
    let r = raster_layer(spec, m, cw, ch).map(Arc::new);
    let mut c = raster_cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= 96
        && let Some(k) = c.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| *k)
    {
        c.remove(&k);
    }
    c.insert(key, (now, r.clone()));
    r
}

/// Over-composite a raster at (`r.x` + `dx`, `r.y` + `dy`) onto `img`.
pub fn composite_raster(img: &mut Image, r: &LayerRaster, dx: i32, dy: i32) {
    let (rx, ry) = (r.x + dx, r.y + dy);
    let (w, h) = (img.w as i32, img.h as i32);
    let ys = ry.max(0)..(ry + r.img.h as i32).min(h);
    let xs = rx.max(0)..(rx + r.img.w as i32).min(w);
    if ys.is_empty() || xs.is_empty() {
        return;
    }
    let iw = img.w;
    img.px[ys.start as usize * iw * 4..ys.end as usize * iw * 4].par_chunks_mut(iw * 4).enumerate().for_each(|(row_i, row)| {
        let sy = (ys.start + row_i as i32 - ry) as usize;
        for x in xs.clone() {
            let si = (sy * r.img.w + (x - rx) as usize) * 4;
            let s = &r.img.px[si..si + 4];
            if s[3] <= 0.0 {
                continue;
            }
            let d = &mut row[x as usize * 4..x as usize * 4 + 4];
            let k = 1.0 - s[3];
            for c in 0..4 {
                d[c] = s[c] + d[c] * k;
            }
        }
    });
}

/// Evaluated layers of a graphic clip's effect list at clip time `t`, in paint order.
pub fn graphic_specs(effects: &[EffectInstance], t: Tick, canvas: (u32, u32)) -> Vec<LayerSpec> {
    effects.iter().filter(|e| filmcraft_project::graphic::is_layer(e)).filter_map(|e| eval_layer(e, t, canvas)).collect()
}

/// Draw all layers of a graphic clip onto `out`; `m` maps graphic canvas pixels to `out` pixels.
pub fn render_graphic(effects: &[EffectInstance], t: Tick, canvas: (u32, u32), m: &Affine, out: &mut Image) {
    for spec in graphic_specs(effects, t, canvas) {
        let lm = xform(&m.then_apply(&layer_matrix(&spec)));
        if let Some(r) = raster_layer_cached(&spec, &lm, out.w, out.h) {
            composite_raster(out, &r, 0, 0);
        }
    }
}

/// All layers composed into one tight image (for the GPU plan): `(image, x, y)` in output pixels.
pub fn render_graphic_tight(effects: &[EffectInstance], t: Tick, canvas: (u32, u32), m: &Affine, w: usize, h: usize) -> Option<(Image, i32, i32)> {
    let rasters: Vec<Arc<LayerRaster>> =
        graphic_specs(effects, t, canvas).iter().filter_map(|spec| raster_layer_cached(spec, &xform(&m.then_apply(&layer_matrix(spec))), w, h)).collect();
    let x0 = rasters.iter().map(|r| r.x).min()?.max(0);
    let y0 = rasters.iter().map(|r| r.y).min()?.max(0);
    let x1 = rasters.iter().map(|r| r.x + r.img.w as i32).max()?.min(w as i32);
    let y1 = rasters.iter().map(|r| r.y + r.img.h as i32).max()?.min(h as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let mut img = Image::new((x1 - x0) as usize, (y1 - y0) as usize);
    for r in &rasters {
        composite_raster(&mut img, r, -x0, -y0);
    }
    Some((img, x0, y0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
    use filmcraft_project::{ParamValue, find_effect};

    fn set(e: &mut EffectInstance, k: &str, v: ParamValue) {
        e.params.get_mut(k).unwrap_or_else(|| panic!("{k}")).value = v;
    }

    #[test]
    fn text_layer_renders_white_text_at_position() {
        let e = new_text_layer("HELLO", Vec2::new(100.0, 150.0), 60.0);
        let mut img = Image::new(400, 200);
        render_graphic(&[e], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
        let ink: Vec<(usize, usize)> = (0..200).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.5).collect();
        assert!(ink.len() > 500);
        let minx = ink.iter().map(|p| p.0).min().unwrap();
        let maxy = ink.iter().map(|p| p.1).max().unwrap();
        assert!((98..=106).contains(&minx), "starts at the position: {minx}");
        assert!((145..=152).contains(&maxy), "sits on the baseline: {maxy}");
        assert!(img.get(ink[0].0, ink[0].1)[0] > 0.4, "white");
    }

    #[test]
    fn shape_appearance_layers() {
        let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
        set(&mut e, "fill_color", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
        set(&mut e, "stroke", ParamValue::Bool(true));
        set(&mut e, "stroke_width", ParamValue::Float(6.0));
        set(&mut e, "stroke_color", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
        set(&mut e, "shadow", ParamValue::Bool(true));
        set(&mut e, "shadow_distance", ParamValue::Float(20.0));
        set(&mut e, "shadow_blur", ParamValue::Float(0.0));
        let mut img = Image::new(200, 200);
        render_graphic(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        let p = img.get(100, 100);
        assert!(p[2] > 0.9 && p[0] < 0.05, "blue fill {p:?}");
        let s = img.get(100, 78);
        assert!(s[0] > 0.9 && s[2] < 0.05, "red outer stroke above the box {s:?}");
        // shadow: below-right of the box, outside the stroke
        let sh = img.get(150, 135);
        assert!(sh[3] > 0.5 && sh[0] < 0.05 && sh[2] < 0.05, "black shadow {sh:?}");
        assert_eq!(img.get(20, 20)[3], 0.0);
    }

    #[test]
    fn inner_and_centre_strokes() {
        for (kind, inside, outside) in [(2u32, true, false), (1, true, true)] {
            let mut e = new_shape_layer(2, Vec2::new(100.0, 100.0), Vec2::new(120.0, 120.0), vec![]);
            set(&mut e, "sides", ParamValue::Float(4.0));
            set(&mut e, "stroke", ParamValue::Bool(true));
            set(&mut e, "stroke_width", ParamValue::Float(6.0));
            set(&mut e, "stroke_type", ParamValue::Choice(kind));
            set(&mut e, "stroke_color", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
            let mut img = Image::new(200, 200);
            render_graphic(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
            // diamond: top vertex at y = 40; probe straight below/above the right vertex (160, 100)
            let green = |x: usize| {
                let p = img.get(x, 100);
                p[1] > 0.9 && p[0] < 0.1
            };
            assert_eq!(green(157), inside, "kind {kind} inside");
            assert_eq!(green(161), outside, "kind {kind} outside");
            assert!(!green(120), "fill in the middle");
        }
    }

    #[test]
    fn rotation_scale_and_opacity() {
        let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(100.0, 10.0), vec![]);
        set(&mut e, "rotation", ParamValue::Float(90.0));
        set(&mut e, "opacity", ParamValue::Float(50.0));
        let mut img = Image::new(200, 200);
        render_graphic(&[e.clone()], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        assert!((img.get(100, 60)[3] - 0.5).abs() < 0.02, "vertical after 90°");
        assert_eq!(img.get(60, 100)[3], 0.0);
        // a 2× output transform (e.g. Motion scale) draws vectors crisply at the larger size
        let mut big = Image::new(400, 400);
        render_graphic(&[e], Tick::ZERO, (200, 200), &Affine::scale(2.0, 2.0), &mut big);
        assert!((big.get(200, 110)[3] - 0.5).abs() < 0.02);
        let q = layer_quad(&eval_layer(&find_effect("graphic_shape").unwrap().instance(), Tick::ZERO, (200, 200)).unwrap());
        assert!((q[0].x - (100.0 - 200.0)).abs() < 1e-6 && (q[2].y - (100.0 + 100.0)).abs() < 1e-6);
    }

    #[test]
    fn tight_image_matches_full_render() {
        let a = new_text_layer("Tight", Vec2::new(50.0, 80.0), 40.0);
        let b = new_shape_layer(1, Vec2::new(150.0, 120.0), Vec2::new(60.0, 60.0), vec![]);
        let fx = vec![b, a];
        let mut full = Image::new(220, 160);
        render_graphic(&fx, Tick::ZERO, (220, 160), &Affine::IDENTITY, &mut full);
        let (tight, x, y) = render_graphic_tight(&fx, Tick::ZERO, (220, 160), &Affine::IDENTITY, 220, 160).unwrap();
        assert!(tight.w < 220 && tight.h < 160);
        for yy in 0..tight.h {
            for xx in 0..tight.w {
                let (p, q) = (tight.get(xx, yy), full.get(xx + x as usize, yy + y as usize));
                assert!((p[3] - q[3]).abs() < 1e-5);
            }
        }
    }
}
