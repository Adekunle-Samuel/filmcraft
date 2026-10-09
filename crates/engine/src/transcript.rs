//! Text-based editing commands (`transcript.*`): the Text panel ▸ Transcript tab.
//!
//! Transcripts belong to media items (`Project::transcripts`, media time); the sequence transcript
//! is derived from them ([`filmcraft_edit::transcript::sequence_words`]). Words of the sequence
//! transcript are addressed by index (`from`, `to`, inclusive), as `transcript.inspect` lists them.
//!
//! Speech recognition goes through a [`Transcriber`]: [`Session::transcriber`] when a host or a
//! test installed one, else the Whisper model named by `model` from `<data dir>/models` (needs the
//! engine feature `whisper`; without it `transcript.generate` fails with a clear error, and agents
//! can still bring their own transcript with `transcript.set`).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use filmcraft_edit as edit;
use filmcraft_edit::transcript::{self as tx, CaptionRules, SeqWord};
use filmcraft_project::{CaptionFormat, CaptionTrack, ItemId, ItemKind, TrackId, Transcript};
use filmcraft_speech::{Options, SpeechError, Transcriber};
use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

/// Where downloaded speech models live (`<data dir>/models`).
pub fn models_dir() -> Option<std::path::PathBuf> {
    crate::autosave::default_data_dir().map(|d| d.join("models"))
}

/// Whether this build can transcribe with Whisper (feature `whisper`).
pub fn speech_available() -> bool {
    filmcraft_speech::available()
}

/// The words of the active sequence's transcript.
pub fn sequence_words(s: &Session) -> Vec<SeqWord> {
    match s.active_sequence() {
        Some(q) => tx::sequence_words(q, &s.project.transcripts),
        None => Vec::new(),
    }
}

fn has_transcript(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if sequence_words(s).is_empty() { Err("the sequence has no transcript (Transcribe first)".into()) } else { Ok(()) }
}

fn has_transcripts(s: &Session) -> std::result::Result<(), String> {
    if s.project.transcripts.is_empty() { Err("there are no transcripts".into()) } else { Ok(()) }
}

/// Why speech-to-text can't run in this build (no installed transcriber, built without `whisper`).
pub(crate) const NO_SPEECH: &str =
    "speech-to-text is not available in this build (built without the `whisper` feature); import a transcript with transcript.set instead";

/// `transcript.generate` can run: a host installed a transcriber or the build has speech-to-text
/// (#97: it reported enabled and then always failed).
pub(crate) fn can_transcribe(s: &Session) -> std::result::Result<(), String> {
    if s.transcriber.is_some() || speech_available() { Ok(()) } else { Err(NO_SPEECH.into()) }
}

/// `transcript.downloadModel` can run: built with `speech-download` (#98).
fn can_download(_: &Session) -> std::result::Result<(), String> {
    if cfg!(feature = "speech-download") {
        Ok(())
    } else {
        Err("model downloads are not available in this build (built without the `speech-download` feature)".into())
    }
}

/// The media item behind a project item (subclips resolve to their parent).
fn media_item(s: &Session, item: ItemId) -> Option<ItemId> {
    s.project.resolve_media(item).map(|(root, _, _)| root)
}

fn ids_p(p: &Value, k: &str) -> Option<Vec<ItemId>> {
    p.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect())
}

/// Items to transcribe: `items` / `item`, else the Project panel selection, else the media of the
/// active sequence's enabled audio clips. Subclips resolve to their media; duplicates are removed.
fn targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let mut raw = ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])).unwrap_or_default();
    if raw.is_empty() {
        raw = s.state.project_selection.clone();
    }
    if raw.is_empty()
        && let Some(q) = s.active_sequence()
    {
        raw = q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|it| it.enabled).map(|it| it.item).collect();
    }
    let mut out = Vec::new();
    for i in raw {
        if let Some(m) = media_item(s, i)
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

/// Longest media transcribed in one go (hours): the mono 16 kHz samples are held in memory.
const MAX_HOURS: usize = 4;
/// Samples read from a source at a time (60 s), so only the mono mix is held whole.
const READ_CHUNK: usize = 60 * filmcraft_speech::SAMPLE_RATE as usize;

/// One media item to transcribe: what the job thread reads.
struct Work {
    item: ItemId,
    name: String,
    src: filmcraft_media::SharedSource,
    /// Length of the media's audio in 16 kHz samples.
    len: usize,
    /// Only these spans of the media are transcribed (`regions`); None = all of it.
    spans: Option<Vec<Span>>,
}

/// Samples `start..start + len` (16 kHz) of a media item's audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

/// Most `regions` one call takes.
pub const MAX_REGIONS: usize = 10_000;
/// Air kept around every voiced region (seconds), so word edges aren't clipped.
pub const REGION_PAD_SECONDS: f64 = 0.3;

/// The 16 kHz sample spans to transcribe for `regions` (`[[start_s, end_s], …]`, media seconds) of
/// audio `len` samples long: each region padded by [`REGION_PAD_SECONDS`], clamped to the audio,
/// sorted and merged where they overlap or touch.
pub fn region_spans(regions: &Value, len: usize) -> std::result::Result<Vec<Span>, String> {
    let list = regions.as_array().ok_or("`regions` must be a list of [startSeconds, endSeconds] pairs")?;
    if list.len() > MAX_REGIONS {
        return Err(format!("at most {MAX_REGIONS} regions, got {}", list.len()));
    }
    let sr = filmcraft_speech::SAMPLE_RATE as f64;
    let end = len as f64;
    let mut raw: Vec<(usize, usize)> = Vec::with_capacity(list.len());
    for (i, r) in list.iter().enumerate() {
        let pair = r.as_array().map(Vec::as_slice);
        let Some([a, b]) = pair else { return Err(format!("region {i} must be [startSeconds, endSeconds]")) };
        let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) else { return Err(format!("region {i} must hold two numbers")) };
        if !a.is_finite() || !b.is_finite() {
            return Err(format!("region {i} is not a finite time"));
        }
        if b < a {
            return Err(format!("region {i} ends before it starts"));
        }
        // finite inputs: the products may overflow to infinity, which `min` brings back in range
        let a = ((a - REGION_PAD_SECONDS).max(0.0) * sr).floor().min(end);
        let b = ((b + REGION_PAD_SECONDS).max(0.0) * sr).ceil().min(end);
        let (a, b) = (a as usize, b as usize);
        if b > a {
            raw.push((a, b));
        }
    }
    raw.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(raw.len());
    for (a, b) in raw {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    if merged.is_empty() {
        return Err("the regions lie outside the media's audio".into());
    }
    Ok(merged.into_iter().map(|(a, b)| Span { start: a, len: b - a }).collect())
}

/// Move words transcribed from the spans' audio laid end to end back to media time. A word
/// belongs to the span its start falls in, and its end is cut at that span's end, so every word
/// stays inside its source region; words past the audio are dropped.
pub fn remap_words(words: &mut Vec<filmcraft_project::Word>, spans: &[Span]) {
    let per = filmcraft_speech::TICKS_PER_SAMPLE;
    let as_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    // (start in the joined audio, end in the joined audio, shift to media time), in ticks
    let mut table = Vec::with_capacity(spans.len());
    let mut at = 0i64;
    for sp in spans {
        let a = at;
        at = at.saturating_add(as_i64(sp.len).saturating_mul(per));
        table.push((a, at, as_i64(sp.start).saturating_mul(per).saturating_sub(a)));
    }
    words.retain_mut(|w| {
        let start = w.start.0.max(0);
        let k = table.partition_point(|e| e.1 <= start);
        let Some(&(a, b, shift)) = table.get(k) else { return false };
        let s0 = start.max(a);
        let e0 = w.end.0.clamp(s0, b);
        w.start = Tick(s0.saturating_add(shift));
        w.end = Tick(e0.saturating_add(shift));
        true
    });
}

/// The media item `item` if it has audio to transcribe (None: not media, no audio, offline).
fn audio_work(s: &Session, item: ItemId) -> Option<Work> {
    let it = s.project.item(item)?;
    let dur = match &it.kind {
        ItemKind::Media(m) => m.duration(),
        _ => return None,
    };
    let src = s.source(item)?;
    if !src.info().has_audio() {
        return None;
    }
    let len = usize::try_from(dur.to_units_floor(filmcraft_speech::SAMPLE_RATE as i64).max(0)).ok()?;
    Some(Work { item, name: it.name.clone(), src, len, spans: None })
}

/// The mono 16 kHz audio a job transcribes for `w`: all of it, or its spans laid end to end.
fn work_audio(w: &Work, cancelled: &dyn Fn() -> bool) -> std::result::Result<Vec<f32>, String> {
    let Some(spans) = &w.spans else { return read_mono(&w.src, 0, w.len, cancelled) };
    let total = spans.iter().fold(0usize, |n, s| n.saturating_add(s.len));
    if total > MAX_HOURS * 3600 * filmcraft_speech::SAMPLE_RATE as usize {
        return Err(format!("the regions add up to more than {MAX_HOURS} hours; transcribe them in parts"));
    }
    let mut out = Vec::with_capacity(total);
    for sp in spans {
        // each span reads exactly `len` samples (zeros past the end), so the layout matches
        // `remap_words`; a cancelled read stops the job anyway
        out.extend(read_mono(&w.src, sp.start, sp.len, cancelled)?);
    }
    Ok(out)
}

/// Mono 16 kHz samples `start..start + len` of a source, read a chunk at a time. Stops early
/// (Ok) when `cancelled` says so.
fn read_mono(src: &filmcraft_media::SharedSource, start: usize, len: usize, cancelled: &dyn Fn() -> bool) -> std::result::Result<Vec<f32>, String> {
    let max = MAX_HOURS * 3600 * filmcraft_speech::SAMPLE_RATE as usize;
    if len > max {
        return Err(format!("the audio is longer than {MAX_HOURS} hours; transcribe it in parts"));
    }
    let mut out = Vec::with_capacity(len);
    let mut at = 0usize;
    while at < len && !cancelled() {
        let n = READ_CHUNK.min(len - at);
        let first = i64::try_from(start.saturating_add(at)).map_err(|_| "audio position out of range".to_string())?;
        let buf = src.audio(first, n, filmcraft_speech::SAMPLE_RATE).map_err(|e| format!("can't read the audio: {e}"))?;
        let mut mono = filmcraft_speech::downmix(&buf.channels);
        mono.resize(n, 0.0);
        out.extend_from_slice(&mono);
        at += n;
    }
    Ok(out)
}

fn speech_err(e: SpeechError) -> EngineError {
    EngineError::Other(e.to_string())
}

/// The transcriber to use: the installed one, else the named catalogue model.
fn transcriber(s: &Session, p: &Value) -> Result<Arc<dyn Transcriber>> {
    if let Some(t) = &s.transcriber {
        return Ok(t.clone());
    }
    // Settings ▸ Media Analysis & Transcription ▸ Speech model
    let model = str_p(p, "model").unwrap_or(&s.prefs.media_analysis.whisper_model);
    if filmcraft_speech::models::find(model).is_none() {
        return Err(speech_err(SpeechError::UnknownModel(model.into())));
    }
    if !filmcraft_speech::available() {
        return Err(EngineError::Other(NO_SPEECH.into()));
    }
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    filmcraft_speech::load(&dir, model).map_err(speech_err)
}

/// Longest `prompt` kept (characters); the recogniser keeps only its last 223 tokens anyway.
const MAX_PROMPT_CHARS: usize = 2000;

/// The recogniser's initial prompt: `prompt` when given, else [`filmcraft_speech::FILLER_PROMPT`]
/// with `keepFillers: true` (so "um" and "uh" are written out), else none.
fn initial_prompt(p: &Value) -> Result<Option<String>> {
    match p.get("prompt").filter(|v| !v.is_null()) {
        Some(Value::String(t)) => {
            let t: String = t.trim().chars().take(MAX_PROMPT_CHARS).collect();
            if !t.is_empty() {
                return Ok(Some(t));
            }
        }
        Some(_) => return Err(bad("transcript.generate", "`prompt` must be text")),
        None => {}
    }
    Ok(bool_p(p, "keepFillers").unwrap_or(false).then(|| filmcraft_speech::FILLER_PROMPT.to_string()))
}

/// Progress units per transcribed item (`jobs.list` shows `done` / `total`).
const ITEM_UNITS: u64 = 1000;

/// A running `transcript.generate` job; [`poll`] stores its transcripts when it finishes.
pub struct PendingTranscripts {
    pub job: u64,
    /// The media items being transcribed.
    pub items: Vec<ItemId>,
    pub results: Arc<Mutex<Option<Vec<(ItemId, Transcript)>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `transcript.generate`: transcribe in a background job (`{job, items, skipped}`; `jobs.list`
/// shows the progress, `jobs.cancel` stops it without changing anything); the transcripts are
/// stored in one undo step ("Transcribe") when it finishes. With `wait: true` (and on the web) it
/// runs to the end first and reports the transcripts (`items: [{item, words, …}]`).
fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let items = targets(s, p);
    if items.is_empty() {
        return Err(bad("transcript.generate", "nothing to transcribe (pass `items`, select clips, or open a sequence with audio)"));
    }
    if let Some(busy) = s.transcript_jobs.iter().flat_map(|j| j.items.iter()).find(|i| items.contains(i)) {
        let name = s.project.item(*busy).map(|i| i.name.clone()).unwrap_or_default();
        return Err(EngineError::Other(format!("\u{201c}{name}\u{201d} is already being transcribed")));
    }
    let t = transcriber(s, p)?;
    // Settings ▸ Media Analysis & Transcription: language (or auto-detect) and speaker labelling
    let ma = &s.prefs.media_analysis;
    let default_language = if ma.language_auto_detect { None } else { Some(ma.default_language.clone()) };
    let opts = Options {
        language: match str_p(p, "language") {
            Some(l) => Some(l).filter(|l| !l.is_empty() && *l != "auto").map(str::to_string),
            None => default_language,
        },
        diarize: bool_p(p, "diarize").unwrap_or(ma.speaker_labeling != "off"),
        max_speakers: u64_p(p, "maxSpeakers").map(|n| n.clamp(1, 32) as usize).unwrap_or(Options::default().max_speakers),
        initial_prompt: initial_prompt(p)?,
    };
    let regions = p.get("regions").filter(|v| !v.is_null());
    if regions.is_some() && items.len() != 1 {
        return Err(bad("transcript.generate", "`regions` are media times of one item: pass exactly one item"));
    }
    let mut work = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        match audio_work(s, item) {
            Some(w) => work.push(w),
            None => skipped.push(item.0),
        }
    }
    if work.is_empty() {
        return Err(EngineError::Other("none of the clips has audio to transcribe".into()));
    }
    if let Some(r) = regions {
        for w in &mut work {
            w.spans = Some(region_spans(r, w.len).map_err(|e| bad("transcript.generate", e))?);
        }
    }
    let n = work.len();
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let label = format!("Transcribe ({n} clip{})", if n == 1 { "" } else { "s" });
    let job = crate::Job { id, label, progress: Default::default(), result: Default::default() };
    job.progress.total.store(ITEM_UNITS.saturating_mul(n as u64), std::sync::atomic::Ordering::Relaxed);
    let results: Arc<Mutex<Option<Vec<(ItemId, Transcript)>>>> = Arc::default();
    let pending = PendingTranscripts { job: id, items: work.iter().map(|w| w.item).collect(), results: results.clone() };
    let (prog, res, out) = (job.progress.clone(), job.result.clone(), results.clone());
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        let cancelled = || prog.cancel.load(Ordering::Relaxed);
        let mut done: Vec<(ItemId, Transcript)> = Vec::new();
        let mut err = None;
        for (k, w) in work.iter().enumerate() {
            let base = ITEM_UNITS.saturating_mul(k as u64);
            prog.done.store(base, Ordering::Relaxed);
            *lock(&prog.status) = format!("{}: reading the audio", w.name);
            let audio = match work_audio(w, &cancelled) {
                Ok(a) => a,
                Err(e) => {
                    err = Some(format!("{}: {e}", w.name));
                    break;
                }
            };
            if cancelled() {
                err = Some("stopped".to_string());
                break;
            }
            // the recogniser's fraction maps onto this item's share of the job; it stops when
            // the job is cancelled
            let mut progress = |f: f32, msg: &str| {
                let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
                prog.done.store(base + (f64::from(f) * ITEM_UNITS as f64) as u64, Ordering::Relaxed);
                *lock(&prog.status) = format!("{}: {msg}", w.name);
                !cancelled()
            };
            match t.transcribe(&audio, &opts, &mut progress) {
                Ok(mut tr) => {
                    if let Some(spans) = &w.spans {
                        remap_words(&mut tr.words, spans);
                    }
                    tr.normalize();
                    done.push((w.item, tr));
                }
                Err(SpeechError::Cancelled) => {
                    err = Some("stopped".to_string());
                    break;
                }
                Err(e) => {
                    err = Some(format!("{}: {e}", w.name));
                    break;
                }
            }
            prog.done.store(base + ITEM_UNITS, Ordering::Relaxed);
        }
        if err.is_none() && cancelled() {
            err = Some("stopped".to_string());
        }
        let secs = t0.elapsed().as_secs_f64();
        let words: usize = done.iter().map(|(_, t)| t.words.len()).sum();
        *lock(&prog.status) = match &err {
            Some(e) if e == "stopped" => "Stopped: nothing was changed".into(),
            Some(e) => e.clone(),
            None => format!("Transcribed {words} word(s) in {} clip(s) ({secs:.1}s)", done.len()),
        };
        let r = match err {
            Some(e) => Err(e),
            None => {
                *lock(&out) = Some(done);
                Ok(filmcraft_export::Report { path: String::new(), frames: words as u64, seconds: secs, bytes: 0, render_fps: 0.0, extra_files: Vec::new() })
            }
        };
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.transcript_jobs.push(pending);
    let wait = bool_p(p, "wait").unwrap_or(false);
    if !(wait || cfg!(target_arch = "wasm32")) {
        if let Err(e) = std::thread::Builder::new().name("filmcraft-transcribe".into()).spawn(run) {
            s.transcript_jobs.retain(|j| j.job != id);
            s.jobs.retain(|j| j.id != id);
            return Err(EngineError::Other(e.to_string()));
        }
        return Ok(json!({"job": id, "items": n, "skipped": skipped}));
    }
    run();
    let report: Vec<Value> = lock(&results)
        .as_ref()
        .map(|r| {
            r.iter()
                .map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "speakers": t.speakers.len(), "language": t.language, "source": t.source}))
                .collect()
        })
        .unwrap_or_default();
    poll(s);
    if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
        return Err(EngineError::Other(e));
    }
    Ok(json!({"job": id, "items": report, "skipped": skipped}))
}

/// Store the transcripts of finished `transcript.generate` jobs (one undo step each) and drop
/// finished or cancelled ones. Called once per UI frame from [`Session::poll_persistence`] and
/// after synchronous runs.
pub fn poll(s: &mut Session) {
    use std::sync::atomic::Ordering;
    let mut i = 0;
    while let Some(pj) = s.transcript_jobs.get(i) {
        let job = s.jobs.iter().find(|j| j.id == pj.job);
        let finished = job.is_none_or(crate::Job::is_finished);
        // cancelled before the results were stored: nothing changes
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.transcript_jobs.remove(i);
        let Some(done) = lock(&pj.results).take().filter(|_| !cancelled) else { continue };
        // media deleted while the job ran get no transcript
        let done: Vec<(ItemId, Transcript)> = done.into_iter().filter(|(i, _)| s.project.item(*i).is_some()).collect();
        if done.is_empty() {
            continue;
        }
        let r = s.edit("Transcribe", move |pr, _| {
            for (i, t) in done {
                pr.transcripts.insert(i, Arc::new(t));
            }
            Ok(())
        });
        if let Err(e) = r {
            s.error_toast("transcript.generate", format!("Transcribe: {e}"));
        }
    }
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let item = u64_p(p, "item").map(ItemId).ok_or_else(|| bad("transcript.set", "`item` is required"))?;
    let item = media_item(s, item).ok_or_else(|| bad("transcript.set", "no such media item"))?;
    let v = p.get("transcript").cloned().ok_or_else(|| bad("transcript.set", "`transcript` is required"))?;
    let mut t: Transcript = serde_json::from_value(v).map_err(|e| bad("transcript.set", e.to_string()))?;
    if t.source.is_empty() {
        t.source = "imported".into();
    }
    t.normalize();
    t.check().map_err(|e| bad("transcript.set", e))?;
    let n = t.words.len();
    s.edit("Set Transcript", move |pr, _| {
        pr.transcripts.insert(item, Arc::new(t));
        Ok(())
    })?;
    Ok(json!({"item": item.0, "words": n}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match ids_p(p, "items").or_else(|| u64_p(p, "item").map(|i| vec![ItemId(i)])) {
        Some(v) => v.into_iter().filter_map(|i| media_item(s, i)).collect(),
        None => s.project.transcripts.keys().copied().collect(),
    };
    let n = items.iter().filter(|i| s.project.transcripts.contains_key(i)).count();
    if n == 0 {
        return Err(EngineError::Other("no transcript to delete".into()));
    }
    s.edit("Delete Transcript", move |pr, _| {
        for i in items {
            pr.transcripts.remove(&i);
        }
        Ok(())
    })?;
    Ok(json!({"deleted": n}))
}

fn word_json(i: usize, w: &SeqWord) -> Value {
    json!({"i": i, "text": w.text, "start": w.start.0, "end": w.end.0, "speaker": w.speaker, "clip": w.clip.0, "item": w.item.0, "confidence": w.confidence})
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let gap = Tick::from_seconds_f64(f64_p(p, "paragraphGapSeconds").unwrap_or(1.5));
    let paras: Vec<Value> = tx::paragraphs(&words, gap)
        .into_iter()
        .map(|r| {
            let text = words[r.clone()].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            json!({"from": r.start, "to": r.end - 1, "speaker": words[r.start].speaker, "start": words[r.start].start.0, "end": words[r.end - 1].end.0, "text": text})
        })
        .collect();
    let speakers: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for w in &words {
            if let Some(n) = &w.speaker
                && !v.contains(n)
            {
                v.push(n.clone());
            }
        }
        v
    };
    let current = tx::word_at(&words, s.playhead());
    Ok(json!({
        "words": words.iter().enumerate().map(|(i, w)| word_json(i, w)).collect::<Vec<_>>(),
        "paragraphs": paras,
        "speakers": speakers,
        "current": current,
        "items": s.project.transcripts.iter().map(|(i, t)| json!({"item": i.0, "words": t.words.len(), "language": t.language, "source": t.source, "speakers": t.speakers.iter().map(|k| &k.name).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

fn search(s: &mut Session, p: &Value) -> Result<Value> {
    let q = str_p(p, "query").ok_or_else(|| bad("transcript.search", "`query` is required"))?;
    let words = sequence_words(s);
    let hits: Vec<Value> = tx::search(&words, q)
        .into_iter()
        .map(|r| json!({"from": r.start, "to": r.end - 1, "start": words[r.start].start.0, "end": words[r.end - 1].end.0}))
        .collect();
    Ok(json!({"matches": hits}))
}

/// Timeline range of the words `from..=to` (frame-snapped outward).
fn range_p(s: &Session, p: &Value, cmd: &str) -> Result<TimeRange> {
    let words = sequence_words(s);
    let from = u64_p(p, "from").ok_or_else(|| bad(cmd, "`from` (word index) is required"))? as usize;
    let to = u64_p(p, "to").map(|n| n as usize).unwrap_or(from);
    tx::word_range(&words, from, to, s.sequence_rate()).ok_or_else(|| bad(cmd, format!("word index out of range (the transcript has {} words)", words.len())))
}

fn range_json(r: TimeRange) -> Value {
    json!({"start": r.start.0, "end": r.end().0})
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let r = range_p(s, p, "transcript.select")?;
    let fd = s.sequence_rate().frame_duration();
    s.edit_sequence("Mark Transcript Selection", |q, _, _| {
        q.mark_in = Some(r.start);
        q.mark_out = Some(r.end() - fd);
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn extract_or_lift(s: &mut Session, p: &Value, extract: bool) -> Result<Value> {
    let cmd = if extract { "transcript.extract" } else { "transcript.lift" };
    let r = range_p(s, p, cmd)?;
    let tg = s.targeting().targeted;
    s.edit_sequence(if extract { "Extract Text" } else { "Lift Text" }, |q, ctx, _| {
        if extract {
            edit::extract(q, &tg, r, ctx);
        } else {
            edit::lift(q, &tg, r, ctx);
        }
        q.mark_in = None;
        q.mark_out = None;
        Ok(())
    })?;
    s.set_playhead(r.start);
    Ok(range_json(r))
}

fn rename_speaker(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("transcript.renameSpeaker", "`name` is required"))?.to_string();
    let item = u64_p(p, "item").map(ItemId).and_then(|i| media_item(s, i));
    // `speaker`: the current name (every transcript), or an index (needs `item`)
    let (old_name, index) = match p.get("speaker") {
        Some(Value::String(n)) => (Some(n.clone()), None),
        Some(v) if v.is_u64() => (None, v.as_u64().map(|n| n as usize)),
        _ => return Err(bad("transcript.renameSpeaker", "`speaker` (name, or index with `item`) is required")),
    };
    if index.is_some() && item.is_none() {
        return Err(bad("transcript.renameSpeaker", "a speaker index needs `item`"));
    }
    let mut n = 0;
    let mut next = s.project.transcripts.clone();
    for (i, t) in next.iter_mut() {
        if item.is_some_and(|x| x != *i) {
            continue;
        }
        let tt = Arc::make_mut(t);
        for (k, sp) in tt.speakers.iter_mut().enumerate() {
            if old_name.as_ref().is_some_and(|o| *o == sp.name) || index == Some(k) {
                sp.name = name.clone();
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err(EngineError::Other("no such speaker".into()));
    }
    s.edit("Rename Speaker", move |pr, _| {
        pr.transcripts = next;
        Ok(())
    })?;
    Ok(json!({"renamed": n}))
}

fn remove_ranges(s: &mut Session, label: &str, ranges: Vec<TimeRange>) -> Result<Value> {
    let n = ranges.len();
    if n == 0 {
        return Ok(json!({"removed": 0, "ticks": 0}));
    }
    let total = s.edit_sequence(label, |q, ctx, _| Ok(tx::ripple_delete_ranges(q, ranges, ctx)))?;
    Ok(json!({"removed": n, "ticks": total.0, "seconds": total.0 as f64 / TICKS_PER_SECOND as f64}))
}

fn remove_pauses(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let min = Tick::from_seconds_f64(f64_p(p, "minSeconds").unwrap_or(1.0));
    let keep = Tick::from_seconds_f64(f64_p(p, "keepSeconds").unwrap_or(0.15));
    let ranges = tx::find_pauses(&words, min, keep, s.sequence_rate());
    remove_ranges(s, "Remove Pauses", ranges)
}

fn remove_fillers(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let fillers: Vec<String> = match p.get("fillers").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => tx::DEFAULT_FILLERS.iter().map(|f| f.to_string()).collect(),
    };
    let hits = tx::find_fillers(&words, &fillers);
    let ranges = tx::filler_ranges(&words, &hits, s.sequence_rate());
    remove_ranges(s, "Remove Filler Words", ranges)
}

fn create_captions(s: &mut Session, p: &Value) -> Result<Value> {
    let words = sequence_words(s);
    let d = CaptionRules::default();
    let rules = CaptionRules {
        max_chars: u64_p(p, "maxChars").map(|n| n as usize).unwrap_or(d.max_chars),
        lines: u64_p(p, "lines").map(|n| n as usize).unwrap_or(d.lines),
        min_duration: f64_p(p, "minSeconds").map(Tick::from_seconds_f64).unwrap_or(d.min_duration),
        max_duration: f64_p(p, "maxSeconds").map(Tick::from_seconds_f64).unwrap_or(d.max_duration),
        gap_frames: p.get("gapFrames").and_then(Value::as_i64).unwrap_or(d.gap_frames),
        break_pause: d.break_pause,
    };
    let blocks = tx::caption_blocks(&words, &rules, s.sequence_rate());
    let format = str_p(p, "format").and_then(CaptionFormat::from_name).unwrap_or_default();
    let name = str_p(p, "name").unwrap_or("Transcript").to_string();
    let n = blocks.len();
    let tid = s.edit_sequence("Create Captions", |q, ctx, st| {
        let tid = TrackId(ctx.alloc());
        let mut t = CaptionTrack::new(tid, name, format);
        t.captions = tx::blocks_to_captions(&blocks, ctx);
        q.caption_tracks.insert(0, t);
        st.caption_selection.clear();
        Ok(tid)
    })?;
    Ok(json!({"track": tid.0, "captions": n}))
}

fn models(_: &mut Session, _: &Value) -> Result<Value> {
    let dir = models_dir();
    Ok(json!({
        "available": filmcraft_speech::available(),
        "default": filmcraft_speech::models::DEFAULT_MODEL,
        "dir": dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "models": filmcraft_speech::models::catalogue().iter().map(|m| json!({
            "id": m.id, "name": m.name, "multilingual": m.multilingual, "description": m.description,
            "license": m.license, "source": m.source, "size": m.size(),
            "installed": dir.as_ref().is_some_and(|d| filmcraft_speech::models::installed(d, m)),
        })).collect::<Vec<_>>(),
    }))
}

/// Download a catalogue model into `<data dir>/models` (feature `speech-download`). Hosts show
/// the size, source and licence (`transcript.models`) and ask before running this.
fn download_model(_: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "model").unwrap_or(filmcraft_speech::models::DEFAULT_MODEL);
    let m = filmcraft_speech::models::find(id).ok_or_else(|| speech_err(SpeechError::UnknownModel(id.into())))?;
    let dir = models_dir().ok_or_else(|| EngineError::Other("no data directory for speech models".into()))?;
    #[cfg(feature = "speech-download")]
    {
        filmcraft_speech::models::download(&dir, m, &mut |_, _, _| true).map_err(speech_err)?;
        Ok(json!({"model": m.id, "dir": filmcraft_speech::models::model_dir(&dir, m).to_string_lossy()}))
    }
    #[cfg(not(feature = "speech-download"))]
    {
        let _ = (m, dir);
        Err(EngineError::Other("model downloads are not available in this build (built without the `speech-download` feature)".into()))
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "transcript.generate",
            "Transcribe…",
            &["Sequence", "Transcript"],
            r#"{"items":[id]?,"model":"whisper-base"?,"language":"en|auto"?,"diarize":bool?,"maxSpeakers":n?,"keepFillers":bool=false,"prompt":str?,"regions":[[startSeconds,endSeconds]]?,"wait":bool=false}"#,
            can_transcribe,
            generate,
            true,
        ),
        spec(
            "transcript.set",
            "Import Transcript",
            &[],
            r#"{"item":id,"transcript":{"language":str,"speakers":[{"name":str}],"words":[{"text":str,"start":tick,"end":tick,"speaker":n?}]}}"#,
            always,
            set,
            true,
        ),
        spec("transcript.delete", "Delete Transcript", &["Sequence", "Transcript"], r#"{"items":[id]?}"#, has_transcripts, delete, true),
        spec("transcript.inspect", "Inspect Transcript", &[], r#"{"paragraphGapSeconds":f?}"#, always, inspect, false),
        spec("transcript.search", "Search Transcript", &[], r#"{"query":str}"#, always, search, false),
        spec("transcript.models", "List Speech Models", &[], "{}", always, models, false),
        spec("transcript.downloadModel", "Download Speech Model", &[], r#"{"model":"whisper-base"?}"#, can_download, download_model, true),
        spec("transcript.select", "Mark Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, select, true),
        spec("transcript.extract", "Extract Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, true), true),
        spec("transcript.lift", "Lift Selected Text", &[], r#"{"from":word,"to":word?}"#, has_transcript, |s, p| extract_or_lift(s, p, false), true),
        spec(
            "transcript.renameSpeaker",
            "Rename Speaker…",
            &[],
            r#"{"speaker":"Speaker 1"|index,"name":str,"item":id?}"#,
            has_transcripts,
            rename_speaker,
            true,
        ),
        spec(
            "transcript.removePauses",
            "Remove Pauses",
            &["Sequence", "Transcript"],
            r#"{"minSeconds":f?,"keepSeconds":f?}"#,
            has_transcript,
            remove_pauses,
            true,
        ),
        spec("transcript.removeFillers", "Remove Filler Words", &["Sequence", "Transcript"], r#"{"fillers":[str]?}"#, has_transcript, remove_fillers, true),
        spec(
            "transcript.createCaptions",
            "Create Captions from Transcript…",
            &["Sequence", "Transcript"],
            r#"{"maxChars":n?,"lines":1|2?,"minSeconds":f?,"maxSeconds":f?,"gapFrames":n?,"format":str?,"name":str?}"#,
            has_transcript,
            create_captions,
            true,
        ),
    ]
}
