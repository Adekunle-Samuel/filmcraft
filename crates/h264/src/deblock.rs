//! Deblocking filter (8.7) for progressive frames, 4:2:0, 8-bit.

use crate::picture::{MbKind, MbState};
use crate::slicedec::{PicState, SliceInfo};
use crate::tables::{ALPHA, BETA, TC0};

/// Reference picture identity + mvs of one 4x4 block, for bS = 1 decisions.
#[derive(Clone, Copy)]
struct Motion {
    ids: [u32; 2],
    mv: [[i16; 2]; 2],
    count: u8,
}

fn block_motion(st: &MbState, sl: &SliceInfo, raster: usize) -> Motion {
    let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
    let mut m = Motion { ids: [u32::MAX; 2], mv: [[0; 2]; 2], count: 0 };
    for l in 0..2 {
        let r = st.ref_idx[l][b8];
        if r >= 0 {
            m.ids[l] = sl.ref_ids[l].get(r as usize).copied().unwrap_or(u32::MAX - 1);
            m.mv[l] = st.mv[l][raster];
            m.count += 1;
        }
    }
    m
}

#[inline(always)]
fn mv_far(a: [i16; 2], b: [i16; 2]) -> bool {
    (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4
}

fn motion_bs(p: &Motion, q: &Motion) -> u8 {
    if p.count != q.count {
        return 1;
    }
    if p.count == 1 {
        let (pi, pm) = if p.ids[0] != u32::MAX { (p.ids[0], p.mv[0]) } else { (p.ids[1], p.mv[1]) };
        let (qi, qm) = if q.ids[0] != u32::MAX { (q.ids[0], q.mv[0]) } else { (q.ids[1], q.mv[1]) };
        if pi != qi {
            return 1;
        }
        return mv_far(pm, qm) as u8;
    }
    if p.count == 0 {
        return 0;
    }
    // two motion vectors each
    let same_set = (p.ids[0] == q.ids[0] && p.ids[1] == q.ids[1]) || (p.ids[0] == q.ids[1] && p.ids[1] == q.ids[0]);
    if !same_set {
        return 1;
    }
    if p.ids[0] != p.ids[1] {
        // two different reference pictures: compare mvs referring to the same picture
        if p.ids[0] == q.ids[0] {
            (mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1])) as u8
        } else {
            (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0])) as u8
        }
    } else {
        // both mvs refer to the same picture
        ((mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1])) && (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0]))) as u8
    }
}

/// bS for the edge between 4x4 block `rp` of MB `p` and 4x4 block `rq` of MB `q`.
#[allow(clippy::too_many_arguments)]
fn compute_bs(p: &MbState, sp: &SliceInfo, rp: usize, q: &MbState, sq: &SliceInfo, rq: usize, mb_edge: bool) -> u8 {
    if p.kind.is_intra() || q.kind.is_intra() {
        return if mb_edge { 4 } else { 3 };
    }
    if (p.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 {
        return 2;
    }
    motion_bs(&block_motion(p, sp, rp), &block_motion(q, sq, rq))
}

#[inline(always)]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Filter one line of samples across an edge. `pix` is the plane, `q0` the index of q0 and `step` the
/// distance between successive samples across the edge.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn filter_line(pix: &mut [u8], q0: usize, step: usize, bs: u8, alpha: i32, beta: i32, tc0: i32, chroma: bool) {
    let p0i = q0 - step;
    let p0 = pix[p0i] as i32;
    let q0v = pix[q0] as i32;
    let p1 = pix[p0i - step] as i32;
    let q1 = pix[q0 + step] as i32;
    if (p0 - q0v).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0v).abs() >= beta {
        return;
    }
    if chroma {
        if bs < 4 {
            let tc = tc0 + 1;
            let delta = clip3(-tc, tc, (((q0v - p0) << 2) + (p1 - q1) + 4) >> 3);
            pix[p0i] = (p0 + delta).clamp(0, 255) as u8;
            pix[q0] = (q0v - delta).clamp(0, 255) as u8;
        } else {
            pix[p0i] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
            pix[q0] = ((2 * q1 + q0v + p1 + 2) >> 2) as u8;
        }
        return;
    }
    let p2 = pix[p0i - 2 * step] as i32;
    let q2 = pix[q0 + 2 * step] as i32;
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0v).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = clip3(-tc, tc, (((q0v - p0) << 2) + (p1 - q1) + 4) >> 3);
        pix[p0i] = (p0 + delta).clamp(0, 255) as u8;
        pix[q0] = (q0v - delta).clamp(0, 255) as u8;
        if ap < beta {
            pix[p0i - step] = (p1 + clip3(-tc0, tc0, (p2 + ((p0 + q0v + 1) >> 1) - (p1 << 1)) >> 1)) as u8;
        }
        if aq < beta {
            pix[q0 + step] = (q1 + clip3(-tc0, tc0, (q2 + ((p0 + q0v + 1) >> 1) - (q1 << 1)) >> 1)) as u8;
        }
    } else {
        let strong = (p0 - q0v).abs() < ((alpha >> 2) + 2);
        if ap < beta && strong {
            let p3 = pix[p0i - 3 * step] as i32;
            pix[p0i] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0v + q1 + 4) >> 3) as u8;
            pix[p0i - step] = ((p2 + p1 + p0 + q0v + 2) >> 2) as u8;
            pix[p0i - 2 * step] = ((2 * p3 + 3 * p2 + p1 + p0 + q0v + 4) >> 3) as u8;
        } else {
            pix[p0i] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        }
        if aq < beta && strong {
            let q3 = pix[q0 + 3 * step] as i32;
            pix[q0] = ((p1 + 2 * p0 + 2 * q0v + 2 * q1 + q2 + 4) >> 3) as u8;
            pix[q0 + step] = ((p0 + q0v + q1 + q2 + 2) >> 2) as u8;
            pix[q0 + 2 * step] = ((2 * q3 + 3 * q2 + q1 + q0v + p0 + 4) >> 3) as u8;
        } else {
            pix[q0] = ((2 * q1 + q0v + p1 + 2) >> 2) as u8;
        }
    }
}

/// Deblock the whole picture in place.
pub fn deblock_picture(pic: &mut PicState) {
    let mb_w = pic.mb_w;
    for addr in 0..pic.mbs.len() {
        deblock_mb(pic, addr, mb_w);
    }
}

/// Deblock one macroblock (macroblocks must be processed in raster order).
#[allow(clippy::needless_range_loop)]
pub fn deblock_mb(pic: &mut PicState, addr: usize, mb_w: usize) {
    let q = pic.mbs[addr];
    if q.slice_num == u32::MAX {
        return;
    }
    let sq = &pic.slices[q.slice_num as usize];
    if sq.disable_deblocking_filter_idc == 1 {
        return;
    }
    let (mx, my) = (addr % mb_w, addr / mb_w);
    let left = if mx > 0 { Some(addr - 1) } else { None };
    let top = if my > 0 { Some(addr - mb_w) } else { None };
    let usable = |n: Option<usize>| -> Option<usize> {
        let n = n?;
        let st = &pic.mbs[n];
        if st.slice_num == u32::MAX {
            return None;
        }
        if sq.disable_deblocking_filter_idc == 2 && st.slice_num != q.slice_num {
            return None;
        }
        Some(n)
    };
    let left = usable(left);
    let top = usable(top);
    let alpha_off = sq.alpha_offset;
    let beta_off = sq.beta_offset;
    let t8 = q.transform_8x8;
    // bS[dir][edge][segment]; dir 0 = vertical edges (x), 1 = horizontal edges (y)
    let mut bs = [[[0u8; 4]; 4]; 2];
    for dir in 0..2 {
        for e in 0..4 {
            if e > 0 && t8 && e % 2 == 1 {
                continue;
            }
            let neighbor = if dir == 0 { left } else { top };
            if e == 0 && neighbor.is_none() {
                continue;
            }
            for k in 0..4 {
                let (rq, rp, pmb) = if dir == 0 {
                    let rq = k * 4 + e;
                    if e == 0 { (rq, k * 4 + 3, neighbor.unwrap()) } else { (rq, rq - 1, addr) }
                } else {
                    let rq = e * 4 + k;
                    if e == 0 { (rq, 12 + k, neighbor.unwrap()) } else { (rq, rq - 4, addr) }
                };
                let p = &pic.mbs[pmb];
                let sp = &pic.slices[p.slice_num as usize];
                bs[dir][e][k] = compute_bs(p, sp, rp, &q, sq, rq, e == 0);
            }
        }
    }
    let width = pic.planes.width;
    let cwidth = pic.planes.cwidth;
    let qp_of = |st: &MbState| if st.kind == MbKind::IPcm { 0 } else { st.qp as i32 };
    // luma
    for dir in 0..2 {
        for e in 0..4 {
            if bs[dir][e] == [0; 4] {
                continue;
            }
            let pmb = if e == 0 { if dir == 0 { left.unwrap() } else { top.unwrap() } } else { addr };
            let qpav = (qp_of(&pic.mbs[pmb]) + qp_of(&q) + 1) >> 1;
            let index_a = (qpav + alpha_off).clamp(0, 51) as usize;
            let index_b = (qpav + beta_off).clamp(0, 51) as usize;
            let alpha = ALPHA[index_a] as i32;
            let beta = BETA[index_b] as i32;
            for k in 0..4 {
                let b = bs[dir][e][k];
                if b == 0 {
                    continue;
                }
                let tc0 = if b < 4 { TC0[index_a][b as usize - 1] as i32 } else { 0 };
                for i in 0..4 {
                    let (x, y) = if dir == 0 { (mx * 16 + e * 4, my * 16 + k * 4 + i) } else { (mx * 16 + k * 4 + i, my * 16 + e * 4) };
                    let step = if dir == 0 { 1 } else { width };
                    filter_line(&mut pic.planes.y, y * width + x, step, b, alpha, beta, tc0, false);
                }
            }
        }
    }
    // chroma: edges 0 and 2 (in 4x4 luma edge units), 8 lines each (4:2:0: chroma edge at 0 and 4 samples)
    for c in 0..2 {
        for dir in 0..2 {
            for ce in 0..2 {
                let e = ce * 2;
                if bs[dir][e] == [0; 4] {
                    continue;
                }
                let pmb = if e == 0 { if dir == 0 { left.unwrap() } else { top.unwrap() } } else { addr };
                let qpp = pic.mbs[pmb].qpc[c] as i32;
                let qpav = (qpp + q.qpc[c] as i32 + 1) >> 1;
                let index_a = (qpav + alpha_off).clamp(0, 51) as usize;
                let index_b = (qpav + beta_off).clamp(0, 51) as usize;
                let alpha = ALPHA[index_a] as i32;
                let beta = BETA[index_b] as i32;
                for i in 0..8 {
                    let b = bs[dir][e][i / 2];
                    if b == 0 {
                        continue;
                    }
                    let tc0 = if b < 4 { TC0[index_a][b as usize - 1] as i32 } else { 0 };
                    let (x, y) = if dir == 0 { (mx * 8 + ce * 4, my * 8 + i) } else { (mx * 8 + i, my * 8 + ce * 4) };
                    let step = if dir == 0 { 1 } else { cwidth };
                    let plane = if c == 0 { &mut pic.planes.cb } else { &mut pic.planes.cr };
                    filter_line(plane, y * cwidth + x, step, b, alpha, beta, tc0, true);
                }
            }
        }
    }
}
