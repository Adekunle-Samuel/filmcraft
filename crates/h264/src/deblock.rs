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

/// Referenced picture ids per 8x8 block and list (u32::MAX = list unused).
fn ref_ids8(st: &MbState, sl: &SliceInfo) -> [[u32; 2]; 4] {
    std::array::from_fn(|b8| {
        std::array::from_fn(|l| {
            let r = st.ref_idx[l][b8];
            if r >= 0 { sl.ref_ids[l].get(r as usize).copied().unwrap_or(u32::MAX - 1) } else { u32::MAX }
        })
    })
}

#[inline(always)]
fn block_motion(st: &MbState, ids8: &[[u32; 2]; 4], raster: usize) -> Motion {
    let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
    let ids = ids8[b8];
    let count = (ids[0] != u32::MAX) as u8 + (ids[1] != u32::MAX) as u8;
    Motion { ids, mv: [st.mv[0][raster], st.mv[1][raster]], count }
}

#[inline(always)]
fn mv_far(a: [i16; 2], b: [i16; 2]) -> bool {
    (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4
}

#[inline]
fn motion_bs(p: &Motion, q: &Motion) -> u8 {
    if p.ids == q.ids && p.mv == q.mv {
        return 0;
    }
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

#[inline(always)]
fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Filter one set of eight samples p3 p2 p1 p0 | q0 q1 q2 q3 across an edge (8.7.2.3 / 8.7.2.4).
#[inline(always)]
fn filter8(v: &mut [u8; 8], bs: u8, alpha: i32, beta: i32, tc0: i32, chroma: bool) {
    let p0 = v[3] as i32;
    let q0 = v[4] as i32;
    let p1 = v[2] as i32;
    let q1 = v[5] as i32;
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if chroma {
        if bs < 4 {
            let tc = tc0 + 1;
            let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
            v[3] = (p0 + delta).clamp(0, 255) as u8;
            v[4] = (q0 - delta).clamp(0, 255) as u8;
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
        }
        return;
    }
    let p2 = v[1] as i32;
    let q2 = v[6] as i32;
    let ap = (p2 - p0).abs();
    let aq = (q2 - q0).abs();
    if bs < 4 {
        let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
        let delta = clip3(-tc, tc, (((q0 - p0) << 2) + (p1 - q1) + 4) >> 3);
        v[3] = (p0 + delta).clamp(0, 255) as u8;
        v[4] = (q0 - delta).clamp(0, 255) as u8;
        if ap < beta {
            v[2] = (p1 + clip3(-tc0, tc0, (p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1)) as u8;
        }
        if aq < beta {
            v[5] = (q1 + clip3(-tc0, tc0, (q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1)) as u8;
        }
    } else {
        let strong = (p0 - q0).abs() < ((alpha >> 2) + 2);
        if ap < beta && strong {
            let p3 = v[0] as i32;
            v[3] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u8;
            v[2] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u8;
            v[1] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u8;
        } else {
            v[3] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        }
        if aq < beta && strong {
            let q3 = v[7] as i32;
            v[4] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u8;
            v[5] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u8;
            v[6] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u8;
        } else {
            v[4] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
        }
    }
}

/// Edge filter parameters for `n` lines; `bs[i]` / `tc0[i]` apply to lines `i * n / 4 .. (i + 1) * n / 4`.
struct EdgeParams {
    bs: [u8; 4],
    tc0: [i32; 4],
    alpha: i32,
    beta: i32,
}

/// Filter a vertical edge whose q0 column is at `x`, for `n` lines starting at row `y`.
#[inline(always)]
fn filter_vertical(pix: &mut [u8], stride: usize, x: usize, y: usize, n: usize, ep: &EdgeParams, chroma: bool) {
    let per = n / 4;
    for i in 0..n {
        let k = i / per;
        let b = ep.bs[k];
        if b == 0 {
            continue;
        }
        let o = (y + i) * stride + x - 4;
        let v: &mut [u8; 8] = (&mut pix[o..o + 8]).try_into().unwrap();
        filter8(v, b, ep.alpha, ep.beta, ep.tc0[k], chroma);
    }
}

/// Filter a horizontal edge whose q0 row is `y`, for `n` columns starting at `x`.
#[inline(always)]
fn filter_horizontal(pix: &mut [u8], stride: usize, x: usize, y: usize, n: usize, ep: &EdgeParams, chroma: bool) {
    let base = (y - 4) * stride + x;
    let rest = &mut pix[base..];
    let (r0, rest) = rest.split_at_mut(stride);
    let (r1, rest) = rest.split_at_mut(stride);
    let (r2, rest) = rest.split_at_mut(stride);
    let (r3, rest) = rest.split_at_mut(stride);
    let (r4, rest) = rest.split_at_mut(stride);
    let (r5, rest) = rest.split_at_mut(stride);
    let (r6, rest) = rest.split_at_mut(stride);
    let r7 = &mut rest[..n];
    let (r0, r1, r2, r3, r4, r5, r6) = (&mut r0[..n], &mut r1[..n], &mut r2[..n], &mut r3[..n], &mut r4[..n], &mut r5[..n], &mut r6[..n]);
    let per = n / 4;
    for c in 0..n {
        let k = c / per;
        let b = ep.bs[k];
        if b == 0 {
            continue;
        }
        let mut v = [r0[c], r1[c], r2[c], r3[c], r4[c], r5[c], r6[c], r7[c]];
        filter8(&mut v, b, ep.alpha, ep.beta, ep.tc0[k], chroma);
        r1[c] = v[1];
        r2[c] = v[2];
        r3[c] = v[3];
        r4[c] = v[4];
        r5[c] = v[5];
        r6[c] = v[6];
    }
}

/// Deblock one macroblock (macroblocks must be processed in raster order).
#[allow(clippy::needless_range_loop)]
pub fn deblock_mb(pic: &mut PicState, addr: usize, mb_w: usize) {
    let q = &pic.mbs[addr];
    if q.slice_num == u32::MAX {
        return;
    }
    let sq = &pic.slices[q.slice_num as usize];
    if sq.disable_deblocking_filter_idc == 1 {
        return;
    }
    let (mx, my) = (addr % mb_w, addr / mb_w);
    let usable = |n: Option<usize>| -> Option<usize> {
        let n = n?;
        let st = &pic.mbs[n];
        if st.slice_num == u32::MAX || (sq.disable_deblocking_filter_idc == 2 && st.slice_num != q.slice_num) {
            return None;
        }
        Some(n)
    };
    let left = usable(if mx > 0 { Some(addr - 1) } else { None });
    let top = usable(if my > 0 { Some(addr - mb_w) } else { None });
    let alpha_off = sq.alpha_offset;
    let beta_off = sq.beta_offset;
    let t8 = q.transform_8x8;
    // bS[dir][edge][segment]; dir 0 = vertical edges (x), 1 = horizontal edges (y)
    let mut bs = [[[0u8; 4]; 4]; 2];
    if q.kind.is_intra() {
        for dir in 0..2 {
            if (if dir == 0 { left } else { top }).is_some() {
                bs[dir][0] = [4; 4];
            }
            for e in 1..4 {
                if !(t8 && e % 2 == 1) {
                    bs[dir][e] = [3; 4];
                }
            }
        }
    } else {
        let qids = ref_ids8(q, sq);
        let q_uniform = (0..2).all(|l| {
            let r = q.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && q.mv[l].iter().all(|m| *m == q.mv[l][0])
        });
        let qm: [Motion; 16] = std::array::from_fn(|r| block_motion(q, &qids, r));
        for dir in 0..2 {
            let neighbor = if dir == 0 { left } else { top };
            if let Some(n) = neighbor {
                let p = &pic.mbs[n];
                if p.kind.is_intra() {
                    bs[dir][0] = [4; 4];
                } else {
                    let pids = ref_ids8(p, &pic.slices[p.slice_num as usize]);
                    for k in 0..4 {
                        let (rq, rp) = if dir == 0 { (k * 4, k * 4 + 3) } else { (k, 12 + k) };
                        bs[dir][0][k] =
                            if (p.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&block_motion(p, &pids, rp), &qm[rq]) };
                    }
                }
            }
            if q_uniform && q.nz_mask == 0 {
                // one motion for the whole MB and no coefficients: all internal edges have bS 0
                continue;
            }
            for e in 1..4 {
                if t8 && e % 2 == 1 {
                    continue;
                }
                for k in 0..4 {
                    let (rq, rp) = if dir == 0 { (k * 4 + e, k * 4 + e - 1) } else { (e * 4 + k, e * 4 + k - 4) };
                    bs[dir][e][k] = if (q.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 { 2 } else { motion_bs(&qm[rp], &qm[rq]) };
                }
            }
        }
    }
    let qp_of = |st: &MbState| if st.kind == MbKind::IPcm { 0 } else { st.qp as i32 };
    let q_qp = qp_of(q);
    let q_qpc = q.qpc;
    let p_qp = [left.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc)), top.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc))];
    let params = |qpp: i32, qpq: i32, b: [u8; 4]| {
        let qpav = (qpp + qpq + 1) >> 1;
        let index_a = (qpav + alpha_off).clamp(0, 51) as usize;
        let index_b = (qpav + beta_off).clamp(0, 51) as usize;
        let tc = |b: u8| if (1..4).contains(&b) { TC0[index_a][b as usize - 1] as i32 } else { 0 };
        EdgeParams { bs: b, tc0: [tc(b[0]), tc(b[1]), tc(b[2]), tc(b[3])], alpha: ALPHA[index_a] as i32, beta: BETA[index_b] as i32 }
    };
    let width = pic.planes.width;
    let cwidth = pic.planes.cwidth;
    // luma
    for dir in 0..2 {
        for e in 0..4 {
            if bs[dir][e] == [0; 4] {
                continue;
            }
            let qpp = if e == 0 { p_qp[dir].map(|p| p.0).unwrap_or(q_qp) } else { q_qp };
            let ep = params(qpp, q_qp, bs[dir][e]);
            if dir == 0 {
                filter_vertical(&mut pic.planes.y, width, mx * 16 + e * 4, my * 16, 16, &ep, false);
            } else {
                filter_horizontal(&mut pic.planes.y, width, mx * 16, my * 16 + e * 4, 16, &ep, false);
            }
        }
    }
    // chroma (4:2:0): edges at chroma samples 0 and 4 use the bS of luma edges 0 and 2
    for c in 0..2 {
        for dir in 0..2 {
            for ce in 0..2 {
                let e = ce * 2;
                if bs[dir][e] == [0; 4] {
                    continue;
                }
                let qpp = if e == 0 { p_qp[dir].map(|p| p.1[c]).unwrap_or(q_qpc[c]) } else { q_qpc[c] } as i32;
                let ep = params(qpp, q_qpc[c] as i32, bs[dir][e]);
                let plane = if c == 0 { &mut pic.planes.cb } else { &mut pic.planes.cr };
                if dir == 0 {
                    filter_vertical(plane, cwidth, mx * 8 + ce * 4, my * 8, 8, &ep, true);
                } else {
                    filter_horizontal(plane, cwidth, mx * 8, my * 8 + ce * 4, 8, &ep, true);
                }
            }
        }
    }
}
