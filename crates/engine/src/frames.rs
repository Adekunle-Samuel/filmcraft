//! Frames for agents (`media.renderFrame`, `media.contactSheet`): PNG stills of a project item
//! (media time) or a sequence (timeline time), downscaled for vision models.
//!
//! Both are queries: they never move a playhead, change the selection or touch the project, so an
//! agent can look at any point of any item while the user keeps editing. Every number is capped:
//! at most [`MAX_FRAMES`] frames, sides of at most [`MAX_SIDE`] pixels (so a sheet is at most
//! `MAX_SIDE`² pixels), and times are clamped to the item's last frame.

use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Session};

/// Longest side of a frame or a contact sheet.
pub const MAX_SIDE: u32 = 2576;
/// Smallest `maxSide` honoured (smaller requests are raised to it).
pub const MIN_SIDE: u32 = 16;
/// `maxSide` when the caller gives none (what vision models read without further downscaling).
pub const DEFAULT_SIDE: u32 = 1568;
/// Most frames on one contact sheet.
pub const MAX_FRAMES: usize = 48;
/// Frames on a contact sheet when the caller gives neither `count` nor `times`.
pub const DEFAULT_FRAMES: usize = 12;
/// Pixels between (and around) the tiles of a contact sheet.
const GAP: u32 = 4;
/// Colour of the gaps.
const GAP_RGBA: [u8; 4] = [24, 24, 24, 255];

/// A rendered still: straight sRGB RGBA8 (opaque) or its PNG.
#[derive(Clone, Debug, PartialEq)]
pub struct Still {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What is rendered.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// A sequence at timeline time (captions burnt in, as the Program monitor shows it).
    Sequence(ItemId),
    /// A media item at media time `offset + t` (subclips: the parent from the subclip's In point).
    Media { item: ItemId, offset: Tick },
}

/// The target of a call with its picture size, length and frame rate.
#[derive(Clone, Debug)]
struct Resolved {
    target: Target,
    /// Requested item (what the caller named) for messages and results.
    id: ItemId,
    name: String,
    size: (u32, u32),
    duration: Tick,
    rate: FrameRate,
}

/// `item` / `sequence` → what to render. Neither: the active sequence.
fn resolve(s: &Session, p: &Value, cmd: &str) -> Result<Resolved> {
    let seq_flag = match p.get("sequence") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(cmd, "`sequence` must be true or false")),
    };
    let item = match p.get("item") {
        None | Some(Value::Null) => None,
        Some(v) => Some(ItemId(v.as_u64().ok_or_else(|| bad(cmd, "`item` must be a project item id"))?)),
    };
    let id = match (item, seq_flag) {
        (Some(_), true) => return Err(bad(cmd, "give `item` or `sequence`, not both")),
        (Some(i), false) => i,
        (None, _) => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
    };
    let it = s.project.item(id).ok_or_else(|| bad(cmd, format!("no project item {}", id.0)))?;
    let name = it.name.clone();
    match &it.kind {
        ItemKind::Sequence(q) => Ok(Resolved {
            target: Target::Sequence(id),
            id,
            name,
            size: (q.settings.width, q.settings.height),
            duration: q.duration(),
            rate: q.settings.frame_rate,
        }),
        ItemKind::Media(_) | ItemKind::Subclip { .. } => {
            let (root, media, range) = s.project.resolve_media(id).ok_or_else(|| bad(cmd, format!("“{name}” has no media")))?;
            let size = media.info.video.as_ref().map(|v| (v.width, v.height)).ok_or_else(|| bad(cmd, format!("“{name}” has no picture")))?;
            let (offset, duration) = match range {
                Some(r) => (r.start, r.duration),
                None => (Tick::ZERO, media.duration()),
            };
            Ok(Resolved { target: Target::Media { item: root, offset }, id, name, size, duration, rate: media.frame_rate() })
        }
        _ => Err(bad(cmd, format!("“{name}” ({}) cannot be rendered as a frame; name a media item or a sequence", it.type_label()))),
    }
}

/// A time in seconds: a finite, non-negative number.
fn seconds_of(v: &Value, cmd: &str, key: &str) -> Result<f64> {
    v.as_f64().filter(|f| f.is_finite() && *f >= 0.0).ok_or_else(|| bad(cmd, format!("`{key}` must be a non-negative number of seconds")))
}

/// `maxSide`: default [`DEFAULT_SIDE`], brought into [`MIN_SIDE`]..=[`MAX_SIDE`].
fn side_p(p: &Value, cmd: &str) -> Result<u32> {
    match p.get("maxSide") {
        None | Some(Value::Null) => Ok(DEFAULT_SIDE),
        Some(v) => {
            let f = v.as_f64().filter(|f| f.is_finite() && *f >= 1.0).ok_or_else(|| bad(cmd, "`maxSide` must be a positive number of pixels"))?;
            Ok((f.min(f64::from(MAX_SIDE)) as u32).clamp(MIN_SIDE, MAX_SIDE))
        }
    }
}

/// An optional count: a whole number in `lo..=hi`.
fn count_p(p: &Value, key: &str, lo: usize, hi: usize, cmd: &str) -> Result<Option<usize>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && *f >= lo as f64 && *f <= hi as f64);
            n.map(|f| Some(f as usize)).ok_or_else(|| bad(cmd, format!("`{key}` must be a whole number from {lo} to {hi}")))
        }
    }
}

/// `t` seconds as a frame-aligned time of the target, clamped to its last frame.
fn clamp_time(r: &Resolved, seconds: f64) -> Tick {
    let last = Tick(r.duration.0.saturating_sub(r.rate.frame_duration().0)).max(Tick::ZERO);
    // clamp in seconds first: huge values must not overflow the tick conversion
    let t = Tick::from_seconds_f64(seconds.min(last.seconds()));
    r.rate.snap(t.clamp(Tick::ZERO, last))
}

/// The largest size with `w`:`h`'s aspect whose longest side is at most `max_side` (never larger
/// than `w`×`h`, at least 1×1).
fn fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    let long = w.max(h);
    if long <= max_side {
        return (w, h);
    }
    let k = f64::from(max_side) / f64::from(long);
    (((f64::from(w) * k).round() as u32).clamp(1, max_side), ((f64::from(h) * k).round() as u32).clamp(1, max_side))
}

/// Render `r` at `t` as opaque RGBA8 of exactly `dw`×`dh`.
fn render_rgba(s: &Session, r: &Resolved, t: Tick, dw: u32, dh: u32, cmd: &str) -> Result<Vec<u8>> {
    let (w, h) = r.size;
    let long = w.max(h).max(1);
    let scale = (f64::from(dw.max(dh)) / f64::from(long)).min(1.0) as f32;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let img = match r.target {
        Target::Sequence(id) => {
            let seq = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
            let (ow, oh) = filmcraft_render::output_size(seq, scale);
            let size = u32::try_from(ow).ok().zip(u32::try_from(oh).ok()).ok_or_else(|| bad(cmd, "the frame exceeds the image size limits"))?;
            filmcraft_project::validate_frame_size(size.0, size.1).map_err(|e| bad(cmd, format!("cannot render “{}”: {e}", r.name)))?;
            let opts = filmcraft_render::RenderOptions { scale, captions: true, ..Default::default() };
            filmcraft_render::render_sequence(&s.project, id, t, opts, &provider)
        }
        Target::Media { item, offset } => filmcraft_render::render_item(&s.project, item, Tick(offset.0.saturating_add(t.0)), scale, &provider)
            .ok_or_else(|| EngineError::Other(format!("could not read the frame at {:.3} s of “{}” (offline or undecodable media)", t.seconds(), r.name)))?,
    };
    let rgba = img.over_black_rgba8();
    let (iw, ih) = (u32::try_from(img.w).unwrap_or(0), u32::try_from(img.h).unwrap_or(0));
    resize(&rgba, iw, ih, dw, dh).ok_or_else(|| EngineError::Other(format!("the renderer returned a damaged frame for “{}”", r.name)))
}

/// Box-filter `src` (RGBA8, `sw`×`sh`) to `dw`×`dh`. `None` when `src` is not that size.
fn resize(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Option<Vec<u8>> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() != sw.checked_mul(sh)?.checked_mul(4)? {
        return None;
    }
    if (sw, sh) == (dw, dh) {
        return Some(src.to_vec());
    }
    let mut out = vec![0u8; dw * dh * 4];
    for (y, row) in out.chunks_exact_mut(dw * 4).enumerate() {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh).div_ceil(dh).clamp(y0 + 1, sh);
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw).div_ceil(dw).clamp(x0 + 1, sw);
            let (mut acc, mut n) = ([0u64; 4], 0u64);
            for yy in y0..y1 {
                let line = src.get((yy * sw + x0) * 4..(yy * sw + x1) * 4)?;
                for p in line.as_chunks::<4>().0 {
                    for (a, v) in acc.iter_mut().zip(p) {
                        *a += u64::from(*v);
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            for (o, a) in px.iter_mut().zip(acc) {
                *o = ((a + n / 2) / n).min(255) as u8;
            }
        }
    }
    Some(out)
}

fn encode(rgba: Vec<u8>, w: u32, h: u32) -> Result<Vec<u8>> {
    filmcraft_export::encode_png(rgba, w, h).map_err(|e| EngineError::Other(format!("encoding the PNG: {e}")))
}

/// `media.renderFrame` without the base64: the PNG and the call's result fields
/// (`item`, `name`, `seconds`, `width`, `height`).
pub fn render_frame(s: &Session, p: &Value) -> Result<(Still, Value)> {
    let cmd = "media.renderFrame";
    let seconds = seconds_of(p.get("seconds").ok_or_else(|| bad(cmd, "need `seconds`"))?, cmd, "seconds")?;
    let max_side = side_p(p, cmd)?;
    let r = resolve(s, p, cmd)?;
    let t = clamp_time(&r, seconds);
    let (w, h) = fit(r.size.0, r.size.1, max_side);
    let png = encode(render_rgba(s, &r, t, w, h, cmd)?, w, h)?;
    let meta = json!({"item": r.id.0, "name": r.name, "seconds": t.seconds(), "width": w, "height": h});
    Ok((Still { png, width: w, height: h }, meta))
}

/// Columns for `n` tiles of aspect `a` (width / height) so the sheet comes out roughly square.
fn default_cols(n: usize, a: f64) -> usize {
    let c = (n as f64 / a.max(0.01)).sqrt().ceil();
    if c.is_finite() { (c as usize).clamp(1, n.max(1)) } else { 1 }
}

/// `media.contactSheet` without the base64: the PNG and the result fields (`item`, `name`,
/// `times`, `cols`, `rows`, `tile`, `width`, `height`).
pub fn contact_sheet(s: &Session, p: &Value) -> Result<(Still, Value)> {
    let cmd = "media.contactSheet";
    let max_side = side_p(p, cmd)?;
    let r = resolve(s, p, cmd)?;
    let times: Vec<Tick> = match p.get("times") {
        None | Some(Value::Null) => {
            let n = count_p(p, "count", 1, MAX_FRAMES, cmd)?.unwrap_or(DEFAULT_FRAMES);
            if r.duration <= Tick::ZERO {
                return Err(bad(cmd, format!("“{}” is empty: there is nothing to sample", r.name)));
            }
            let d = r.duration.seconds();
            (0..n).map(|i| clamp_time(&r, d * (i as f64 + 0.5) / n as f64)).collect()
        }
        Some(Value::Array(a)) => {
            if a.is_empty() || a.len() > MAX_FRAMES {
                return Err(bad(cmd, format!("`times` must list 1 to {MAX_FRAMES} times")));
            }
            a.iter().map(|v| seconds_of(v, cmd, "times").map(|t| clamp_time(&r, t))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad(cmd, "`times` must be a list of seconds")),
    };
    let n = times.len();
    let (sw, sh) = (r.size.0.max(1), r.size.1.max(1));
    let aspect = f64::from(sw) / f64::from(sh);
    let cols = count_p(p, "cols", 1, MAX_FRAMES, cmd)?.map_or_else(|| default_cols(n, aspect), |c| c.min(n));
    let rows = n.div_ceil(cols);
    let (cols_u, rows_u) = (cols as u32, rows as u32);
    // the tile size that fits both directions
    let avail_w = max_side.saturating_sub(GAP * (cols_u + 1)) / cols_u;
    let avail_h = max_side.saturating_sub(GAP * (rows_u + 1)) / rows_u;
    let tw = (f64::from(avail_w).min(f64::from(avail_h) * aspect).floor() as u32).min(sw);
    let th = ((f64::from(tw) / aspect).floor() as u32).min(sh);
    if tw < 2 || th < 2 {
        return Err(bad(cmd, format!("{n} frames do not fit in {max_side} pixels: raise `maxSide` or ask for fewer frames")));
    }
    let width = cols_u * tw + GAP * (cols_u + 1);
    let height = rows_u * th + GAP * (rows_u + 1);
    let (wu, hu) = (width as usize, height as usize);
    let mut sheet: Vec<u8> = GAP_RGBA.iter().copied().cycle().take(wu * hu * 4).collect();
    for (k, t) in times.iter().enumerate() {
        let tile = render_rgba(s, &r, *t, tw, th, cmd)?;
        let (cx, cy) = ((k % cols) as u32, (k / cols) as u32);
        let (x0, y0) = ((GAP + cx * (tw + GAP)) as usize, (GAP + cy * (th + GAP)) as usize);
        let row_bytes = tw as usize * 4;
        for (ty, src_row) in tile.chunks_exact(row_bytes).enumerate() {
            let at = ((y0 + ty) * wu + x0) * 4;
            if let Some(dst) = sheet.get_mut(at..at + row_bytes) {
                dst.copy_from_slice(src_row);
            }
        }
    }
    let png = encode(sheet, width, height)?;
    let meta = json!({
        "item": r.id.0,
        "name": r.name,
        "times": times.iter().map(|t| t.seconds()).collect::<Vec<_>>(),
        "cols": cols,
        "rows": rows,
        "tile": {"width": tw, "height": th},
        "width": width,
        "height": height,
    });
    Ok((Still { png, width, height }, meta))
}

/// Standard base64 (RFC 4648, with padding).
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |i: u32| char::from(A.get((i & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let b = [c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0), c.get(2).copied().unwrap_or(0)];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(sym(v >> 18));
        out.push(sym(v >> 12));
        out.push(if c.len() > 1 { sym(v >> 6) } else { '=' });
        out.push(if c.len() > 2 { sym(v) } else { '=' });
    }
    out
}

fn with_png(still: Still, mut meta: Value) -> Value {
    meta["png"] = json!(base64(&still.png));
    meta
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "media.renderFrame",
            label: "Render Frame",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id?,"sequence":bool?,"seconds":f,"maxSide":n?}"#,
            enabled: always,
            run: |s, p| render_frame(s, p).map(|(still, meta)| with_png(still, meta)),
            journal: false,
        },
        CommandSpec {
            id: "media.contactSheet",
            label: "Contact Sheet",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id?,"sequence":bool?,"count":1..48?,"times":[f]?,"cols":n?,"maxSide":n?}"#,
            enabled: always,
            run: |s, p| contact_sheet(s, p).map(|(still, meta)| with_png(still, meta)),
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
