//! Edit plan commands (`plan.*`): validate, preview and apply an [`EditPlan`] (see
//! `docs/edit-plans.md`).
//!
//! An assistant never edits the timeline directly: it writes an edit plan, `plan.preview` shows
//! what it would cut (with a `sourceHash` of the source sequence and its transcripts), and
//! `plan.apply` applies it as **one undo step**, by default into a new sequence, refusing when the
//! source changed since the preview. `plan.applyVariations` applies up to
//! [`MAX_VARIATIONS`] plans as one undo step, one new sequence each.
//!
//! The plan is hostile input: it is size-capped, strictly parsed and validated by
//! [`filmcraft_edit::plan`] before anything is touched; references to the project (LUTs, Lumetri
//! presets, the grade reference) are checked here, and the caption style object is read leniently
//! (unknown keys are warnings, see [`filmcraft_edit::plan_style`]).
//!
//! Applying runs inside [`Session::grouped`], so every part below folds into the one undo step and
//! any error rolls all of it back:
//!
//! 1. copy the source (or edit it in place), ripple-delete the removals, add the markers;
//! 2. captions from the edited transcript, styled by `captions.style` (and its `case`);
//! 3. `grade.matchItem` through `lumetri.matchToItem` on the picture clips, then `grade.lut`
//!    (+ `lutStrength`) as the Lumetri Creative look and `grade.preset` as a Lumetri preset;
//! 4. `audio.targetLufs` (clamped to −30…−5): the Mix fader is moved until the mix's integrated
//!    loudness (`audio.loudness`) is on target.
//!
//! `captions.burnIn` (and `export`) are returned as `exportParams` for `file.exportMedia`;
//! `plan.apply` never exports. `output.aspect` and `captions.template` are reported in `skipped`.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};

use filmcraft_edit::EditCtx;
use filmcraft_edit::plan::{self, CaptionsPlan, CompiledPlan, EditPlan, OutputMode};
use filmcraft_edit::plan_style::{self, CaptionLook};
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{
    CaptionFormat, CaptionStyle, CaptionTrack, ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, ParamValue, Project, Sequence, TrackId,
};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, bad, str_p};
use crate::{EditorState, EngineError, MediaPool, Result, Session};

/// Most plans `plan.applyVariations` takes.
pub const MAX_VARIATIONS: usize = 6;
/// Characters of removed text shown per removal in `plan.preview`.
const PREVIEW_TEXT_CHARS: usize = 300;
/// `audio.targetLufs` is clamped to this range.
pub const LUFS_RANGE: (f64, f64) = (-30.0, -5.0);
/// How close (LU) the loudness pass aims, and the miss that becomes a warning.
const LUFS_STEP: f64 = 0.1;
const LUFS_TOLERANCE: f64 = 1.0;
/// Measure / adjust rounds of the loudness pass.
const LUFS_ROUNDS: usize = 4;

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

fn has_sequences(s: &Session) -> std::result::Result<(), String> {
    if s.project.sequences().next().is_some() { Ok(()) } else { Err("the project has no sequence".into()) }
}

fn secs(t: Tick) -> f64 {
    t.0 as f64 / TICKS_PER_SECOND as f64
}

/// The `plan` parameter: an EditPlan object or its JSON text.
fn plan_of(v: Option<&Value>) -> std::result::Result<EditPlan, String> {
    let owned;
    let text = match v {
        Some(Value::String(t)) => t.as_str(),
        Some(v @ Value::Object(_)) => {
            owned = serde_json::to_string(v).map_err(|e| e.to_string())?;
            owned.as_str()
        }
        _ => return Err("`plan` (an EditPlan object, or its JSON text) is required".into()),
    };
    plan::parse_plan(text).map_err(|e| e.to_string())
}

/// FNV-1a, 64 bit: a stable hash (the same bytes give the same hash in every run and build).
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Hash of the source sequence and the transcripts its words come from: `plan.apply` refuses a
/// plan previewed against another state (word indices and times would point elsewhere).
fn source_hash(p: &Project, id: ItemId, q: &Sequence, words: &[SeqWord]) -> String {
    let mut h = Fnv::new();
    h.write(b"filmcraft.plan.source.v1");
    h.write(&id.0.to_le_bytes());
    match serde_json::to_vec(q) {
        Ok(b) => h.write(&b),
        Err(e) => h.write(e.to_string().as_bytes()),
    }
    let items: BTreeSet<ItemId> = words.iter().map(|w| w.item).collect();
    for i in items {
        h.write(&i.0.to_le_bytes());
        if let Some(t) = p.transcripts.get(&i) {
            match serde_json::to_vec(&**t) {
                Ok(b) => h.write(&b),
                Err(e) => h.write(e.to_string().as_bytes()),
            }
        }
    }
    format!("{:016x}", h.0)
}

/// A plan compiled against its source.
struct Prepared {
    source: ItemId,
    source_name: String,
    words: Vec<SeqWord>,
    compiled: CompiledPlan,
    hash: String,
    extras: Extras,
}

/// What the look and sound parts of a plan resolve to in this project.
struct Extras {
    /// The output frame size.
    frame: (u32, u32),
    /// `captions.style` read onto the default caption style.
    look: CaptionLook,
    /// `grade.lut` as a `lib:` / `builtin:` reference.
    lut: Option<String>,
    /// `audio.targetLufs`, clamped to [`LUFS_RANGE`].
    target_lufs: Option<f64>,
    warnings: Vec<String>,
}

/// A LUT reference (`lib:<id>`, `builtin:<id>`), a library LUT's id or name, or a built-in LUT's
/// id or name (case-insensitive), as the reference `lumetri.setLook` takes.
fn resolve_lut(p: &Project, spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if spec.starts_with("lib:") || spec.starts_with("builtin:") {
        return filmcraft_render::luts::resolve(Some(p), spec).map(|_| spec.to_string());
    }
    if let Some(l) = p.luts.iter().find(|l| l.id == spec).or_else(|| p.luts.iter().find(|l| l.name.trim().eq_ignore_ascii_case(spec))) {
        return Some(format!("lib:{}", l.id));
    }
    filmcraft_render::luts::builtins()
        .iter()
        .find(|b| b.id.eq_ignore_ascii_case(spec) || b.label.eq_ignore_ascii_case(spec))
        .map(|b| format!("builtin:{}", b.id))
}

/// Short excerpt of a plan string for messages.
fn excerpt(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    if t.chars().count() < s.chars().count() { format!("{t}…") } else { t }
}

/// The picture size of a clip's item: media with video, subclips of it, nested sequences
/// (graphics and adjustment layers are canvases, not pictures).
fn picture_size(p: &Project, item: ItemId) -> Option<(u32, u32)> {
    match &p.item(item)?.kind {
        ItemKind::Graphic { .. } | ItemKind::AdjustmentLayer { .. } => None,
        _ => p.source_size(item),
    }
}

/// Check the plan's references to the project and resolve its look and sound parts: errors
/// (unknown LUT, preset or grade reference) and warnings (clamped loudness, caption style keys).
fn extras(p: &Project, plan: &EditPlan, q: &Sequence) -> (Extras, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let (w, h) = (q.settings.width, q.settings.height);
    let frame = (w, h);
    let look = match plan.captions.as_ref().and_then(|c| c.style.as_ref()) {
        Some(v) => plan_style::caption_look(v, &CaptionStyle::default()),
        None => CaptionLook { style: CaptionStyle::default(), case: None, warnings: Vec::new() },
    };
    warnings.extend(look.warnings.iter().cloned());
    let mut lut = None;
    if let Some(g) = &plan.grade {
        if let Some(id) = g.match_item {
            match p.item(ItemId(id)).map(|i| &i.kind) {
                Some(ItemKind::Sequence(_)) => {}
                Some(_) if picture_size(p, ItemId(id)).is_some() => {}
                Some(_) => errors.push(format!("grade.matchItem: item {id} has no picture to match (name a video item, subclip or sequence)")),
                None => errors.push(format!("grade.matchItem: {id} is not an item of this project")),
            }
        }
        if let Some(spec) = g.lut.as_deref() {
            match resolve_lut(p, spec) {
                Some(r) => lut = Some(r),
                None => errors.push(format!(
                    "grade.lut: no LUT \u{201c}{}\u{201d} (lut.list lists the library and built-in LUTs; import a .cube file with lut.import first)",
                    excerpt(spec)
                )),
            }
        }
        if g.lut_strength.is_some() && g.lut.is_none() {
            warnings.push("grade.lutStrength: ignored without grade.lut".into());
        }
        if let Some(name) = g.preset.as_deref()
            && filmcraft_render::lumetri_presets::find(name.trim()).is_none()
        {
            errors.push(format!("grade.preset: no Lumetri preset \u{201c}{}\u{201d} (lumetri.presets lists them)", excerpt(name)));
        }
        if g.match_item.is_none() && g.lut.is_none() && g.preset.is_none() {
            warnings.push("grade: names no matchItem, lut or preset; nothing to grade".into());
        }
    }
    let mut target_lufs = None;
    if let Some(t) = plan.audio.as_ref().and_then(|a| a.target_lufs).filter(|t| t.is_finite()) {
        let c = t.clamp(LUFS_RANGE.0, LUFS_RANGE.1);
        if c != t {
            warnings.push(format!("audio.targetLufs: {t} LUFS is outside {}…{} LUFS; using {c}", LUFS_RANGE.0, LUFS_RANGE.1));
        }
        target_lufs = Some(c);
    }
    (Extras { frame, look, lut, target_lufs, warnings }, errors)
}

/// Resolve the source and compile; every problem as a list.
fn prepare_raw(s: &Session, plan: &EditPlan) -> std::result::Result<Prepared, Vec<String>> {
    let id = plan
        .source
        .sequence
        .map(ItemId)
        .or(s.state.active_sequence)
        .ok_or_else(|| vec!["no sequence is open (open one, or name it in source.sequence)".to_string()])?;
    let q = s.project.sequence(id).ok_or_else(|| vec![format!("source.sequence: {} is not a sequence of this project", id.0)])?;
    let words = tx::sequence_words(q, &s.project.transcripts);
    let compiled = plan::compile(plan, q, &words, q.settings.frame_rate).map_err(|e| e.errors());
    let (extras, more) = extras(&s.project, plan, q);
    let compiled = match compiled {
        Ok(c) if more.is_empty() => c,
        Ok(_) => return Err(more),
        Err(mut v) => {
            v.extend(more);
            return Err(v);
        }
    };
    let hash = source_hash(&s.project, id, q, &words);
    let source_name = s.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
    Ok(Prepared { source: id, source_name, words, compiled, hash, extras })
}

fn prepare(s: &Session, plan: &EditPlan, cmd: &str) -> Result<Prepared> {
    prepare_raw(s, plan).map_err(|v| bad(cmd, v.join("; ")))
}

/// Parts of the plan `plan.apply` does not carry out, with why.
fn skipped(plan: &EditPlan) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if plan.output.aspect.is_some() {
        out.push(("output.aspect", "output.aspect is not applied yet: the new sequence keeps the source frame size"));
    }
    if plan.captions.as_ref().is_some_and(|c| c.template.is_some()) {
        out.push((
            "captions.template",
            "captions.template: graphics templates can't style a caption track yet; the captions use captions.style (or the default caption style)",
        ));
    }
    if plan.export.is_some() {
        out.push(("export", "plan.apply never exports: run file.exportMedia with the returned exportParams once the edit is approved"));
    }
    out
}

/// The compiler's warnings, then the look and sound notes, then what is skipped.
fn plan_warnings(prep: &Prepared, plan: &EditPlan) -> Vec<String> {
    let mut w = prep.compiled.warnings.clone();
    w.extend(prep.extras.warnings.iter().cloned());
    w.extend(skipped(plan).into_iter().map(|(_, m)| m.to_string()));
    w
}

/// Caption line rules: the plan's, with `maxChars` defaulting to what fits the frame at the
/// caption size (42 in a 16:9 frame, about 18 in 9:16).
fn caption_rules(c: &CaptionsPlan, x: &Extras) -> CaptionRules {
    let d = CaptionRules::default();
    let fit = plan_style::caption_chars_for(x.frame.0, x.frame.1, x.look.style.size);
    CaptionRules { max_chars: c.max_chars.unwrap_or(d.max_chars.min(fit)), lines: c.lines.unwrap_or(d.lines), ..d }
}

fn mode_name(m: OutputMode) -> &'static str {
    match m {
        OutputMode::NewSequence => "newSequence",
        OutputMode::InPlace => "inPlace",
    }
}

fn validate(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = match plan_of(p.get("plan")) {
        Ok(x) => x,
        Err(e) => return Ok(json!({"ok": false, "errors": [e], "warnings": []})),
    };
    Ok(match prepare_raw(s, &plan) {
        Ok(prep) => json!({"ok": true, "errors": [], "warnings": plan_warnings(&prep, &plan)}),
        Err(errors) => json!({"ok": false, "errors": errors, "warnings": []}),
    })
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plan.preview";
    let plan = plan_of(p.get("plan")).map_err(|e| bad(CMD, e))?;
    let prep = prepare(s, &plan, CMD)?;
    let c = &prep.compiled;
    let texts = plan::removed_texts(&prep.words, &c.removals, PREVIEW_TEXT_CHARS);
    let removals: Vec<Value> = c
        .removals
        .iter()
        .zip(texts)
        .map(|(r, text)| {
            json!({
                "start": secs(r.range.start), "end": secs(r.range.end()), "duration": secs(r.range.duration),
                "startTick": r.range.start.0, "endTick": r.range.end().0,
                "reason": r.reason, "kind": r.kind.name(), "text": text,
            })
        })
        .collect();
    let rate = s.project.sequence(prep.source).map(|q| q.settings.frame_rate).unwrap_or_default();
    let kept = plan::words_after(&prep.words, &c.removals);
    let captions = plan.captions.as_ref().map(|cp| tx::caption_blocks(&kept, &caption_rules(cp, &prep.extras), rate).len());
    let skipped: Vec<&str> = skipped(&plan).into_iter().map(|(k, _)| k).collect();
    Ok(json!({
        "sequence": prep.source.0,
        "sourceHash": prep.hash,
        "title": plan.title,
        "rationale": plan.rationale,
        "mode": mode_name(plan.output.mode),
        "name": output_name(&plan, &prep),
        "removals": removals,
        "before": secs(c.before),
        "after": secs(c.after),
        "removedSeconds": secs(c.removed()),
        "segments": c.segments,
        "words": {"total": prep.words.len(), "kept": kept.len()},
        "captions": captions,
        "markers": c.markers.iter().map(|m| json!({"name": m.name, "at": secs(m.at)})).collect::<Vec<_>>(),
        "warnings": plan_warnings(&prep, &plan),
        "skipped": skipped,
    }))
}

/// Name of the sequence a plan makes: `output.name`, else "<source> — <title>".
fn output_name(plan: &EditPlan, prep: &Prepared) -> String {
    match plan.output.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => format!("{} \u{2014} {}", prep.source_name, plan.title.trim()),
    }
}

/// What applying one plan did.
struct Applied {
    sequence: ItemId,
    name: String,
    removed: Tick,
    duration: Tick,
    captions: Option<usize>,
    grade: Option<Value>,
    loudness: Option<Value>,
    /// What the apply passes could not do (silent mix, loudness target missed…).
    warnings: Vec<String>,
}

/// Apply one compiled plan inside an edit closure: copy the source (unless in place), ripple-delete
/// the removals, add styled captions and markers. `snap` is the project before the edit (media
/// durations).
fn apply_one(pr: &mut Project, snap: &Arc<Project>, media: &Arc<MediaPool>, plan: &EditPlan, prep: &Prepared, name: &str) -> Result<Applied> {
    let src = pr.sequence(prep.source).ok_or(EngineError::NoSequence)?.clone();
    let target = match plan.output.mode {
        OutputMode::InPlace => prep.source,
        OutputMode::NewSequence => {
            let label = pr.item(prep.source).map(|i| i.label);
            let bin = pr.root.parent_of(prep.source).filter(|b| *b != pr.root.id);
            pr.add_item(name, label.unwrap_or(Label::Forest), ItemKind::Sequence(Box::new(src)), bin)
        }
    };
    let transcripts = pr.transcripts.clone();
    let (snap_d, snap_s, media) = (snap.clone(), snap.clone(), media.clone());
    let durations = move |id: ItemId| -> Option<Tick> { crate::media_duration(&snap_d, &media, id) };
    let starts = move |id: ItemId| crate::media_start(&snap_s, id);
    let mut next = pr.next_id;
    let x = &prep.extras;
    let (removed, duration, captions) = {
        let seq = pr.sequence_mut(target).ok_or(EngineError::NoSequence)?;
        let rate = seq.settings.frame_rate;
        let mut ctx = EditCtx { next_id: &mut next, media_duration: &durations, media_start: &starts, min_duration: rate.frame_duration() };
        let ranges: Vec<TimeRange> = prep.compiled.removals.iter().map(|r| r.range).collect();
        // sequence markers follow the cuts (right to left, so earlier positions stay valid)
        for r in ranges.iter().rev() {
            crate::sequence_tools::ripple_markers(&mut seq.markers, r.end(), Tick::ZERO - r.duration);
        }
        let removed = tx::ripple_delete_ranges(seq, ranges, &mut ctx);
        for m in &prep.compiled.markers {
            let id = MarkerId(ctx.alloc());
            seq.markers.push(Marker {
                id,
                start: m.at,
                duration: Tick::ZERO,
                name: m.name.clone(),
                comment: String::new(),
                kind: MarkerKind::Comment,
                color: Label::Green,
            });
        }
        seq.markers.sort_by_key(|m| m.start);
        let mut captions = None;
        if let Some(c) = &plan.captions {
            // the transcript of the edited sequence, so captions follow the cuts
            let words = tx::sequence_words(seq, &transcripts);
            let blocks = tx::caption_blocks(&words, &caption_rules(c, x), rate);
            if !blocks.is_empty() {
                let mut t = CaptionTrack::new(TrackId(ctx.alloc()), "Captions".into(), CaptionFormat::default());
                t.captions = tx::blocks_to_captions(&blocks, &mut ctx);
                if let Some(case) = x.look.case {
                    for cap in &mut t.captions {
                        cap.text = plan_style::apply_case(&cap.text, case);
                    }
                }
                t.style = x.look.style.clone();
                seq.caption_tracks.insert(0, t);
            }
            captions = Some(blocks.len());
        }
        seq.check().map_err(EngineError::Other)?;
        (removed, seq.duration(), captions)
    };
    pr.next_id = next;
    let name = pr.item(target).map(|i| i.name.clone()).unwrap_or_default();
    Ok(Applied { sequence: target, name, removed, duration, captions, grade: None, loudness: None, warnings: Vec::new() })
}

/// The picture clips of sequence `seq` (what the grade touches).
fn picture_clips(p: &Project, seq: ItemId) -> Vec<ClipId> {
    let Some(q) = p.sequence(seq) else { return Vec::new() };
    q.video_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| picture_size(p, it.item).is_some()).map(|it| it.id).collect()
}

/// Set (or add) an effect parameter.
fn set_param(e: &mut filmcraft_project::EffectInstance, id: &str, v: ParamValue) {
    match e.params.get_mut(id) {
        Some(p) => {
            p.keyframes.clear();
            p.value = v;
        }
        None => {
            e.params.insert(id.to_string(), filmcraft_project::Param::new(v));
        }
    }
}

/// `grade`: match to the reference (`lumetri.matchToItem`), then the look LUT on each clip's
/// Lumetri Color (added when missing) and the Lumetri preset as one more Lumetri Color. Runs on
/// the active sequence (the plan's output).
fn grade_pass(s: &mut Session, plan: &EditPlan, prep: &Prepared, a: &mut Applied) -> Result<()> {
    const CMD: &str = "plan.apply";
    let Some(g) = &plan.grade else { return Ok(()) };
    if g.match_item.is_none() && prep.extras.lut.is_none() && g.preset.is_none() {
        return Ok(());
    }
    let clips = picture_clips(&s.project, a.sequence);
    if clips.is_empty() {
        a.warnings.push("grade: the edited sequence has no video clips to grade".into());
        return Ok(());
    }
    let mut report = json!({"clips": clips.len()});
    if let Some(id) = g.match_item {
        let ids: Vec<u64> = clips.iter().map(|c| c.0).collect();
        let r = s.execute("lumetri.matchToItem", json!({"clips": ids, "item": id})).map_err(|e| bad(CMD, format!("grade.matchItem: {e}")))?;
        report["matchItem"] = json!(id);
        report["matched"] = json!(r.get("clips").and_then(Value::as_array).map_or(0, Vec::len));
    }
    let preset = match g.preset.as_deref() {
        Some(n) => {
            Some(filmcraft_render::lumetri_presets::find(n.trim()).ok_or_else(|| bad(CMD, format!("grade.preset: no Lumetri preset `{}`", excerpt(n))))?)
        }
        None => None,
    };
    let lut = prep.extras.lut.clone();
    if lut.is_some() || preset.is_some() {
        let lumetri = filmcraft_project::find_effect("lumetri").ok_or_else(|| bad(CMD, "grade: Lumetri Color is not available"))?;
        let strength = g.lut_strength.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0));
        let wanted: std::collections::HashSet<ClipId> = clips.iter().copied().collect();
        let (lut2, preset2) = (lut.clone(), preset.clone());
        s.edit_sequence("Grade", move |q, _, _| {
            for it in q.video_tracks.iter_mut().flat_map(|t| t.items.iter_mut()).filter(|i| wanted.contains(&i.id)) {
                // standard effects go before the intrinsic ones, like effects.apply
                let intrinsic =
                    |it: &filmcraft_project::TrackItem| it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len());
                if let Some(r) = &lut2 {
                    let i = match it.effects.iter().position(|e| e.effect == "lumetri") {
                        Some(i) => i,
                        None => {
                            let pos = intrinsic(it);
                            it.effects.insert(pos, lumetri.instance());
                            pos
                        }
                    };
                    if let Some(e) = it.effects.get_mut(i) {
                        set_param(e, "look_lut", ParamValue::Text(r.clone()));
                        if let Some(k) = strength {
                            set_param(e, "look_intensity", ParamValue::Float(k * 100.0));
                        }
                    }
                }
                if let Some(pr) = &preset2 {
                    let pos = intrinsic(it);
                    it.effects.insert(pos, pr.instance());
                }
            }
            Ok(())
        })?;
        if let Some(r) = &lut {
            report["lut"] = json!(r);
            report["lutName"] = json!(filmcraft_render::luts::label(Some(&s.project), r));
            report["lutStrength"] = json!(strength.unwrap_or(1.0));
        }
        if let Some(p) = &preset {
            report["preset"] = json!(p.name);
        }
    }
    a.grade = Some(report);
    Ok(())
}

/// Integrated loudness and true peak of the active sequence's mix (`audio.loudness`).
fn mix_loudness(s: &mut Session) -> std::result::Result<(Option<f64>, Option<f64>), String> {
    let r = s.execute("audio.loudness", json!({})).map_err(|e| e.to_string())?;
    Ok((r.get("integratedLufs").and_then(Value::as_f64), r.get("truePeakDbtp").and_then(Value::as_f64)))
}

/// `audio.targetLufs`: move the Mix fader by the measured difference until the mix's integrated
/// loudness is within [`LUFS_STEP`] of the target ([`LUFS_ROUNDS`] rounds at most: effects on the
/// Mix need not be linear). A silent or unmeasurable mix is a warning, not an error.
fn loudness_pass(s: &mut Session, prep: &Prepared, a: &mut Applied) -> Result<()> {
    let Some(target) = prep.extras.target_lufs else { return Ok(()) };
    let (first, mut peak) = match mix_loudness(s) {
        Ok(v) => v,
        Err(e) => {
            a.warnings.push(format!("audio.targetLufs: the mix could not be measured ({e}); loudness unchanged"));
            return Ok(());
        }
    };
    let Some(first) = first else {
        a.warnings.push("audio.targetLufs: the mix is silent; loudness unchanged".into());
        return Ok(());
    };
    let fader = |s: &Session| s.project.sequence(a.sequence).map_or(0.0, |q| q.master_volume_db);
    let fader0 = fader(s);
    let mut now = first;
    for _ in 0..LUFS_ROUNDS {
        let delta = target - now;
        if !delta.is_finite() || delta.abs() <= LUFS_STEP {
            break;
        }
        s.edit_sequence("Loudness", move |q, _, _| {
            q.master_volume_db = (q.master_volume_db + delta).clamp(-96.0, 48.0);
            Ok(())
        })?;
        match mix_loudness(s) {
            Ok((Some(l), p)) => {
                now = l;
                peak = p;
            }
            _ => break,
        }
    }
    if (now - target).abs() > LUFS_TOLERANCE {
        a.warnings.push(format!(
            "audio.targetLufs: the mix reached {now:.1} LUFS, not {target} (the Mix fader is automated, at its limit, or followed by a limiter)"
        ));
    }
    if let Some(p) = peak.filter(|p| *p > -1.0) {
        a.warnings.push(format!("audio.targetLufs: the true peak is {p:.1} dBTP (above −1 dBTP); add a limiter on the Mix before exporting"));
    }
    a.loudness = Some(json!({
        "targetLufs": target,
        "beforeLufs": first,
        "afterLufs": now,
        "gainDb": fader(s) - fader0,
        "truePeakDbtp": peak,
    }));
    Ok(())
}

/// The passes that need the session (rendering, measuring), on the plan's output sequence, which
/// must be the active one. Inside the caller's [`Session::grouped`] step.
fn finish(s: &mut Session, plan: &EditPlan, prep: &Prepared, a: &mut Applied) -> Result<()> {
    s.state.active_sequence = Some(a.sequence);
    grade_pass(s, plan, prep, a)?;
    loudness_pass(s, prep, a)?;
    a.duration = s.project.sequence(a.sequence).map_or(a.duration, Sequence::duration);
    Ok(())
}

fn open(st: &mut EditorState, id: ItemId) {
    if !st.open_sequences.contains(&id) {
        st.open_sequences.push(id);
    }
    st.playheads.entry(id).or_insert(Tick::ZERO);
}

/// What to pass to `file.exportMedia` for this output: the sequence, `burnCaptions` and the
/// plan's export preset and path (None when the plan asks for neither burn-in nor an export).
fn export_params(a: &Applied, plan: &EditPlan) -> Option<Value> {
    let burn = plan.captions.as_ref().is_some_and(|c| c.burn_in);
    if !burn && plan.export.is_none() {
        return None;
    }
    let mut v = json!({"sequence": a.sequence.0, "burnCaptions": burn});
    if let Some(e) = &plan.export {
        if let Some(p) = &e.preset {
            v["preset"] = json!(p);
        }
        if let Some(p) = &e.path {
            v["path"] = json!(p);
        }
    }
    Some(v)
}

fn applied_json(a: &Applied, prep: &Prepared, plan: &EditPlan) -> Value {
    let mut warnings = plan_warnings(prep, plan);
    warnings.extend(a.warnings.iter().cloned());
    let mut out = json!({
        "sequence": a.sequence.0,
        "name": a.name,
        "removedS": secs(a.removed),
        "durationS": secs(a.duration),
        "removals": prep.compiled.removals.len(),
        "captions": a.captions,
        "markers": prep.compiled.markers.len(),
        "grade": a.grade,
        "loudness": a.loudness,
        "warnings": warnings,
        "skipped": skipped(plan).into_iter().map(|(k, _)| k).collect::<Vec<_>>(),
    });
    if let Some(e) = export_params(a, plan) {
        out["exportParams"] = e;
    }
    out
}

fn check_hash(prep: &Prepared, p: &Value, cmd: &str) -> Result<()> {
    let want = str_p(p, "sourceHash").ok_or_else(|| bad(cmd, "`sourceHash` (from plan.preview) is required"))?;
    if want != prep.hash {
        return Err(EngineError::Other(format!(
            "the source sequence or its transcript changed since plan.preview (sourceHash {want} is now {}); preview the plan again",
            prep.hash
        )));
    }
    Ok(())
}

fn short(title: &str) -> String {
    let t: String = title.trim().chars().take(60).collect();
    if t.chars().count() < title.trim().chars().count() { format!("{t}…") } else { t }
}

/// Apply one plan as one step of an enclosing [`Session::grouped`]: the timeline edit, then the
/// session passes. Leaves its output active and open.
fn apply_in_group(s: &mut Session, plan: &EditPlan, prep: &Prepared, name: &str, label: &str) -> Result<Applied> {
    let media = s.media.clone();
    let mut a = s.edit(label, |pr, st| {
        let snap = Arc::new(pr.clone());
        let a = apply_one(pr, &snap, &media, plan, prep, name)?;
        st.active_sequence = Some(a.sequence);
        st.selection.clear();
        st.caption_selection.clear();
        open(st, a.sequence);
        Ok(a)
    })?;
    finish(s, plan, prep, &mut a)?;
    Ok(a)
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plan.apply";
    let plan = plan_of(p.get("plan")).map_err(|e| bad(CMD, e))?;
    let prep = prepare(s, &plan, CMD)?;
    check_hash(&prep, p, CMD)?;
    let name = output_name(&plan, &prep);
    let label = format!("Apply Edit Plan \u{201c}{}\u{201d}", short(&plan.title));
    let journal = s.journal.len();
    let done = s.grouped(&label, |s| apply_in_group(s, &plan, &prep, &name, &label))?;
    // the commands run inside are part of plan.apply, which is journaled on its own
    s.journal.truncate(journal);
    s.events.push(crate::Event::OpenSequence(done.sequence));
    Ok(applied_json(&done, &prep, &plan))
}

fn apply_variations(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plan.applyVariations";
    let list = p.get("plans").and_then(Value::as_array).ok_or_else(|| bad(CMD, format!("`plans` (1–{MAX_VARIATIONS} EditPlans) is required")))?;
    if list.is_empty() || list.len() > MAX_VARIATIONS {
        return Err(bad(CMD, format!("`plans` must hold 1–{MAX_VARIATIONS} plans, not {}", list.len())));
    }
    let mut work: Vec<(EditPlan, Prepared, String)> = Vec::with_capacity(list.len());
    for (k, v) in list.iter().enumerate() {
        let plan = plan_of(Some(v)).map_err(|e| bad(CMD, format!("plans[{k}]: {e}")))?;
        if plan.output.mode == OutputMode::InPlace {
            return Err(bad(CMD, format!("plans[{k}]: variations always go into new sequences (output.mode must be newSequence)")));
        }
        let prep = prepare_raw(s, &plan).map_err(|e| bad(CMD, format!("plans[{k}]: {}", e.join("; "))))?;
        if let Some((_, first, _)) = work.first()
            && first.source != prep.source
        {
            return Err(bad(CMD, format!("plans[{k}]: edits another source sequence than plans[0]")));
        }
        let name = format!("{} \u{2014} v{}", output_name(&plan, &prep), k + 1);
        work.push((plan, prep, name));
    }
    if let Some((_, prep, _)) = work.first() {
        check_hash(prep, p, CMD)?;
    }
    let label = format!("Apply {} Edit Plan Variations", work.len());
    let journal = s.journal.len();
    let done = s.grouped(&label, |s| {
        let mut out = Vec::with_capacity(work.len());
        for (plan, prep, name) in &work {
            out.push(apply_in_group(s, plan, prep, name, &label)?);
        }
        s.state.active_sequence = out.first().map(|a| a.sequence);
        Ok(out)
    })?;
    s.journal.truncate(journal);
    if let Some(first) = done.first() {
        s.events.push(crate::Event::OpenSequence(first.sequence));
    }
    let seqs: Vec<Value> = done.iter().zip(&work).map(|(a, (plan, prep, _))| applied_json(a, prep, plan)).collect();
    Ok(json!({"sequences": seqs}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("plan.validate", "Validate Edit Plan", r#"{"plan":EditPlan|str}"#, has_sequences, validate, false),
        spec("plan.preview", "Preview Edit Plan", r#"{"plan":EditPlan|str}"#, has_sequences, preview, false),
        spec("plan.apply", "Apply Edit Plan", r#"{"plan":EditPlan|str,"sourceHash":str}"#, has_sequences, apply, true),
        spec("plan.applyVariations", "Apply Edit Plan Variations", r#"{"plans":[EditPlan|str],"sourceHash":str}"#, has_sequences, apply_variations, true),
    ]
}
