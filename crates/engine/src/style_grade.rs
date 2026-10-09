//! Grade from a reference (A4.2).
//!
//! - `lumetri.matchToItem {clips?, item, samples?=6 (1–12), faceDetection?=true}`: match the
//!   clips' colour to a reference media item (or subclip, or sequence). `samples` evenly spaced
//!   frames of the reference and of each clip (with its Lumetri switched off) are tiled into one
//!   mosaic each, and [`filmcraft_render::color_match::solve`] (the Apply Match solver) finds
//!   the wheels, their lightness and saturation that move the clip's mosaic statistics to the
//!   reference's. Every clip is written in one undo step; a clip without Lumetri Color gets one
//!   in the same step.
//! - `lumetri.bakeLut {clip, size?=33 (17|33|65), name}`: bake the clip's Lumetri grade into a
//!   `.cube` file in `<data dir>/luts/` ([`filmcraft_render::lut_bake`]) and add it to the
//!   project's LUT library (as `lut.import` does), so `lumetri.setLook` / `setInputLut` can
//!   apply it. Spatial sections are skipped and reported in `warnings`.

use filmcraft_project::{ClipId, ItemId, ItemKind, TrackKind};
use filmcraft_render::Image;
use filmcraft_render::color_match;
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, clip_p, has_seq, item_p, str_p};
use crate::{Result, Session};

/// Width the sampled frames are rendered at before the mosaic (the solver works at ≤ 96 px).
const SAMPLE_WIDTH: f32 = 192.0;
/// Width of one mosaic tile.
const TILE_WIDTH: usize = 96;
pub const DEFAULT_SAMPLES: usize = 6;
pub const MAX_SAMPLES: usize = 12;
/// What a baked LUT expects as input.
pub const BAKE_INPUT: &str = "SDR Rec. 709, display-referred (FilmCraft's sRGB-encoded grading signal, 0–1)";

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "lumetri.matchToItem",
            label: "Match Grade to Item",
            menu: &[],
            shortcut: None,
            params: r#"{"clips":[id]?,"item":id,"samples":1..12=6,"faceDetection":bool=true}"#,
            enabled: has_seq,
            run: match_to_item,
            journal: true,
        },
        CommandSpec {
            id: "lumetri.bakeLut",
            label: "Bake Lumetri to LUT",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id,"size":17|33|65=33,"name":str}"#,
            enabled: has_seq,
            run: bake_lut,
            journal: true,
        },
    ]
}

/// `samples` (absent / null = the default).
fn samples_p(p: &Value, cmd: &str) -> Result<usize> {
    match p.get("samples") {
        None | Some(Value::Null) => Ok(DEFAULT_SAMPLES),
        Some(v) => v
            .as_f64()
            .filter(|f| f.is_finite() && (1.0..=MAX_SAMPLES as f64).contains(f))
            .map(|f| f.round() as usize)
            .ok_or_else(|| bad(cmd, format!("`samples` must be a number from 1 to {MAX_SAMPLES}"))),
    }
}

/// `n` times spread evenly inside `range` (frame centres of n equal parts).
fn spread_times(range: TimeRange, n: usize) -> Vec<Tick> {
    let n = n.max(1) as i128;
    (0..n).map(|k| range.start + Tick(((range.duration.0.max(0) as i128 * (2 * k + 1)) / (2 * n)) as i64)).collect()
}

/// Stack frames (each shrunk to ≤ [`TILE_WIDTH`]) into one image; narrower tiles are padded with
/// transparent pixels, which the statistics ignore.
pub fn mosaic(frames: &[Image]) -> Option<Image> {
    let tiles: Vec<Image> = frames.iter().filter(|f| f.w > 0 && f.h > 0 && f.px.len() >= f.w * f.h * 4).map(|f| color_match::shrink(f, TILE_WIDTH)).collect();
    let w = tiles.iter().map(|t| t.w).max()?;
    let h: usize = tiles.iter().map(|t| t.h).sum();
    let mut out = Image::new(w, h);
    let mut y0 = 0usize;
    for t in &tiles {
        for y in 0..t.h {
            let (Some(src), Some(dst)) = (t.px.get(y * t.w * 4..(y + 1) * t.w * 4), out.px.get_mut((y0 + y) * w * 4..((y0 + y) * w + t.w) * 4)) else {
                continue;
            };
            dst.copy_from_slice(src);
        }
        y0 += t.h;
    }
    out.px.iter().any(|v| *v != 0.0).then_some(out)
}

/// The reference frames: `n` evenly spaced frames of a media item, subclip or sequence.
fn reference_frames(s: &Session, item: ItemId, n: usize, cmd: &str) -> Result<Vec<Image>> {
    let pi = s.project.item(item).ok_or_else(|| bad(cmd, format!("no item {}", item.0)))?;
    let (render_id, range, width) = match &pi.kind {
        ItemKind::Sequence(q) => (item, TimeRange::new(Tick::ZERO, q.duration().clamp(Tick::ZERO, Tick::MAX)), q.settings.width),
        ItemKind::Media(_) | ItemKind::Subclip { .. } => {
            let (root, clip, sub) = s.project.resolve_media(item).ok_or_else(|| bad(cmd, "the reference is not a media item"))?;
            let v = clip.info.video.as_ref().ok_or_else(|| bad(cmd, "the reference has no picture"))?;
            let full = TimeRange::new(Tick::ZERO, clip.info.duration.clamp(Tick::ZERO, Tick::MAX));
            let range = match sub {
                Some(r) => {
                    let start = r.start.clamp(Tick::ZERO, full.end());
                    TimeRange::new(start, r.duration.clamp(Tick::ZERO, full.end() - start))
                }
                None => full,
            };
            (root, range, v.width)
        }
        _ => return Err(bad(cmd, "the reference must be a media item, subclip or sequence")),
    };
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let scale = (SAMPLE_WIDTH / width.max(1) as f32).min(1.0);
    let frames: Vec<Image> =
        spread_times(range, n).into_iter().filter_map(|t| filmcraft_render::render_item(&s.project, render_id, t, scale, &provider)).collect();
    if frames.is_empty() {
        return Err(bad(cmd, "the reference has no picture to match (offline or empty)"));
    }
    Ok(frames)
}

fn match_to_item(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "lumetri.matchToItem";
    let item = item_p(p, "item").ok_or_else(|| bad(cmd, "need `item` (the reference media item)"))?;
    let n = samples_p(p, cmd)?;
    let skin = bool_p(p, "faceDetection").unwrap_or(true);
    let seq_id = s.state.active_sequence.ok_or_else(|| bad(cmd, "no active sequence"))?;
    let q = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let is_video = |c: ClipId| q.find_item(c).is_some_and(|(t, _)| q.track(t).is_some_and(|t| t.kind == TrackKind::Video));
    let clips: Vec<ClipId> = match p.get("clips") {
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for x in a {
                let c = x.as_u64().map(ClipId).ok_or_else(|| bad(cmd, format!("`clips` holds a non-id: {x}")))?;
                if !is_video(c) {
                    return Err(bad(cmd, format!("{} is not a video clip in the active sequence", c.0)));
                }
                if !v.contains(&c) {
                    v.push(c);
                }
            }
            v
        }
        Some(Value::Null) | None => s.state.selection.iter().copied().filter(|c| is_video(*c)).collect(),
        Some(o) => return Err(bad(cmd, format!("`clips` must be an array of clip ids, not {o}"))),
    };
    if clips.is_empty() {
        return Err(bad(cmd, "no video clips (pass `clips` or select some)"));
    }
    let reference = mosaic(&reference_frames(s, item, n, cmd)?).ok_or_else(|| bad(cmd, "the reference frames are blank"))?;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: (SAMPLE_WIDTH / q.settings.width.max(1) as f32).min(1.0), working_output: true, ..Default::default() };
    let lumetri_def = filmcraft_project::find_effect("lumetri").ok_or_else(|| bad(cmd, "Lumetri Color is not available"))?;
    let mut solved = Vec::new();
    for c in &clips {
        let Some((_, it)) = q.find_item(*c) else { continue };
        let idx = it.effects.iter().position(|e| e.effect == "lumetri");
        let base = idx.and_then(|i| it.effects.get(i)).cloned().unwrap_or_else(|| lumetri_def.instance());
        // the clip as Lumetri sees it: its own Lumetri switched off
        let mut probe = (*s.project).clone();
        if let (Some(i), Some((_, pi))) = (idx, probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(*c)))
            && let Some(e) = pi.effects.get_mut(i)
        {
            e.enabled = false;
        }
        let frames: Vec<Image> =
            spread_times(it.range(), n).into_iter().filter_map(|t| filmcraft_render::render_clip(&probe, seq_id, *c, t, opts, &provider)).collect();
        let Some(current) = mosaic(&frames) else { return Err(bad(cmd, format!("clip {} has no picture to match", c.0))) };
        let m = color_match::solve(&current, &reference, &base, skin);
        solved.push((*c, idx, base, m));
    }
    let report: Vec<Value> = solved
        .iter()
        .map(|(c, _, _, m)| {
            json!({"clip": c.0, "shadows": m.shadows, "midtones": m.midtones, "highlights": m.highlights,
                   "lightness": m.lightness, "saturation": m.saturation, "distanceBefore": m.before, "distanceAfter": m.after})
        })
        .collect();
    s.edit_sequence("Match Grade to Item", |q, _, _| {
        for (c, idx, base, m) in &solved {
            let (_, it) = q.find_item_mut(*c).ok_or(filmcraft_edit::EditError::NoItem(*c))?;
            let i = match idx {
                Some(i) => *i,
                None => {
                    // like effects.apply: standard effects go before the intrinsic ones
                    let pos = it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len());
                    it.effects.insert(pos, base.clone());
                    pos
                }
            };
            let e = it.effects.get_mut(i).ok_or_else(|| bad(cmd, "no Lumetri"))?;
            crate::color::write_match(e, m);
        }
        Ok(())
    })?;
    Ok(json!({"item": item.0, "samples": n, "clips": report}))
}

fn bake_lut(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "lumetri.bakeLut";
    let clip = clip_p(p, "clip").ok_or_else(|| bad(cmd, "need `clip`"))?;
    let size = match p.get("size") {
        None | Some(Value::Null) => 33,
        Some(v) => v
            .as_u64()
            .map(|n| n as usize)
            .filter(|n| filmcraft_render::lut_bake::SIZES.contains(n))
            .ok_or_else(|| bad(cmd, format!("`size` must be 17, 33 or 65, not {v}")))?,
    };
    let name = crate::style_analysis::sanitize_name(str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?, cmd)?;
    let q = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let (tid, it) = q.find_item(clip).ok_or_else(|| bad(cmd, format!("no clip {} in the active sequence", clip.0)))?;
    if q.track(tid).is_none_or(|t| t.kind != TrackKind::Video) {
        return Err(bad(cmd, "not a video clip"));
    }
    let effects: Vec<filmcraft_project::EffectInstance> = it.effects.iter().filter(|e| e.effect == "lumetri").cloned().collect();
    if effects.is_empty() {
        return Err(bad(cmd, "the clip has no Lumetri Color to bake"));
    }
    // animated grades are baked at the playhead (inside the clip)
    let last = (it.end() - Tick(1)).max(it.start);
    let t = it.source_time_at(s.playhead().clamp(it.start, last));
    let mut warnings = Vec::new();
    if q.settings.color.working.is_hdr() {
        warnings.push("the sequence works in HDR; the LUT is baked for SDR Rec. 709 and won't match the HDR grade".to_string());
    }
    let mut baked = filmcraft_render::lut_bake::bake_lumetri(Some(&s.project), &effects, t, size).map_err(|e| bad(cmd, e))?;
    warnings.append(&mut baked.warnings);
    baked.lut.title = format!("FilmCraft bake: {name}");
    let text = baked.lut.to_cube();
    let dir = s.style.data_dir().map(|d| d.join("luts")).ok_or_else(|| bad(cmd, "no data directory to keep LUTs in"))?;
    std::fs::create_dir_all(&dir).map_err(|e| bad(cmd, format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{name}.cube"));
    filmcraft_format::atomic_write(&path, text.as_bytes()).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    let path_s = path.to_string_lossy().to_string();
    let (id, _) = crate::color::import_lut(s, &path_s, Some(&name))?;
    Ok(json!({
        "clip": clip.0,
        "path": path_s,
        "size": size,
        "id": id,
        "ref": format!("lib:{id}"),
        "name": name,
        "input": BAKE_INPUT,
        "warnings": warnings,
    }))
}

#[cfg(test)]
#[path = "style_grade_tests.rs"]
mod tests;
