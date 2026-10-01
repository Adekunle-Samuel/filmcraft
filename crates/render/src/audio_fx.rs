//! Clip audio effects: project effect instances → `filmcraft-audio-dsp` processors.
//!
//! The mixer is pull-based (any sample range, any order), but most audio effects are stateful
//! (filters, dynamics, delay/reverb tails). A small cache keeps processed effect chains per clip
//! keyed by the next timeline sample they expect: sequential consumers (playback, export, meters)
//! continue their chain; any other request starts a fresh chain with enough pre-roll for the
//! chain's tails to build up. Latency (limiter look-ahead, STFT effects) is compensated by feeding
//! the chain ahead of its output.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use filmcraft_audio_dsp::AudioEffect;
use filmcraft_project::{EffectInstance, TrackItem};
use filmcraft_time::Tick;

/// How a project effect maps onto a DSP effect.
struct Mapping {
    dsp: &'static str,
    /// Pre-roll (seconds) for a fresh chain; closures get the effect instance at the start time.
    preroll: fn(&EffectInstance, Tick) -> f64,
    /// Set DSP parameters from the (keyframed) project parameters at media time `mt`.
    apply: fn(&mut dyn AudioEffect, &EffectInstance, Tick),
}

fn ignore(_: bool) {}

fn f(e: &EffectInstance, id: &str, mt: Tick) -> f32 {
    e.f64_at(id, mt) as f32
}

fn mapping(id: &str) -> Option<Mapping> {
    Some(match id {
        "amplify" => Mapping { dsp: "amplify", preroll: |_, _| 0.0, apply: |d, e, t| ignore(d.set_param("gain", f(e, "gain", t))) },
        "dynamics" => Mapping {
            dsp: "compressor",
            preroll: |e, t| (f(e, "release", t) as f64 / 1000.0 * 5.0).clamp(0.05, 3.0),
            apply: |d, e, t| {
                d.set_param("threshold", f(e, "threshold", t));
                d.set_param("ratio", f(e, "ratio", t));
                d.set_param("attack", f(e, "attack", t));
                d.set_param("release", f(e, "release", t));
            },
        },
        "hard_limiter" => Mapping {
            dsp: "limiter",
            preroll: |e, t| (f(e, "release", t) as f64 / 1000.0 * 5.0).clamp(0.05, 2.0),
            apply: |d, e, t| {
                d.set_param("ceiling", f(e, "max", t));
                d.set_param("input_gain", f(e, "boost", t).max(0.0));
                d.set_param("release", f(e, "release", t));
            },
        },
        "delay" => Mapping {
            dsp: "delay",
            preroll: |e, t| {
                // enough echoes to decay by ~80 dB
                let time = f(e, "delay", t).max(0.001) as f64;
                let fb = (f(e, "feedback", t) as f64 / 100.0).clamp(0.0, 0.95);
                let repeats = if fb > 0.001 { (-4.0 / fb.log10()).clamp(1.0, 60.0) } else { 1.0 };
                (time * repeats).min(8.0)
            },
            apply: |d, e, t| {
                d.set_param("time", f(e, "delay", t) * 1000.0);
                d.set_param("feedback", f(e, "feedback", t));
                d.set_param("mix", f(e, "mix", t));
            },
        },
        "parametric_eq" => Mapping {
            dsp: "simple_eq",
            preroll: |_, _| 0.05,
            apply: |d, e, t| {
                d.set_param("low_freq", f(e, "low_freq", t));
                d.set_param("low", f(e, "low_gain", t));
                d.set_param("mid_freq", f(e, "mid_freq", t));
                d.set_param("mid", f(e, "mid_gain", t));
                d.set_param("mid_q", f(e, "mid_q", t));
                d.set_param("high_freq", f(e, "high_freq", t));
                d.set_param("high", f(e, "high_gain", t));
            },
        },
        // Single-band filters via band 1 of the parametric EQ (types: see FILTER_TYPE_NAMES).
        "highpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 4.0, f(e, "cutoff", t), 0.707) },
        "lowpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 3.0, f(e, "cutoff", t), 0.707) },
        "bandpass" => Mapping { dsp: "parametric_eq", preroll: |_, _| 0.05, apply: |d, e, t| band(d, 6.0, f(e, "center", t), f(e, "q", t)) },
        "denoise" => Mapping { dsp: "denoise", preroll: |_, _| 1.0, apply: |d, e, t| ignore(d.set_param("amount", f(e, "amount", t))) },
        "dehummer" => Mapping {
            dsp: "dehum",
            preroll: |_, _| 0.2,
            apply: |d, e, t| ignore(d.set_param("frequency", e.param("freq").and_then(|p| p.value_at(t).as_f64()).unwrap_or(1.0) as f32)),
        },
        "studio_reverb" => Mapping {
            dsp: "reverb",
            preroll: |e, t| 0.3 * 30f64.powf(f(e, "decay", t) as f64 / 100.0).min(8.0),
            apply: |d, e, t| {
                d.set_param("size", f(e, "room", t));
                d.set_param("decay", (0.3 * 30f32.powf(f(e, "decay", t) / 100.0)).clamp(0.1, 20.0));
                d.set_param("damping", f(e, "damping", t));
                let (dry, wet) = (f(e, "dry", t).max(0.0), f(e, "wet", t).max(0.0));
                d.set_param("mix", if dry + wet > 0.0 { wet / (dry + wet) * 100.0 } else { 0.0 });
            },
        },
        "invert_a" => Mapping { dsp: "invert", preroll: |_, _| 0.0, apply: |_, _, _| {} },
        "pitch_shifter" => Mapping {
            dsp: "pitch_shifter",
            preroll: |_, _| 0.1,
            apply: |d, e, t| ignore(d.set_param("semitones", (f(e, "semitones", t) + f(e, "cents", t) / 100.0).clamp(-12.0, 12.0))),
        },
        _ => return None,
    })
}

fn band(d: &mut dyn AudioEffect, kind: f32, freq: f32, q: f32) {
    d.set_param("b1.on", 1.0);
    d.set_param("b1.type", kind);
    d.set_param("b1.freq", freq);
    d.set_param("b1.q", q);
}

/// The item's enabled, DSP-backed audio effects in rack order.
fn active(item: &TrackItem) -> Vec<(&EffectInstance, Mapping)> {
    item.effects.iter().filter(|e| e.enabled && !e.def().is_some_and(|d| d.intrinsic)).filter_map(|e| mapping(&e.effect).map(|m| (e, m))).collect()
}

/// Whether the item has audio effects that change its signal.
pub fn has_effects(item: &TrackItem) -> bool {
    !active(item).is_empty()
}

struct Chain {
    fx: Vec<Box<dyn AudioEffect>>,
    latency: usize,
    /// Next timeline sample this chain will output.
    next_out: i64,
}

type Key = (u64, u64, u32);

fn cache() -> &'static Mutex<HashMap<Key, Vec<Chain>>> {
    static C: OnceLock<Mutex<HashMap<Key, Vec<Chain>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn structure_hash(item: &TrackItem) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (e, m) in active(item) {
        e.effect.hash(&mut h);
        m.dsp.hash(&mut h);
    }
    h.finish()
}

const BLOCK: usize = 256;

/// Run `input` (stereo, `input[c].len()` samples starting at timeline sample `in0`) through the
/// chain, setting keyframed parameters per block.
fn run(chain: &mut Chain, item: &TrackItem, in0: i64, input: &mut [Vec<f32>; 2], sr: u32) {
    let act = active(item);
    let n = input[0].len();
    let mut i = 0;
    while i < n {
        let end = (i + BLOCK).min(n);
        let mt = item.source_time_at(Tick::from_units(in0 + i as i64, sr as i64));
        for ((e, m), d) in act.iter().zip(chain.fx.iter_mut()) {
            (m.apply)(d.as_mut(), e, mt);
        }
        let [l, r] = input;
        let mut chans: [&mut [f32]; 2] = [&mut l[i..end], &mut r[i..end]];
        for d in chain.fx.iter_mut() {
            d.process(&mut chans);
        }
        i = end;
    }
}

/// Process the item's audio for timeline samples `[a0, a0 + n)`. `read(x0, len)` returns the clip's
/// raw stereo audio for timeline samples `[x0, x0 + len)` (silence outside the clip).
pub fn process(item: &TrackItem, a0: i64, n: usize, sr: u32, read: &dyn Fn(i64, usize) -> [Vec<f32>; 2]) -> [Vec<f32>; 2] {
    let key = (item.id.0, structure_hash(item), sr);
    let taken = {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.get_mut(&key).and_then(|v| v.iter().position(|ch| ch.next_out == a0).map(|i| v.swap_remove(i)))
    };
    let (mut chain, mut input, skip) = match taken {
        Some(chain) => {
            let l = chain.latency as i64;
            (chain, read(a0 + l, n), 0usize)
        }
        None => {
            let act = active(item);
            let mt = item.source_time_at(Tick::from_units(a0, sr as i64));
            let pre_s = act.iter().map(|(e, m)| (m.preroll)(e, mt)).fold(0.0, f64::max);
            let pre = (pre_s * sr as f64).ceil() as usize;
            let mut fx: Vec<Box<dyn AudioEffect>> = Vec::new();
            for (_, m) in &act {
                if let Some(d) = filmcraft_audio_dsp::create_effect(m.dsp, sr as f32, 2) {
                    fx.push(d);
                }
            }
            let latency = fx.iter().map(|d| d.latency()).sum::<usize>();
            let chain = Chain { fx, latency, next_out: a0 - pre as i64 };
            let x0 = a0 - pre as i64;
            (chain, read(x0 + latency as i64, pre + n), pre)
        }
    };
    let in0 = chain.next_out + chain.latency as i64;
    run(&mut chain, item, in0, &mut input, sr);
    chain.next_out = a0 + n as i64;
    let out = [input[0][skip..].to_vec(), input[1][skip..].to_vec()];
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let v = c.entry(key).or_default();
    v.push(chain);
    if v.len() > 4 {
        v.remove(0);
    }
    if c.len() > 256 {
        c.clear();
    }
    out
}
