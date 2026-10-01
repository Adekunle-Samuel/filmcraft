//! Colour commands: the project LUT library (`lut.*`), Lumetri LUT slots and section switches
//! (`lumetri.*`).
//!
//! Lumetri commands address a clip by `clip` (default: the first selected video clip with
//! Lumetri Color, else the first selected video clip — Lumetri is added if missing).

use filmcraft_color::{Lut, LutFormat};
use filmcraft_project::{ClipId, ParamValue, ProjectLut, TrackKind};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, clip_p, has_seq, str_p};
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("lut.import", "Import LUT…", &[], r#"{"path":str,"name":str?}"#, always, import),
        query("lut.list", "List LUTs", "{}", list),
        spec("lut.remove", "Remove LUT", &[], r#"{"id":str}"#, always, remove),
        query("lut.export", "Export LUT", r#"{"lut":"lib:<id>"|"builtin:<id>","path":str,"format":"cube"|"3dl"?}"#, export),
        spec("lumetri.setInputLut", "Set Input LUT", &[], r#"{"clip":id?,"lut":"lib:<id>"|"builtin:<id>"|""?,"path":str?}"#, has_seq, |s, p| {
            set_lut(s, p, "input_lut")
        }),
        spec("lumetri.setLook", "Set Creative Look", &[], r#"{"clip":id?,"lut":"lib:<id>"|"builtin:<id>"|""?,"path":str?}"#, has_seq, |s, p| {
            set_lut(s, p, "look_lut")
        }),
        spec(
            "lumetri.setSection",
            "Toggle Lumetri Section",
            &[],
            r#"{"clip":id?,"section":"basic"|"creative"|"curves"|"wheels"|"hsl"|"vignette","on":bool?}"#,
            has_seq,
            set_section,
        ),
    ]
}

fn import_lut(s: &mut Session, path: &str, name: Option<&str>) -> Result<(String, Value)> {
    let text = std::fs::read_to_string(path).map_err(|e| bad("lut.import", format!("{path}: {e}")))?;
    let fmt = LutFormat::from_path(path).ok_or_else(|| bad("lut.import", "expected a .cube or .3dl file"))?;
    let lut = Lut::parse(&text, Some(fmt)).map_err(|e| bad("lut.import", format!("{path}: {e}")))?;
    let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "LUT".into());
    let name = name.map(str::to_string).unwrap_or(stem);
    // re-importing identical content reuses the entry
    if let Some(l) = s.project.luts.iter().find(|l| *l.text == *text) {
        return Ok((l.id.clone(), json!({"id": l.id, "ref": format!("lib:{}", l.id), "name": l.name, "reused": true})));
    }
    let info = json!({
        "size3d": lut.cube.as_ref().map(|c| c.size),
        "size1d": lut.shaper.as_ref().map(|c| c.size()),
        "title": lut.title,
    });
    let mut id = String::new();
    s.edit("Import LUT", |pr, _| {
        id = format!("lut{}", pr.alloc_id());
        pr.luts.push(ProjectLut {
            id: id.clone(),
            name: name.clone(),
            source_path: Some(path.to_string()),
            format: fmt.extension().into(),
            text: text.as_str().into(),
        });
        Ok(())
    })?;
    Ok((id.clone(), json!({"id": id, "ref": format!("lib:{id}"), "name": name, "lut": info})))
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("lut.import", "need `path`"))?.to_string();
    Ok(import_lut(s, &path, str_p(p, "name"))?.1)
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let lib: Vec<Value> = s
        .project
        .luts
        .iter()
        .map(|l| json!({"id": l.id, "ref": format!("lib:{}", l.id), "name": l.name, "format": l.format, "source": l.source_path}))
        .collect();
    let builtin: Vec<Value> = filmcraft_render::luts::builtins()
        .iter()
        .map(|b| json!({"ref": format!("builtin:{}", b.id), "name": b.label, "input": filmcraft_render::luts::input_builtins().any(|i| i.id == b.id)}))
        .collect();
    Ok(json!({"library": lib, "builtin": builtin}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "id").ok_or_else(|| bad("lut.remove", "need `id`"))?.trim_start_matches("lib:").to_string();
    if !s.project.luts.iter().any(|l| l.id == id) {
        return Err(bad("lut.remove", format!("no LUT `{id}`")));
    }
    s.edit("Remove LUT", |pr, _| {
        pr.luts.retain(|l| l.id != id);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let r = str_p(p, "lut").ok_or_else(|| bad("lut.export", "need `lut`"))?;
    let path = str_p(p, "path").ok_or_else(|| bad("lut.export", "need `path`"))?;
    let lut = filmcraft_render::luts::resolve(Some(&s.project), r).ok_or_else(|| bad("lut.export", format!("unknown LUT `{r}`")))?;
    let fmt = match str_p(p, "format") {
        Some("3dl") => LutFormat::ThreeDl,
        Some("cube") => LutFormat::Cube,
        Some(o) => return Err(bad("lut.export", format!("unknown format `{o}`"))),
        None => LutFormat::from_path(path).unwrap_or(LutFormat::Cube),
    };
    let text = match fmt {
        LutFormat::Cube => lut.to_cube(),
        LutFormat::ThreeDl => lut.to_3dl().map_err(|e| bad("lut.export", e))?,
    };
    std::fs::write(path, &text).map_err(|e| bad("lut.export", format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": text.len()}))
}

/// The target clip and the index of its Lumetri effect (applying Lumetri when missing).
pub(crate) fn lumetri_clip(s: &mut Session, p: &Value, cmd: &str) -> Result<(ClipId, usize)> {
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let video = |c: &ClipId| seq.find_item(*c).is_some_and(|(t, _)| seq.track(t).is_some_and(|t| t.kind == TrackKind::Video));
    let has = |c: &ClipId| seq.find_item(*c).is_some_and(|(_, it)| it.effects.iter().any(|e| e.effect == "lumetri"));
    let clip = match clip_p(p, "clip") {
        Some(c) if video(&c) => c,
        Some(_) => return Err(bad(cmd, "not a video clip")),
        None => {
            let sel: Vec<ClipId> = s.state.selection.iter().copied().filter(video).collect();
            sel.iter().copied().find(has).or(sel.first().copied()).ok_or_else(|| bad(cmd, "select a video clip"))?
        }
    };
    if !has(&clip) {
        s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}))?;
    }
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let idx = seq.find_item(clip).and_then(|(_, it)| it.effects.iter().position(|e| e.effect == "lumetri")).ok_or_else(|| bad(cmd, "no Lumetri"))?;
    Ok((clip, idx))
}

/// Set a Lumetri parameter (inserting it when an older project's instance lacks it).
pub(crate) fn set_lumetri_param(s: &mut Session, clip: ClipId, idx: usize, param: &str, v: ParamValue, label: &str) -> Result<()> {
    let param = param.to_string();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(idx).ok_or_else(|| bad("lumetri", "no Lumetri"))?;
        e.params.entry(param.clone()).or_insert_with(|| filmcraft_project::Param::new(v.clone())).value = v.clone();
        Ok(())
    })?;
    Ok(())
}

fn set_lut(s: &mut Session, p: &Value, param: &str) -> Result<Value> {
    let cmd = if param == "input_lut" { "lumetri.setInputLut" } else { "lumetri.setLook" };
    let r = match (str_p(p, "path"), str_p(p, "lut")) {
        (Some(path), _) => {
            let path = path.to_string();
            format!("lib:{}", import_lut(s, &path, None)?.0)
        }
        (None, Some(r)) => r.to_string(),
        (None, None) => String::new(),
    };
    if !r.is_empty() && filmcraft_render::luts::resolve(Some(&s.project), &r).is_none() {
        return Err(bad(cmd, format!("unknown LUT `{r}`")));
    }
    let (clip, idx) = lumetri_clip(s, p, cmd)?;
    set_lumetri_param(s, clip, idx, param, ParamValue::Text(r.clone()), if param == "input_lut" { "Input LUT" } else { "Creative Look" })?;
    Ok(json!({"clip": clip.0, "lut": r, "name": filmcraft_render::luts::label(Some(&s.project), &r)}))
}

fn set_section(s: &mut Session, p: &Value) -> Result<Value> {
    let sec = str_p(p, "section").ok_or_else(|| bad("lumetri.setSection", "need `section`"))?;
    let param = match sec.to_ascii_lowercase().as_str() {
        "basic" | "basic correction" => "basic_on",
        "creative" => "creative_on",
        "curves" => "curves_on",
        "wheels" | "color wheels" | "color wheels & match" => "wheels_on",
        "hsl" | "hsl secondary" => "hsl_on",
        "vignette" => "vignette_on",
        o => return Err(bad("lumetri.setSection", format!("unknown section `{o}`"))),
    };
    let (clip, idx) = lumetri_clip(s, p, "lumetri.setSection")?;
    let cur = s
        .active_sequence()
        .and_then(|q| q.find_item(clip))
        .and_then(|(_, it)| it.effects.get(idx).and_then(|e| e.param(param)).and_then(|v| v.value.as_bool()))
        .unwrap_or(param != "hsl_on");
    let v = bool_p(p, "on").unwrap_or(!cur);
    set_lumetri_param(s, clip, idx, param, ParamValue::Bool(v), "Lumetri Section")?;
    Ok(json!({"clip": clip.0, "param": param, "on": v}))
}
