//! GOP-aware random access shared by the container sources (MP4/MOV, Matroska/WebM).
//!
//! Seeking: find the sample whose presentation interval covers the requested time, decode forward
//! from the preceding sync sample, and cache every decoded frame (keyed by pts). Playback requests
//! for the next frames therefore hit the cache or continue the running decoder without re-seeking.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use filmcraft_color::ColorInfo;
use filmcraft_frame::VideoFrame;

use crate::CodecError;
use crate::video::VideoDecoder;

/// Process-wide decode counters of every [`GopCache`] (benchmarks and diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GopStats {
    /// Requests answered from the decoded-frame cache.
    pub hits: u64,
    /// Requests that had to decode.
    pub misses: u64,
    /// Decoder restarts at a sync sample (seeks).
    pub seeks: u64,
    /// Samples fed to decoders.
    pub decoded: u64,
    /// Decoded frames evicted from the cache.
    pub evicted: u64,
}

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static SEEKS: AtomicU64 = AtomicU64::new(0);
static DECODED: AtomicU64 = AtomicU64::new(0);
static EVICTED: AtomicU64 = AtomicU64::new(0);

/// The counters so far (they only grow; subtract two snapshots to measure an interval).
pub fn gop_stats() -> GopStats {
    GopStats {
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        seeks: SEEKS.load(Ordering::Relaxed),
        decoded: DECODED.load(Ordering::Relaxed),
        evicted: EVICTED.load(Ordering::Relaxed),
    }
}

impl std::ops::Sub for GopStats {
    type Output = GopStats;
    fn sub(self, o: GopStats) -> GopStats {
        GopStats {
            hits: self.hits - o.hits,
            misses: self.misses - o.misses,
            seeks: self.seeks - o.seeks,
            decoded: self.decoded - o.decoded,
            evicted: self.evicted - o.evicted,
        }
    }
}

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
        Self { state: Mutex::new(State { decoder: None, next: usize::MAX, frames: BTreeMap::new(), bytes: 0 }), explicit_color, budget: 384 << 20 }
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
                EVICTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The frame presented at `target` (track units, clamped to the stream).
    pub fn frame(&self, s: &dyn VideoSamples, target: i64) -> crate::Result<Arc<VideoFrame>> {
        let n = s.count();
        let i = s.sample_at(target.max(0)).or_else(|| (n > 0).then(|| n - 1)).ok_or_else(|| CodecError::Decode("empty track".into()))?;
        let want_pts = s.pts(i);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = st.frames.get(&want_pts) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(f.clone());
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        if st.decoder.is_none() {
            st.decoder = Some(s.make_decoder()?);
            st.next = usize::MAX;
        }
        let key = s.sync_before(i);
        // Continue the running decoder when the wanted sample is ahead within this GOP run.
        let continuing = st.next != usize::MAX && st.next > key && st.next <= i + 16 && st.next <= n;
        if !continuing {
            SEEKS.fetch_add(1, Ordering::Relaxed);
            if let Some(d) = st.decoder.as_mut() {
                d.reset();
            }
            st.next = key;
        }
        let limit = (i + 64).min(n);
        while st.next < limit {
            let k = st.next;
            let data = s.read(k)?;
            let out = st.decoder.as_mut().expect("decoder").decode(&data, s.pts(k))?;
            st.next += 1;
            DECODED.fetch_add(1, Ordering::Relaxed);
            for d in out {
                self.store(&mut st, d.pts, d.frame);
            }
            if st.frames.contains_key(&want_pts) {
                break;
            }
        }
        if !st.frames.contains_key(&want_pts) {
            let out = st.decoder.as_mut().expect("decoder").flush();
            for d in out {
                self.store(&mut st, d.pts, d.frame);
            }
            st.next = usize::MAX;
        }
        // nearest decoded frame at or before the wanted pts (robust to decoder pts quirks)
        st.frames
            .get(&want_pts)
            .cloned()
            .or_else(|| st.frames.range(..=want_pts).next_back().map(|(_, f)| f.clone()))
            .ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }
}
