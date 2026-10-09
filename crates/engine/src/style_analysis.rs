//! Style from a reference (A4.1): learn the "treatment" of a reference video.
//!
//! - `media.analyze {item, maxFrames?=240 (1–600), wait?=false}` runs a background job
//!   (`jobs.list` shows its progress, `jobs.cancel` stops it) that decodes up to `maxFrames`
//!   evenly spaced frames of a media item (or subclip) at ≈256 px, finds the cuts between them
//!   ([`filmcraft_render::scene`]), measures each shot's keyframe colour, the programme loudness
//!   and, when the item has a transcript, the speech rhythm. The result is a
//!   [`StyleProfile`](filmcraft_render::style::StyleProfile) cached on the session (never in the
//!   project file) and returned by `media.analysis {item}`.
//! - The style library keeps profiles as JSON files in `<data dir>/styles/`: `style.save {name,
//!   item}`, `style.list`, `style.delete {name}`.
//!
//! See `docs/style-analysis.md` for the profile fields and the limits of the method.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use filmcraft_project::ItemId;
use filmcraft_render::scene::{SceneDetector, SceneOptions};
use filmcraft_render::style::{self, AudioProfile, FormatProfile, Keyframe, StyleProfile};
use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, item_p, str_p};
use crate::{EngineError, Result, Session};

/// Longest side of the frames analysed.
const ANALYSIS_WIDTH: f64 = 256.0;
pub const DEFAULT_MAX_FRAMES: usize = 240;
pub const MAX_FRAMES: usize = 600;
/// Longest style name (characters).
pub const MAX_NAME_CHARS: usize = 64;
/// Style files larger than this are not read.
const MAX_STYLE_FILE_BYTES: u64 = 4 << 20;
/// Most styles `style.list` returns.
const MAX_LISTED: usize = 500;
const STYLE_FORMAT: &str = "filmcraft-style";

/// Session state: finished analyses (keyed by item), running jobs and where the library lives.
#[derive(Default)]
pub struct StyleState {
    pub cache: HashMap<ItemId, StyleProfile>,
    pub jobs: Vec<PendingAnalysis>,
    dir: Option<PathBuf>,
}

impl StyleState {
    /// Use `data_dir` for the style library (and baked LUTs).
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
    }
    /// The data directory: the one set by the host, else the per-user default (none in unit
    /// tests, so they never touch the real one).
    pub fn data_dir(&self) -> Option<PathBuf> {
        self.dir.clone().or_else(|| if cfg!(test) { None } else { crate::autosave::default_data_dir() })
    }
}

/// A running `media.analyze` job.
pub struct PendingAnalysis {
    pub job: u64,
    pub item: ItemId,
    pub result: Arc<Mutex<Option<StyleProfile>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("media.analyze", "Analyze Style", r#"{"item":id,"maxFrames":1..600=240,"wait":bool=false}"#, has_media, analyze, false),
        spec("media.analysis", "Style Analysis", r#"{"item":id}"#, always, analysis, false),
        spec("style.save", "Save Style", r#"{"name":str,"item":id}"#, has_profiles, save, false),
        spec("style.list", "List Styles", "{}", always, list, false),
        spec("style.delete", "Delete Style", r#"{"name":str}"#, always, delete, false),
    ]
}

fn has_media(s: &Session) -> std::result::Result<(), String> {
    let any = s.project.items.values().any(|i| matches!(&i.kind, filmcraft_project::ItemKind::Media(m) if m.info.video.is_some() || m.info.audio.is_some()));
    if any { Ok(()) } else { Err("import media to analyse first".into()) }
}

fn has_profiles(s: &Session) -> std::result::Result<(), String> {
    if s.style.cache.is_empty() { Err("analyse a media item first (media.analyze)".into()) } else { Ok(()) }
}

/// Parse `maxFrames` (absent / null = the default).
fn max_frames_p(p: &Value, cmd: &str) -> Result<usize> {
    match p.get("maxFrames") {
        None | Some(Value::Null) => Ok(DEFAULT_MAX_FRAMES),
        Some(v) => v
            .as_f64()
            .filter(|f| f.is_finite() && (1.0..=MAX_FRAMES as f64).contains(f))
            .map(|f| f.round() as usize)
            .ok_or_else(|| bad(cmd, format!("`maxFrames` must be a number from 1 to {MAX_FRAMES}"))),
    }
}

/// Everything the job needs, gathered on the session's thread.
struct Plan {
    item: ItemId,
    root: ItemId,
    name: String,
    project: Arc<filmcraft_project::Project>,
    src: filmcraft_media::SharedSource,
    range: TimeRange,
    /// Media times of the analysed frames (empty without picture).
    times: Vec<Tick>,
    scale: f32,
    /// (sample rate, channels) of the sound to measure.
    audio: Option<(u32, usize)>,
    profile: StyleProfile,
}

fn plan(s: &Session, p: &Value) -> Result<Plan> {
    let cmd = "media.analyze";
    let item = item_p(p, "item").ok_or_else(|| bad(cmd, "need `item` (a media item id)"))?;
    let max_frames = max_frames_p(p, cmd)?;
    let pi = s.project.item(item).ok_or_else(|| bad(cmd, format!("no item {}", item.0)))?;
    let name = pi.name.clone();
    let (root, clip, sub) = s.project.resolve_media(item).ok_or_else(|| bad(cmd, "not a media item or subclip (sequences can't be analysed)"))?;
    let info = clip.info.clone();
    if info.video.is_none() && info.audio.is_none() {
        return Err(bad(cmd, "the item has no picture and no sound"));
    }
    // hostile durations / ranges: everything stays within 0 … Tick::MAX
    let full = TimeRange::new(Tick::ZERO, info.duration.clamp(Tick::ZERO, Tick::MAX));
    let range = match sub {
        Some(r) => {
            let start = r.start.clamp(Tick::ZERO, full.end());
            TimeRange::new(start, r.duration.clamp(Tick::ZERO, full.end() - start))
        }
        None => full,
    };
    if range.duration <= Tick::ZERO {
        return Err(bad(cmd, "the item is empty"));
    }
    if s.style.jobs.iter().any(|j| j.item == item) {
        return Err(EngineError::Other("this item is already being analysed".into()));
    }
    let src = s.source(root).ok_or_else(|| EngineError::Other("the media is offline or still loading".into()))?;
    let video = info.video.clone();
    let (times, scale, interval) = match &video {
        Some(v) => {
            let still = matches!(info.kind, filmcraft_media::MediaKind::Still);
            let fd = v.frame_rate.sane().frame_duration().0.max(1);
            let frames = if still { 1 } else { (range.duration.0 / fd).max(1) };
            let n = (max_frames as i64).min(frames).max(1);
            let times: Vec<Tick> = (0..n).map(|k| range.start + Tick((range.duration.0 as i128 * k as i128 / n as i128) as i64)).collect();
            (times, (ANALYSIS_WIDTH / v.width.max(1) as f64).min(1.0) as f32, range.duration.seconds() / n as f64)
        }
        None => (Vec::new(), 1.0, 0.0),
    };
    let audio =
        info.audio.as_ref().filter(|a| (8_000..=384_000).contains(&a.sample_rate) && a.channels > 0).map(|a| (a.sample_rate, a.channels.clamp(1, 8) as usize));
    let speech = s.project.transcripts.get(&root).and_then(|t| style::speech_profile(&t.words, range, filmcraft_edit::transcript::DEFAULT_FILLERS));
    let profile = StyleProfile {
        version: style::PROFILE_VERSION,
        item: item.0,
        name: name.clone(),
        analyzed_frames: times.len(),
        sample_interval_seconds: interval,
        format: FormatProfile {
            width: video.as_ref().map_or(0, |v| v.width),
            height: video.as_ref().map_or(0, |v| v.height),
            aspect: video.as_ref().map_or_else(|| "none".into(), |v| style::aspect_label(v.width, v.height, v.par)),
            fps: video.as_ref().map_or(0.0, |v| v.frame_rate.sane().as_f64()),
            duration_seconds: range.duration.seconds(),
            has_video: video.is_some(),
            has_audio: info.audio.is_some(),
        },
        speech,
        ..Default::default()
    };
    Ok(Plan { item, root, name, project: s.project.clone(), src, range, times, scale, audio, profile })
}

/// The analysis itself (runs on the job's thread). `Err("stopped")` when cancelled.
fn run_analysis(pl: Plan, prog: &filmcraft_export::Progress) -> std::result::Result<StyleProfile, String> {
    let Plan { root, name, project, src, range, times, scale, audio, mut profile, .. } = pl;
    let stopped = || prog.cancel.load(Ordering::Relaxed);
    let mut done = 0u64;
    let step = |done: &mut u64, status: String| {
        *done += 1;
        prog.done.store(*done, Ordering::Relaxed);
        *lock(&prog.status) = status;
    };
    // ---- picture: shots and colour
    if !times.is_empty() {
        let shared = src.clone();
        let provider = move |id: ItemId| (id == root).then(|| shared.clone());
        let mut det = SceneDetector::new();
        let mut colors = Vec::with_capacity(times.len());
        let mut last: Option<(Vec<u8>, usize, usize)> = None;
        let mut failed = 0usize;
        for (k, t) in times.iter().enumerate() {
            if stopped() {
                return Err("stopped".into());
            }
            match filmcraft_render::render_item(&project, root, *t, scale, &provider) {
                Some(img) if img.w > 0 && img.h > 0 => {
                    let rgba = img.to_rgba8();
                    det.push_rgba(&rgba, img.w, img.h);
                    colors.push(style::frame_color(&img));
                    last = Some((rgba, img.w, img.h));
                }
                _ => {
                    // keep the indices aligned: repeat the previous picture (no cut)
                    failed += 1;
                    match &last {
                        Some((rgba, w, h)) => det.push_rgba(rgba, *w, *h),
                        None => det.push_rgba(&[0, 0, 0, 255], 1, 1),
                    };
                    colors.push(None);
                }
            }
            step(&mut done, format!("{name}: frame {} of {}", k + 1, times.len()));
        }
        if failed > 0 {
            profile.notes.push(format!("{failed} of {} frames could not be decoded", times.len()));
        }
        // at sparse sampling, a shot shorter than ¼ s can't be told apart from a flash
        let spacing = profile.sample_interval_seconds.max(1e-6);
        let min_shot = ((0.25 / spacing).round() as usize).clamp(1, 6);
        let cut_idx = det.cuts(&SceneOptions { sensitivity: 50.0, min_shot_frames: min_shot });
        let cuts: Vec<Tick> = cut_idx.iter().filter_map(|i| times.get(*i).copied()).collect();
        profile.shots = style::shot_profile(&cuts, range);
        // shots as sample index ranges
        let mut bounds = vec![0usize];
        bounds.extend(cut_idx.iter().copied().filter(|i| *i > 0 && *i < times.len()));
        bounds.push(times.len());
        bounds.dedup();
        let shots: Vec<(usize, usize)> = bounds.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))).filter(|(a, b)| b > a).collect();
        let end = range.end();
        let mut weighted = Vec::new();
        let mut all_keys = Vec::new();
        for (k, (a, b)) in shots.iter().enumerate() {
            let mid = a + (b - a) / 2;
            let (Some(t0), Some(tm)) = (times.get(*a).copied(), times.get(mid).copied()) else { continue };
            let t1 = times.get(*b).copied().unwrap_or(end);
            let color = colors.get(mid).copied().flatten();
            if let Some(c) = color {
                weighted.push((c, (t1 - t0).seconds()));
            }
            all_keys.push(Keyframe { shot: k, time: tm.0, seconds: (tm - range.start).seconds(), color });
        }
        profile.color = style::color_summary(&weighted);
        profile.keyframes = style::spread(all_keys.len(), style::MAX_KEYFRAMES).into_iter().filter_map(|i| all_keys.get(i).cloned()).collect();
        if profile.color.is_none() {
            profile.notes.push("no frame had visible picture: no colour statistics".into());
        }
    } else {
        profile.notes.push("no picture: shot and colour analysis skipped".into());
    }
    // ---- sound: loudness
    if let Some((sr, ch)) = audio {
        let mut meter = filmcraft_audio_dsp::loudness::LoudnessMeter::new(sr as f64, ch);
        let (a0, a1) = (range.start.to_units_floor(sr as i64), range.end().to_units_floor(sr as i64));
        let mut pos = a0;
        let mut measured = 0i64;
        while pos < a1 {
            if stopped() {
                return Err("stopped".into());
            }
            let n = (a1 - pos).min(sr as i64).max(1) as usize;
            match src.audio(pos, n, sr) {
                Ok(buf) => {
                    let slices: Vec<&[f32]> = buf.channels.iter().take(ch).map(Vec::as_slice).collect();
                    meter.process(&slices);
                }
                Err(e) => {
                    profile.notes.push(format!("sound could not be decoded after {:.1} s: {e}", measured as f64 / sr as f64));
                    break;
                }
            }
            pos = pos.saturating_add(n as i64);
            measured = measured.saturating_add(n as i64);
            step(&mut done, format!("{name}: loudness {:.0} s", measured as f64 / sr as f64));
        }
        let sm = meter.summary();
        let fin = |v: f64| v.is_finite().then_some(v);
        profile.audio = Some(AudioProfile {
            integrated_lufs: fin(sm.integrated_lufs),
            loudness_range_lu: fin(sm.loudness_range_lu).filter(|_| sm.integrated_lufs.is_finite()),
            true_peak_dbtp: fin(sm.true_peak_dbtp),
            sample_rate: sr,
            channels: ch as u32,
            analyzed_seconds: measured as f64 / sr as f64,
        });
    } else if profile.format.has_audio {
        profile.notes.push("the sound has an unsupported sample rate: loudness skipped".into());
    }
    Ok(profile)
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let pl = plan(s, p)?;
    let (item, name) = (pl.item, pl.name.clone());
    let audio_chunks = pl.audio.map_or(0, |(sr, _)| {
        let (a0, a1) = (pl.range.start.to_units_floor(sr as i64), pl.range.end().to_units_floor(sr as i64));
        (a1.saturating_sub(a0).max(0) as u64).div_ceil(sr.max(1) as u64)
    });
    let total = pl.times.len() as u64 + audio_chunks;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("Analyze Style ({name})"), progress: Default::default(), result: Default::default() };
    job.progress.total.store(total, Ordering::Relaxed);
    let out: Arc<Mutex<Option<StyleProfile>>> = Arc::default();
    let (prog, res, slot) = (job.progress.clone(), job.result.clone(), out.clone());
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = run_analysis(pl, &prog);
        let secs = t0.elapsed().as_secs_f64();
        let done = prog.done.load(Ordering::Relaxed);
        *lock(&prog.status) = match &r {
            Ok(p) => format!("{} shot(s), {} frame(s) analysed in {secs:.1}s", p.shots.count, p.analyzed_frames),
            Err(e) if e == "stopped" => "Stopped".into(),
            Err(e) => e.clone(),
        };
        let r = r.map(|p| {
            *lock(&slot) = Some(p);
            filmcraft_export::Report {
                path: String::new(),
                frames: done,
                seconds: secs,
                bytes: 0,
                render_fps: done as f64 / secs.max(1e-6),
                extra_files: Vec::new(),
            }
        });
        *lock(&res) = Some(r);
        prog.finished.store(true, Ordering::Relaxed);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    s.style.jobs.push(PendingAnalysis { job: id, item, result: out });
    let mut ret = json!({"job": id, "item": item.0, "frames": total});
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
        poll(s);
        if let Some(pr) = s.style.cache.get(&item) {
            ret["profile"] = serde_json::to_value(pr).unwrap_or_default();
        }
    } else {
        std::thread::Builder::new().name("filmcraft-style-analysis".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    if let Some(Err(e)) = s.jobs.iter().find(|j| j.id == id).and_then(|j| lock(&j.result).clone()) {
        return Err(EngineError::Other(e));
    }
    Ok(ret)
}

/// Move finished analyses into the cache and drop finished / cancelled jobs. Called once per UI
/// frame from [`Session::poll_persistence`] and after synchronous runs.
pub fn poll(s: &mut Session) {
    let mut i = 0;
    while i < s.style.jobs.len() {
        let Some(pj) = s.style.jobs.get(i) else { break };
        let job = s.jobs.iter().find(|j| j.id == pj.job);
        let finished = job.is_none_or(|j| j.progress.finished.load(Ordering::Relaxed));
        let cancelled = job.is_some_and(|j| j.progress.cancel.load(Ordering::Relaxed));
        if !finished {
            i += 1;
            continue;
        }
        let pj = s.style.jobs.remove(i);
        let Some(profile) = lock(&pj.result).take().filter(|_| !cancelled) else { continue };
        s.toast(format!("Style analysis of {}: {} shot(s), {:.1} cuts/min", profile.name, profile.shots.count, profile.shots.cuts_per_minute));
        s.style.cache.insert(pj.item, profile);
    }
}

fn analysis(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "media.analysis";
    let item = item_p(p, "item").ok_or_else(|| bad(cmd, "need `item`"))?;
    if let Some(pr) = s.style.cache.get(&item) {
        return Ok(json!({"item": item.0, "profile": pr}));
    }
    if let Some(j) = s.style.jobs.iter().find(|j| j.item == item) {
        return Ok(json!({"item": item.0, "pending": true, "job": j.job}));
    }
    Err(bad(cmd, format!("item {} has not been analysed (run media.analyze)", item.0)))
}

/// A style name usable as a file name: trimmed, 1–64 characters, no path separators, no
/// characters Windows refuses, not starting with a dot.
pub fn sanitize_name(name: &str, cmd: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(bad(cmd, "the name is empty"));
    }
    if n.chars().count() > MAX_NAME_CHARS {
        return Err(bad(cmd, format!("the name is longer than {MAX_NAME_CHARS} characters")));
    }
    if n.starts_with('.') || n.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(bad(cmd, "the name can't contain path separators, control characters or : * ? \" < > |, or start with a dot"));
    }
    Ok(n.to_string())
}

fn styles_dir(s: &Session, cmd: &str) -> Result<PathBuf> {
    s.style.data_dir().map(|d| d.join("styles")).ok_or_else(|| bad(cmd, "no data directory to keep styles in"))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StyleFile {
    format: String,
    version: u32,
    name: String,
    #[serde(default)]
    saved_unix: i64,
    profile: StyleProfile,
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "style.save";
    let name = sanitize_name(str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?, cmd)?;
    let item = item_p(p, "item").ok_or_else(|| bad(cmd, "need `item`"))?;
    let profile = s.style.cache.get(&item).cloned().ok_or_else(|| bad(cmd, format!("item {} has not been analysed (run media.analyze)", item.0)))?;
    let dir = styles_dir(s, cmd)?;
    std::fs::create_dir_all(&dir).map_err(|e| bad(cmd, format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{name}.json"));
    let f = StyleFile { format: STYLE_FORMAT.into(), version: 1, name: name.clone(), saved_unix: crate::autosave::unix_now(), profile };
    let bytes = serde_json::to_vec_pretty(&f).map_err(|e| bad(cmd, e.to_string()))?;
    filmcraft_format::atomic_write(&path, &bytes).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    Ok(json!({"name": name, "path": path.to_string_lossy(), "bytes": bytes.len()}))
}

fn read_style(path: &Path) -> std::result::Result<StyleFile, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_STYLE_FILE_BYTES {
        return Err(format!("larger than {} bytes", MAX_STYLE_FILE_BYTES));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let f: StyleFile = serde_json::from_slice(&bytes).map_err(|e| format!("not a style file: {e}"))?;
    if f.format != STYLE_FORMAT {
        return Err(format!("not a style file (format `{}`)", f.format));
    }
    Ok(f)
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let Ok(dir) = styles_dir(s, "style.list") else { return Ok(json!({"styles": [], "dir": Value::Null})) };
    let mut styles = Vec::new();
    let mut errors = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        paths.sort();
        for path in paths.into_iter().take(MAX_LISTED) {
            let stem = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            match read_style(&path) {
                Ok(f) => styles.push(json!({"name": stem, "path": path.to_string_lossy(), "savedUnix": f.saved_unix, "profile": f.profile})),
                Err(e) => errors.push(json!({"name": stem, "error": e})),
            }
        }
    }
    Ok(json!({"styles": styles, "errors": errors, "dir": dir.to_string_lossy()}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "style.delete";
    let name = sanitize_name(str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?, cmd)?;
    let path = styles_dir(s, cmd)?.join(format!("{name}.json"));
    if !path.is_file() {
        return Err(bad(cmd, format!("no style named `{name}`")));
    }
    std::fs::remove_file(&path).map_err(|e| bad(cmd, format!("{}: {e}", path.display())))?;
    Ok(json!({"name": name, "deleted": true}))
}

#[cfg(test)]
#[path = "style_analysis_tests.rs"]
mod tests;
