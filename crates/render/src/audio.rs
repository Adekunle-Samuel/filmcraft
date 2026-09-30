//! Sequence audio mixdown (clip gain → Volume/Channel Volume/Panner → track volume/pan →
//! master), with audio transitions, mute/solo, speed changes (varispeed resampling).
//!
//! The dedicated `audio` crate (M7) adds the effect DSP, submixes and meters; this module is the
//! reference mixer used by playback and export.

use filmcraft_frame::AudioBuffer;
use filmcraft_project::{Project, Sequence, TrackItem};
use filmcraft_time::{Tick, TimeRange};

use crate::{SourceProvider, transitions};

pub fn db_to_gain(db: f64) -> f32 {
    if db <= -96.0 { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

/// Mix `frames` stereo samples of sequence audio starting at sample `start` (sequence rate).
pub fn mix_sequence(project: &Project, seq: &Sequence, start: i64, frames: usize, sources: &dyn SourceProvider) -> AudioBuffer {
    let sr = seq.settings.sample_rate;
    let mut out = AudioBuffer::silence(sr, 2, frames);
    let range = TimeRange::from_bounds(Tick::from_units(start, sr as i64), Tick::from_units(start + frames as i64, sr as i64));
    let any_solo = seq.audio_tracks.iter().any(|t| t.solo);
    for track in &seq.audio_tracks {
        if track.muted || (any_solo && !track.solo) {
            continue;
        }
        let mut tbuf = AudioBuffer::silence(sr, 2, frames);
        for item in track.items.iter().filter(|i| i.enabled && i.range().overlaps(&range)) {
            mix_item(project, item, start, frames, sr, sources, &mut tbuf, 1.0);
        }
        // audio transitions: attenuate the covered region of each side
        for tr in &track.transitions {
            if !tr.range().overlaps(&range) {
                continue;
            }
            // Re-mix: recompute the covered samples with crossfade gains.
            let from = tr.from.and_then(|id| track.item(id));
            let to = tr.to.and_then(|id| track.item(id));
            let s0 = tr.start.to_units_floor(sr as i64).max(start);
            let s1 = tr.end().to_units_floor(sr as i64).min(start + frames as i64);
            if s1 <= s0 {
                continue;
            }
            let n = (s1 - s0) as usize;
            let off = (s0 - start) as usize;
            for ch in tbuf.channels.iter_mut() {
                ch[off..off + n].fill(0.0);
            }
            let mut a = AudioBuffer::silence(sr, 2, n);
            let mut b = AudioBuffer::silence(sr, 2, n);
            if let Some(f) = from {
                mix_item(project, f, s0, n, sr, sources, &mut a, 1.0);
            }
            if let Some(tt) = to {
                mix_item(project, tt, s0, n, sr, sources, &mut b, 1.0);
            }
            let dur = (tr.end() - tr.start).to_units_floor(sr as i64).max(1) as f32;
            let base = (s0 - tr.start.to_units_floor(sr as i64)) as f32;
            for i in 0..n {
                let p = (base + i as f32) / dur;
                let (ga, gb) = transitions::audio_gains(&tr.effect.effect, p);
                for c in 0..2 {
                    tbuf.channels[c][off + i] += a.channels[c][i] * ga + b.channels[c][i] * gb;
                }
            }
        }
        let g = db_to_gain(track.volume_db);
        let pan = (track.pan / 100.0).clamp(-1.0, 1.0) as f32;
        let (gl, gr) = pan_gains(pan);
        out.mix_from(&tbuf, &[g * gl, g * gr]);
    }
    let mg = db_to_gain(seq.master_volume_db);
    if (mg - 1.0).abs() > 1e-6 {
        for ch in &mut out.channels {
            for s in ch.iter_mut() {
                *s *= mg;
            }
        }
    }
    out
}

/// -3 dB constant-power pan law, normalised so centre = unity.
fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    let k = std::f32::consts::SQRT_2;
    ((a.cos() * k).min(1.0), (a.sin() * k).min(1.0))
}

#[allow(clippy::too_many_arguments)]
fn mix_item(project: &Project, item: &TrackItem, start: i64, frames: usize, sr: u32, sources: &dyn SourceProvider, out: &mut AudioBuffer, extra: f32) {
    let Some(src) = sources.source(item.item) else { return };
    if !src.info().has_audio() {
        // nested sequences
        if let Some(nested) = project.sequence(item.item) {
            let rel0 = start - item.start.to_units_floor(sr as i64) + item.source_in.to_units_floor(sr as i64);
            let b = mix_sequence(project, nested, rel0, frames, sources);
            out.mix_from(&b, &[extra, extra]);
        }
        return;
    }
    let item_s0 = item.start.to_units_floor(sr as i64);
    let item_s1 = item.end().to_units_floor(sr as i64);
    let a0 = start.max(item_s0);
    let a1 = (start + frames as i64).min(item_s1);
    if a1 <= a0 {
        return;
    }
    let n = (a1 - a0) as usize;
    let speed = item.speed.abs().max(1e-6);
    let src_start_ticks = item.source_time_at(Tick::from_units(a0, sr as i64));
    let buf = if (speed - 1.0).abs() < 1e-9 && !item.reverse {
        src.audio(src_start_ticks.to_units_floor(sr as i64), n, sr).ok()
    } else {
        // varispeed: read a longer span and resample linearly
        let span = ((n as f64) * speed).ceil() as usize + 2;
        let s0 = if item.reverse { src_start_ticks.to_units_floor(sr as i64) - span as i64 } else { src_start_ticks.to_units_floor(sr as i64) };
        src.audio(s0, span, sr).ok().map(|raw| {
            let mut b = AudioBuffer::silence(sr, raw.channel_count(), n);
            for (c, ch) in raw.channels.iter().enumerate() {
                for i in 0..n {
                    let pos = if item.reverse { span as f64 - 1.0 - i as f64 * speed } else { i as f64 * speed };
                    let i0 = pos.floor().max(0.0) as usize;
                    let fr = (pos - i0 as f64) as f32;
                    let a = ch.get(i0).copied().unwrap_or(0.0);
                    let bb = ch.get(i0 + 1).copied().unwrap_or(a);
                    b.channels[c][i] = a + (bb - a) * fr;
                }
            }
            b
        })
    };
    let Some(buf) = buf else { return };
    // gains: clip gain × Volume (keyframed, per 64-sample block) × channel volume × panner
    let clip_gain = db_to_gain(item.gain_db);
    let vol = item.effect("volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let chv = item.effect("channel_volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let pan = item.effect("panner").filter(|e| e.enabled);
    let off = (a0 - start) as usize;
    let stereo_src = buf.channel_count() >= 2;
    for blk in (0..n).step_by(64) {
        let t_tl = Tick::from_units(a0 + blk as i64, sr as i64);
        let mt = item.source_time_at(t_tl);
        let v = vol.map(|e| db_to_gain(e.f64_at("level", mt))).unwrap_or(1.0);
        let (cl, cr) = chv.map(|e| (db_to_gain(e.f64_at("left", mt)), db_to_gain(e.f64_at("right", mt)))).unwrap_or((1.0, 1.0));
        let (pl, pr) = pan.map(|e| pan_gains((e.f64_at("balance", mt) / 100.0) as f32)).unwrap_or((1.0, 1.0));
        let g = clip_gain * v * extra;
        let end = (blk + 64).min(n);
        for i in blk..end {
            let l = buf.channels[0][i];
            let r = if stereo_src { buf.channels[1][i] } else { l };
            out.channels[0][off + i] += l * g * cl * pl;
            out.channels[1][off + i] += r * g * cr * pr;
        }
    }
}

/// Peak (min, max) pairs per bucket of `bucket` samples for channel `ch` — waveform display.
pub fn peaks(buf: &AudioBuffer, ch: usize, bucket: usize) -> Vec<(f32, f32)> {
    let Some(c) = buf.channels.get(ch) else { return Vec::new() };
    c.chunks(bucket.max(1)).map(|b| b.iter().fold((0f32, 0f32), |(lo, hi), s| (lo.min(*s), hi.max(*s)))).collect()
}
