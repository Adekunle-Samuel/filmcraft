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
    /// The decoder only produces intra pictures: frames decode independently and in parallel.
    intra: bool,
    /// Idle decoders for parallel intra decoding.
    spare: Vec<Box<dyn VideoDecoder>>,
    /// Next sample (decode order) to feed, and the sync sample the current run started from.
    next: usize,
    start: usize,
    /// Highest pts the running decoder has output since it started (pictures leave in pts order).
    out_max: i64,
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

/// The cache always has room for this many frames, whatever their size: a frame-threaded decoder
/// runs up to ~2x its thread count ahead of the frame it returns, and playback prefetches ahead
/// of the playhead, so a byte budget alone would evict 4K frames before they are shown and force
/// a re-decode from the keyframe.
const MIN_FRAMES: usize = 64;

/// A running decoder up to this many samples before the wanted sample's sync sample keeps going
/// rather than restarting at the sync sample.
const CONTINUE_THROUGH: usize = 48;

impl GopCache {
    pub fn new(explicit_color: Option<ColorInfo>) -> Self {
        Self {
            state: Mutex::new(State {
                decoder: None,
                intra: false,
                spare: Vec::new(),
                next: usize::MAX,
                start: 0,
                out_max: i64::MIN,
                frames: BTreeMap::new(),
                bytes: 0,
            }),
            explicit_color,
            budget: 384 << 20,
        }
    }

    fn store(&self, st: &mut State, pts: i64, mut f: VideoFrame) {
        if let Some(c) = self.explicit_color
            && !matches!(f.data, filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_))
        {
            f.color = c;
        }
        let budget = self.budget.max(MIN_FRAMES * f.byte_size());
        st.bytes += f.byte_size();
        if let Some(old) = st.frames.insert(pts, Arc::new(f)) {
            st.bytes -= old.byte_size();
        }
        // evict frames far from the most recent (keep a window around the working position)
        while st.bytes > budget && st.frames.len() > 2 {
            let first = *st.frames.keys().next().expect("non-empty");
            let last = *st.frames.keys().next_back().expect("non-empty");
            let victim = if pts - first > last - pts { first } else { last };
            if let Some(v) = st.frames.remove(&victim) {
                st.bytes -= v.byte_size();
                EVICTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Store decoder output (in presentation order) and advance `out_max`.
    fn store_output(&self, st: &mut State, out: Vec<crate::video::DecodedFrame>) {
        for d in out {
            st.out_max = st.out_max.max(d.pts);
            self.store(st, d.pts, d.frame);
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
            let d = s.make_decoder()?;
            st.intra = d.intra_only();
            st.decoder = Some(d);
            st.next = usize::MAX;
        }
        // Nothing cached: the work ahead (possibly a seek and a GOP of decoding) is only worth it
        // while someone still wants the frame.
        if filmcraft_media::cancel::cancelled() {
            return Err(CodecError::Cancelled);
        }
        if st.intra {
            return self.intra_frame(st, s, i, want_pts);
        }
        let key = s.sync_before(i);
        // Continue the running decoder when it has passed the wanted sample's sync sample and
        // either has not reached the sample yet or has been fed it without outputting it yet
        // (a frame-threaded decoder holds many pictures in flight). Otherwise the frame was
        // evicted or lies in another GOP: restart at the sync sample.
        let running = st.next != usize::MAX && st.next <= n;
        // Decoding on through a short stretch into the next GOP is cheaper than a restart, and
        // playback wants those frames anyway.
        let near = st.next <= key && key - st.next <= CONTINUE_THROUGH;
        let continuing = running && (st.next > key || near) && (i >= st.next || (st.start <= i && want_pts > st.out_max));
        if !continuing {
            SEEKS.fetch_add(1, Ordering::Relaxed);
            if let Some(d) = st.decoder.as_mut() {
                d.reset();
            }
            st.next = key;
            st.start = key;
            st.out_max = i64::MIN;
        }
        let limit = (i.max(st.next) + 64).min(n);
        while st.next < limit {
            if filmcraft_media::cancel::cancelled() {
                // The decoder state stays consistent (`next`, `out_max`): a later request continues.
                return Err(CodecError::Cancelled);
            }
            let k = st.next;
            let data = s.read(k)?;
            let out = st.decoder.as_mut().expect("decoder").decode(&data, s.pts(k))?;
            st.next += 1;
            DECODED.fetch_add(1, Ordering::Relaxed);
            self.store_output(&mut st, out);
            if st.frames.contains_key(&want_pts) {
                break;
            }
        }
        if !st.frames.contains_key(&want_pts) {
            let out = st.decoder.as_mut().expect("decoder").flush();
            self.store_output(&mut st, out);
            st.next = usize::MAX;
        }
        // nearest decoded frame at or before the wanted pts (robust to decoder pts quirks)
        st.frames
            .get(&want_pts)
            .cloned()
            .or_else(|| st.frames.range(..=want_pts).next_back().map(|(_, f)| f.clone()))
            .ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }

    /// Intra-only streams: decode the one sample outside the lock, so several frame workers
    /// decode different frames of the same source at once.
    fn intra_frame(&self, st: std::sync::MutexGuard<'_, State>, s: &dyn VideoSamples, i: usize, want_pts: i64) -> crate::Result<Arc<VideoFrame>> {
        let mut st = st;
        let spare = st.spare.pop();
        drop(st);
        let mut dec = match spare {
            Some(d) => d,
            None => s.make_decoder()?,
        };
        let res = s.read(i).and_then(|data| {
            DECODED.fetch_add(1, Ordering::Relaxed);
            dec.decode(&data, want_pts)
        });
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.spare.len() < 16 {
            st.spare.push(dec);
        }
        for d in res? {
            self.store(&mut st, d.pts, d.frame);
        }
        st.frames
            .get(&want_pts)
            .cloned()
            .or_else(|| st.frames.range(..=want_pts).next_back().map(|(_, f)| f.clone()))
            .ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::DecodedFrame;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    /// `n` samples, a sync sample every `gop`, pts = 1000 · index (no reordering).
    struct Samples {
        n: usize,
        gop: usize,
        delay: usize,
        intra: bool,
        resets: Arc<AtomicUsize>,
        decodes: Arc<AtomicUsize>,
    }

    /// Outputs each picture `delay` samples late, like a frame-threaded decoder.
    struct Dec {
        delay: usize,
        intra: bool,
        held: VecDeque<i64>,
        resets: Arc<AtomicUsize>,
        decodes: Arc<AtomicUsize>,
    }

    fn picture(pts: i64) -> DecodedFrame {
        let i = (pts / 1000) as u32;
        DecodedFrame { pts, frame: VideoFrame::rgba8(1, 1, vec![i as u8, (i >> 8) as u8, 0, 255]) }
    }

    impl VideoDecoder for Dec {
        fn decode(&mut self, _sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
            self.decodes.fetch_add(1, Ordering::Relaxed);
            self.held.push_back(pts);
            let mut out = Vec::new();
            while self.held.len() > self.delay {
                out.push(picture(self.held.pop_front().expect("held")));
            }
            Ok(out)
        }
        fn flush(&mut self) -> Vec<DecodedFrame> {
            self.held.drain(..).map(picture).collect()
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::Relaxed);
            self.held.clear();
        }
        fn name(&self) -> &str {
            "test"
        }
        fn intra_only(&self) -> bool {
            self.intra
        }
    }

    impl VideoSamples for Samples {
        fn count(&self) -> usize {
            self.n
        }
        fn pts(&self, i: usize) -> i64 {
            i as i64 * 1000
        }
        fn sync_before(&self, i: usize) -> usize {
            if self.intra { i } else { i / self.gop * self.gop }
        }
        fn sample_at(&self, t: i64) -> Option<usize> {
            let i = (t / 1000) as usize;
            (i < self.n).then_some(i)
        }
        fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
            Ok(vec![i as u8])
        }
        fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
            Ok(Box::new(Dec { delay: self.delay, intra: self.intra, held: VecDeque::new(), resets: self.resets.clone(), decodes: self.decodes.clone() }))
        }
    }

    fn samples(n: usize, gop: usize, delay: usize, intra: bool) -> Samples {
        Samples { n, gop, delay, intra, resets: Default::default(), decodes: Default::default() }
    }

    fn index_of(f: &VideoFrame) -> usize {
        match &f.data {
            filmcraft_frame::PixelData::Rgba8(d) => d[0] as usize | (d[1] as usize) << 8,
            _ => unreachable!(),
        }
    }

    #[test]
    fn frames_held_by_a_threaded_decoder_do_not_restart_it() {
        // 24 pictures in flight (more than any fixed look-ahead margin), one long GOP.
        let s = samples(300, 250, 24, false);
        let c = GopCache::new(None);
        // Playback order with prefetch running ahead: a later frame first, then earlier ones the
        // decoder has been fed but not output yet.
        for i in [10usize, 11, 40, 12, 30, 13, 60, 14, 15] {
            assert_eq!(index_of(&c.frame(&s, i as i64 * 1000).expect("frame")), i);
        }
        assert_eq!(s.resets.load(Ordering::Relaxed), 1, "only the first request seeks");
        assert!(s.decodes.load(Ordering::Relaxed) <= 60 + 24 + 1);
    }

    #[test]
    fn running_decoder_continues_through_a_nearby_sync_sample() {
        let s = samples(200, 30, 0, false);
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, 25_000).expect("frame")), 25);
        // frame 40 lies in the next GOP (sync sample 30): decode 26..40 instead of restarting at 30
        assert_eq!(index_of(&c.frame(&s, 40_000).expect("frame")), 40);
        assert_eq!(index_of(&c.frame(&s, 28_000).expect("frame")), 28);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
        // far ahead: restart at the sync sample rather than decode everything in between
        assert_eq!(index_of(&c.frame(&s, 150_000).expect("frame")), 150);
        assert_eq!(s.resets.load(Ordering::Relaxed), 2);
        assert!(s.decodes.load(Ordering::Relaxed) <= 41 + 1);
    }

    #[test]
    fn intra_frames_decode_in_parallel_once_each() {
        let s = Arc::new(samples(64, 1, 0, true));
        let c = Arc::new(GopCache::new(None));
        std::thread::scope(|scope| {
            for t in 0..4usize {
                let (s, c) = (s.clone(), c.clone());
                scope.spawn(move || {
                    for k in 0..64usize {
                        let i = (k * 7 + t * 16) % 64;
                        assert_eq!(index_of(&c.frame(&*s, i as i64 * 1000).expect("frame")), i);
                    }
                });
            }
        });
        // a frame two threads miss at the same moment may decode twice; nothing more
        assert!(s.decodes.load(Ordering::Relaxed) <= 64 + 4 * 4);
        assert_eq!(s.resets.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancelled_request_stops_decoding_and_keeps_state() {
        let s = samples(300, 250, 0, false);
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, 5_000).expect("frame")), 5);
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let r = filmcraft_media::cancel::with_cancel(&flag, || c.frame(&s, 200_000));
        assert!(matches!(r, Err(CodecError::Cancelled)));
        assert_eq!(s.decodes.load(Ordering::Relaxed), 6);
        // cached frames are still served, and the decoder carries on from where it was
        let r = filmcraft_media::cancel::with_cancel(&flag, || c.frame(&s, 3_000));
        assert_eq!(index_of(&r.expect("cached")), 3);
        assert_eq!(index_of(&c.frame(&s, 7_000).expect("frame")), 7);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
    }
}
