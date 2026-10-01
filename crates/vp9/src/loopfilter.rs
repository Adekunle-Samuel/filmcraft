//! Loop filter process (8.8).

use crate::frame::{MiGrid, MiInfo};
use crate::header::{LoopFilterParams, Segmentation};
use crate::tables::*;

/// Frame-level loop filter setup (8.8.1) plus the per-level limits of 8.8.4.
pub struct LfFrame {
    /// LvlLookup[segment_id][ref][modeType].
    pub lvl: [[[u8; 2]; 4]; 8],
    /// (limit, blimit, thresh) per filter level.
    pub limits: [(i32, i32, i32); 64],
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub ss_x: usize,
    pub ss_y: usize,
    pub bit_depth: u8,
}

impl LfFrame {
    pub fn new(lf: &LoopFilterParams, seg: &Segmentation, mi_rows: usize, mi_cols: usize, ss_x: bool, ss_y: bool, bit_depth: u8) -> LfFrame {
        let mut lvl = [[[0u8; 2]; 4]; 8];
        let n_shift = lf.level >> 5;
        for (seg_id, l) in lvl.iter_mut().enumerate() {
            let mut lvl_seg = lf.level as i32;
            if seg.feature_active(seg_id as u8, SEG_LVL_ALT_L) {
                let d = seg.feature_data[seg_id][SEG_LVL_ALT_L] as i32;
                lvl_seg = if seg.abs_or_delta_update { d } else { d + lf.level as i32 };
                lvl_seg = lvl_seg.clamp(0, MAX_LOOP_FILTER);
            }
            if !lf.delta_enabled {
                *l = [[lvl_seg as u8; 2]; 4];
            } else {
                let intra = lvl_seg + ((lf.ref_deltas[0] as i32) << n_shift);
                l[0][0] = intra.clamp(0, MAX_LOOP_FILTER) as u8;
                l[0][1] = l[0][0];
                for rf in 1..4 {
                    for mode in 0..2 {
                        let v = lvl_seg + ((lf.ref_deltas[rf] as i32) << n_shift) + ((lf.mode_deltas[mode] as i32) << n_shift);
                        l[rf][mode] = v.clamp(0, MAX_LOOP_FILTER) as u8;
                    }
                }
            }
        }
        let mut limits = [(0, 0, 0); 64];
        let sh = lf.sharpness as i32;
        let shift = if sh > 4 {
            2
        } else if sh > 0 {
            1
        } else {
            0
        };
        for (l, e) in limits.iter_mut().enumerate() {
            let l = l as i32;
            let limit = if sh > 0 { (l >> shift).clamp(1, 9 - sh) } else { (l >> shift).max(1) };
            *e = (limit, 2 * (l + 2) + limit, l >> 4);
        }
        LfFrame { lvl, limits, mi_rows, mi_cols, ss_x: ss_x as usize, ss_y: ss_y as usize, bit_depth }
    }

    fn level(&self, mi: &MiInfo) -> u8 {
        let mode_type = matches!(mi.y_mode, NEARESTMV | NEARMV | NEWMV) as usize;
        let rf = mi.ref_frame[0].max(0) as usize;
        self.lvl[mi.seg_id as usize & 7][rf][mode_type]
    }
}

/// A mutable view of one plane: sample (x, y) of the plane is `data[(y - oy) * stride + x - ox]`.
pub struct PlaneView<'a> {
    pub data: &'a mut [u16],
    pub stride: usize,
    pub ox: usize,
    pub oy: usize,
}

/// Loop filter one superblock (all planes, both passes) at (`row`, `col`) in MI units (8.8.2).
pub fn filter_superblock(views: &mut [PlaneView; 3], mi: &MiGrid, f: &LfFrame, row: usize, col: usize) {
    for (plane, view) in views.iter_mut().enumerate() {
        for pass in 0..2 {
            filter_sb_plane(view, mi, f, plane, pass, row, col);
        }
    }
}

fn filter_sb_plane(v: &mut PlaneView, mi: &MiGrid, f: &LfFrame, plane: usize, pass: usize, row: usize, col: usize) {
    let (sub_x, sub_y) = if plane > 0 { (f.ss_x, f.ss_y) } else { (0, 0) };
    let (sub, edge_len) = if pass == 0 { (sub_x, 64 >> sub_y) } else { (sub_y, 64 >> sub_x) };
    let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
    // Step between the samples across the edge.
    let across = if pass == 0 { 1isize } else { v.stride as isize };
    for edge in 0..(16 >> sub) {
        let mut i = 0;
        while i < edge_len {
            // Group of 8 samples sharing loopRow / loopCol.
            let (x, y) =
                if pass == 0 { (col * 8 + edge * (4 << sub_x), row * 8 + (i << sub_y)) } else { (col * 8 + (i << sub_x), row * 8 + edge * (4 << sub_y)) };
            // Skip groups entirely off screen (the varying coordinate is re-checked per sample).
            if x >= 8 * mi_cols || y >= 8 * mi_rows || (pass == 0 && x == 0) || (pass == 1 && y == 0) {
                i += 8;
                continue;
            }
            let loop_col = ((x >> 3) >> sub_x) << sub_x;
            let loop_row = ((y >> 3) >> sub_y) << sub_y;
            let m = mi.at(loop_row, loop_col);
            let tx_sz = if plane > 0 {
                if m.sb_size < BLOCK_8X8 { 0 } else { m.tx_size.min(MAX_TXSIZE[SS_SIZE_LOOKUP[m.sb_size as usize][f.ss_x][f.ss_y].min(12) as usize]) }
            } else {
                m.tx_size
            };
            let sb_size = if sub == 0 { m.sb_size } else { m.sb_size.max(BLOCK_16X16) } as usize;
            let skip = m.skip;
            let is_intra = m.ref_frame[0] <= INTRA_FRAME;
            let is_block_edge = if pass == 0 { x % (8 * NUM_8X8_WIDE[sb_size] as usize) == 0 } else { y % (8 * NUM_8X8_HIGH[sb_size] as usize) == 0 };
            let is32 = edge % 8 == 0;
            let lvl = f.level(m);
            if lvl == 0 {
                i += 8;
                continue;
            }
            let base_size = if tx_sz == TX_4X4 && is32 { TX_8X8 } else { tx_sz.min(TX_16X16) };
            let filter_size =
                if base_size == TX_16X16 && ((pass == 0 && sub_x == 1 && (x >> 3) == mi_cols - 1) || (pass == 1 && sub_y == 1 && (y >> 3) == mi_rows - 1)) {
                    TX_8X8
                } else {
                    base_size
                };
            let (limit, blimit, thresh) = f.limits[lvl as usize];
            let tx_edge_normal = edge % (1 << tx_sz) == 0;
            for k in i..(i + 8).min(edge_len) {
                let (sx, sy) = if pass == 0 { (x, row * 8 + (k << sub_y)) } else { (col * 8 + (k << sub_x), y) };
                if sx >= 8 * mi_cols || sy >= 8 * mi_rows {
                    continue;
                }
                let is_tx_edge = if pass == 1 && sub_x == 1 && mi_cols & 1 == 1 && edge & 1 == 1 && sx + 8 >= mi_cols * 8 { false } else { tx_edge_normal };
                let apply = is_block_edge || (is_tx_edge && (is_intra || !skip));
                if !apply {
                    continue;
                }
                let px = sx >> sub_x;
                let py = sy >> sub_y;
                let pos = ((py - v.oy) * v.stride + px - v.ox) as isize;
                filter_sample(v.data, pos, across, limit, blimit, thresh, filter_size, f.bit_depth);
            }
            i += 8;
        }
    }
}

/// Sample filtering process (8.8.5) at `pos` (the q0 sample), samples across the edge `step`
/// apart. The samples are gathered into `v` (`v[8 + k]` = sample at offset k) and only modified
/// samples are written back.
#[allow(clippy::too_many_arguments)]
#[inline]
fn filter_sample(d: &mut [u16], pos: isize, step: isize, limit: i32, blimit: i32, thresh: i32, filter_size: u8, bit_depth: u8) {
    let reach: usize = if filter_size == TX_16X16 { 8 } else { 4 };
    let step = step as usize;
    let start = pos as usize - reach * step;
    let mut v = [0i32; 16];
    {
        let span = &d[start..start + (2 * reach - 1) * step + 1];
        for k in 0..2 * reach {
            v[8 - reach + k] = span[k * step] as i32;
        }
    }
    let (p3, p2, p1, p0, q0, q1, q2, q3) = (v[4], v[5], v[6], v[7], v[8], v[9], v[10], v[11]);
    let shift = bit_depth as u32 - 8;
    // Filter mask process (8.8.5.1).
    let limit_bd = limit << shift;
    let blimit_bd = blimit << shift;
    if (p3 - p2).abs() > limit_bd
        || (p2 - p1).abs() > limit_bd
        || (p1 - p0).abs() > limit_bd
        || (q1 - q0).abs() > limit_bd
        || (q2 - q1).abs() > limit_bd
        || (q3 - q2).abs() > limit_bd
        || (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit_bd
    {
        return;
    }
    let thresh_bd = thresh << shift;
    let hev = (p1 - p0).abs() > thresh_bd || (q1 - q0).abs() > thresh_bd;
    let one = 1 << shift;
    let flat = filter_size >= TX_8X8
        && (p1 - p0).abs() <= one
        && (q1 - q0).abs() <= one
        && (p2 - p0).abs() <= one
        && (q2 - q0).abs() <= one
        && (p3 - p0).abs() <= one
        && (q3 - q0).abs() <= one;
    let (lo, hi) = if filter_size == TX_4X4 || !flat {
        narrow_filter(&mut v, hev, bit_depth);
        (6, 10)
    } else {
        let flat2 = filter_size >= TX_16X16 && (0..4).all(|k| (v[3 - k] - p0).abs() <= one && (v[12 + k] - q0).abs() <= one);
        if flat2 {
            wide_filter(&mut v, 4);
            (1, 15)
        } else {
            wide_filter(&mut v, 3);
            (5, 11)
        }
    };
    for k in lo..hi {
        d[(pos + (k as isize - 8) * step as isize) as usize] = v[k] as u16;
    }
}

/// Narrow filter process (8.8.5.2) on gathered samples.
#[inline]
fn narrow_filter(v: &mut [i32; 16], hev: bool, bit_depth: u8) {
    let shift = bit_depth as u32 - 8;
    let lo = -(1 << (bit_depth - 1));
    let hi = (1 << (bit_depth - 1)) - 1;
    let c = |x: i32| x.clamp(lo, hi);
    let off = 0x80 << shift;
    let (ps1, ps0, qs0, qs1) = (v[6] - off, v[7] - off, v[8] - off, v[9] - off);
    let mut filter = if hev { c(ps1 - qs1) } else { 0 };
    filter = c(filter + 3 * (qs0 - ps0));
    let filter1 = c(filter + 4) >> 3;
    let filter2 = c(filter + 3) >> 3;
    v[8] = c(qs0 - filter1) + off;
    v[7] = c(ps0 + filter2) + off;
    if !hev {
        let f = (filter1 + 1) >> 1;
        v[9] = c(qs1 - f) + off;
        v[6] = c(ps1 + f) + off;
    }
}

/// Wide filter process (8.8.5.3) with 2^log2 taps on gathered samples; the sum over j of the
/// specification is computed as a sliding window over the clamped indices (identical results).
#[inline]
fn wide_filter(v: &mut [i32; 16], log2: u32) {
    let n = (1isize << (log2 - 1)) - 1;
    let src = *v;
    let s = |k: isize| src[(k.clamp(-(n + 1), n) + 8) as usize];
    let mut sum: i32 = (-n..=n).map(|j| s(-n + j)).sum();
    let round = 1i32 << (log2 - 1);
    for i in -n..n {
        v[(i + 8) as usize] = (sum + s(i) + round) >> log2;
        sum += s(i + 1 + n) - s(i - n);
    }
}
