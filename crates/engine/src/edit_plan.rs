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
//! [`filmcraft_edit::plan`] before anything is touched.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use filmcraft_edit::plan::{self, CaptionsPlan, CompiledPlan, EditPlan, OutputMode};
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{ItemId, Project, Sequence};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

use crate::commands::{CommandSpec, bad};
use crate::{Result, Session};

/// Characters of removed text shown per removal in `plan.preview`.
const PREVIEW_TEXT_CHARS: usize = 300;

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
    let compiled = plan::compile(plan, q, &words, q.settings.frame_rate).map_err(|e| e.errors())?;
    let hash = source_hash(&s.project, id, q, &words);
    let source_name = s.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
    Ok(Prepared { source: id, source_name, words, compiled, hash })
}

fn prepare(s: &Session, plan: &EditPlan, cmd: &str) -> Result<Prepared> {
    prepare_raw(s, plan).map_err(|v| bad(cmd, v.join("; ")))
}

/// Parts of the plan `plan.apply` does not carry out (yet), with why.
fn skipped(plan: &EditPlan) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if plan.output.aspect.is_some() {
        out.push(("output.aspect", "output.aspect is not applied yet: the new sequence keeps the source frame size"));
    }
    if let Some(c) = &plan.captions {
        if c.burn_in {
            out.push(("captions.burnIn", "captions.burnIn: captions are added as a caption track; burn them in when exporting"));
        }
        if c.style.is_some() {
            out.push(("captions.style", "captions.style is not applied yet: style the caption track afterwards"));
        }
        if c.template.is_some() {
            out.push(("captions.template", "captions.template is not applied yet: apply a caption template afterwards"));
        }
    }
    if plan.grade.is_some() {
        out.push(("grade", "grade is not applied by plan.apply yet: use the lumetri.* commands on the edited sequence"));
    }
    if plan.audio.is_some() {
        out.push(("audio", "audio loudness is not applied by plan.apply yet: use sequence.normalizeMixTrack on the edited sequence"));
    }
    if plan.export.is_some() {
        out.push(("export", "plan.apply never exports: run file.exportMedia once the edit is approved"));
    }
    out
}

fn warnings_with_skipped(c: &CompiledPlan, plan: &EditPlan) -> Vec<String> {
    let mut w = c.warnings.clone();
    w.extend(skipped(plan).into_iter().map(|(_, m)| m.to_string()));
    w
}

fn caption_rules(c: &CaptionsPlan) -> CaptionRules {
    let d = CaptionRules::default();
    CaptionRules { max_chars: c.max_chars.unwrap_or(d.max_chars), lines: c.lines.unwrap_or(d.lines), ..d }
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
        Ok(prep) => json!({"ok": true, "errors": [], "warnings": warnings_with_skipped(&prep.compiled, &plan)}),
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
    let captions = plan.captions.as_ref().map(|cp| tx::caption_blocks(&kept, &caption_rules(cp), rate).len());
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
        "warnings": warnings_with_skipped(c, &plan),
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

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("plan.validate", "Validate Edit Plan", r#"{"plan":EditPlan|str}"#, has_sequences, validate, false),
        spec("plan.preview", "Preview Edit Plan", r#"{"plan":EditPlan|str}"#, has_sequences, preview, false),
    ]
}
