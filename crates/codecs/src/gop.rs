//! GOP-aware random access shared by the container sources (MP4/MOV, Matroska/WebM).
//!
//! Seeking: find the sample whose presentation interval covers the requested time, decode forward
//! from the preceding sync sample, and cache every decoded frame (keyed by pts). Playback requests
//! for the next frames therefore hit the cache or continue the running decoder without re-seeking.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use filmcraft_color::ColorInfo;
use filmcraft_frame::VideoFrame;

use crate::CodecError;
use crate::video::VideoDecoder;

/// A container's video sample table, in decode (file) order.
pub trait VideoSamples {
    fn count(&self) -> usize;
    /// Presentation timestamp of sample `i` (track units).
    fn pts(&self, i: usize) -> i64;
    /// Nearest sync sample at or before `i`.
    fn sync_before(&self, i: usize) -> usize;
    /// The sample presented at `t` (track units), if any.
    fn sample_at(&self, t: i64) -> Option<usize>;
    fn read(&self, i: usize) -> crate::Result<Vec<u8>>;
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>>;
}

struct State {
    decoder: Option<Box<dyn VideoDecoder>>,
    /// Next sample (decode order) to feed.
    next: usize,
    /// Decoded frames by presentation pts (bounded).
    frames: BTreeMap<i64, Arc<VideoFrame>>,
    bytes: usize,
    /// The shared decoder is out decoding (see [`GopCache::frame`]).
    busy: bool,
}

/// Decoder + decoded-frame cache for one video track.
pub struct GopCache {
    state: Mutex<State>,
    /// Colour signalled by the container, which wins over the bitstream's.
    explicit_color: Option<ColorInfo>,
    budget: usize,
}

impl GopCache {
    pub fn new(explicit_color: Option<ColorInfo>) -> Self {
        Self { state: Mutex::new(State { decoder: None, next: usize::MAX, frames: BTreeMap::new(), bytes: 0, busy: false }), explicit_color, budget: 384 << 20 }
    }

    fn store(&self, st: &mut State, pts: i64, mut f: VideoFrame) {
        if let Some(c) = self.explicit_color
            && !matches!(f.data, filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_))
        {
            f.color = c;
        }
        st.bytes += f.byte_size();
        st.frames.insert(pts, Arc::new(f));
        // evict frames far from the most recent (keep a window around the working position)
        while st.bytes > self.budget && st.frames.len() > 2 {
            let first = *st.frames.keys().next().expect("non-empty");
            let last = *st.frames.keys().next_back().expect("non-empty");
            let victim = if pts - first > last - pts { first } else { last };
            if let Some(v) = st.frames.remove(&victim) {
                st.bytes -= v.byte_size();
            }
        }
    }

    /// The frame presented at `target` (track units, clamped to the stream).
    ///
    /// The decoder is taken out of the shared state while it decodes, so the lock is never held
    /// across a decode: decoders may run slices on rayon, and a rayon worker waiting in a decode
    /// can pick up another render job that asks this same source for a frame. A request that
    /// finds the shared decoder busy decodes with a private decoder instead of waiting.
    pub fn frame(&self, s: &dyn VideoSamples, target: i64) -> crate::Result<Arc<VideoFrame>> {
        let n = s.count();
        let i = s.sample_at(target.max(0)).or_else(|| (n > 0).then(|| n - 1)).ok_or_else(|| CodecError::Decode("empty track".into()))?;
        let want_pts = s.pts(i);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = st.frames.get(&want_pts) {
            return Ok(f.clone());
        }
        let shared = !st.busy;
        let (mut dec, mut next) = if shared {
            st.busy = true;
            (st.decoder.take(), st.next)
        } else {
            (None, usize::MAX)
        };
        drop(st);
        let decoded = (|| -> crate::Result<Vec<crate::video::DecodedFrame>> {
            if dec.is_none() {
                dec = Some(s.make_decoder()?);
                next = usize::MAX;
            }
            let d = dec.as_mut().expect("decoder");
            Self::decode_to(s, d.as_mut(), &mut next, i, want_pts, n)
        })();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if shared {
            st.busy = false;
            st.decoder = dec;
            st.next = if decoded.is_ok() { next } else { usize::MAX };
        }
        let out = decoded?;
        for d in out {
            self.store(&mut st, d.pts, d.frame);
        }
        // nearest decoded frame at or before the wanted pts (robust to decoder pts quirks)
        st.frames
            .get(&want_pts)
            .cloned()
            .or_else(|| st.frames.range(..=want_pts).next_back().map(|(_, f)| f.clone()))
            .ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }

    /// Decode with `d` (positioned at `next`) until the picture with `want_pts` (sample `i`) comes out.
    fn decode_to(
        s: &dyn VideoSamples,
        d: &mut dyn VideoDecoder,
        next: &mut usize,
        i: usize,
        want_pts: i64,
        n: usize,
    ) -> crate::Result<Vec<crate::video::DecodedFrame>> {
        let mut key = s.sync_before(i);
        // Continue the running decoder when the wanted sample is ahead within this GOP run.
        let continuing = *next != usize::MAX && *next > key && *next <= i + 16 && *next <= n;
        if !continuing {
            // The container's sync flags may be wrong for the codec (an MP4 without `stss` marks
            // every sample): step back to a sample the decoder can start from.
            while key > 0 {
                let data = s.read(key)?;
                if d.is_random_access(&data) != Some(false) {
                    break;
                }
                key = s.sync_before(key - 1);
            }
            d.reset();
            *next = key;
        }
        let limit = (i + 64).min(n);
        let mut out = Vec::new();
        let mut found = false;
        while *next < limit {
            let k = *next;
            let data = s.read(k)?;
            let pics = d.decode(&data, s.pts(k))?;
            *next += 1;
            found |= pics.iter().any(|p| p.pts == want_pts);
            out.extend(pics);
            if found {
                break;
            }
        }
        if !found {
            out.extend(d.flush());
            *next = usize::MAX;
        }
        Ok(out)
    }
}
