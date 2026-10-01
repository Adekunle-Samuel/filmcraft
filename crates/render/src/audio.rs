//! Clip-level audio: clip gain → clip effects → Volume / Channel Volume / Panner, audio transitions
//! and speed changes (varispeed resampling), summed per track. The track/submix/Mix graph that
//! consumes it is [`crate::mixer`].

use filmcraft_frame::AudioBuffer;
use filmcraft_project::{Project, Sequence, Track, TrackItem};
use filmcraft_time::{Tick, TimeRange};

use crate::{SourceProvider, transitions};

pub fn db_to_gain(db: f64) -> f32 {
    if db <= -96.0 { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

/// Mix `frames` stereo samples of sequence audio starting at sample `start` (sequence rate), through
/// the full mixer graph ([`crate::mixer`]). Export and playback both use this.
pub fn mix_sequence(project: &Project, seq: &Sequence, start: i64, frames: usize, sources: &dyn SourceProvider) -> AudioBuffer {
    crate::mixer::mix_graph(project, seq, start, frames, sources, None)
}

/// The summed clip audio of one track (clip gain, clip effects, Volume / Channel Volume / Panner,
/// audio transitions) for sequence samples `[start, start + frames)`: the track's mixer input.
pub fn track_input(project: &Project, track: &Track, start: i64, frames: usize, sr: u32, sources: &dyn SourceProvider) -> AudioBuffer {
    let range = TimeRange::from_bounds(Tick::from_units(start, sr as i64), Tick::from_units(start + frames as i64, sr as i64));
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
    tbuf
}

/// Balance for stereo signals (clip Panner, stereo track pan): centre = unity on both sides, turning
/// one way attenuates the other side along the −3 dB constant-power curve (normalised so the
/// centre is 0 dB); hard left/right silences the opposite side.
pub fn pan_gains(pan: f32) -> (f32, f32) {
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
    if item.essential.as_ref().is_some_and(|e| e.mute) {
        return;
    }
    let n = (a1 - a0) as usize;
    let buf = effected(item, src.as_ref(), a0, n, sr);
    // gains: clip gain × Volume (keyframed, per 64-sample block) × channel volume × panner
    let clip_gain = db_to_gain(item.gain_db);
    let vol = item.effect("volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let chv = item.effect("channel_volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false));
    let pan = item.effect("panner").filter(|e| e.enabled);
    let off = (a0 - start) as usize;
    // Gains change on an absolute 64-sample grid, so the output does not depend on how callers cut
    // the timeline into requests.
    let mut blk = 0usize;
    while blk < n {
        let t_tl = Tick::from_units((a0 + blk as i64).div_euclid(64) * 64, sr as i64);
        let mt = item.source_time_at(t_tl);
        let v = vol.map(|e| db_to_gain(e.f64_at("level", mt))).unwrap_or(1.0);
        let (cl, cr) = chv.map(|e| (db_to_gain(e.f64_at("left", mt)), db_to_gain(e.f64_at("right", mt)))).unwrap_or((1.0, 1.0));
        let (pl, pr) = pan.map(|e| pan_gains((e.f64_at("balance", mt) / 100.0) as f32)).unwrap_or((1.0, 1.0));
        let g = clip_gain * v * extra;
        let end = (((a0 + blk as i64).div_euclid(64) + 1) * 64 - a0).min(n as i64) as usize;
        for i in blk..end {
            let (l, r) = (buf[0][i], buf[1][i]);
            out.channels[0][off + i] += l * g * cl * pl;
            out.channels[1][off + i] += r * g * cr * pr;
        }
        blk = end;
    }
}

/// The clip's audio after speed/reverse and clip effects (before clip gain, Volume and Panner) for
/// timeline samples `[a0, a0 + n)`.
fn effected(item: &TrackItem, src: &dyn filmcraft_media::MediaSource, a0: i64, n: usize, sr: u32) -> [Vec<f32>; 2] {
    let read = |x0: i64, len: usize| raw_stereo(item, src, x0, len, sr);
    if crate::audio_fx::has_effects(item) { crate::audio_fx::process(item, a0, n, sr, &read) } else { read(a0, n) }
}

/// One clip's own signal for timeline samples `[start, start + frames)` (sequence rate): clip gain
/// and clip effects, without Volume / Channel Volume / Panner, transitions or Mute (silence outside
/// the clip). Loudness Auto-Match and ducking analyse this. `None` when the clip has no audio
/// source (nested sequences are not analysed).
pub fn clip_signal(item: &TrackItem, start: i64, frames: usize, sr: u32, sources: &dyn SourceProvider) -> Option<[Vec<f32>; 2]> {
    let src = sources.source(item.item)?;
    if !src.info().has_audio() {
        return None;
    }
    let mut out = [vec![0.0f32; frames], vec![0.0f32; frames]];
    let a0 = start.max(item.start.to_units_floor(sr as i64));
    let a1 = (start + frames as i64).min(item.end().to_units_floor(sr as i64));
    if a1 > a0 {
        let n = (a1 - a0) as usize;
        let off = (a0 - start) as usize;
        let g = db_to_gain(item.gain_db);
        let buf = effected(item, src.as_ref(), a0, n, sr);
        for (dst, b) in out.iter_mut().zip(&buf) {
            for (d, x) in dst[off..off + n].iter_mut().zip(b) {
                *d = x * g;
            }
        }
    }
    Some(out)
}

/// The clip's audio (after speed/reverse) for timeline samples `[x0, x0 + len)` as stereo, with
/// silence outside the clip. Mono sources are duplicated to both channels.
fn raw_stereo(item: &TrackItem, src: &dyn filmcraft_media::MediaSource, x0: i64, len: usize, sr: u32) -> [Vec<f32>; 2] {
    let mut out = [vec![0.0f32; len], vec![0.0f32; len]];
    let item_s0 = item.start.to_units_floor(sr as i64);
    let item_s1 = item.end().to_units_floor(sr as i64);
    let a0 = x0.max(item_s0);
    let a1 = (x0 + len as i64).min(item_s1);
    if a1 <= a0 {
        return out;
    }
    let n = (a1 - a0) as usize;
    let off = (a0 - x0) as usize;
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
    let Some(buf) = buf else { return out };
    let right = if buf.channel_count() >= 2 { 1 } else { 0 };
    for (c, dst) in out.iter_mut().enumerate() {
        let ch = &buf.channels[if c == 0 { 0 } else { right }];
        let m = n.min(ch.len());
        dst[off..off + m].copy_from_slice(&ch[..m]);
    }
    out
}

/// Peak (min, max) pairs per bucket of `bucket` samples for channel `ch` — waveform display.
pub fn peaks(buf: &AudioBuffer, ch: usize, bucket: usize) -> Vec<(f32, f32)> {
    let Some(c) = buf.channels.get(ch) else { return Vec::new() };
    c.chunks(bucket.max(1)).map(|b| b.iter().fold((0f32, 0f32), |(lo, hi), s| (lo.min(*s), hi.max(*s)))).collect()
}
