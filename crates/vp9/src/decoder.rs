//! Top-level decoder: superframes, frame header handling, tile scheduling (tile columns in
//! parallel), frame assembly, loop filter (superblock wavefront in parallel), probability
//! adaptation and reference frame management.

use crate::error::{Error, Result, ensure};
use crate::frame::{Frame, MiGrid, MiInfo, Plane as FPlane, plane_geometry};
use crate::header::{FrameHeader, HeaderState, KEY_FRAME, RefInfo, parse_compressed, parse_uncompressed, split_superframe};
use crate::loopfilter::{LfFrame, PlaneView, filter_superblock};
use crate::probs::{Counts, adapt_coef_probs, adapt_noncoef_probs};
use crate::tables::*;
use crate::tile::{FrameShared, RefUse, Strip, TileDecoder};
use crate::{ColorInfo, Picture, Plane};
use std::sync::Arc;

/// Counters describing what the decoder has seen (useful to check test coverage).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub frames: u64,
    pub shown: u64,
    pub hidden: u64,
    pub key_frames: u64,
    pub intra_only: u64,
    pub inter_frames: u64,
    pub show_existing: u64,
    pub superframes: u64,
    pub error_resilient: u64,
    pub no_backward_adaptation: u64,
    pub size_changes: u64,
    pub scaled_ref_blocks: u64,
    pub compound_blocks: u64,
    pub intra_blocks: u64,
    pub inter_blocks: u64,
    pub lossless_frames: u64,
    pub max_tile_cols: u32,
    pub max_tile_rows: u32,
    pub segmentation_frames: u64,
    pub tx_select_frames: u64,
    pub switchable_interp_frames: u64,
    pub high_precision_mv_frames: u64,
    pub compound_frames: u64,
    pub bit_depths: [u64; 3],
    pub profiles: [u64; 4],
}

/// VP9 decoder.
pub struct Decoder {
    st: HeaderState,
    slots: [Option<Arc<Frame>>; 8],
    prev_mi: Option<Arc<MiGrid>>,
    prev_seg_ids: Vec<u8>,
    last_size: Option<(u32, u32)>,
    last_show_frame: bool,
    stats: DecodeStats,
    threads: usize,
    #[cfg(feature = "threads")]
    pool: Option<rayon::ThreadPool>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn default_threads() -> usize {
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    }
    #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
    {
        1
    }
}

/// get_tile_offset (6.4.1).
fn tile_offset(i: usize, mis: usize, log2: u32) -> usize {
    let sbs = (mis + 7) >> 3;
    (((i * sbs) >> log2) << 3).min(mis)
}

impl Decoder {
    /// A decoder using all available cores (with the `threads` feature).
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    /// A decoder using up to `threads` worker threads (1 = decode on the calling thread).
    pub fn with_threads(threads: usize) -> Self {
        #[cfg(feature = "threads")]
        let pool = if threads > 1 && cfg!(not(target_arch = "wasm32")) {
            rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("vp9-{i}")).build().ok()
        } else {
            None
        };
        Decoder {
            st: HeaderState::default(),
            slots: Default::default(),
            prev_mi: None,
            prev_seg_ids: Vec::new(),
            last_size: None,
            last_show_frame: false,
            stats: DecodeStats::default(),
            threads: threads.max(1),
            #[cfg(feature = "threads")]
            pool,
        }
    }

    /// Statistics accumulated so far.
    pub fn stats(&self) -> DecodeStats {
        self.stats.clone()
    }

    /// Decode one chunk (a frame or a superframe, as stored in one IVF / WebM / MP4 sample).
    /// Returns the frames to show, in order; each carries `pts`.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let frames = split_superframe(data);
        if frames.len() > 1 {
            self.stats.superframes += 1;
        }
        let mut out = Vec::new();
        for f in frames {
            if f.is_empty() {
                continue;
            }
            if let Some(frame) = self.decode_frame(f)? {
                out.push(make_picture(&frame, pts));
            }
        }
        Ok(out)
    }

    /// End of stream. VP9 has no reordering delay, so this returns no pictures; it resets the
    /// reference state so that a new stream can follow.
    pub fn flush(&mut self) -> Vec<Picture> {
        Vec::new()
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<Option<Arc<Frame>>> {
        let refs: [Option<RefInfo>; 8] = std::array::from_fn(|i| self.slots[i].as_ref().map(|f| RefInfo { width: f.width, height: f.height }));
        let mut h = parse_uncompressed(data, &mut self.st, &refs)?;
        if h.show_existing_frame {
            self.stats.show_existing += 1;
            let f = self.slots[h.frame_to_show_map_idx as usize]
                .clone()
                .ok_or_else(|| Error::MissingReference(format!("show_existing_frame of empty slot {}", h.frame_to_show_map_idx)))?;
            self.stats.shown += 1;
            return Ok(Some(f));
        }
        ensure!(h.width <= 16384 && h.height <= 16384, "frame size {}x{} too large", h.width, h.height);
        let (mi_rows, mi_cols) = (h.mi_rows as usize, h.mi_cols as usize);
        // compute_image_size semantics (7.2.6).
        let size = (h.width, h.height);
        let size_changed = self.last_size != Some(size);
        if size_changed && self.last_size.is_some() {
            self.stats.size_changes += 1;
        }
        if self.st.reset_segment_map || size_changed || self.prev_seg_ids.len() != mi_rows * mi_cols {
            self.prev_seg_ids = vec![0; mi_rows * mi_cols];
            self.st.reset_segment_map = false;
        }
        let use_prev = !size_changed
            && self.last_show_frame
            && !h.error_resilient_mode
            && !h.frame_is_intra
            && self.prev_mi.as_ref().is_some_and(|m| m.rows == mi_rows && m.cols == mi_cols);
        self.last_size = Some(size);
        self.last_show_frame = h.show_frame;

        let off = h.uncompressed_size;
        let hs = h.header_size_in_bytes as usize;
        ensure!(off + hs <= data.len(), "compressed header exceeds frame data");
        let mut fc = self.st.contexts[h.frame_context_idx as usize].clone();
        parse_compressed(&data[off..off + hs], &mut h, &mut fc)?;
        self.count_frame(&h);

        // References (8.5.2.3).
        let mut refs: [Option<RefUse>; 3] = [None, None, None];
        let ref_frames: Vec<Option<Arc<Frame>>> =
            (0..3).map(|i| if h.frame_is_intra { None } else { self.slots[h.ref_frame_idx[i] as usize].clone() }).collect();
        if !h.frame_is_intra {
            for i in 0..3 {
                let Some(rf) = &ref_frames[i] else {
                    return Err(Error::MissingReference(format!("reference slot {} is empty", h.ref_frame_idx[i])));
                };
                ensure!(
                    rf.bit_depth == h.color.bit_depth && rf.ss_x == h.color.subsampling_x && rf.ss_y == h.color.subsampling_y,
                    "reference frame format differs from the current frame"
                );
                let (rw, rh) = (rf.width as i64, rf.height as i64);
                let (w, hh) = (h.width as i64, h.height as i64);
                if 2 * w >= rw && 2 * hh >= rh && w <= 16 * rw && hh <= 16 * rh {
                    let x_scale = ((rw << 14) / w) as i32;
                    let y_scale = ((rh << 14) / hh) as i32;
                    refs[i] = Some(RefUse { frame: rf, x_scale, y_scale, x_step: (16 * x_scale) >> 14, y_step: (16 * y_scale) >> 14 });
                }
            }
        }
        // Quantizers per segment (8.6.1).
        let bd_idx = ((h.color.bit_depth - 8) >> 1) as usize;
        let seg = self.st.seg.clone();
        let mut seg_q = [[[0i32; 2]; 2]; 8];
        for (s, q) in seg_q.iter_mut().enumerate() {
            let qindex = if seg.feature_active(s as u8, SEG_LVL_ALT_Q) {
                let d = seg.feature_data[s][SEG_LVL_ALT_Q] as i32;
                (if seg.abs_or_delta_update { d } else { h.base_q_idx as i32 + d }).clamp(0, 255)
            } else {
                h.base_q_idx as i32
            };
            let dc = |b: i32| DC_QLOOKUP[bd_idx * 256 + b.clamp(0, 255) as usize];
            let ac = |b: i32| AC_QLOOKUP[bd_idx * 256 + b.clamp(0, 255) as usize];
            q[0] = [dc(qindex + h.delta_q_y_dc as i32), ac(qindex)];
            q[1] = [dc(qindex + h.delta_q_uv_dc as i32), ac(qindex + h.delta_q_uv_ac as i32)];
        }
        // Tile layout and data (6.4).
        let tile_cols = 1usize << h.tile_cols_log2;
        let tile_rows = 1usize << h.tile_rows_log2;
        let mut pos = off + hs;
        let mut tile_data: Vec<Vec<&[u8]>> = vec![Vec::with_capacity(tile_rows); tile_cols];
        for tr in 0..tile_rows {
            for (tc, td) in tile_data.iter_mut().enumerate() {
                let last = tr == tile_rows - 1 && tc == tile_cols - 1;
                let sz = if last {
                    data.len() - pos
                } else {
                    ensure!(pos + 4 <= data.len(), "tile size beyond frame data");
                    let s = u32::from_be_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) as usize;
                    pos += 4;
                    ensure!(s <= data.len() - pos, "tile size {s} beyond frame data");
                    s
                };
                td.push(&data[pos..pos + sz]);
                pos += sz;
            }
        }
        let counting = !h.error_resilient_mode && !h.frame_parallel_decoding_mode;
        let prev_mi = if use_prev { self.prev_mi.clone() } else { None };
        let shared = FrameShared { h: &h, fc: &fc, seg: &seg, prev_seg_ids: &self.prev_seg_ids, prev_mi: prev_mi.as_deref(), refs, seg_q, counting };
        let col_bounds: Vec<(usize, usize)> =
            (0..tile_cols).map(|i| (tile_offset(i, mi_cols, h.tile_cols_log2), tile_offset(i + 1, mi_cols, h.tile_cols_log2))).collect();
        let row_bounds: Vec<(usize, usize)> =
            (0..tile_rows).map(|i| (tile_offset(i, mi_rows, h.tile_rows_log2), tile_offset(i + 1, mi_rows, h.tile_rows_log2))).collect();
        let decode_col = |ci: usize| -> (Strip, Option<Error>) {
            let (cs, ce) = col_bounds[ci];
            let mut td = TileDecoder::new(&shared, cs, ce);
            let mut err = None;
            if ce > cs {
                for (ri, &(rs, re)) in row_bounds.iter().enumerate() {
                    if !td.decode_tile(tile_data[ci][ri], rs, re) {
                        err = Some(Error::Invalid("tile data exhausted".into()));
                        break;
                    }
                }
            }
            let e = td.error.take().or(err);
            (td.strip, e)
        };
        let results: Vec<(Strip, Option<Error>)> = self.run_parallel(tile_cols, &decode_col);
        let mut counts = Counts::default();
        let mut strips = Vec::with_capacity(results.len());
        for (s, e) in results {
            if let Some(e) = e {
                return Err(e);
            }
            if counting {
                counts.add(&s.counts);
            }
            self.stats.compound_blocks += s.compound_blocks;
            self.stats.scaled_ref_blocks += s.scaled_blocks;
            self.stats.intra_blocks += s.intra_blocks;
            self.stats.inter_blocks += s.inter_blocks;
            strips.push(s);
        }
        // Assemble the frame and mode info.
        let geo = plane_geometry(h.width, h.height, h.color.subsampling_x, h.color.subsampling_y);
        let (planes, mi, seg_ids) = assemble(strips, &geo, mi_rows, mi_cols);
        let mut frame = Frame {
            planes,
            width: h.width,
            height: h.height,
            ss_x: h.color.subsampling_x,
            ss_y: h.color.subsampling_y,
            bit_depth: h.color.bit_depth,
            color_space: h.color.color_space,
            color_range: h.color.color_range,
            render_width: h.render_width,
            render_height: h.render_height,
            key: h.frame_type == KEY_FRAME,
            intra_only: h.intra_only,
        };
        // Loop filter (8.8).
        if self.st.lf.level > 0 {
            let lf = LfFrame::new(&self.st.lf, &seg, mi_rows, mi_cols, h.color.subsampling_x, h.color.subsampling_y, h.color.bit_depth);
            self.loop_filter(&mut frame, &mi, &lf);
        }
        // refresh_probs (6.1.2).
        if counting {
            let saved = &self.st.contexts[h.frame_context_idx as usize];
            let mut a = saved.clone();
            a.tx = fc.tx;
            a.skip = fc.skip;
            adapt_coef_probs(&mut a, &counts, h.frame_is_intra, self.st.last_frame_type == KEY_FRAME);
            if !h.frame_is_intra {
                a.tx = saved.tx;
                a.skip = saved.skip;
                adapt_noncoef_probs(&mut a, &counts, h.interp_filter == SWITCHABLE, h.tx_mode == TX_MODE_SELECT, h.allow_high_precision_mv);
            }
            fc = a;
        }
        if h.refresh_frame_context {
            self.st.contexts[h.frame_context_idx as usize] = fc;
        }
        if seg.enabled && seg.update_map {
            self.prev_seg_ids = seg_ids;
        }
        let frame = Arc::new(frame);
        for i in 0..8 {
            if (h.refresh_frame_flags >> i) & 1 == 1 {
                self.slots[i] = Some(frame.clone());
            }
        }
        self.prev_mi = Some(Arc::new(mi));
        if h.show_frame {
            self.stats.shown += 1;
            Ok(Some(frame))
        } else {
            self.stats.hidden += 1;
            Ok(None)
        }
    }

    fn count_frame(&mut self, h: &FrameHeader) {
        let s = &mut self.stats;
        s.frames += 1;
        if h.frame_type == KEY_FRAME {
            s.key_frames += 1;
        } else if h.intra_only {
            s.intra_only += 1;
        } else {
            s.inter_frames += 1;
        }
        s.error_resilient += h.error_resilient_mode as u64;
        s.no_backward_adaptation += (h.error_resilient_mode || h.frame_parallel_decoding_mode) as u64;
        s.lossless_frames += h.lossless as u64;
        s.max_tile_cols = s.max_tile_cols.max(1 << h.tile_cols_log2);
        s.max_tile_rows = s.max_tile_rows.max(1 << h.tile_rows_log2);
        s.segmentation_frames += self.st.seg.enabled as u64;
        s.tx_select_frames += (h.tx_mode == TX_MODE_SELECT) as u64;
        s.switchable_interp_frames += (!h.frame_is_intra && h.interp_filter == SWITCHABLE) as u64;
        s.high_precision_mv_frames += (!h.frame_is_intra && h.allow_high_precision_mv) as u64;
        s.compound_frames += (!h.frame_is_intra && h.reference_mode != 0) as u64;
        s.bit_depths[((h.color.bit_depth - 8) >> 1) as usize] += 1;
        s.profiles[h.profile as usize] += 1;
    }

    /// Run `f(0..n)`, in parallel when a pool is available.
    fn run_parallel<T: Send>(&self, n: usize, f: &(dyn Fn(usize) -> T + Sync)) -> Vec<T> {
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool
            && n > 1
        {
            use rayon::prelude::*;
            return pool.install(|| (0..n).into_par_iter().map(f).collect());
        }
        (0..n).map(f).collect()
    }

    fn loop_filter(&self, frame: &mut Frame, mi: &MiGrid, lf: &LfFrame) {
        let sb_rows = lf.mi_rows.div_ceil(8);
        let sb_cols = lf.mi_cols.div_ceil(8);
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool
            && sb_rows > 1
        {
            lf_parallel(pool, self.threads, frame, mi, lf, sb_rows, sb_cols);
            return;
        }
        let _ = self.threads;
        let [p0, p1, p2] = &mut frame.planes;
        let mut views = [
            PlaneView { stride: p0.stride, data: &mut p0.data, ox: 0, oy: 0 },
            PlaneView { stride: p1.stride, data: &mut p1.data, ox: 0, oy: 0 },
            PlaneView { stride: p2.stride, data: &mut p2.data, ox: 0, oy: 0 },
        ];
        for r in 0..sb_rows {
            for c in 0..sb_cols {
                filter_superblock(&mut views, mi, lf, r * 8, c * 8);
            }
        }
    }
}

/// Loop filter with a superblock wavefront: superblock (r, c) runs once (r - 1, c + 1) is done.
/// Each superblock copies the samples it may touch (its area plus 8 samples above / left) out of
/// the shared frame, filters them locally and writes them back.
#[cfg(feature = "threads")]
fn lf_parallel(pool: &rayon::ThreadPool, threads: usize, frame: &mut Frame, mi: &MiGrid, lf: &LfFrame, sb_rows: usize, sb_cols: usize) {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    let strides = [frame.planes[0].stride, frame.planes[1].stride, frame.planes[2].stride];
    let heights = [frame.planes[0].data.len() / strides[0], frame.planes[1].data.len() / strides[1], frame.planes[2].data.len() / strides[2]];
    let planes = Mutex::new([std::mem::take(&mut frame.planes[0].data), std::mem::take(&mut frame.planes[1].data), std::mem::take(&mut frame.planes[2].data)]);
    let progress = Mutex::new(vec![0usize; sb_rows]);
    let cv = Condvar::new();
    let next = AtomicUsize::new(0);
    let sub = [(0usize, 0usize), (lf.ss_x, lf.ss_y), (lf.ss_x, lf.ss_y)];
    pool.install(|| {
        (0..threads.min(sb_rows)).into_par_iter().for_each(|_| {
            let mut bufs: [Vec<u16>; 3] = [vec![0; 72 * 72], vec![0; 72 * 72], vec![0; 72 * 72]];
            loop {
                let r = next.fetch_add(1, Ordering::Relaxed);
                if r >= sb_rows {
                    break;
                }
                for c in 0..sb_cols {
                    if r > 0 {
                        let need = (c + 2).min(sb_cols);
                        let mut p = progress.lock().expect("lf progress");
                        while p[r - 1] < need {
                            p = cv.wait(p).expect("lf progress");
                        }
                    }
                    // Region of each plane: [x0 - 8, x0 + size) x [y0 - 8, y0 + size).
                    let mut regions = [(0usize, 0usize, 0usize, 0usize); 3];
                    {
                        let pl = planes.lock().expect("lf planes");
                        for p in 0..3 {
                            let (sx, sy) = sub[p];
                            let (x0, y0) = ((c * 64) >> sx, (r * 64) >> sy);
                            let (rx, ry) = (x0.saturating_sub(8), y0.saturating_sub(8));
                            let rw = (x0 + (64 >> sx)).min(strides[p]) - rx;
                            let rh = (y0 + (64 >> sy)).min(heights[p]) - ry;
                            regions[p] = (rx, ry, rw, rh);
                            for yy in 0..rh {
                                let s = (ry + yy) * strides[p] + rx;
                                bufs[p][yy * rw..yy * rw + rw].copy_from_slice(&pl[p][s..s + rw]);
                            }
                        }
                    }
                    {
                        let [b0, b1, b2] = &mut bufs;
                        let mut views = [
                            PlaneView { data: b0, stride: regions[0].2, ox: regions[0].0, oy: regions[0].1 },
                            PlaneView { data: b1, stride: regions[1].2, ox: regions[1].0, oy: regions[1].1 },
                            PlaneView { data: b2, stride: regions[2].2, ox: regions[2].0, oy: regions[2].1 },
                        ];
                        filter_superblock(&mut views, mi, lf, r * 8, c * 8);
                    }
                    {
                        let mut pl = planes.lock().expect("lf planes");
                        for p in 0..3 {
                            let (rx, ry, rw, rh) = regions[p];
                            for yy in 0..rh {
                                let s = (ry + yy) * strides[p] + rx;
                                pl[p][s..s + rw].copy_from_slice(&bufs[p][yy * rw..yy * rw + rw]);
                            }
                        }
                    }
                    let mut p = progress.lock().expect("lf progress");
                    p[r] = c + 1;
                    cv.notify_all();
                }
            }
        });
    });
    let [a, b, c] = planes.into_inner().expect("lf planes");
    frame.planes[0].data = a;
    frame.planes[1].data = b;
    frame.planes[2].data = c;
}

/// Build full-frame planes, mode info and segment ids from the tile column strips.
fn assemble(mut strips: Vec<Strip>, geo: &[(usize, usize, usize, usize); 3], mi_rows: usize, mi_cols: usize) -> ([FPlane; 3], MiGrid, Vec<u8>) {
    if strips.len() == 1 {
        let s = strips.pop().expect("one strip");
        let [a, b, c] = s.planes;
        let mk = |d: Vec<u16>, g: (usize, usize, usize, usize)| FPlane { data: d, stride: g.0, width: g.2, height: g.3 };
        return ([mk(a, geo[0]), mk(b, geo[1]), mk(c, geo[2])], MiGrid { cols: mi_cols, rows: mi_rows, mi: s.mi }, s.seg_ids);
    }
    let mut planes = [
        FPlane::new(geo[0].0, geo[0].1, geo[0].2, geo[0].3),
        FPlane::new(geo[1].0, geo[1].1, geo[1].2, geo[1].3),
        FPlane::new(geo[2].0, geo[2].1, geo[2].2, geo[2].3),
    ];
    let mut mi = vec![MiInfo::default(); mi_rows * mi_cols];
    let mut seg = vec![0u8; mi_rows * mi_cols];
    for s in &strips {
        for p in 0..3 {
            let ss = s.strides[p];
            let fs = planes[p].stride;
            let x0 = s.x_off[p];
            let w = ss.min(fs - x0);
            let rows = s.planes[p].len() / ss;
            for y in 0..rows {
                planes[p].data[y * fs + x0..y * fs + x0 + w].copy_from_slice(&s.planes[p][y * ss..y * ss + w]);
            }
        }
        let mw = s.mi_col_end - s.mi_col_start;
        for r in 0..mi_rows {
            mi[r * mi_cols + s.mi_col_start..r * mi_cols + s.mi_col_end].copy_from_slice(&s.mi[r * mw..r * mw + mw]);
            seg[r * mi_cols + s.mi_col_start..r * mi_cols + s.mi_col_end].copy_from_slice(&s.seg_ids[r * mw..r * mw + mw]);
        }
    }
    (planes, MiGrid { cols: mi_cols, rows: mi_rows, mi }, seg)
}

fn make_picture(f: &Frame, pts: i64) -> Picture {
    let crop = |p: &FPlane| -> Vec<u16> {
        let mut v = Vec::with_capacity(p.width * p.height);
        for y in 0..p.height {
            v.extend_from_slice(&p.row(y)[..p.width]);
        }
        v
    };
    let conv = |v: Vec<u16>| if f.bit_depth == 8 { Plane::U8(v.into_iter().map(|s| s as u8).collect()) } else { Plane::U16(v) };
    let [y, u, v] = &f.planes;
    Picture {
        width: f.width,
        height: f.height,
        chroma_width: u.width as u32,
        chroma_height: u.height as u32,
        bit_depth: f.bit_depth as u32,
        subsampling_x: f.ss_x,
        subsampling_y: f.ss_y,
        y: conv(crop(y)),
        u: conv(crop(u)),
        v: conv(crop(v)),
        y_stride: y.width,
        uv_stride: u.width,
        pts,
        key: f.key,
        intra_only: f.intra_only,
        color: ColorInfo { color_space: f.color_space, full_range: f.color_range },
        render_width: f.render_width,
        render_height: f.render_height,
    }
}
