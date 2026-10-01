//! The audio mixer graph (Audio Track Mixer): tracks → submixes → Mix.
//!
//! Signal path of every strip, in Premiere's documented order:
//!
//! ```text
//! track input (clips: gain → clip effects → clip Volume / Channel Volume / Panner, transitions)
//!   → input map / mono fold → pre-fader inserts → pre-fader sends → mute → fader (volume)
//!   → meter → post-fader inserts → post-fader sends → pan / balance → output (submix or Mix)
//! submix: bus sum → the same strip path → output (Mix or a later submix)
//! Mix:    bus sum → pre-fader inserts → fader → meter → post-fader inserts → out
//! ```
//!
//! * **Sample accurate.** Automated lanes (volume, pan, mute, send levels) are evaluated per sample
//!   from their keyframes; effect parameters update on an absolute 64-sample grid. The output does
//!   not depend on how callers cut the timeline into requests.
//! * **Latency compensated.** Inserts that report latency (look-ahead limiter, STFT effects) delay
//!   their strip; every route into a bus gets a compensation delay so all inputs of a bus line up,
//!   and the whole graph is read ahead by its total latency, so the output is aligned with the
//!   timeline. Automation is evaluated at *content* time (input time minus the latency so far).
//! * **Stateful and pull-based.** Graph state (insert DSP, compensation delays) is cached per graph
//!   structure and keyed by the next sample it will produce. Sequential consumers (playback, export,
//!   meters) continue it; any other request starts fresh with a pre-roll long enough for effect
//!   tails and the graph latency.
//! * **Solo.** When any strip is soloed, a strip stays audible if it is soloed or solo-safe, feeds
//!   (directly or through submixes) a soloed or solo-safe strip, or is fed by one. Everything else
//!   is solo-muted (silent, sends included). Track mute sits after the pre-fader sends, so
//!   pre-fader sends of a muted track keep sending, as in Premiere.
//! * **Pan law.** Mono tracks pan with the −3 dB constant-power law (centre = −3 dB per side);
//!   stereo (and, for now, 5.1) tracks and sends use balance (centre = unity, the far side follows
//!   the same constant-power curve normalised to the centre).
//!
//! [`LiveMix`] carries what the UI does while playing: control overrides while a fader is held
//! (with Touch's ramp back to the automation), per-strip meters, and the newest project snapshot.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_frame::AudioBuffer;
use filmcraft_project::mixer::{LANE_MUTE, LANE_PAN, LANE_VOLUME, fx_lane, send_lane};
use filmcraft_project::{AudioChannels, EffectInstance, InputMap, Param, ParamValue, Project, Sequence, Track, TrackId};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::SourceProvider;
use crate::audio::{pan_gains, track_input};
use crate::audio_fx::{Mapping, mapping};

/// Strip id of the Mix (master) track in [`LiveMix`] and commands.
pub const MASTER: TrackId = filmcraft_project::mixer::MASTER_STRIP;
/// Effect parameters update on this absolute sample grid.
pub const PARAM_BLOCK: i64 = 64;
/// Longest pre-roll for a fresh graph (seconds).
const MAX_PREROLL_S: f64 = 3.0;

// ------------------------------------------------------------------------------------- live state

/// A control held (or just released) on the mixer while playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Override {
    pub value: f64,
    /// Released at `.0`; the value ramps back to the automation over `.1` (Touch).
    pub release: Option<(Tick, Tick)>,
}

/// What the live mixer UI feeds into playback (and reads back from it).
#[derive(Default)]
pub struct LiveMix {
    overrides: Mutex<HashMap<(TrackId, String), Override>>,
    active: AtomicBool,
    meters: Mutex<HashMap<TrackId, [f32; 2]>>,
    project: Mutex<Option<Arc<Project>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LiveMix {
    pub fn new() -> Self {
        Self::default()
    }
    /// Hold a control at `value` (a fader or knob being touched/moved).
    pub fn hold(&self, strip: TrackId, lane: &str, value: f64) {
        lock(&self.overrides).insert((strip, lane.to_string()), Override { value, release: None });
        self.active.store(true, Ordering::Release);
    }
    /// Release a held control at `at`: playback ramps from the held value back to the automation
    /// over `ramp` (zero = jump back immediately).
    pub fn release(&self, strip: TrackId, lane: &str, at: Tick, ramp: Tick) {
        let mut g = lock(&self.overrides);
        if ramp.0 <= 0 {
            g.remove(&(strip, lane.to_string()));
        } else if let Some(o) = g.get_mut(&(strip, lane.to_string())) {
            o.release = Some((at, ramp));
        }
        self.active.store(!g.is_empty(), Ordering::Release);
    }
    pub fn clear(&self, strip: TrackId, lane: &str) {
        let mut g = lock(&self.overrides);
        g.remove(&(strip, lane.to_string()));
        self.active.store(!g.is_empty(), Ordering::Release);
    }
    pub fn clear_all(&self) {
        lock(&self.overrides).clear();
        self.active.store(false, Ordering::Release);
    }
    /// Whether any control is overridden (rendered audio previews are bypassed then).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn get(&self, strip: TrackId, lane: &str) -> Option<Override> {
        lock(&self.overrides).get(&(strip, lane.to_string())).copied()
    }
    pub fn overrides(&self) -> HashMap<(TrackId, String), Override> {
        lock(&self.overrides).clone()
    }
    /// Peak levels (linear, L/R, post-fader) per strip since the last call; [`MASTER`] is the Mix.
    pub fn take_meters(&self) -> HashMap<TrackId, [f32; 2]> {
        std::mem::take(&mut *lock(&self.meters))
    }
    fn post_meters(&self, m: &[(TrackId, [f32; 2])]) {
        let mut g = lock(&self.meters);
        for (id, p) in m {
            let e = g.entry(*id).or_insert([0.0; 2]);
            e[0] = e[0].max(p[0]);
            e[1] = e[1].max(p[1]);
        }
    }
    /// Publish the newest project snapshot (playback picks up edits made while it runs).
    pub fn publish_project(&self, p: Arc<Project>) {
        *lock(&self.project) = Some(p);
    }
    pub fn project(&self) -> Option<Arc<Project>> {
        lock(&self.project).clone()
    }
}

// ------------------------------------------------------------------------------------- pan laws

/// −3 dB constant-power pan of a mono signal: (left, right) gains; centre = 0.7071 each.
pub fn pan_law_mono(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}

/// Stereo balance (centre = unity). Same curve as the clip Panner.
pub fn balance(pan: f32) -> (f32, f32) {
    pan_gains(pan.clamp(-1.0, 1.0))
}

#[inline]
fn db_gain(db: f64) -> f32 {
    if db <= filmcraft_project::mixer::FADER_MIN_DB { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

// ------------------------------------------------------------------------------------- lanes

/// One automatable control of a strip, ready for per-sample evaluation.
struct Lane<'a> {
    param: Option<&'a Param>,
    stat: f64,
    ov: Option<Override>,
    /// Switch lanes (mute) hold between keyframes whatever their interpolation.
    hold: bool,
}

impl<'a> Lane<'a> {
    fn new(track: &'a Track, strip: TrackId, key: &str, ovs: &HashMap<(TrackId, String), Override>) -> Self {
        let reads = track.mixer.mode.reads();
        let param = track.lane(key).filter(|p| reads && p.is_animated());
        let hold = filmcraft_project::mixer::lane_info(key).is_some_and(|i| i.hold);
        Lane { param, stat: track.lane_static(key).unwrap_or(0.0), ov: ovs.get(&(strip, key.to_string())).copied(), hold }
    }
    fn auto(&self, t: Tick) -> f64 {
        match self.param {
            None => self.stat,
            Some(p) if self.hold => {
                let i = p.keyframes.partition_point(|k| k.time <= t);
                p.keyframes[i.saturating_sub(1)].value.as_f64().unwrap_or(0.0)
            }
            Some(p) => p.scalar_at(t),
        }
    }
    /// The value when it is the same at every sample.
    fn constant(&self) -> Option<f64> {
        match self.ov {
            Some(Override { value, release: None }) => Some(value),
            Some(_) => None,
            None => self.param.is_none().then_some(self.stat),
        }
    }
    fn at(&self, t: Tick) -> f64 {
        match self.ov {
            None => self.auto(t),
            Some(Override { value, release: None }) => value,
            Some(Override { value, release: Some((r, d)) }) => {
                if t < r {
                    value
                } else if t >= r + d {
                    self.auto(t)
                } else {
                    let u = (t - r).0 as f64 / d.0.max(1) as f64;
                    value + (self.auto(t) - value) * u
                }
            }
        }
    }
    /// Per-sample values for content samples `c0 ..`, mapped through `f`, into `out`.
    /// Returns the constant instead when the lane does not change.
    fn fill(&self, c0: i64, sr: u32, out: &mut [f32], f: impl Fn(f64) -> f32) -> Option<f32> {
        if let Some(v) = self.constant() {
            return Some(f(v));
        }
        for (i, o) in out.iter_mut().enumerate() {
            *o = f(self.at(Tick::from_units(c0 + i as i64, sr as i64)));
        }
        None
    }
}

// ------------------------------------------------------------------------------------- plan

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Track,
    Submix,
    Master,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tap {
    /// After the pre-fader inserts.
    Pre,
    /// After the post-fader inserts (before pan).
    Post,
    /// The strip output (after pan).
    Out,
}

struct Insert<'a> {
    slot: usize,
    effect: Cow<'a, EffectInstance>,
    map: Mapping,
}

struct Strip<'a> {
    track: Cow<'a, Track>,
    kind: Kind,
    pre: Vec<Insert<'a>>,
    post: Vec<Insert<'a>>,
    /// Silent (solo-muted, or muted with nothing tapping before the mute).
    silent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RouteSpec {
    src: usize,
    tap: Tap,
    /// Send index (sends), `None` for the main output.
    send: Option<usize>,
    target: usize,
}

struct Plan<'a> {
    strips: Vec<Strip<'a>>,
    routes: Vec<RouteSpec>,
}

fn inserts<'a>(track: &'a Track, post: bool) -> Vec<Insert<'a>> {
    let reads = track.mixer.mode.reads();
    track
        .effects
        .iter()
        .enumerate()
        .take(filmcraft_project::mixer::MAX_INSERTS)
        .filter(|(_, e)| e.enabled && e.post_fader == post && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false))
        .filter_map(|(slot, e)| mapping(&e.effect).map(|m| (slot, e, m)))
        .map(|(slot, e, map)| {
            let effect = if reads { Cow::Borrowed(e) } else { Cow::Owned(strip_keyframes(e)) };
            Insert { slot, effect, map }
        })
        .collect()
}

fn strip_keyframes(e: &EffectInstance) -> EffectInstance {
    let mut c = e.clone();
    for p in c.params.values_mut() {
        p.keyframes.clear();
    }
    c
}

impl<'a> Plan<'a> {
    fn build(seq: &'a Sequence, ovs: &HashMap<(TrackId, String), Override>) -> Plan<'a> {
        let mut strips: Vec<Strip<'a>> = Vec::new();
        for t in &seq.audio_tracks {
            strips.push(Strip { track: Cow::Borrowed(t), kind: Kind::Track, pre: inserts(t, false), post: inserts(t, true), silent: false });
        }
        for t in &seq.submix_tracks {
            strips.push(Strip { track: Cow::Borrowed(t), kind: Kind::Submix, pre: inserts(t, false), post: inserts(t, true), silent: false });
        }
        let m = seq.master_strip();
        strips.push(Strip { track: Cow::Owned(m), kind: Kind::Master, pre: Vec::new(), post: Vec::new(), silent: false });
        let mi = strips.len() - 1;
        // master inserts borrow from the sequence (same effects as the owned copy)
        {
            let reads = seq.master_mixer.mode.reads();
            let mk = |post: bool| -> Vec<Insert<'a>> {
                seq.master_effects
                    .iter()
                    .enumerate()
                    .take(filmcraft_project::mixer::MAX_INSERTS)
                    .filter(|(_, e)| e.enabled && e.post_fader == post)
                    .filter_map(|(slot, e)| {
                        mapping(&e.effect).map(|map| Insert { slot, effect: if reads { Cow::Borrowed(e) } else { Cow::Owned(strip_keyframes(e)) }, map })
                    })
                    .collect()
            };
            strips[mi].pre = mk(false);
            strips[mi].post = mk(true);
        }
        // live overrides of effect parameters
        for s in strips.iter_mut() {
            let id = s.track.id;
            for ins in s.pre.iter_mut().chain(s.post.iter_mut()) {
                let slot = ins.slot;
                let pids: Vec<String> = ins.effect.params.keys().cloned().collect();
                for pid in pids {
                    if let Some(o) = ovs.get(&(id, fx_lane(slot, &pid))) {
                        let e = ins.effect.to_mut();
                        if let Some(p) = e.param_mut(&pid) {
                            p.value = ParamValue::Float(o.value);
                            p.keyframes.clear();
                        }
                    }
                }
            }
        }
        // routes
        let ntr = seq.audio_tracks.len();
        let index_of = |id: TrackId| seq.submix_tracks.iter().position(|t| t.id == id).map(|i| ntr + i);
        let mut routes = Vec::new();
        for (si, s) in strips.iter().enumerate() {
            if s.kind == Kind::Master {
                continue;
            }
            // submixes may only feed submixes after them (no feedback)
            let valid = |ti: usize| s.kind == Kind::Track || ti > si;
            for (k, snd) in s.track.mixer.sends.iter().enumerate().take(filmcraft_project::mixer::MAX_SENDS) {
                if let Some(ti) = index_of(snd.target).filter(|&ti| valid(ti)) {
                    routes.push(RouteSpec { src: si, tap: if snd.pre_fader { Tap::Pre } else { Tap::Post }, send: Some(k), target: ti });
                }
            }
            let target = s.track.mixer.output.and_then(index_of).filter(|&ti| valid(ti)).unwrap_or(mi);
            routes.push(RouteSpec { src: si, tap: Tap::Out, send: None, target });
        }
        let mut plan = Plan { strips, routes };
        plan.solo(ovs);
        plan
    }

    /// Solo-mute and static-mute pruning.
    fn solo(&mut self, ovs: &HashMap<(TrackId, String), Override>) {
        let n = self.strips.len();
        let soloed: Vec<usize> = (0..n).filter(|&i| self.strips[i].kind != Kind::Master && self.strips[i].track.solo).collect();
        if !soloed.is_empty() {
            let mut keep: HashSet<usize> = soloed.iter().copied().collect();
            keep.extend((0..n).filter(|&i| self.strips[i].kind != Kind::Master && self.strips[i].track.mixer.solo_safe));
            let seeds: Vec<usize> = keep.iter().copied().collect();
            // downstream and upstream closures
            for &s in &seeds {
                let mut stack = vec![s];
                while let Some(x) = stack.pop() {
                    for r in self.routes.iter().filter(|r| r.src == x) {
                        if keep.insert(r.target) {
                            stack.push(r.target);
                        }
                    }
                }
                let mut stack = vec![s];
                while let Some(x) = stack.pop() {
                    for r in self.routes.iter().filter(|r| r.target == x) {
                        if keep.insert(r.src) {
                            stack.push(r.src);
                        }
                    }
                }
            }
            for (i, s) in self.strips.iter_mut().enumerate() {
                if s.kind != Kind::Master && !keep.contains(&i) {
                    s.silent = true;
                }
            }
        }
        // statically muted with nothing before the mute that is heard: skip entirely
        for (i, s) in self.strips.iter_mut().enumerate() {
            let id = s.track.id;
            let mute_static = Lane::new(&s.track, id, LANE_MUTE, ovs).constant().is_some_and(|m| m >= 0.5);
            let pre_sends = self.routes.iter().any(|r| r.src == i && r.tap == Tap::Pre);
            if s.kind != Kind::Master && mute_static && !pre_sends {
                s.silent = true;
            }
        }
    }

    fn structure_hash(&self, sr: u32) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sr.hash(&mut h);
        for s in &self.strips {
            s.track.id.hash(&mut h);
            s.kind.hash(&mut h);
            for (side, v) in [(0u8, &s.pre), (1u8, &s.post)] {
                for i in v.iter() {
                    (side, i.slot, &i.effect.effect, i.map.dsp).hash(&mut h);
                }
            }
        }
        self.routes.hash(&mut h);
        h.finish()
    }

    /// Longest pre-roll the inserts need for a fresh graph (samples).
    fn preroll(&self, t: Tick, sr: u32) -> usize {
        let s = self.strips.iter().flat_map(|s| s.pre.iter().chain(&s.post)).map(|i| (i.map.preroll)(&i.effect, t)).fold(0.0, f64::max);
        (s.min(MAX_PREROLL_S) * sr as f64).ceil() as usize
    }
}

// ------------------------------------------------------------------------------------- state

struct DelayLine {
    buf: Vec<[f32; 2]>,
    pos: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        DelayLine { buf: vec![[0.0; 2]; len], pos: 0 }
    }
    /// Delay `x` in place.
    fn run(&mut self, x: &mut [Vec<f32>; 2]) {
        if self.buf.is_empty() {
            return;
        }
        let n = self.buf.len();
        for i in 0..x[0].len() {
            let out = self.buf[self.pos];
            self.buf[self.pos] = [x[0][i], x[1][i]];
            x[0][i] = out[0];
            x[1][i] = out[1];
            self.pos = (self.pos + 1) % n;
        }
    }
    fn clear(&mut self) {
        self.buf.fill([0.0; 2]);
    }
}

struct StripState {
    pre: Vec<Box<dyn AudioEffect>>,
    post: Vec<Box<dyn AudioEffect>>,
    lat_pre: usize,
    lat_post: usize,
    /// Latency of the strip's input (bus alignment).
    in_lat: usize,
    /// Compensation delay per outgoing route (same order as the plan's routes from this strip).
    delays: Vec<DelayLine>,
}

struct GraphState {
    next_out: i64,
    strips: Vec<StripState>,
    latency: usize,
}

fn build_state(plan: &Plan, sr: u32) -> GraphState {
    let mk = |v: &[Insert]| -> Vec<Box<dyn AudioEffect>> { v.iter().filter_map(|i| filmcraft_audio_dsp::create_effect(i.map.dsp, sr as f32, 2)).collect() };
    let mut strips: Vec<StripState> = plan
        .strips
        .iter()
        .map(|s| {
            let pre = mk(&s.pre);
            let post = mk(&s.post);
            let lat_pre = pre.iter().map(|d| d.latency()).sum();
            let lat_post = post.iter().map(|d| d.latency()).sum();
            StripState { pre, post, lat_pre, lat_post, in_lat: 0, delays: Vec::new() }
        })
        .collect();
    let tap_lat = |s: &StripState, tap: Tap| s.in_lat + if tap == Tap::Pre { s.lat_pre } else { s.lat_pre + s.lat_post };
    // strips are in topological order (routes only go forward)
    for i in 0..strips.len() {
        let in_lat = plan.routes.iter().filter(|r| r.target == i).map(|r| tap_lat(&strips[r.src], r.tap)).max().unwrap_or(0);
        strips[i].in_lat = in_lat;
    }
    for r in &plan.routes {
        let d = strips[r.target].in_lat - tap_lat(&strips[r.src], r.tap);
        strips[r.src].delays.push(DelayLine::new(d));
    }
    let m = strips.len() - 1;
    let latency = strips[m].in_lat + strips[m].lat_pre + strips[m].lat_post;
    GraphState { next_out: 0, strips, latency }
}

fn cache() -> &'static Mutex<HashMap<u64, Vec<GraphState>>> {
    static C: OnceLock<Mutex<HashMap<u64, Vec<GraphState>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Total latency (samples) of the sequence's mixer graph: how far ahead of the timeline the graph
/// is read so its output lines up.
pub fn graph_latency(seq: &Sequence) -> usize {
    let plan = Plan::build(seq, &HashMap::new());
    build_state(&plan, seq.settings.sample_rate).latency
}

// ------------------------------------------------------------------------------------- processing

type Stereo = [Vec<f32>; 2];

fn silent(n: usize) -> Stereo {
    [vec![0.0; n], vec![0.0; n]]
}

/// Run `chain` over `buf` (input-time samples from `x0`) with parameters at content time
/// `x - lat` updated on the absolute [`PARAM_BLOCK`] grid.
fn run_inserts(chain: &mut [Box<dyn AudioEffect>], ins: &[Insert], buf: &mut Stereo, x0: i64, lat: usize, sr: u32) {
    if chain.is_empty() {
        return;
    }
    let n = buf[0].len();
    let mut i = 0usize;
    while i < n {
        let pos = x0 + i as i64;
        let end = (((pos.div_euclid(PARAM_BLOCK) + 1) * PARAM_BLOCK - x0) as usize).min(n);
        // parameters of the whole grid block (also when a request starts mid-block)
        let t = Tick::from_units(pos.div_euclid(PARAM_BLOCK) * PARAM_BLOCK - lat as i64, sr as i64);
        for (d, e) in chain.iter_mut().zip(ins) {
            (e.map.apply)(d.as_mut(), &e.effect, t);
        }
        let [l, r] = buf;
        let mut ch: [&mut [f32]; 2] = [&mut l[i..end], &mut r[i..end]];
        for d in chain.iter_mut() {
            d.process(&mut ch);
        }
        i = end;
    }
}

/// Multiply by a per-sample (or constant) gain pair.
fn apply_gain(buf: &mut Stereo, g: Option<f32>, per: &[f32]) {
    match g {
        Some(1.0) => {}
        Some(g) => {
            for c in buf.iter_mut() {
                for s in c.iter_mut() {
                    *s *= g;
                }
            }
        }
        None => {
            for c in buf.iter_mut() {
                for (s, g) in c.iter_mut().zip(per) {
                    *s *= g;
                }
            }
        }
    }
}

/// Apply a pan/balance lane (values −100 … 100) with `law`.
fn apply_pan(buf: &mut Stereo, lane: &Lane, c0: i64, sr: u32, law: fn(f32) -> (f32, f32), scratch: &mut [f32]) {
    match lane.fill(c0, sr, scratch, |v| v as f32) {
        Some(p) => {
            let (gl, gr) = law(p / 100.0);
            if gl != 1.0 || gr != 1.0 {
                buf[0].iter_mut().for_each(|s| *s *= gl);
                buf[1].iter_mut().for_each(|s| *s *= gr);
            }
        }
        None => {
            for (i, p) in scratch.iter().enumerate() {
                let (gl, gr) = law(p / 100.0);
                buf[0][i] *= gl;
                buf[1][i] *= gr;
            }
        }
    }
}

struct StripOut {
    /// (route index in the plan, delayed signal)
    routes: Vec<(usize, Stereo)>,
    meter: [f32; 2],
    /// Final signal (Mix only).
    out: Option<Stereo>,
}

#[allow(clippy::too_many_arguments)]
fn run_strip(
    plan: &Plan,
    si: usize,
    st: &mut StripState,
    mut buf: Stereo,
    x0: i64,
    skip: usize,
    sr: u32,
    ovs: &HashMap<(TrackId, String), Override>,
) -> StripOut {
    let s = &plan.strips[si];
    let tr: &Track = &s.track;
    let id = tr.id;
    let n = buf[0].len();
    let my_routes: Vec<(usize, &RouteSpec)> = plan.routes.iter().enumerate().filter(|(_, r)| r.src == si).collect();
    let mut outs: Vec<(usize, Stereo)> = Vec::new();
    let mut scratch = vec![0f32; n];
    // input map / mono fold (tracks)
    let mono = tr.channels == AudioChannels::Mono;
    if s.kind != Kind::Master {
        let [l, r] = &mut buf;
        match tr.mixer.input_map {
            InputMap::Stereo => {}
            InputMap::Left => r.copy_from_slice(l),
            InputMap::Right => l.copy_from_slice(r),
            InputMap::Swap => std::mem::swap(l, r),
            InputMap::Mono => {
                for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                    let m = (*a + *b) * 0.5;
                    *a = m;
                    *b = m;
                }
            }
        }
        if mono && tr.mixer.input_map != InputMap::Mono {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let m = (*a + *b) * 0.5;
                *a = m;
                *b = m;
            }
        }
    }
    let lat_a = st.in_lat;
    run_inserts(&mut st.pre, &s.pre, &mut buf, x0, lat_a, sr);
    let lat_b = st.in_lat + st.lat_pre;
    let c_b = x0 - lat_b as i64;
    let send = |tap: Tap, buf: &Stereo, c0: i64, scratch: &mut [f32], st: &mut StripState, outs: &mut Vec<(usize, Stereo)>| {
        for (k, (ri, r)) in my_routes.iter().enumerate() {
            if r.tap != tap {
                continue;
            }
            let mut sig = buf.clone();
            if let Some(si) = r.send {
                let snd = &tr.mixer.sends[si];
                if snd.muted {
                    sig = silent(n);
                } else {
                    let lane = Lane::new(tr, id, &send_lane(si), ovs);
                    let g = lane.fill(c0, sr, scratch, db_gain);
                    apply_gain(&mut sig, g, scratch);
                    let (gl, gr) = balance((snd.pan / 100.0) as f32);
                    if gl != 1.0 || gr != 1.0 {
                        sig[0].iter_mut().for_each(|x| *x *= gl);
                        sig[1].iter_mut().for_each(|x| *x *= gr);
                    }
                }
            }
            st.delays[k].run(&mut sig);
            outs.push((*ri, sig));
        }
    };
    send(Tap::Pre, &buf, c_b, &mut scratch, st, &mut outs);
    // mute → fader
    let mute = Lane::new(tr, id, LANE_MUTE, ovs);
    match mute.fill(c_b, sr, &mut scratch, |m| if m >= 0.5 { 0.0 } else { 1.0 }) {
        Some(g) => apply_gain(&mut buf, Some(g), &[]),
        None => apply_gain(&mut buf, None, &scratch),
    }
    let vol = Lane::new(tr, id, LANE_VOLUME, ovs);
    let g = vol.fill(c_b, sr, &mut scratch, db_gain);
    apply_gain(&mut buf, g, &scratch);
    let mut meter = [0f32; 2];
    for (c, m) in meter.iter_mut().enumerate() {
        *m = buf[c][skip.min(n)..].iter().fold(0f32, |a, x| a.max(x.abs()));
    }
    run_inserts(&mut st.post, &s.post, &mut buf, x0, lat_b, sr);
    let lat_c = lat_b + st.lat_post;
    let c_c = x0 - lat_c as i64;
    send(Tap::Post, &buf, c_c, &mut scratch, st, &mut outs);
    if s.kind == Kind::Master {
        return StripOut { routes: outs, meter, out: Some(buf) };
    }
    let pan = Lane::new(tr, id, LANE_PAN, ovs);
    apply_pan(&mut buf, &pan, c_c, sr, if mono { pan_law_mono } else { balance }, &mut scratch);
    send(Tap::Out, &buf, c_c, &mut scratch, st, &mut outs);
    StripOut { routes: outs, meter, out: None }
}

/// Mix `frames` stereo samples of the sequence starting at sample `start` through the mixer graph.
/// `live` adds the UI's held controls and receives the meters.
pub fn mix_graph(project: &Project, seq: &Sequence, start: i64, frames: usize, sources: &dyn SourceProvider, live: Option<&LiveMix>) -> AudioBuffer {
    let sr = seq.settings.sample_rate.max(1);
    let ovs = live.filter(|l| l.is_active()).map(LiveMix::overrides).unwrap_or_default();
    let plan = Plan::build(seq, &ovs);
    let key = plan.structure_hash(sr);
    let taken = {
        let mut c = lock(cache());
        c.get_mut(&key).and_then(|v| v.iter().position(|g| g.next_out == start).map(|i| v.swap_remove(i)))
    };
    let (mut st, skip) = match taken {
        Some(st) => (st, 0usize),
        None => {
            let st = build_state(&plan, sr);
            let pre = plan.preroll(Tick::from_units(start, sr as i64), sr).max(st.latency);
            (st, pre)
        }
    };
    let lat = st.latency as i64;
    let x0 = start + lat - skip as i64;
    let total = skip + frames;
    let ns = plan.strips.len();
    let mut bus: Vec<Option<Stereo>> = (0..ns).map(|_| None).collect();
    let mut meters: Vec<(TrackId, [f32; 2])> = Vec::new();
    let range = filmcraft_time::TimeRange::from_bounds(Tick::from_units(x0, sr as i64), Tick::from_units(x0 + total as i64, sr as i64));
    // tracks: independent, in parallel
    let ntr = plan.strips.iter().take_while(|s| s.kind == Kind::Track).count();
    let (track_states, rest) = st.strips.split_at_mut(ntr);
    let track_outs: Vec<Option<StripOut>> = track_states
        .par_iter_mut()
        .enumerate()
        .map(|(si, ss)| {
            let s = &plan.strips[si];
            if s.silent {
                ss.delays.iter_mut().for_each(DelayLine::clear);
                return None;
            }
            let has_clips = s.track.items.iter().any(|i| i.enabled && i.range().overlaps(&range));
            if !has_clips && s.pre.is_empty() && s.post.is_empty() && ss.delays.iter().all(|d| d.buf.is_empty()) {
                return None;
            }
            let input = if has_clips {
                let b = track_input(project, &s.track, x0, total, sr, sources);
                let mut it = b.channels.into_iter();
                [it.next().unwrap_or_else(|| vec![0.0; total]), it.next().unwrap_or_else(|| vec![0.0; total])]
            } else {
                silent(total)
            };
            Some(run_strip(&plan, si, ss, input, x0, skip, sr, &ovs))
        })
        .collect();
    let deliver = |o: StripOut, si: usize, bus: &mut Vec<Option<Stereo>>, meters: &mut Vec<(TrackId, [f32; 2])>| {
        meters.push((plan.strips[si].track.id, o.meter));
        for (ri, sig) in o.routes {
            let t = plan.routes[ri].target;
            let b = bus[t].get_or_insert_with(|| silent(total));
            for c in 0..2 {
                for (d, s) in b[c].iter_mut().zip(&sig[c]) {
                    *d += *s;
                }
            }
        }
        o.out
    };
    for (si, o) in track_outs.into_iter().enumerate() {
        if let Some(o) = o {
            deliver(o, si, &mut bus, &mut meters);
        }
    }
    // submixes in order, then the Mix
    let mut out = silent(total);
    for (k, ss) in rest.iter_mut().enumerate() {
        let si = ntr + k;
        let s = &plan.strips[si];
        if s.silent {
            ss.delays.iter_mut().for_each(DelayLine::clear);
            continue;
        }
        let input = bus[si].take().unwrap_or_else(|| silent(total));
        let o = run_strip(&plan, si, ss, input, x0, skip, sr, &ovs);
        if let Some(fin) = deliver(o, si, &mut bus, &mut meters) {
            out = fin;
        }
    }
    st.next_out = start + frames as i64;
    {
        let mut c = lock(cache());
        let v = c.entry(key).or_default();
        v.push(st);
        if v.len() > 4 {
            v.remove(0);
        }
        if c.len() > 64 {
            c.clear();
        }
    }
    if let Some(l) = live {
        l.post_meters(&meters);
    }
    let [l, r] = out;
    AudioBuffer { sample_rate: sr, channels: vec![l[skip..].to_vec(), r[skip..].to_vec()] }
}
