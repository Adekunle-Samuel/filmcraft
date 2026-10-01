//! Graphics commands (`graphics.*`, `fonts.list`): graphic clips with text and shape layers.
//!
//! A graphic clip's layers are the `graphic_text` / `graphic_shape` effect instances on the clip
//! (see `filmcraft_project::graphic`). Commands address a clip by `clip` (default: the first
//! selected graphic clip, else the topmost graphic clip under the playhead) and a layer by
//! `layer` = index among the clip's graphic layers, 0 = back (default: the selected layer, else
//! the frontmost). Property values are keyframe-aware: on an animated property they set the value
//! at the playhead.

use filmcraft_geom::Vec2;
use filmcraft_project::graphic::{self, LayerContent, SHAPE_OPTS, eval_layer, layer_display_name, layer_indices, new_shape_layer, new_text_layer};
use filmcraft_project::{ClipId, ItemId, ItemKind, Label, ParamValue, Sequence, TrackKind};
use filmcraft_render::graphic_clip::{layer_local_bounds, layer_quad};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

/// Whether a track item is a graphic clip.
pub fn is_graphic(s: &Session, seq: &Sequence, c: ClipId) -> bool {
    seq.find_item(c).and_then(|(_, it)| s.project.item(it.item)).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
}

/// The graphic clip a command acts on: `clip`, else the first selected graphic clip, else the
/// topmost graphic clip under the playhead.
pub fn target_clip(s: &Session, p: &Value) -> Option<ClipId> {
    let seq = s.active_sequence()?;
    if let Some(c) = u64_p(p, "clip").map(ClipId) {
        return is_graphic(s, seq, c).then_some(c);
    }
    if let Some(c) = s.state.selection.iter().copied().find(|c| is_graphic(s, seq, *c)) {
        return Some(c);
    }
    let t = s.playhead();
    seq.video_tracks.iter().rev().filter_map(|tr| tr.item_at(t)).map(|it| it.id).find(|c| is_graphic(s, seq, *c))
}

fn has_graphic(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a graphic clip".into())
}

/// Effect index of graphic layer `layer` (or the selected / frontmost layer) of `clip`.
fn layer_effect_index(s: &Session, clip: ClipId, p: &Value) -> Result<(usize, usize)> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics", "no such clip"))?;
    let idx = layer_indices(&it.effects);
    if idx.is_empty() {
        return Err(bad("graphics", "the graphic has no layers"));
    }
    let l = match p.get("layer") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        Some(Value::String(name)) => (0..idx.len())
            .find(|&i| layer_display_name(&it.effects[idx[i]], i).eq_ignore_ascii_case(name))
            .ok_or_else(|| bad("graphics", format!("no layer named `{name}`")))?,
        _ => s.state.graphic_layers.first().copied().filter(|l| *l < idx.len()).unwrap_or(idx.len() - 1),
    };
    let e = *idx.get(l).ok_or_else(|| bad("graphics", format!("no layer {l}")))?;
    Ok((l, e))
}

fn vec2_p(p: &Value, k: &str) -> Option<Vec2> {
    let a = p.get(k)?.as_array()?;
    Some(Vec2::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// Find or create the graphic source item for the active sequence's frame size.
fn graphic_source(p: &mut filmcraft_project::Project, w: u32, h: u32, rate: filmcraft_time::FrameRate) -> ItemId {
    if let Some((id, _)) =
        p.items.iter().find(|(_, i)| matches!(i.kind, ItemKind::Graphic { width, height, rate: r } if width == w && height == h && r == rate))
    {
        return *id;
    }
    p.add_item("Graphic", Label::Rose, ItemKind::Graphic { width: w, height: h, rate }, None)
}

/// Place a new graphic clip holding `layer` at the playhead: on the first video track above the
/// topmost clip at the playhead that is free for the duration (a track is added if needed).
fn new_graphic_clip(s: &mut Session, layer: filmcraft_project::EffectInstance, name: &str, p: &Value) -> Result<ClipId> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let t = time_p(s, p, "").unwrap_or_else(|| s.playhead());
    let seconds = f64_p(p, "seconds").unwrap_or(5.0).max(0.01);
    let want_track = u64_p(p, "track").map(|v| v as usize);
    let name = name.to_string();
    s.edit("New Graphic", |pr, st| {
        let (w, h, rate) = {
            let q = pr.sequence(seq_id).ok_or(EngineError::NoSequence)?;
            (q.settings.width, q.settings.height, q.settings.frame_rate)
        };
        let src = graphic_source(pr, w, h, rate);
        let t = rate.snap(t);
        let dur = rate.snap_nearest(Tick::from_seconds_f64(seconds)).max(rate.frame_duration());
        let mut ti = pr.make_track_item(src, TrackKind::Video, t, TimeRange::new(Tick::ZERO, dur), rate).ok_or_else(|| bad("graphics.newText", "bad item"))?;
        ti.name = name;
        ti.effects.push(layer);
        for e in &mut ti.effects {
            filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
        }
        let id = ti.id;
        let track_id = filmcraft_project::TrackId(pr.alloc_id());
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let range = TimeRange::new(t, dur);
        let free = |tr: &filmcraft_project::Track| !tr.items.iter().any(|i| i.range().overlaps(&range));
        let idx = match want_track {
            Some(i) if i < q.video_tracks.len() && free(&q.video_tracks[i]) => i,
            Some(i) if i < q.video_tracks.len() => return Err(bad("graphics.newText", format!("V{} is not free here", i + 1))),
            _ => {
                let top = q.video_tracks.iter().rposition(|tr| tr.item_at(t).is_some()).map_or(0, |k| k + 1);
                match (top..q.video_tracks.len()).find(|&i| free(&q.video_tracks[i]) && !q.video_tracks[i].locked) {
                    Some(i) => i,
                    None => {
                        let n = q.video_tracks.len() + 1;
                        q.video_tracks.push(filmcraft_project::Track::new(track_id, TrackKind::Video, format!("Video {n}")));
                        q.video_tracks.len() - 1
                    }
                }
            }
        };
        q.video_tracks[idx].items.push(ti);
        q.video_tracks[idx].sort();
        q.check().map_err(EngineError::Other)?;
        st.selection = vec![id];
        st.graphic_layers = vec![0];
        Ok(id)
    })
}

/// Add a layer to an existing graphic clip; returns the new layer index.
fn add_layer(s: &mut Session, clip: ClipId, mut layer: filmcraft_project::EffectInstance) -> Result<usize> {
    let frame = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
    filmcraft_project::resolve_auto_points(&mut layer, frame, frame);
    s.edit_sequence("Add Graphic Layer", |q, _, st| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        // new layers go in front: after the last graphic layer
        let pos = layer_indices(&it.effects).last().map_or(it.effects.len(), |l| l + 1);
        it.effects.insert(pos, layer);
        let n = layer_indices(&it.effects).len() - 1;
        st.selection = vec![clip];
        st.graphic_layers = vec![n];
        Ok(n)
    })
}

/// Map friendly property names to parameter ids.
fn param_id(k: &str) -> &str {
    match k {
        "fontSize" => "size",
        "fontStyle" | "style" => "font_style",
        "fillColor" | "color" => "fill_color",
        "strokeColor" => "stroke_color",
        "strokeWidth" => "stroke_width",
        "backgroundColor" => "background_color",
        "shadowColor" => "shadow_color",
        "baselineShift" => "baseline_shift",
        "fauxBold" => "faux_bold",
        "fauxItalic" => "faux_italic",
        "boxWidth" => "box_width",
        "anchorPoint" => "anchor",
        "cornerRadius" => "corner_radius",
        other => other,
    }
}

/// JSON value for a property, accepting names for choices ("center", "small caps", "ellipse"…).
fn to_param(template: &ParamValue, id: &str, v: &Value) -> Option<ParamValue> {
    if let (ParamValue::Choice(_), Value::String(name)) = (template, v) {
        let opts: &[&str] = match id {
            "align" => graphic::ALIGN_OPTS,
            "caps" => graphic::CAPS_OPTS,
            "stroke_type" | "stroke2_type" => graphic::STROKE_OPTS,
            "shape" => SHAPE_OPTS,
            _ => &[],
        };
        let n = name.to_ascii_lowercase().replace('_', " ");
        let n = if n == "centre" { "center".to_string() } else { n };
        return opts.iter().position(|o| o.to_ascii_lowercase() == n).map(|i| ParamValue::Choice(i as u32));
    }
    crate::commands::json_to_param(template, v)
}

/// Set properties `props` on a layer (keyframe-aware at time `tl`).
fn set_props(s: &mut Session, clip: ClipId, eidx: usize, props: &serde_json::Map<String, Value>, tl: Tick, label: &str) -> Result<()> {
    let props = props.clone();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        let e = it.effects.get_mut(eidx).ok_or_else(|| bad("graphics.set", "no such layer"))?;
        for (k, v) in &props {
            if k == "enabled" {
                e.enabled = v.as_bool().unwrap_or(true);
                continue;
            }
            let id = param_id(k);
            let prm = e.params.get_mut(id).ok_or_else(|| bad("graphics.set", format!("no property `{k}`")))?;
            let pv = to_param(&prm.value, id, v).ok_or_else(|| bad("graphics.set", format!("`{k}`: value has the wrong type")))?;
            prm.set_at(mt, pv);
        }
        Ok(())
    })
}

/// Layers of a graphic clip with their evaluated bounds (sequence/canvas pixels).
fn list_layers(s: &Session, clip: ClipId) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics.list", "no such clip"))?;
    let size = match s.project.item(it.item).map(|p| &p.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (seq.settings.width, seq.settings.height),
    };
    let ph = s.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let layers: Vec<Value> = layer_indices(&it.effects)
        .iter()
        .enumerate()
        .filter_map(|(i, &ei)| {
            let e = &it.effects[ei];
            let sp = eval_layer(e, mt, size)?;
            let q = layer_quad(&sp);
            let (kind, text) = match &sp.content {
                LayerContent::Text(t) => ("text", Some(t.text.clone())),
                LayerContent::Shape(sh) => (SHAPE_OPTS.get(sh.shape as usize).copied().unwrap_or("Shape"), None),
            };
            Some(json!({
                "layer": i,
                "effectIndex": ei,
                "name": layer_display_name(e, i),
                "kind": kind,
                "text": text,
                "enabled": e.enabled,
                "position": [sp.transform.position.x, sp.transform.position.y],
                "localBounds": layer_local_bounds(&sp),
                "quad": q.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
            }))
        })
        .collect();
    Ok(json!({"clip": clip.0, "name": it.name, "start": it.start.0, "duration": it.duration.0, "canvas": [size.0, size.1], "layers": layers}))
}

fn quad_bounds(q: &[Vec2; 4]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in q {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

/// Layer bounds of `layers` (layer indices) of `clip` at the playhead: (effect index, position, bounds).
fn layer_boxes(s: &Session, clip: ClipId, layers: &[usize]) -> Result<(Vec<(usize, Vec2, [f64; 4])>, (u32, u32))> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics.align", "no such clip"))?;
    let size = match s.project.item(it.item).map(|p| &p.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (seq.settings.width, seq.settings.height),
    };
    let mt = it.source_time_at(s.playhead().clamp(it.start, it.end() - Tick(1)));
    let idx = layer_indices(&it.effects);
    let mut out = Vec::new();
    for &l in layers {
        let ei = *idx.get(l).ok_or_else(|| bad("graphics.align", format!("no layer {l}")))?;
        let sp = eval_layer(&it.effects[ei], mt, size).ok_or_else(|| bad("graphics.align", "bad layer"))?;
        out.push((ei, sp.transform.position, quad_bounds(&layer_quad(&sp))));
    }
    Ok((out, size))
}

fn layers_p(s: &Session, clip: ClipId, p: &Value) -> Result<Vec<usize>> {
    if let Some(a) = p.get("layers").and_then(Value::as_array) {
        return Ok(a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect());
    }
    if !s.state.graphic_layers.is_empty() {
        return Ok(s.state.graphic_layers.clone());
    }
    let (l, _) = layer_effect_index(s, clip, p)?;
    Ok(vec![l])
}

fn move_layers(s: &mut Session, clip: ClipId, moves: Vec<(usize, Vec2)>, label: &str) -> Result<()> {
    let tl = s.playhead();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        for (ei, pos) in &moves {
            if let Some(prm) = it.effects.get_mut(*ei).and_then(|e| e.params.get_mut("position")) {
                prm.set_at(mt, ParamValue::Vec2(*pos));
            }
        }
        Ok(())
    })
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "graphics.newText",
            "Text",
            &["Graphics and Titles", "New Layer"],
            Some("Cmd+T"),
            r#"{"text":str="New Text","position":[x,y]?,"clip":id?,"newClip":bool?,"size":px=100,"font":str?,"fontStyle":str?,"seconds":f64=5,"track":index?,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let text = str_p(p, "text").unwrap_or("New Text").to_string();
                let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
                let pos = vec2_p(p, "position").unwrap_or(Vec2::new(w as f64 / 2.0, h as f64 / 2.0));
                let mut layer = new_text_layer(&text, pos, f64_p(p, "size").unwrap_or(100.0));
                for (k, id) in [("font", "font"), ("fontStyle", "font_style")] {
                    if let Some(v) = str_p(p, k) {
                        layer.params.insert(id.into(), filmcraft_project::Param::new(ParamValue::Text(v.into())));
                    }
                }
                let into = if p.get("newClip").and_then(Value::as_bool) == Some(true) { None } else { u64_p(p, "clip").map(ClipId) };
                let into = into.filter(|c| s.active_sequence().is_some_and(|q| is_graphic(s, q, *c)));
                let (clip, layer_i) = match into {
                    Some(c) => (c, add_layer(s, c, layer)?),
                    None => {
                        let name = text.lines().next().filter(|l| !l.trim().is_empty()).unwrap_or("Graphic").to_string();
                        (new_graphic_clip(s, layer, &name, p)?, 0)
                    }
                };
                Ok(json!({"clip": clip.0, "layer": layer_i}))
            },
        ),
        spec(
            "graphics.newShape",
            "Shape",
            &["Graphics and Titles", "New Layer"],
            None,
            r#"{"shape":"rectangle|ellipse|polygon|path","position":[x,y]?,"size":[w,h]=[400,200],"points":[[x,y],…]?,"clip":id?,"seconds":f64=5}"#,
            has_seq,
            |s, p| {
                let shape = str_p(p, "shape").unwrap_or("rectangle").to_ascii_lowercase();
                let k = SHAPE_OPTS
                    .iter()
                    .position(|o| o.to_ascii_lowercase() == shape)
                    .ok_or_else(|| bad("graphics.newShape", format!("unknown shape `{shape}`")))? as u32;
                let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
                let pos = vec2_p(p, "position").unwrap_or(Vec2::new(w as f64 / 2.0, h as f64 / 2.0));
                let size = vec2_p(p, "size").unwrap_or(Vec2::new(400.0, 200.0));
                let points: Vec<[f32; 2]> = p
                    .get("points")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|q| Some([q.get(0)?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])).collect())
                    .unwrap_or_default();
                if k == 3 && points.len() < 3 {
                    return Err(bad("graphics.newShape", "a path needs at least 3 points"));
                }
                let layer = new_shape_layer(k, pos, size, points);
                let into = u64_p(p, "clip").map(ClipId).filter(|c| s.active_sequence().is_some_and(|q| is_graphic(s, q, *c)));
                let (clip, layer_i) = match into {
                    Some(c) => (c, add_layer(s, c, layer)?),
                    None => (new_graphic_clip(s, layer, "Shape", p)?, 0),
                };
                Ok(json!({"clip": clip.0, "layer": layer_i}))
            },
        ),
        spec(
            "graphics.setText",
            "Edit Text",
            &[],
            None,
            r#"{"clip":id?,"layer":n|name?,"text":str,"merge":bool? (coalesce with the previous Edit Text undo step)}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.setText", "no graphic clip"))?;
                let (_, ei) = layer_effect_index(s, clip, p)?;
                let text = str_p(p, "text").ok_or_else(|| bad("graphics.setText", "need `text`"))?.to_string();
                let merge = p.get("merge").and_then(Value::as_bool).unwrap_or(false) && s.history.undo.last().is_some_and(|h| h.0 == "Edit Text");
                let mut props = serde_json::Map::new();
                props.insert("text".into(), Value::String(text));
                let ph = s.playhead();
                set_props(s, clip, ei, &props, ph, "Edit Text")?;
                if merge && s.history.undo.len() >= 2 {
                    // keep the snapshot from before the typing session
                    s.history.undo.pop();
                }
                Ok(Value::Null)
            },
        ),
        spec(
            "graphics.set",
            "Set Graphic Properties",
            &[],
            None,
            r##"{"clip":id?,"layer":n|name?,"props":{"font":"Inter","font_style":"Bold","size":120,"align":"center","tracking":50,"leading":0,"fill_color":"#ffcc00","stroke":true,"stroke_width":6,"background":true,"shadow":true,"position":[x,y],"scale":100,"rotation":0,"opacity":100,…},"time":ticks?}"##,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.set", "no graphic clip"))?;
                let (_, ei) = layer_effect_index(s, clip, p)?;
                let props = p.get("props").and_then(Value::as_object).ok_or_else(|| bad("graphics.set", "need `props`"))?.clone();
                let tl = time_p(s, p, "").unwrap_or_else(|| s.playhead());
                set_props(s, clip, ei, &props, tl, "Change Graphic Property")?;
                Ok(Value::Null)
            },
        ),
        spec("graphics.selectLayer", "Select Graphic Layer", &[], None, r#"{"clip":id?,"layers":[n]}"#, has_graphic, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.selectLayer", "no graphic clip"))?;
            let layers: Vec<usize> =
                p.get("layers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()).unwrap_or_default();
            s.state.selection = vec![clip];
            s.state.graphic_layers = layers;
            Ok(Value::Null)
        }),
        spec("graphics.deleteLayer", "Delete Graphic Layer", &[], None, r#"{"clip":id?,"layer":n?}"#, has_graphic, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.deleteLayer", "no graphic clip"))?;
            let (_, ei) = layer_effect_index(s, clip, p)?;
            s.edit_sequence("Delete Graphic Layer", |q, _, st| {
                let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                it.effects.remove(ei);
                st.graphic_layers.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        spec(
            "graphics.arrangeLayer",
            "Arrange Graphic Layer",
            &[],
            None,
            r#"{"clip":id?,"layer":n?,"to":"front|back|forward|backward"|index}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.arrangeLayer", "no graphic clip"))?;
                let (l, _) = layer_effect_index(s, clip, p)?;
                let to = p.get("to").cloned().unwrap_or(json!("front"));
                let n = s.state.graphic_layers.len();
                let _ = n;
                s.edit_sequence("Arrange Graphic Layer", |q, _, st| {
                    let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                    let idx = layer_indices(&it.effects);
                    let last = idx.len() - 1;
                    let dest = match &to {
                        Value::Number(v) => (v.as_u64().unwrap_or(0) as usize).min(last),
                        Value::String(w) => match w.as_str() {
                            "front" => last,
                            "back" => 0,
                            "forward" => (l + 1).min(last),
                            "backward" => l.saturating_sub(1),
                            o => return Err(bad("graphics.arrangeLayer", format!("unknown `to` `{o}`"))),
                        },
                        _ => last,
                    };
                    let e = it.effects.remove(idx[l]);
                    let mut idx2 = layer_indices(&it.effects);
                    let insert_at = if dest >= idx2.len() { idx2.pop().map_or(idx[0], |x| x + 1) } else { idx2[dest] };
                    it.effects.insert(insert_at, e);
                    st.graphic_layers = vec![dest];
                    Ok(())
                })?;
                Ok(Value::Null)
            },
        ),
        spec(
            "graphics.align",
            "Align Layers",
            &[],
            None,
            r#"{"clip":id?,"layers":[n]?,"align":"left|hcenter|right|top|vcenter|bottom","to":"frame|selection"="frame" (one layer always aligns to the frame)}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.align", "no graphic clip"))?;
                let layers = layers_p(s, clip, p)?;
                let (boxes, size) = layer_boxes(s, clip, &layers)?;
                let how = str_p(p, "align").ok_or_else(|| bad("graphics.align", "need `align`"))?;
                let to_sel = str_p(p, "to") == Some("selection") && boxes.len() > 1;
                let r = if to_sel {
                    boxes.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, b| [a[0].min(b.2[0]), a[1].min(b.2[1]), a[2].max(b.2[2]), a[3].max(b.2[3])])
                } else {
                    [0.0, 0.0, size.0 as f64, size.1 as f64]
                };
                let mut moves = Vec::new();
                for (ei, pos, b) in boxes {
                    let (dx, dy) = match how {
                        "left" => (r[0] - b[0], 0.0),
                        "right" => (r[2] - b[2], 0.0),
                        "hcenter" | "center" => ((r[0] + r[2]) / 2.0 - (b[0] + b[2]) / 2.0, 0.0),
                        "top" => (0.0, r[1] - b[1]),
                        "bottom" => (0.0, r[3] - b[3]),
                        "vcenter" | "middle" => (0.0, (r[1] + r[3]) / 2.0 - (b[1] + b[3]) / 2.0),
                        o => return Err(bad("graphics.align", format!("unknown alignment `{o}`"))),
                    };
                    moves.push((ei, Vec2::new(pos.x + dx, pos.y + dy)));
                }
                move_layers(s, clip, moves, "Align Layers")?;
                Ok(Value::Null)
            },
        ),
        spec(
            "graphics.distribute",
            "Distribute Layers",
            &[],
            None,
            r#"{"clip":id?,"layers":[n] (3 or more)?,"axis":"horizontal|vertical"}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.distribute", "no graphic clip"))?;
                let layers = layers_p(s, clip, p)?;
                let (mut boxes, _) = layer_boxes(s, clip, &layers)?;
                if boxes.len() < 3 {
                    return Err(bad("graphics.distribute", "select three or more layers"));
                }
                let vertical = str_p(p, "axis") == Some("vertical");
                let c = |b: &[f64; 4]| if vertical { (b[1] + b[3]) / 2.0 } else { (b[0] + b[2]) / 2.0 };
                boxes.sort_by(|a, b| c(&a.2).total_cmp(&c(&b.2)));
                let (first, last) = (c(&boxes[0].2), c(&boxes[boxes.len() - 1].2));
                let n = boxes.len() - 1;
                let moves = boxes
                    .iter()
                    .enumerate()
                    .map(|(i, (ei, pos, b))| {
                        let d = first + (last - first) * i as f64 / n as f64 - c(b);
                        (*ei, if vertical { Vec2::new(pos.x, pos.y + d) } else { Vec2::new(pos.x + d, pos.y) })
                    })
                    .collect();
                move_layers(s, clip, moves, "Distribute Layers")?;
                Ok(Value::Null)
            },
        ),
        query("graphics.list", "List Graphic Layers", r#"{"clip":id?}"#, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.list", "no graphic clip"))?;
            list_layers(s, clip)
        }),
        query("fonts.list", "List Fonts", r#"{"system":bool=true (scan the system font folders)}"#, |_, p| {
            if p.get("system").and_then(Value::as_bool).unwrap_or(true) {
                filmcraft_text::fonts::scan_system();
            }
            Ok(json!(filmcraft_text::families().into_iter().map(|(f, st)| json!({"family": f, "styles": st})).collect::<Vec<_>>()))
        }),
    ]
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;
