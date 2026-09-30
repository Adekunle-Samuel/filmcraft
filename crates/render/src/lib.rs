//! Sequence evaluation and the CPU reference compositor.
//!
//! [`render_sequence`] turns (project, sequence, time, scale) into a premultiplied linear-light
//! image. Tracks composite bottom (V1) to top. Per item: fetch the source frame at the mapped media
//! time (already reduced for low-resolution playback), run standard effects, then the fixed effects
//! (Motion → Opacity/blend), then composite. Transitions combine outgoing/incoming layers.
//!
//! The same code renders monitors, thumbnails and exports, and is the oracle for the GPU path.

pub mod audio;
pub mod blend;
pub mod effects;
pub mod image;
pub mod plan;
pub mod transitions;

use std::sync::Arc;

use filmcraft_geom::{Affine, Vec2};
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_project::{ItemId, ItemKind, ParamValue, Project, Sequence, TrackItem};
use filmcraft_time::{Tick, TimeDisplay, format_time};

pub use blend::Blend;
pub use image::Image;

/// Resolves project items to media sources (the engine owns the media pool).
pub trait SourceProvider: Sync {
    fn source(&self, item: ItemId) -> Option<SharedSource>;
}

impl<F: Fn(ItemId) -> Option<SharedSource> + Sync> SourceProvider for F {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        self(item)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Output scale relative to the sequence frame size (1.0 = full, 0.5 = ½ resolution…).
    pub scale: f32,
    /// Skip standard effects (fast scrubbing / "Toggle Effects" off).
    pub effects: bool,
    /// Nesting depth guard.
    pub depth: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { scale: 1.0, effects: true, depth: 0 }
    }
}

/// Output size for a sequence at a scale.
pub fn output_size(seq: &Sequence, scale: f32) -> (usize, usize) {
    (((seq.settings.width as f32 * scale).round() as usize).max(1), ((seq.settings.height as f32 * scale).round() as usize).max(1))
}

/// Render sequence `seq_id` at timeline time `t`.
pub fn render_sequence(project: &Project, seq_id: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Image {
    let Some(seq) = project.sequence(seq_id) else { return Image::new(1, 1) };
    render_seq(project, seq, t, opts, sources)
}

fn render_seq(project: &Project, seq: &Sequence, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Image {
    let (w, h) = output_size(seq, opts.scale);
    let mut canvas = Image::new(w, h);
    if opts.depth > 8 {
        return canvas;
    }
    let tc = format_time(t, seq.settings.frame_rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    for track in &seq.video_tracks {
        if !track.enabled {
            continue;
        }
        // transition covering t?
        if let Some(tr) = track.transitions.iter().find(|tr| tr.range().contains(t)) {
            let a = tr.from.and_then(|id| track.item(id)).filter(|i| i.enabled);
            let b = tr.to.and_then(|id| track.item(id)).filter(|i| i.enabled);
            let la = a
                .and_then(|i| item_layer(project, seq, i, t, opts, sources, &tc))
                .map(|(img, op, _)| with_opacity(img, op))
                .unwrap_or_else(|| Image::new(w, h));
            let lb = b
                .and_then(|i| item_layer(project, seq, i, t, opts, sources, &tc))
                .map(|(img, op, _)| with_opacity(img, op))
                .unwrap_or_else(|| Image::new(w, h));
            let mut p = tr.progress(t) as f32;
            if tr.reverse {
                p = 1.0 - p;
            }
            let mixed = transitions::apply(&tr.effect, &la, &lb, p);
            blend::composite(&mut canvas, &mixed, 1.0, Blend::Normal);
            continue;
        }
        let Some(item) = track.item_at(t) else { continue };
        if !item.enabled {
            continue;
        }
        let pi = project.item(item.item);
        if let Some(pi) = pi
            && matches!(pi.kind, ItemKind::AdjustmentLayer { .. })
        {
            if !opts.effects {
                continue;
            }
            let mt = item.source_time_at(t);
            let mut adjusted = canvas.clone();
            let cx = effects::FxCtx { t: mt, px_scale: opts.scale, seconds: (t - item.start).seconds(), timecode: &tc, clip_name: &item.name };
            for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic)) {
                effects::apply(&mut adjusted, e, &cx);
            }
            let (op, bl) = opacity_blend(item, mt);
            // Adjustment layer opacity mixes adjusted over original.
            let mut out = canvas.clone();
            let mut adj = adjusted;
            adj.scale_alpha(op);
            blend::composite(&mut out, &adj, 1.0, bl);
            canvas = out;
            continue;
        }
        if let Some((layer, op, bl)) = item_layer(project, seq, item, t, opts, sources, &tc) {
            blend::composite(&mut canvas, &layer, op, bl);
        }
    }
    canvas
}

fn with_opacity(mut img: Image, op: f32) -> Image {
    img.scale_alpha(op);
    img
}

pub(crate) fn opacity_blend(item: &TrackItem, mt: Tick) -> (f32, Blend) {
    match item.effect("opacity") {
        Some(e) if e.enabled => {
            let op = (e.f64_at("opacity", mt) / 100.0).clamp(0.0, 1.0) as f32;
            let bl = match e.param("blend").map(|p| &p.value) {
                Some(ParamValue::Choice(c)) => Blend::from_index(*c),
                _ => Blend::Normal,
            };
            (op, bl)
        }
        _ => (1.0, Blend::Normal),
    }
}

/// Size of an item's source at full resolution.
pub(crate) fn source_size(project: &Project, item: ItemId) -> Option<(u32, u32)> {
    match &project.item(item)?.kind {
        ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
        ItemKind::Sequence(s) => Some((s.settings.width, s.settings.height)),
        ItemKind::AdjustmentLayer { width, height, .. } => Some((*width, *height)),
        ItemKind::Subclip { parent, .. } => source_size(project, *parent),
    }
}

/// The Motion transform of an item at media time `mt`, mapping full-res source pixels to
/// full-res sequence pixels.
pub fn motion_matrix(seq: &Sequence, item: &TrackItem, src: (u32, u32), mt: Tick) -> Affine {
    let (sw, sh) = (seq.settings.width as f64, seq.settings.height as f64);
    let mut pos = Vec2::new(sw / 2.0, sh / 2.0);
    let mut anchor = Vec2::new(src.0 as f64 / 2.0, src.1 as f64 / 2.0);
    let mut scale = Vec2::new(1.0, 1.0);
    let mut rot = 0.0;
    if let Some(m) = item.effect("motion").filter(|m| m.enabled) {
        let p = m.vec2_at("position", mt);
        if !p.x.is_nan() && m.param("position").is_some() {
            pos = p;
        }
        let a = m.vec2_at("anchor", mt);
        if !a.x.is_nan() && m.param("anchor").is_some() {
            anchor = a;
        }
        let s = m.f64_at("scale", mt) / 100.0;
        let uniform = m.param("uniform_scale").and_then(|p| p.value.as_bool()).unwrap_or(true);
        let swid = if uniform { s } else { m.f64_at("scale_width", mt) / 100.0 };
        scale = Vec2::new(swid, s);
        rot = m.f64_at("rotation", mt);
    }
    if item.scale_to_frame {
        let fit = (sw / src.0 as f64).min(sh / src.1 as f64);
        scale = scale * fit;
    }
    Affine::motion(pos, scale, rot, anchor)
}

/// Render one track item's layer at timeline time `t` into a canvas-sized image.
/// Returns (layer, opacity, blend).
pub(crate) fn item_layer(
    project: &Project,
    seq: &Sequence,
    item: &TrackItem,
    t: Tick,
    opts: RenderOptions,
    sources: &dyn SourceProvider,
    tc: &str,
) -> Option<(Image, f32, Blend)> {
    let (w, h) = output_size(seq, opts.scale);
    let mt = item.source_time_at(t);
    let src_size = source_size(project, item.item)?;
    let motion = motion_matrix(seq, item, src_size, mt);
    // How many output pixels one source pixel covers → request a reduced frame when possible.
    let lin = ((motion.a * motion.a + motion.b * motion.b).sqrt()).max((motion.c * motion.c + motion.d * motion.d).sqrt());
    let want = (lin * opts.scale as f64).clamp(1.0 / 64.0, 1.0) as f32;
    let pi = project.item(item.item)?;
    let mut layer = match &pi.kind {
        ItemKind::Media(_) | ItemKind::Subclip { .. } => {
            let src = sources.source(item.item)?;
            let frame = src.video_frame(FrameRequest { time: mt, scale: want }).ok()?;
            let n = decimation(frame.width as f32, src_size.0 as f32 * want);
            let (w, h, px) = frame.to_linear_f32_decimated(n);
            Image { w, h, px }
        }
        ItemKind::Sequence(nested) => {
            let sub = RenderOptions { scale: want, effects: opts.effects, depth: opts.depth + 1 };
            render_seq(project, nested, mt, sub, sources)
        }
        ItemKind::AdjustmentLayer { .. } => return None,
    };
    let px_scale = layer.w as f32 / src_size.0.max(1) as f32;
    if opts.effects {
        let cx = effects::FxCtx { t: mt, px_scale, seconds: (t - item.start).seconds(), timecode: tc, clip_name: &item.name };
        for e in item.effects.iter().filter(|e| e.def().is_some_and(|d| !d.intrinsic)) {
            effects::apply(&mut layer, e, &cx);
        }
    }
    // layer px → source px → sequence px → output px
    let m = Affine::scale(opts.scale as f64, opts.scale as f64).then_apply(&motion).then_apply(&Affine::scale(1.0 / px_scale as f64, 1.0 / px_scale as f64));
    let placed = if layer.w == w
        && layer.h == h
        && (m.a - 1.0).abs() < 1e-9
        && (m.d - 1.0).abs() < 1e-9
        && m.b == 0.0
        && m.c == 0.0
        && m.e.abs() < 1e-9
        && m.f.abs() < 1e-9
    {
        layer
    } else {
        layer.transformed(w, h, &m)
    };
    let (op, bl) = opacity_blend(item, mt);
    Some((placed, op, bl))
}

/// Largest power-of-two box decimation that keeps at least `target_w` pixels of width.
fn decimation(have_w: f32, target_w: f32) -> usize {
    let mut n = 1usize;
    while n < 16 && have_w / (n as f32 * 2.0) >= target_w.max(1.0) {
        n *= 2;
    }
    n
}

/// Render a single project item (e.g. for the Source monitor) at media time `t`.
pub fn render_item(project: &Project, item: ItemId, t: Tick, scale: f32, sources: &dyn SourceProvider) -> Option<Image> {
    let pi = project.item(item)?;
    match &pi.kind {
        ItemKind::Sequence(s) => Some(render_seq(project, s, t, RenderOptions { scale, ..Default::default() }, sources)),
        _ => {
            let src = sources.source(item)?;
            let f = src.video_frame(FrameRequest { time: t, scale }).ok()?;
            let full_w = src.info().video.as_ref().map_or(f.width, |v| v.width) as f32;
            let n = decimation(f.width as f32, full_w * scale);
            let (w, h, px) = f.to_linear_f32_decimated(n);
            Some(Image { w, h, px })
        }
    }
}

/// A shared, clonable source map for tests and simple hosts.
#[derive(Default, Clone)]
pub struct SourceMap(pub std::collections::HashMap<ItemId, SharedSource>);

impl SourceProvider for SourceMap {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        self.0.get(&item).cloned()
    }
}

pub fn arc_source(s: impl filmcraft_media::MediaSource + 'static) -> SharedSource {
    Arc::new(s)
}

#[cfg(test)]
mod tests;
