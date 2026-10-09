//! Edit plans: a typed, versioned description of a whole edit (cuts, cleanup, captions, markers…)
//! that an assistant writes and a deterministic executor validates, previews and applies.
//!
//! The LLM never edits the timeline. It writes an [`EditPlan`]; [`compile`] turns it into frame-
//! aligned [`Removal`]s on the source sequence and the engine (`plan.*` commands) applies them as one
//! undo step. A plan is **hostile input**: [`parse_plan`] caps its size and rejects unknown fields,
//! and [`compile`] checks every number and index ([`validate`]) before using it.
//!
//! Compiling fuses the plan's cuts:
//!
//! - word cuts (`cuts.removeWords`, inclusive word indices as `transcript.inspect` lists them) go
//!   through [`tx::word_range`]; fillers through [`tx::find_fillers`] + [`tx::filler_ranges`];
//!   pauses through [`tx::find_pauses`]; `cuts.keepOnly` becomes the removals between the kept words;
//! - seconds-based ranges are clamped to the sequence. A `cuts.removeRanges` boundary that falls
//!   inside a word is moved **outward** to the word's edge (the partly covered word is cut whole);
//!   silences, untranscribed sounds and every other cut never take a kept word: the kept words are
//!   carved out of them, so their boundaries land in the inter-word gaps;
//! - boundaries are then snapped to frames where that keeps every kept word whole;
//! - overlapping cuts are merged, and a kept fragment shorter than [`MIN_FRAGMENT_S`] between two
//!   cuts (with no kept word in it) is cut too.
//!
//! The result never has a removal boundary strictly inside a kept word (a word not wholly removed).

use filmcraft_project::Sequence;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::{self as tx, SeqWord};

/// The plan format version this build understands.
pub const PLAN_VERSION: u32 = 1;
/// Largest plan accepted by [`parse_plan`] (bytes of JSON).
pub const MAX_PLAN_BYTES: usize = 2 * 1024 * 1024;
/// Most removals a plan may ask for, and most removals a compiled plan may have.
pub const MAX_REMOVALS: usize = 10_000;
/// Longest title (characters).
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest name of an output sequence or marker (characters).
pub const MAX_NAME_CHARS: usize = 200;
/// Longest reason, rationale, path or preset (characters).
pub const MAX_TEXT_CHARS: usize = 4_000;
/// Most markers.
pub const MAX_MARKERS: usize = 1_000;
/// Most filler words or phrases.
pub const MAX_FILLERS: usize = 256;
/// Kept fragments shorter than this (seconds) between two removals are removed too.
pub const MIN_FRAGMENT_S: f64 = 0.3;
/// Longest pause rule (`minS` / `keepS`, seconds).
pub const MAX_PAUSE_S: f64 = 3_600.0;
/// Warnings kept (the rest are counted in a last line).
pub const MAX_WARNINGS: usize = 200;
/// Removals considered before merging (fillers and pauses of a very long transcript).
const MAX_RAW_PIECES: usize = 200_000;

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

/// An edit plan (JSON, camelCase keys, unknown keys rejected). `{"version":1,"title":"x"}` is the
/// smallest valid plan (it changes nothing).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EditPlan {
    /// Must be [`PLAN_VERSION`].
    pub version: u32,
    pub title: String,
    /// Why the plan cuts what it cuts (shown to the user).
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub source: PlanSource,
    #[serde(default)]
    pub output: PlanOutput,
    #[serde(default)]
    pub cuts: Cuts,
    #[serde(default)]
    pub cleanup: Cleanup,
    #[serde(default)]
    pub captions: Option<CaptionsPlan>,
    #[serde(default)]
    pub grade: Option<GradePlan>,
    #[serde(default)]
    pub audio: Option<AudioPlan>,
    #[serde(default)]
    pub markers: Vec<PlanMarker>,
    /// The duration the edit aims for (a miss of more than 5 % is a warning).
    #[serde(default)]
    pub target_duration_s: Option<f64>,
    /// An export to run after the edit (never run by `plan.apply`).
    #[serde(default)]
    pub export: Option<ExportPlan>,
}

/// The sequence the plan edits (None: the active sequence).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanSource {
    #[serde(default)]
    pub sequence: Option<u64>,
}

/// Where the edit goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputMode {
    /// A copy of the source sequence (the source stays untouched).
    #[default]
    #[serde(alias = "NewSequence", alias = "new_sequence")]
    NewSequence,
    /// The source sequence itself.
    #[serde(alias = "InPlace", alias = "in_place")]
    InPlace,
}

/// Output frame aspect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aspect {
    #[serde(rename = "16:9")]
    Wide,
    #[serde(rename = "9:16")]
    Vertical,
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "4:5")]
    Portrait,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanOutput {
    #[serde(default)]
    pub mode: OutputMode,
    /// Name of the new sequence (default "<source> — <title>").
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub aspect: Option<Aspect>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Cuts {
    /// Words to remove (inclusive sequence-transcript word indices).
    #[serde(default)]
    pub remove_words: Vec<WordCut>,
    /// Time ranges to remove (sequence seconds).
    #[serde(default)]
    pub remove_ranges: Vec<RangeCut>,
    /// Keep only these words: everything between them is removed.
    #[serde(default)]
    pub keep_only: Option<Vec<WordSpan>>,
}

/// Words `from..=to` (`to` defaults to `from`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WordCut {
    pub from: usize,
    #[serde(default)]
    pub to: Option<usize>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RangeCut {
    pub start_s: f64,
    pub end_s: f64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WordSpan {
    pub from: usize,
    #[serde(default)]
    pub to: Option<usize>,
}

/// A span of sequence time in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Span {
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Cleanup {
    /// Remove filler words: the listed words / phrases, or [`tx::DEFAULT_FILLERS`] when the list
    /// is empty. None: keep fillers.
    #[serde(default)]
    pub fillers: Option<Vec<String>>,
    #[serde(default)]
    pub pauses: Option<PauseRule>,
    /// Silences to remove (from `audio.detectSilence`; the caller passes them).
    #[serde(default)]
    pub silences: Vec<Span>,
    /// Voiced sounds without a transcribed word to remove (opt-in).
    #[serde(default)]
    pub untranscribed: Vec<Span>,
}

/// Shorten pauses between words of at least `minS`, keeping `keepS` next to each word.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PauseRule {
    #[serde(default = "default_pause_min")]
    pub min_s: f64,
    #[serde(default = "default_pause_keep")]
    pub keep_s: f64,
}

fn default_pause_min() -> f64 {
    1.0
}
fn default_pause_keep() -> f64 {
    0.15
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CaptionsPlan {
    /// Characters per line (default 42).
    #[serde(default)]
    pub max_chars: Option<usize>,
    /// Lines per caption (default 2).
    #[serde(default)]
    pub lines: Option<usize>,
    #[serde(default)]
    pub burn_in: bool,
    #[serde(default)]
    pub style: Option<Value>,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GradePlan {
    #[serde(default)]
    pub match_item: Option<u64>,
    #[serde(default)]
    pub lut: Option<String>,
    #[serde(default)]
    pub lut_strength: Option<f64>,
    #[serde(default)]
    pub preset: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioPlan {
    #[serde(default)]
    pub target_lufs: Option<f64>,
}

/// A marker at a time (seconds of the source sequence) or at the start of a word (exactly one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanMarker {
    #[serde(default)]
    pub time_s: Option<f64>,
    #[serde(default)]
    pub word: Option<usize>,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExportPlan {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// What a removal came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemovalKind {
    /// `cuts.removeWords` (and the words outside `cuts.keepOnly`).
    Word,
    /// `cuts.removeRanges`.
    Range,
    Filler,
    Pause,
    Silence,
    Untranscribed,
}

impl RemovalKind {
    pub fn name(self) -> &'static str {
        match self {
            RemovalKind::Word => "word",
            RemovalKind::Range => "range",
            RemovalKind::Filler => "filler",
            RemovalKind::Pause => "pause",
            RemovalKind::Silence => "silence",
            RemovalKind::Untranscribed => "untranscribed",
        }
    }
}

/// A range of the source sequence to ripple-delete.
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    pub range: TimeRange,
    pub reason: String,
    pub kind: RemovalKind,
}

/// A marker to add, at its time in the edited sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledMarker {
    /// Time in the source sequence.
    pub source: Tick,
    /// Time in the edited sequence (a marker inside a removal moves to the cut).
    pub at: Tick,
    pub name: String,
}

/// A compiled plan: sorted, disjoint, non-touching removals and what they do.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledPlan {
    pub removals: Vec<Removal>,
    pub warnings: Vec<String>,
    /// Source duration.
    pub before: Tick,
    /// Duration once the removals are gone.
    pub after: Tick,
    /// Kept segments (pieces of the source that remain).
    pub segments: usize,
    pub markers: Vec<CompiledMarker>,
}

impl CompiledPlan {
    /// Total removed time.
    pub fn removed(&self) -> Tick {
        self.before - self.after
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("the plan is {0} bytes of JSON; the limit is {MAX_PLAN_BYTES}")]
    TooLarge(usize),
    #[error("the plan is not a valid EditPlan: {0}")]
    Parse(String),
    /// Every problem found (each names the field, e.g. `cuts.removeWords[2].to`).
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
}

impl PlanError {
    /// The problems as a list (one entry for parse and size errors).
    pub fn errors(&self) -> Vec<String> {
        match self {
            PlanError::Invalid(v) => v.clone(),
            e => vec![e.to_string()],
        }
    }
}

/// Parse a plan from JSON text, refusing more than [`MAX_PLAN_BYTES`] and unknown fields.
pub fn parse_plan(text: &str) -> Result<EditPlan, PlanError> {
    if text.len() > MAX_PLAN_BYTES {
        return Err(PlanError::TooLarge(text.len()));
    }
    serde_json::from_str(text).map_err(|e| PlanError::Parse(e.to_string()))
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Every problem of `plan` for a sequence transcript of `words` words (empty: the plan is valid).
/// Checks the version, sizes, that every number is finite and in range and that every word index
/// exists. [`compile`] refuses a plan with problems.
pub fn validate(plan: &EditPlan, words: usize) -> Vec<String> {
    let mut p: Vec<String> = Vec::new();
    // stop collecting at some point: a hostile plan can have 10 000 bad entries
    fn push(p: &mut Vec<String>, m: String) {
        if p.len() < MAX_WARNINGS {
            p.push(m);
        }
    }
    if plan.version != PLAN_VERSION {
        push(&mut p, format!("version: {} is not supported (use {PLAN_VERSION})", plan.version));
    }
    if plan.title.trim().is_empty() {
        push(&mut p, "title: is empty".into());
    }
    if chars(&plan.title) > MAX_TITLE_CHARS {
        push(&mut p, format!("title: longer than {MAX_TITLE_CHARS} characters"));
    }
    if chars(&plan.rationale) > MAX_TEXT_CHARS * 4 {
        push(&mut p, format!("rationale: longer than {} characters", MAX_TEXT_CHARS * 4));
    }
    if let Some(n) = &plan.output.name {
        if n.trim().is_empty() {
            push(&mut p, "output.name: is empty".into());
        }
        if chars(n) > MAX_NAME_CHARS {
            push(&mut p, format!("output.name: longer than {MAX_NAME_CHARS} characters"));
        }
    }
    let c = &plan.cuts;
    let cl = &plan.cleanup;
    let asked = c
        .remove_words
        .len()
        .saturating_add(c.remove_ranges.len())
        .saturating_add(c.keep_only.as_ref().map_or(0, Vec::len))
        .saturating_add(cl.silences.len())
        .saturating_add(cl.untranscribed.len());
    if asked > MAX_REMOVALS {
        push(&mut p, format!("the plan lists {asked} cuts, silences and spans; the limit is {MAX_REMOVALS}"));
        // don't walk a huge plan entry by entry
        return p;
    }
    let word =
        |field: String, i: usize| -> Option<String> { (i >= words).then(|| format!("{field}: word {i} is out of range (the transcript has {words} words)")) };
    let text = |field: String, s: &str, max: usize| -> Option<String> { (chars(s) > max).then(|| format!("{field}: longer than {max} characters")) };
    for (k, w) in c.remove_words.iter().enumerate() {
        for m in [word(format!("cuts.removeWords[{k}].from"), w.from), w.to.and_then(|t| word(format!("cuts.removeWords[{k}].to"), t))].into_iter().flatten() {
            push(&mut p, m);
        }
        if let Some(m) = text(format!("cuts.removeWords[{k}].reason"), &w.reason, MAX_TEXT_CHARS) {
            push(&mut p, m);
        }
    }
    let span = |field: String, a: f64, b: f64| -> Option<String> {
        if !a.is_finite() || !b.is_finite() {
            Some(format!("{field}: startS and endS must be finite numbers"))
        } else if a > b {
            Some(format!("{field}: startS {a} is after endS {b}"))
        } else {
            None
        }
    };
    for (k, r) in c.remove_ranges.iter().enumerate() {
        if let Some(m) = span(format!("cuts.removeRanges[{k}]"), r.start_s, r.end_s) {
            push(&mut p, m);
        }
        if let Some(m) = text(format!("cuts.removeRanges[{k}].reason"), &r.reason, MAX_TEXT_CHARS) {
            push(&mut p, m);
        }
    }
    if let Some(keep) = &c.keep_only {
        if keep.is_empty() {
            push(&mut p, "cuts.keepOnly: is empty (it would remove everything)".into());
        }
        if words == 0 {
            push(&mut p, "cuts.keepOnly: the sequence has no transcript".into());
        }
        for (k, w) in keep.iter().enumerate() {
            for m in [word(format!("cuts.keepOnly[{k}].from"), w.from), w.to.and_then(|t| word(format!("cuts.keepOnly[{k}].to"), t))].into_iter().flatten() {
                push(&mut p, m);
            }
        }
    }
    if let Some(f) = &cl.fillers {
        if f.len() > MAX_FILLERS {
            push(&mut p, format!("cleanup.fillers: more than {MAX_FILLERS} entries"));
        }
        for (k, s) in f.iter().enumerate().take(MAX_FILLERS) {
            if let Some(m) = text(format!("cleanup.fillers[{k}]"), s, 100) {
                push(&mut p, m);
            }
        }
    }
    if let Some(r) = &cl.pauses {
        for (name, v) in [("minS", r.min_s), ("keepS", r.keep_s)] {
            if !v.is_finite() || !(0.0..=MAX_PAUSE_S).contains(&v) {
                push(&mut p, format!("cleanup.pauses.{name}: {v} must be between 0 and {MAX_PAUSE_S} seconds"));
            }
        }
    }
    for (k, s) in cl.silences.iter().enumerate() {
        if let Some(m) = span(format!("cleanup.silences[{k}]"), s.start_s, s.end_s) {
            push(&mut p, m);
        }
    }
    for (k, s) in cl.untranscribed.iter().enumerate() {
        if let Some(m) = span(format!("cleanup.untranscribed[{k}]"), s.start_s, s.end_s) {
            push(&mut p, m);
        }
    }
    if let Some(cap) = &plan.captions {
        if cap.max_chars.is_some_and(|n| !(1..=500).contains(&n)) {
            push(&mut p, "captions.maxChars: must be between 1 and 500".into());
        }
        if cap.lines.is_some_and(|n| !(1..=4).contains(&n)) {
            push(&mut p, "captions.lines: must be between 1 and 4".into());
        }
        if let Some(m) = cap.template.as_deref().and_then(|t| text("captions.template".into(), t, MAX_NAME_CHARS)) {
            push(&mut p, m);
        }
    }
    if let Some(g) = &plan.grade {
        if g.lut_strength.is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v)) {
            push(&mut p, "grade.lutStrength: must be between 0 and 1".into());
        }
        for (f, v) in [("grade.lut", &g.lut), ("grade.preset", &g.preset)] {
            if let Some(m) = v.as_deref().and_then(|t| text(f.into(), t, MAX_TEXT_CHARS)) {
                push(&mut p, m);
            }
        }
    }
    if let Some(a) = &plan.audio
        && a.target_lufs.is_some_and(|v| !v.is_finite() || !(-70.0..=0.0).contains(&v))
    {
        push(&mut p, "audio.targetLufs: must be between -70 and 0".into());
    }
    if plan.markers.len() > MAX_MARKERS {
        push(&mut p, format!("markers: more than {MAX_MARKERS}"));
    } else {
        for (k, m) in plan.markers.iter().enumerate() {
            match (m.time_s, m.word) {
                (Some(t), None) if !t.is_finite() => push(&mut p, format!("markers[{k}].timeS: must be a finite number")),
                (Some(_), None) => {}
                (None, Some(w)) => {
                    if let Some(e) = word(format!("markers[{k}].word"), w) {
                        push(&mut p, e);
                    }
                }
                _ => push(&mut p, format!("markers[{k}]: give exactly one of timeS and word")),
            }
            if m.name.trim().is_empty() {
                push(&mut p, format!("markers[{k}].name: is empty"));
            }
            if let Some(e) = text(format!("markers[{k}].name"), &m.name, MAX_NAME_CHARS) {
                push(&mut p, e);
            }
        }
    }
    if plan.target_duration_s.is_some_and(|t| !t.is_finite() || t <= 0.0) {
        push(&mut p, "targetDurationS: must be a positive number".into());
    }
    if let Some(e) = &plan.export {
        for (f, v) in [("export.preset", &e.preset), ("export.path", &e.path)] {
            if let Some(m) = v.as_deref().and_then(|t| text(f.into(), t, MAX_TEXT_CHARS)) {
                push(&mut p, m);
            }
        }
    }
    p
}

// ---------------------------------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------------------------------

/// Warnings, capped at [`MAX_WARNINGS`] (the rest are counted).
struct Warnings {
    list: Vec<String>,
    dropped: usize,
}

impl Warnings {
    fn push(&mut self, m: String) {
        if self.list.len() < MAX_WARNINGS {
            self.list.push(m);
        } else {
            self.dropped += 1;
        }
    }
    fn finish(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.list.push(format!("… and {} more warnings", self.dropped));
        }
        self.list
    }
}

fn secs(t: Tick) -> f64 {
    t.0 as f64 / TICKS_PER_SECOND as f64
}

/// `x` seconds clamped to `[0, dur]` (`x` is finite: checked by [`validate`]).
fn clamp_secs(x: f64, dur: Tick) -> Tick {
    let d = secs(dur);
    let x = if x.is_finite() { x.max(0.0).min(d) } else { 0.0 };
    Tick::from_seconds_f64(x).clamp(Tick::ZERO, dur)
}

/// Words sorted by start, with a running maximum of their ends, for overlap queries.
struct WordIndex {
    /// (start, end, word index) by start; zero-length words are left out (nothing is inside them).
    w: Vec<(Tick, Tick, usize)>,
    max_end: Vec<Tick>,
}

impl WordIndex {
    fn new(words: &[SeqWord]) -> Self {
        let mut w: Vec<(Tick, Tick, usize)> = words.iter().enumerate().filter(|(_, x)| x.end > x.start).map(|(i, x)| (x.start, x.end, i)).collect();
        w.sort_unstable();
        let mut max_end = Vec::with_capacity(w.len());
        let mut m = Tick::MIN;
        for x in &w {
            m = m.max(x.1);
            max_end.push(m);
        }
        Self { w, max_end }
    }

    /// Words overlapping the open interval `(a, b)`: `start < b && end > a`.
    fn overlapping(&self, a: Tick, b: Tick) -> impl Iterator<Item = &(Tick, Tick, usize)> {
        let hi = self.w.partition_point(|x| x.0 < b);
        let lo = self.max_end.partition_point(|e| *e <= a).min(hi);
        self.w.get(lo..hi).unwrap_or_default().iter().filter(move |x| x.1 > a)
    }

    /// Words strictly containing `t`.
    fn containing(&self, t: Tick) -> impl Iterator<Item = &(Tick, Tick, usize)> {
        self.overlapping(t, t).filter(move |x| x.0 < t && t < x.1)
    }
}

/// How a piece reacts to a kept word it cuts into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fix {
    /// Grow to take the whole word (`cuts.removeRanges`).
    Expand,
    /// Leave the word whole (everything else).
    Carve,
}

#[derive(Clone, Debug)]
struct Piece {
    s: Tick,
    e: Tick,
    reason: String,
    kind: RemovalKind,
}

fn is_kept(kept: &[bool], i: usize) -> bool {
    kept.get(i).copied().unwrap_or(false)
}

/// Compile `plan` against `seq` (whose sequence transcript is `words`, as
/// [`tx::sequence_words`] gives it) at frame rate `rate`.
pub fn compile(plan: &EditPlan, seq: &Sequence, words: &[SeqWord], rate: FrameRate) -> Result<CompiledPlan, PlanError> {
    let problems = validate(plan, words.len());
    if !problems.is_empty() {
        return Err(PlanError::Invalid(problems));
    }
    let rate = rate.sane();
    let fd = rate.frame_duration();
    let dur = seq.duration().clamp(Tick::ZERO, Tick::MAX);
    let n = words.len();
    let idx = WordIndex::new(words);
    let mut warn = Warnings { list: Vec::new(), dropped: 0 };
    // words the plan removes by index (they may be cut into freely)
    let mut removed = vec![false; n];
    let mut pieces: Vec<(Piece, Fix)> = Vec::new();
    let word_text = |a: usize, b: usize| -> String {
        let t: Vec<&str> = words.get(a..=b).unwrap_or_default().iter().take(8).map(|w| w.text.as_str()).collect();
        let more = if b.saturating_sub(a) >= 8 { " …" } else { "" };
        format!("{}{more}", t.join(" "))
    };
    let add_words = |pieces: &mut Vec<(Piece, Fix)>, removed: &mut [bool], a: usize, b: usize, r: Option<TimeRange>, reason: String, kind: RemovalKind| {
        let (Some(wa), Some(wb)) = (words.get(a), words.get(b)) else { return };
        // the word range snapped to frames, and never less than the words themselves
        let (mut s, mut e) = (wa.start, wb.end.max(wa.start));
        if let Some(r) = r {
            s = s.min(r.start);
            e = e.max(r.end());
        }
        for x in removed.get_mut(a..=b).unwrap_or_default() {
            *x = true;
        }
        pieces.push((Piece { s: s.clamp(Tick::ZERO, dur), e: e.clamp(Tick::ZERO, dur), reason, kind }, Fix::Carve));
    };

    // ---- word cuts ----
    for c in &plan.cuts.remove_words {
        let to = c.to.unwrap_or(c.from);
        let (a, b) = (c.from.min(to), c.from.max(to));
        let r = tx::word_range(words, a, b, rate);
        add_words(&mut pieces, &mut removed, a, b, r, c.reason.clone(), RemovalKind::Word);
    }
    // ---- fillers ----
    if let Some(f) = &plan.cleanup.fillers {
        let list: Vec<String> = if f.is_empty() { tx::DEFAULT_FILLERS.iter().map(|s| s.to_string()).collect() } else { f.clone() };
        for h in tx::find_fillers(words, &list) {
            if h.is_empty() || h.end > n {
                continue;
            }
            let (a, b) = (h.start, h.end - 1);
            let r = tx::filler_ranges(words, std::slice::from_ref(&h), rate).into_iter().next();
            let reason = format!("filler \u{201c}{}\u{201d}", word_text(a, b));
            add_words(&mut pieces, &mut removed, a, b, r, reason, RemovalKind::Filler);
        }
    }
    // ---- keep only ----
    if let Some(keep) = &plan.cuts.keep_only {
        // difference array over word indices: O(words + spans)
        let mut diff = vec![0i64; n + 1];
        for k in keep {
            let to = k.to.unwrap_or(k.from);
            let (a, b) = (k.from.min(to), k.from.max(to));
            if b >= n {
                continue;
            }
            if let Some(x) = diff.get_mut(a) {
                *x += 1;
            }
            if let Some(x) = diff.get_mut(b + 1) {
                *x -= 1;
            }
        }
        let mut level = 0i64;
        let keep_mask: Vec<bool> = diff
            .iter()
            .take(n)
            .map(|d| {
                level += d;
                level > 0
            })
            .collect();
        // each run of words outside the kept spans is removed together with the pauses around it
        let mut i = 0;
        while i < n {
            if keep_mask.get(i).copied().unwrap_or(true) {
                i += 1;
                continue;
            }
            let a = i;
            while i < n && !keep_mask.get(i).copied().unwrap_or(true) {
                i += 1;
            }
            let b = i - 1;
            let s = if a == 0 { Tick::ZERO } else { words.get(a - 1).map_or(Tick::ZERO, |w| w.end) };
            let e = words.get(i).map_or(dur, |w| w.start);
            let reason = format!("not in keepOnly: \u{201c}{}\u{201d}", word_text(a, b));
            let r = (e > s).then(|| TimeRange::from_bounds(s, e));
            add_words(&mut pieces, &mut removed, a, b, r, reason, RemovalKind::Word);
        }
    }
    // ---- seconds-based ranges: boundaries inside a word move outward to take it whole ----
    for (k, c) in plan.cuts.remove_ranges.iter().enumerate() {
        let (mut s, mut e) = (clamp_secs(c.start_s, dur), clamp_secs(c.end_s, dur));
        if e <= s {
            warn.push(format!("cuts.removeRanges[{k}] ({:.2}–{:.2} s) is empty inside the sequence; skipped", c.start_s, c.end_s));
            continue;
        }
        if let Some(w) = idx.containing(s).filter(|x| is_kept_by(&removed, x.2)).map(|x| x.0).min() {
            let w = w.max(Tick::ZERO);
            warn.push(format!("cuts.removeRanges[{k}] started inside a word at {:.2} s; moved to {:.2} s to cut the whole word", secs(s), secs(w)));
            s = w;
        }
        if let Some(w) = idx.containing(e).filter(|x| is_kept_by(&removed, x.2)).map(|x| x.1).max() {
            let w = w.min(dur);
            warn.push(format!("cuts.removeRanges[{k}] ended inside a word at {:.2} s; moved to {:.2} s to cut the whole word", secs(e), secs(w)));
            e = w;
        }
        for x in idx.overlapping(s, e) {
            if x.0 >= s
                && x.1 <= e
                && let Some(r) = removed.get_mut(x.2)
            {
                *r = true;
            }
        }
        pieces.push((Piece { s, e, reason: c.reason.clone(), kind: RemovalKind::Range }, Fix::Expand));
    }
    let kept: Vec<bool> = removed.iter().map(|r| !r).collect();
    // ---- pauses, silences, untranscribed sounds ----
    if let Some(r) = &plan.cleanup.pauses {
        let (min, keep) = (Tick::from_seconds_f64(r.min_s), Tick::from_seconds_f64(r.keep_s));
        for x in tx::find_pauses(words, min, keep, rate) {
            let reason = format!("pause of {:.2} s", secs(x.duration) + 2.0 * r.keep_s);
            pieces.push((Piece { s: x.start.clamp(Tick::ZERO, dur), e: x.end().clamp(Tick::ZERO, dur), reason, kind: RemovalKind::Pause }, Fix::Carve));
        }
    }
    for (list, kind, what) in
        [(&plan.cleanup.silences, RemovalKind::Silence, "silence"), (&plan.cleanup.untranscribed, RemovalKind::Untranscribed, "untranscribed sound")]
    {
        for sp in list {
            let (s, e) = (clamp_secs(sp.start_s, dur), clamp_secs(sp.end_s, dur));
            if e > s {
                pieces.push((Piece { s, e, reason: what.to_string(), kind }, Fix::Carve));
            }
        }
    }
    if pieces.len() > MAX_RAW_PIECES {
        return Err(PlanError::Invalid(vec![format!("the plan makes {} cuts before merging; the limit is {MAX_RAW_PIECES}", pieces.len())]));
    }

    // ---- carve kept words out of every piece ----
    let mut carved: Vec<Piece> = Vec::with_capacity(pieces.len());
    for (p, fix) in pieces {
        if p.e <= p.s {
            continue;
        }
        let mut cur = p.s;
        let mut changed = false;
        let mut hits: Vec<(Tick, Tick)> = idx.overlapping(p.s, p.e).filter(|x| is_kept(&kept, x.2)).map(|x| (x.0, x.1)).collect();
        hits.sort_unstable();
        for (ws, we) in hits {
            changed = true;
            if ws > cur {
                carved.push(Piece { s: cur, e: ws, ..p.clone() });
            }
            cur = cur.max(we);
        }
        if p.e > cur {
            carved.push(Piece { s: cur, e: p.e, ..p.clone() });
        }
        if changed && (fix == Fix::Expand || matches!(p.kind, RemovalKind::Silence | RemovalKind::Untranscribed)) {
            warn.push(format!(
                "{} at {:.2}–{:.2} s cut into a kept word; its boundary was moved to the gap between words",
                p.kind.name(),
                secs(p.s),
                secs(p.e)
            ));
        }
    }

    // ---- frame-align boundaries where that keeps kept words whole ----
    let kept_in = |a: Tick, b: Tick| idx.overlapping(a, b).any(|x| is_kept(&kept, x.2));
    let inside_kept = |t: Tick| idx.containing(t).any(|x| is_kept(&kept, x.2));
    for p in &mut carved {
        let down = rate.snap(p.s);
        if down < p.s {
            let up = down + fd;
            if !kept_in(down, p.s) {
                p.s = down;
            } else if up < p.e && !inside_kept(up) {
                p.s = up;
            }
        }
        let down = rate.snap(p.e);
        if down < p.e {
            let up = (down + fd).min(dur);
            if up > p.e && !kept_in(p.e, up) {
                p.e = up;
            } else if down > p.s && !inside_kept(down) {
                p.e = down;
            }
        }
    }

    // ---- merge ----
    let mut merged = merge(carved);

    // ---- drop short kept fragments between cuts ----
    let min_frag = Tick::from_seconds_f64(MIN_FRAGMENT_S);
    let mut out: Vec<Piece> = Vec::with_capacity(merged.len());
    for p in merged.drain(..) {
        if let Some(last) = out.last_mut() {
            let gap = p.s - last.e;
            if gap > Tick::ZERO && gap < min_frag && !kept_in(last.e, p.s) {
                warn.push(format!("removed a {:.2} s fragment at {:.2} s left between two cuts", secs(gap), secs(last.e)));
                last.e = p.e;
                if !last.reason.contains(&p.reason) {
                    join_reason(&mut last.reason, &p.reason);
                }
                continue;
            }
        }
        out.push(p);
    }

    // ---- final guard: no boundary strictly inside a word that is not wholly removed ----
    let out = settle(out, words, &idx)?;
    if out.len() > MAX_REMOVALS {
        return Err(PlanError::Invalid(vec![format!("the plan makes {} cuts; the limit is {MAX_REMOVALS}", out.len())]));
    }

    // ---- stats ----
    let removals: Vec<Removal> = out.into_iter().map(|p| Removal { range: TimeRange::from_bounds(p.s, p.e), reason: p.reason, kind: p.kind }).collect();
    let removed_total = removals.iter().fold(Tick::ZERO, |a, r| a + r.range.duration);
    let after = (dur - removed_total).max(Tick::ZERO);
    let segments = kept_segments(&removals, dur);
    if let Some(t) = plan.target_duration_s {
        let a = secs(after);
        let miss = (a - t) / t * 100.0;
        if miss.abs() > 5.0 {
            warn.push(format!("the edit lasts {a:.1} s; the target is {t:.1} s ({miss:+.1} %)"));
        }
    }
    let markers = plan
        .markers
        .iter()
        .filter_map(|m| {
            let source = match (m.time_s, m.word) {
                (Some(t), _) => clamp_secs(t, dur),
                (None, Some(w)) => words.get(w)?.start.clamp(Tick::ZERO, dur),
                (None, None) => return None,
            };
            Some(CompiledMarker { source, at: map_time(&removals, source), name: m.name.trim().to_string() })
        })
        .collect();
    Ok(CompiledPlan { removals, warnings: warn.finish(), before: dur, after, segments, markers })
}

fn is_kept_by(removed: &[bool], i: usize) -> bool {
    !removed.get(i).copied().unwrap_or(true)
}

fn join_reason(into: &mut String, more: &str) {
    if into.split("; ").any(|r| r == more) || more.is_empty() {
        return;
    }
    if into.split("; ").count() >= 3 {
        if !into.ends_with('…') {
            into.push_str("; …");
        }
        return;
    }
    if !into.is_empty() {
        into.push_str("; ");
    }
    into.push_str(more);
}

/// Sort and merge overlapping or touching pieces (reasons joined, the first piece's kind kept).
fn merge(mut v: Vec<Piece>) -> Vec<Piece> {
    v.retain(|p| p.e > p.s);
    v.sort_by_key(|p| (p.s, p.e));
    let mut out: Vec<Piece> = Vec::with_capacity(v.len());
    for p in v {
        match out.last_mut() {
            Some(l) if p.s <= l.e => {
                l.e = l.e.max(p.e);
                let r = p.reason;
                join_reason(&mut l.reason, &r);
            }
            _ => out.push(p),
        }
    }
    out
}

/// Whether `[s, e]` lies inside one of the sorted, disjoint `pieces`.
fn covered(pieces: &[Piece], s: Tick, e: Tick) -> bool {
    let i = pieces.partition_point(|p| p.s <= s);
    i.checked_sub(1).and_then(|i| pieces.get(i)).is_some_and(|p| p.e >= e)
}

/// Shrink pieces until no boundary is strictly inside a word they don't wholly remove (only
/// overlapping transcript words can need more than one pass; bounded).
fn settle(mut v: Vec<Piece>, words: &[SeqWord], idx: &WordIndex) -> Result<Vec<Piece>, PlanError> {
    for _ in 0..64 {
        let kept: Vec<bool> = words.iter().map(|w| !covered(&v, w.start, w.end.max(w.start))).collect();
        let mut changed = false;
        for p in &mut v {
            if let Some(e) = idx.containing(p.s).filter(|x| is_kept(&kept, x.2)).map(|x| x.1).max() {
                p.s = e;
                changed = true;
            }
            if let Some(s) = idx.containing(p.e).filter(|x| is_kept(&kept, x.2)).map(|x| x.0).min() {
                p.e = s;
                changed = true;
            }
        }
        if !changed {
            return Ok(v);
        }
        v = merge(v);
    }
    Err(PlanError::Invalid(vec!["the cuts could not be placed without clipping a word (the transcript has overlapping words)".into()]))
}

/// Pieces of `[0, dur)` left once `removals` (sorted, disjoint) are gone.
fn kept_segments(removals: &[Removal], dur: Tick) -> usize {
    let mut n = 0;
    let mut t = Tick::ZERO;
    for r in removals {
        if r.range.start > t {
            n += 1;
        }
        t = t.max(r.range.end());
    }
    if dur > t {
        n += 1;
    }
    n
}

/// Where source time `t` lands once `removals` (sorted, disjoint) are ripple-deleted (a time inside
/// a removal lands on the cut).
pub fn map_time(removals: &[Removal], t: Tick) -> Tick {
    let mut shift = Tick::ZERO;
    for r in removals {
        if r.range.start >= t {
            break;
        }
        shift += r.range.end().min(t) - r.range.start;
    }
    t - shift
}

/// The words left once `removals` (sorted, disjoint) are ripple-deleted, at their new times: a word
/// whose midpoint falls inside a removal is gone.
pub fn words_after(words: &[SeqWord], removals: &[Removal]) -> Vec<SeqWord> {
    let mut out = Vec::new();
    for w in words {
        let mid = Tick(w.start.0 + (w.end.0 - w.start.0) / 2);
        let k = removals.partition_point(|r| r.range.start <= mid);
        let gone = k.checked_sub(1).and_then(|k| removals.get(k)).is_some_and(|r| r.range.contains(mid));
        if gone {
            continue;
        }
        let mut w = w.clone();
        let (s, e) = (map_time(removals, w.start), map_time(removals, w.end));
        w.start = s;
        w.end = e.max(s);
        out.push(w);
    }
    out
}

/// Text of the words whose midpoint is inside `r` (at most `max` characters).
pub fn text_in(words: &[SeqWord], r: TimeRange, max: usize) -> String {
    let mut out = String::new();
    for w in words {
        let mid = Tick(w.start.0 + (w.end.0 - w.start.0) / 2);
        if !r.contains(mid) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.text);
        if out.chars().count() > max {
            let cut: String = out.chars().take(max).collect();
            return format!("{cut}…");
        }
    }
    out
}

// (not `plan/tests.rs`: the repository ignores directories named `plan`)
#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
