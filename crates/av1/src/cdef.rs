//! CDEF (7.15).

use crate::frame::FrameBuf;
use crate::spec_tables::*;
use crate::state::FrameState;

/// Run CDEF on `fs.cur`; returns the CdefFrame, or None when no block is filtered (CdefFrame
/// equals CurrFrame).
pub(crate) fn apply(fs: &FrameState) -> Option<FrameBuf> {
    let fh = &fs.fh;
    if fh.coded_lossless || fh.allow_intrabc || !fs.seq.enable_cdef {
        return None;
    }
    let (mi_rows, mi_cols) = (fh.mi_rows as usize, fh.mi_cols as usize);
    let mut out = fs.cur.clone();
    let mut any = false;
    let mut r = 0;
    while r < mi_rows {
        let mut c = 0;
        while c < mi_cols {
            let idx = fs.cdef_idx(r & !15, c & !15);
            if idx >= 0 && cdef_block(fs, &mut out, r, c, idx as usize) {
                any = true;
            }
            c += 2;
        }
        r += 2;
    }
    any.then_some(out)
}

fn cdef_block(fs: &FrameState, out: &mut FrameBuf, r: usize, c: usize, idx: usize) -> bool {
    let mi = &fs.mi;
    let sk = |rr: usize, cc: usize| -> bool { if rr < mi.rows && cc < mi.cols { mi.skip[mi.idx(rr, cc)] } else { true } };
    let skip = sk(r, c) && sk(r + 1, c) && sk(r, c + 1) && sk(r + 1, c + 1);
    if skip {
        return false;
    }
    let coeff_shift = fs.bit_depth - 8;
    let (y_dir, var) = direction(fs, r, c);
    let cp = &fs.fh.cdef;
    let mut pri = (cp.y_pri[idx] << coeff_shift) as i32;
    let sec = (cp.y_sec[idx] << coeff_shift) as i32;
    let dir = if pri == 0 { 0 } else { y_dir };
    let var_str = if (var >> 6) != 0 { (31 - ((var >> 6) as u32).leading_zeros()).min(12) as i32 } else { 0 };
    pri = if var != 0 { (pri * (4 + var_str) + 8) >> 4 } else { 0 };
    let damping = cp.damping as i32 + coeff_shift as i32;
    filter(fs, out, 0, r, c, pri, sec, damping, dir);
    if fs.num_planes == 1 {
        return true;
    }
    let pri = (cp.uv_pri[idx] << coeff_shift) as i32;
    let sec = (cp.uv_sec[idx] << coeff_shift) as i32;
    let dir = if pri == 0 { 0 } else { CDEF_UV_DIR[fs.ssx][fs.ssy][y_dir] as usize };
    let damping = cp.damping as i32 + coeff_shift as i32 - 1;
    filter(fs, out, 1, r, c, pri, sec, damping, dir);
    filter(fs, out, 2, r, c, pri, sec, damping, dir);
    true
}

fn direction(fs: &FrameState, r: usize, c: usize) -> (usize, i32) {
    let mut cost = [0i32; 8];
    let mut partial = [[0i32; 15]; 8];
    let x0 = c << 2;
    let y0 = r << 2;
    let pl = &fs.cur.planes[0];
    let sh = fs.bit_depth - 8;
    for i in 0..8 {
        let row = pl.row(y0 + i);
        for j in 0..8 {
            let x = (row[x0 + j] as i32 >> sh) - 128;
            partial[0][i + j] += x;
            partial[1][i + j / 2] += x;
            partial[2][i] += x;
            partial[3][3 + i - j / 2] += x;
            partial[4][7 + i - j] += x;
            partial[5][3 - i / 2 + j] += x;
            partial[6][j] += x;
            partial[7][i / 2 + j] += x;
        }
    }
    for i in 0..8 {
        cost[2] += partial[2][i] * partial[2][i];
        cost[6] += partial[6][i] * partial[6][i];
    }
    cost[2] *= DIV_TABLE[8] as i32;
    cost[6] *= DIV_TABLE[8] as i32;
    for i in 0..7 {
        cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i]) * DIV_TABLE[i + 1] as i32;
        cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i]) * DIV_TABLE[i + 1] as i32;
    }
    cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8] as i32;
    cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8] as i32;
    let mut i = 1;
    while i < 8 {
        for j in 0..5 {
            cost[i] += partial[i][3 + j] * partial[i][3 + j];
        }
        cost[i] *= DIV_TABLE[8] as i32;
        for j in 0..3 {
            cost[i] += (partial[i][j] * partial[i][j] + partial[i][10 - j] * partial[i][10 - j]) * DIV_TABLE[2 * j + 2] as i32;
        }
        i += 2;
    }
    let mut best = 0;
    let mut y_dir = 0;
    for (i, &c) in cost.iter().enumerate() {
        if c > best {
            best = c;
            y_dir = i;
        }
    }
    (y_dir, (best - cost[(y_dir + 4) & 7]) >> 10)
}

#[inline(always)]
fn constrain(diff: i32, threshold: i32, damping: i32) -> i32 {
    if threshold == 0 {
        return 0;
    }
    let adj = (damping - (31 - (threshold as u32).leading_zeros()) as i32).max(0);
    let mag = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
    if diff < 0 { -mag } else { mag }
}

#[allow(clippy::too_many_arguments)]
fn filter(fs: &FrameState, out: &mut FrameBuf, plane: usize, r: usize, c: usize, pri: i32, sec: i32, damping: i32, dir: usize) {
    let coeff_shift = fs.bit_depth - 8;
    let (sub_x, sub_y) = if plane > 0 { (fs.ssx, fs.ssy) } else { (0, 0) };
    let x0 = (c * 4) >> sub_x;
    let y0 = (r * 4) >> sub_y;
    let w = 8 >> sub_x;
    let h = 8 >> sub_y;
    let src = &fs.cur.planes[plane];
    let dst = &mut out.planes[plane];
    let (mi_rows, mi_cols) = (fs.fh.mi_rows as isize, fs.fh.mi_cols as isize);
    let pri_tap = ((pri >> coeff_shift) & 1) as usize;
    let get = |y: isize, x: isize| -> Option<i32> {
        let cand_r = (y << sub_y) >> 2;
        let cand_c = (x << sub_x) >> 2;
        if cand_c >= 0 && cand_c < mi_cols && cand_r >= 0 && cand_r < mi_rows { Some(src.at(x as usize, y as usize) as i32) } else { None }
    };
    for i in 0..h {
        for j in 0..w {
            let x = src.at(x0 + j, y0 + i) as i32;
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2 {
                for sign in [-1isize, 1] {
                    let yy = (y0 + i) as isize + sign * CDEF_DIRECTIONS[dir][k][0] as isize;
                    let xx = (x0 + j) as isize + sign * CDEF_DIRECTIONS[dir][k][1] as isize;
                    if let Some(p) = get(yy, xx) {
                        sum += CDEF_PRI_TAPS[pri_tap][k] as i32 * constrain(p - x, pri, damping);
                        max = max.max(p);
                        min = min.min(p);
                    }
                    for dir_off in [-2isize, 2] {
                        let d2 = ((dir as isize + dir_off) & 7) as usize;
                        let yy = (y0 + i) as isize + sign * CDEF_DIRECTIONS[d2][k][0] as isize;
                        let xx = (x0 + j) as isize + sign * CDEF_DIRECTIONS[d2][k][1] as isize;
                        if let Some(s) = get(yy, xx) {
                            sum += CDEF_SEC_TAPS[pri_tap][k] as i32 * constrain(s - x, sec, damping);
                            max = max.max(s);
                            min = min.min(s);
                        }
                    }
                }
            }
            let v = (x + ((8 + sum - (sum < 0) as i32) >> 4)).clamp(min, max);
            dst.set(x0 + j, y0 + i, v as u16);
        }
    }
}
