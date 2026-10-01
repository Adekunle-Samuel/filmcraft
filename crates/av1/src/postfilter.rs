//! In-loop post filters run by decode_frame_wrapup: loop filter (7.14), CDEF (7.15),
//! super-resolution upscaling (7.16) and loop restoration (7.17).

use crate::frame::Plane;
use crate::spec_tables::*;
use crate::state::FrameState;

pub(crate) fn apply(fs: &mut FrameState) {
    let lvl = fs.fh.lf.level;
    if lvl[0] != 0 || lvl[1] != 0 {
        loop_filter(fs);
    }
    let cdef = crate::cdef::apply(fs);
    let (up_cur, up_cdef) = if fs.fh.use_superres {
        let c = crate::restoration::upscale(fs, &fs.cur);
        let d = cdef.as_ref().map(|f| crate::restoration::upscale(fs, f));
        (c, d)
    } else {
        (std::mem::take(&mut fs.cur), cdef)
    };
    fs.cur = if fs.fh.lr.uses_lr { crate::restoration::loop_restoration(fs, &up_cur, up_cdef.as_ref().unwrap_or(&up_cur)) } else { up_cdef.unwrap_or(up_cur) };
}

// ---------------------------------------------------------------------------------------------
// Loop filter (7.14)

fn loop_filter(fs: &mut FrameState) {
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as usize, fs.fh.mi_cols as usize);
    for plane in 0..fs.num_planes {
        if plane == 0 || fs.fh.lf.level[1 + plane] != 0 {
            for pass in 0..2 {
                let row_step = if plane == 0 { 1 } else { 1 << fs.ssy };
                let col_step = if plane == 0 { 1 } else { 1 << fs.ssx };
                let mut row = 0;
                while row < mi_rows {
                    let mut col = 0;
                    while col < mi_cols {
                        edge(fs, plane, pass, row, col);
                        col += col_step;
                    }
                    row += row_step;
                }
            }
        }
    }
}

fn edge(fs: &mut FrameState, plane: usize, pass: usize, row: usize, col: usize) {
    let (sub_x, sub_y) = if plane == 0 { (0, 0) } else { (fs.ssx, fs.ssy) };
    let (dx, dy) = if pass == 0 { (1i32, 0i32) } else { (0, 1) };
    let x = col * 4;
    let y = row * 4;
    let row = row | sub_y;
    let col = col | sub_x;
    let on_screen = !(x >= fs.fh.frame_width as usize || y >= fs.fh.frame_height as usize || (pass == 0 && x == 0) || (pass == 1 && y == 0));
    if !on_screen {
        return;
    }
    let xp = x >> sub_x;
    let yp = y >> sub_y;
    let prev_row = row - ((dy as usize) << sub_y);
    let prev_col = col - ((dx as usize) << sub_x);
    let mi = &fs.mi;
    let i = mi.idx(row, col);
    let mi_size = mi.mi_size[i] as usize;
    let tx_sz = fs.lf_tx_size(plane, row >> sub_y, col >> sub_x);
    let plane_size = fs.plane_residual_size(mi_size, plane);
    let skip = mi.skip[i];
    let is_intra = mi.ref_frame[i][0] <= INTRA_FRAME as i8;
    let prev_tx_sz = fs.lf_tx_size(plane, prev_row >> sub_y, prev_col >> sub_x);
    let is_block_edge = if pass == 0 {
        xp.is_multiple_of(4 * NUM_4X4_BLOCKS_WIDE[plane_size] as usize)
    } else {
        yp.is_multiple_of(4 * NUM_4X4_BLOCKS_HIGH[plane_size] as usize)
    };
    let is_tx_edge = if pass == 0 { xp.is_multiple_of(TX_WIDTH[tx_sz] as usize) } else { yp.is_multiple_of(TX_HEIGHT[tx_sz] as usize) };
    let apply_filter = is_tx_edge && (is_block_edge || !skip || is_intra);
    // filter size (7.14.3)
    let base_size = if pass == 0 { TX_WIDTH[prev_tx_sz].min(TX_WIDTH[tx_sz]) } else { TX_HEIGHT[prev_tx_sz].min(TX_HEIGHT[tx_sz]) } as usize;
    let filter_size = if plane == 0 { 16.min(base_size) } else { 8.min(base_size) };
    let (mut lvl, mut limit, mut blimit, mut thresh) = strength(fs, row, col, plane, pass);
    if lvl == 0 {
        (lvl, limit, blimit, thresh) = strength(fs, prev_row, prev_col, plane, pass);
    }
    if !apply_filter || lvl == 0 {
        return;
    }
    let bd = fs.bit_depth;
    let pl = &mut fs.cur.planes[plane];
    for k in 0..4 {
        sample_filter(pl, xp + (dy as usize) * k, yp + (dx as usize) * k, plane, limit, blimit, thresh, dx, dy, filter_size, bd);
    }
}

/// Adaptive filter strength (7.14.4): (lvl, limit, blimit, thresh).
fn strength(fs: &FrameState, row: usize, col: usize, plane: usize, pass: usize) -> (i32, i32, i32, i32) {
    let mi = &fs.mi;
    let i = mi.idx(row, col);
    let segment = mi.segment_id[i] as usize;
    let rf = mi.ref_frame[i][0];
    let mode = mi.y_mode[i] as usize;
    let mode_type = (mode >= NEARESTMV && mode != GLOBALMV && mode != GLOBAL_GLOBALMV) as usize;
    let delta_lf = if fs.fh.delta_lf_multi { mi.delta_lf[i][if plane == 0 { pass } else { plane + 1 }] } else { mi.delta_lf[i][0] } as i32;
    // 7.14.5
    let idx = if plane == 0 { pass } else { plane + 1 };
    let base = (delta_lf + fs.fh.lf.level[idx] as i32).clamp(0, MAX_LOOP_FILTER as i32);
    let mut lvl_seg = base;
    let feature = SEG_LVL_ALT_LF_Y_V + idx;
    if fs.fh.seg.enabled && fs.fh.seg.features.enabled[segment][feature] {
        lvl_seg = (fs.fh.seg.features.data[segment][feature] + lvl_seg).clamp(0, MAX_LOOP_FILTER as i32);
    }
    if fs.fh.lf.delta_enabled {
        let n_shift = lvl_seg >> 5;
        let d = &fs.fh.lf.deltas;
        if rf == INTRA_FRAME as i8 {
            lvl_seg += d.ref_deltas[INTRA_FRAME] << n_shift;
        } else if rf > INTRA_FRAME as i8 {
            lvl_seg += (d.ref_deltas[rf as usize] << n_shift) + (d.mode_deltas[mode_type] << n_shift);
        }
        lvl_seg = lvl_seg.clamp(0, MAX_LOOP_FILTER as i32);
    }
    let lvl = lvl_seg;
    let sharp = fs.fh.lf.sharpness as i32;
    let shift = if sharp > 4 {
        2
    } else if sharp > 0 {
        1
    } else {
        0
    };
    let limit = if sharp > 0 { (lvl >> shift).clamp(1, 9 - sharp) } else { (lvl >> shift).max(1) };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    (lvl, limit, blimit, thresh)
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn sample_filter(pl: &mut Plane, x: usize, y: usize, plane: usize, limit: i32, blimit: i32, thresh: i32, dx: i32, dy: i32, filter_size: usize, bd: u32) {
    let stride = pl.stride as isize;
    let step = dy as isize * stride + dx as isize;
    let base = y as isize * stride + x as isize;
    let reach: isize = if filter_size >= 16 { 7 } else { 4 };
    let mut v = [0i32; 16];
    for k in -reach..reach {
        v[(k + 8) as usize] = pl.data[(base + k * step) as usize] as i32;
    }
    let at = |k: isize| -> i32 { v[(k + 8) as usize] };
    let (q0, q1, q2, q3) = (at(0), at(1), at(2), at(3));
    let (p0, p1, p2, p3) = (at(-1), at(-2), at(-3), at(-4));
    let d = &mut pl.data;
    let sh = bd - 8;
    let thresh_bd = thresh << sh;
    let hev = (p1 - p0).abs() > thresh_bd || (q1 - q0).abs() > thresh_bd;
    let filter_len = if filter_size == 4 {
        4
    } else if plane != 0 {
        6
    } else if filter_size == 8 {
        8
    } else {
        16
    };
    let limit_bd = limit << sh;
    let blimit_bd = blimit << sh;
    let mut mask = (p1 - p0).abs() > limit_bd || (q1 - q0).abs() > limit_bd || (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit_bd;
    if filter_len >= 6 {
        mask |= (p2 - p1).abs() > limit_bd || (q2 - q1).abs() > limit_bd;
    }
    if filter_len >= 8 {
        mask |= (p3 - p2).abs() > limit_bd || (q3 - q2).abs() > limit_bd;
    }
    if mask {
        return;
    }
    let t_bd = 1 << sh;
    let flat = if filter_size >= 8 {
        let mut m = (p1 - p0).abs() > t_bd || (q1 - q0).abs() > t_bd || (p2 - p0).abs() > t_bd || (q2 - q0).abs() > t_bd;
        if filter_len >= 8 {
            m |= (p3 - p0).abs() > t_bd || (q3 - q0).abs() > t_bd;
        }
        !m
    } else {
        false
    };
    let flat2 = if filter_size >= 16 {
        let (q4, q5, q6, p4, p5, p6) = (at(4), at(5), at(6), at(-5), at(-6), at(-7));
        !((p6 - p0).abs() > t_bd
            || (q6 - q0).abs() > t_bd
            || (p5 - p0).abs() > t_bd
            || (q5 - q0).abs() > t_bd
            || (p4 - p0).abs() > t_bd
            || (q4 - q0).abs() > t_bd)
    } else {
        false
    };
    if filter_size == 4 || !flat {
        // narrow filter (7.14.6.3)
        let half = 1i32 << (bd - 1);
        let c = |v: i32| v.clamp(-half, half - 1);
        let off = 0x80 << sh;
        let (ps1, ps0, qs0, qs1) = (p1 - off, p0 - off, q0 - off, q1 - off);
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        d[base as usize] = (c(qs0 - filter1) + off) as u16;
        d[(base - step) as usize] = (c(ps0 + filter2) + off) as u16;
        if !hev {
            let f = (filter1 + 1) >> 1;
            d[(base + step) as usize] = (c(qs1 - f) + off) as u16;
            d[(base - 2 * step) as usize] = (c(ps1 + f) + off) as u16;
        }
    } else {
        let log2 = if filter_size == 8 || !flat2 { 3 } else { 4 };
        // wide filter (7.14.6.4)
        let n: isize = if log2 == 4 {
            6
        } else if plane == 0 {
            3
        } else {
            2
        };
        let n2: isize = if log2 == 3 && plane == 0 { 0 } else { 1 };
        let mut f = [0i32; 12];
        for i in -n..n {
            let mut t = 0;
            for j in -n..=n {
                let p = (i + j).clamp(-(n + 1), n);
                let tap = if j.abs() <= n2 { 2 } else { 1 };
                t += at(p) * tap;
            }
            f[(i + n) as usize] = (t + (1 << (log2 - 1))) >> log2;
        }
        for i in -n..n {
            d[(base + i * step) as usize] = f[(i + n) as usize] as u16;
        }
    }
}
