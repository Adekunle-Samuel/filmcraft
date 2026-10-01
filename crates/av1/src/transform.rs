//! Inverse transforms (spec 7.13), implemented step by step as specified.

use crate::spec_tables::*;

#[inline(always)]
fn round2_64(x: i64, n: u32) -> i32 {
    if n == 0 { x as i32 } else { ((x + (1i64 << (n - 1))) >> n) as i32 }
}

#[inline(always)]
pub(crate) fn round2(x: i32, n: u32) -> i32 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

#[inline(always)]
fn cos128(angle: i32) -> i64 {
    let a = (angle & 255) as usize;
    (match a {
        0..=64 => COS128_LOOKUP[a] as i32,
        65..=128 => -(COS128_LOOKUP[128 - a] as i32),
        129..=192 => -(COS128_LOOKUP[a - 128] as i32),
        _ => COS128_LOOKUP[256 - a] as i32,
    }) as i64
}

#[inline(always)]
fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

fn brev(num_bits: u32, x: usize) -> usize {
    let mut t = 0;
    for i in 0..num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
    }
    t
}

/// Butterfly rotation B( a, b, angle, flip ).
#[inline(always)]
fn bf(t: &mut [i32], a: usize, b: usize, angle: i32, flip: bool) {
    let x = t[a] as i64 * cos128(angle) - t[b] as i64 * sin128(angle);
    let y = t[a] as i64 * sin128(angle) + t[b] as i64 * cos128(angle);
    t[a] = round2_64(x, 12);
    t[b] = round2_64(y, 12);
    if flip {
        t.swap(a, b);
    }
}

/// Hadamard rotation H( a, b, flip, r ).
#[inline(always)]
fn hd(t: &mut [i32], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a];
    let y = t[b];
    let lo = -(1i32 << (r - 1));
    let hi = (1i32 << (r - 1)) - 1;
    t[a] = (x + y).clamp(lo, hi);
    t[b] = (x - y).clamp(lo, hi);
}

/// Inverse DCT of length 2^n (7.13.2.3), in place.
pub(crate) fn inverse_dct(t: &mut [i32], n: u32, r: u32) {
    // 7.13.2.2 permutation
    let len = 1usize << n;
    let mut copy = [0i32; 64];
    copy[..len].copy_from_slice(&t[..len]);
    for i in 0..len {
        t[i] = copy[brev(n, i)];
    }
    if n == 6 {
        for i in 0..16 {
            bf(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            bf(t, 16 + i, 31 - i, 6 + ((brev(3, 7 - i) as i32) << 3), false);
        }
    }
    if n == 6 {
        for i in 0..16 {
            hd(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            bf(t, 8 + i, 15 - i, 12 + ((brev(2, 3 - i) as i32) << 4), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hd(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                bf(t, 62 - i * 4 - j, 33 + i * 4 + j, 60 - 16 * brev(2, i) as i32 + 64 * j as i32, true);
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            bf(t, 4 + i, 7 - i, 56 - 32 * i as i32, false);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hd(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                bf(t, 30 - 4 * i - j, 17 + 4 * i + j, 24 + ((j as i32) << 6) + (((1 - i) as i32) << 5), true);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hd(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        bf(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0);
    }
    if n >= 3 {
        for i in 0..2 {
            hd(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(t, 14 - i, 9 + i, 48 + 64 * i as i32, true);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hd(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                bf(t, 61 - i * 8 - j, 34 + i * 8 + j, 56 - i as i32 * 32 + (j as i32 >> 1) * 64, true);
            }
        }
    }
    for i in 0..2 {
        hd(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        bf(t, 6, 5, 32, true);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hd(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(t, 29 - i, 18 + i, 48 + (i as i32 >> 1) * 64, true);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hd(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hd(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(t, 13 - i, 10 + i, 32, true);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hd(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hd(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(t, 27 - i, 20 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hd(t, 32 + i, 47 - i, false, r);
            hd(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hd(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(t, 55 - i, 40 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hd(t, i, 63 - i, false, r);
        }
    }
}

const SINPI_1_9: i64 = 1321;
const SINPI_2_9: i64 = 2482;
const SINPI_3_9: i64 = 3344;
const SINPI_4_9: i64 = 3803;

fn inverse_adst4(t: &mut [i32]) {
    let mut s = [0i64; 7];
    let mut x = [0i64; 4];
    let t0 = t[0] as i64;
    let t1 = t[1] as i64;
    let t2 = t[2] as i64;
    let t3 = t[3] as i64;
    s[0] = SINPI_1_9 * t0;
    s[1] = SINPI_2_9 * t0;
    s[2] = SINPI_3_9 * t1;
    s[3] = SINPI_4_9 * t2;
    s[4] = SINPI_1_9 * t2;
    s[5] = SINPI_2_9 * t3;
    s[6] = SINPI_4_9 * t3;
    let a7 = t0 - t2;
    let b7 = a7 + t3;
    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = SINPI_3_9 * b7;
    s[0] += s[5];
    s[1] -= s[6];
    x[0] = s[0] + s[3];
    x[1] = s[1] + s[3];
    x[2] = s[2];
    x[3] = s[0] + s[1];
    x[3] -= s[3];
    for i in 0..4 {
        t[i] = round2_64(x[i], 12);
    }
}

fn adst_in_permute(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let idx = if i & 1 == 1 { i - 1 } else { n0 - i - 1 };
        t[i] = copy[idx];
    }
}

fn adst_out_permute(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        t[i] = if i & 1 == 1 { -copy[idx] } else { copy[idx] };
    }
}

fn inverse_adst8(t: &mut [i32], r: u32) {
    adst_in_permute(t, 3);
    for i in 0..4 {
        bf(t, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true);
    }
    for i in 0..4 {
        hd(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true);
    }
    for i in 0..2 {
        for j in 0..2 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_permute(t, 3);
}

fn inverse_adst16(t: &mut [i32], r: u32) {
    adst_in_permute(t, 4);
    for i in 0..8 {
        bf(t, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true);
    }
    for i in 0..8 {
        hd(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true);
        bf(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true);
    }
    for i in 0..4 {
        for j in 0..2 {
            hd(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            bf(t, 4 + 8 * j + 3 * i, 5 + 8 * j + i, 48 - 32 * i as i32, true);
        }
    }
    for i in 0..2 {
        for j in 0..4 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_permute(t, 4);
}

fn inverse_adst(t: &mut [i32], n: u32, r: u32) {
    match n {
        2 => inverse_adst4(t),
        3 => inverse_adst8(t, r),
        _ => inverse_adst16(t, r),
    }
}

fn inverse_identity(t: &mut [i32], n: u32) {
    match n {
        2 => {
            for v in t[..4].iter_mut() {
                *v = round2_64(*v as i64 * 5793, 12);
            }
        }
        3 => {
            for v in t[..8].iter_mut() {
                *v *= 2;
            }
        }
        4 => {
            for v in t[..16].iter_mut() {
                *v = round2_64(*v as i64 * 11586, 12);
            }
        }
        _ => {
            for v in t[..32].iter_mut() {
                *v *= 4;
            }
        }
    }
}

fn inverse_wht(t: &mut [i32], shift: u32) {
    let mut a = t[0] >> shift;
    let mut c = t[1] >> shift;
    let mut d = t[2] >> shift;
    let mut b = t[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind1d {
    Dct,
    Adst,
    Identity,
}

fn row_kind(tx_type: usize) -> Kind1d {
    match tx_type {
        DCT_DCT | ADST_DCT | FLIPADST_DCT | H_DCT => Kind1d::Dct,
        DCT_ADST | ADST_ADST | DCT_FLIPADST | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST | H_ADST | H_FLIPADST => Kind1d::Adst,
        _ => Kind1d::Identity,
    }
}

fn col_kind(tx_type: usize) -> Kind1d {
    match tx_type {
        DCT_DCT | DCT_ADST | DCT_FLIPADST | V_DCT => Kind1d::Dct,
        ADST_DCT | ADST_ADST | FLIPADST_DCT | FLIPADST_FLIPADST | ADST_FLIPADST | FLIPADST_ADST | V_ADST | V_FLIPADST => Kind1d::Adst,
        _ => Kind1d::Identity,
    }
}

/// 2D inverse transform (7.13.3). `dequant` is 64x64 (row stride 64, only the top-left 32x32
/// may be non-zero); the residual is written to `residual` with row stride `w`.
pub(crate) fn inverse_transform_2d(dequant: &[i32], residual: &mut [i32], tx_sz: usize, tx_type: usize, lossless: bool, bit_depth: u32) {
    let log2w = TX_WIDTH_LOG2[tx_sz] as u32;
    let log2h = TX_HEIGHT_LOG2[tx_sz] as u32;
    let w = 1usize << log2w;
    let h = 1usize << log2h;
    let row_shift = if lossless { 0 } else { TRANSFORM_ROW_SHIFT[tx_sz] as u32 };
    let col_shift = if lossless { 0 } else { 4 };
    let row_clamp = bit_depth + 8;
    let col_clamp = (bit_depth + 6).max(16);
    let rk = row_kind(tx_type);
    let ck = col_kind(tx_type);
    let mut t = [0i32; 64];
    let rect2 = log2w.abs_diff(log2h) == 1;
    for i in 0..h {
        if i >= 32 {
            // rows beyond 32 have no coefficients but still go through the transform
            t[..w].iter_mut().for_each(|v| *v = 0);
        } else {
            for j in 0..w {
                t[j] = if j < 32 { dequant[i * 64 + j] } else { 0 };
            }
        }
        if rect2 {
            for v in t[..w].iter_mut() {
                *v = round2_64(*v as i64 * 2896, 12);
            }
        }
        if lossless {
            inverse_wht(&mut t, 2);
        } else {
            match rk {
                Kind1d::Dct => inverse_dct(&mut t, log2w, row_clamp),
                Kind1d::Adst => inverse_adst(&mut t, log2w, row_clamp),
                Kind1d::Identity => inverse_identity(&mut t, log2w),
            }
        }
        let lo = -(1i32 << (col_clamp - 1));
        let hi = (1i32 << (col_clamp - 1)) - 1;
        for j in 0..w {
            residual[i * w + j] = round2(t[j], row_shift).clamp(lo, hi);
        }
    }
    for j in 0..w {
        for i in 0..h {
            t[i] = residual[i * w + j];
        }
        if lossless {
            inverse_wht(&mut t, 0);
        } else {
            match ck {
                Kind1d::Dct => inverse_dct(&mut t, log2h, col_clamp),
                Kind1d::Adst => inverse_adst(&mut t, log2h, col_clamp),
                Kind1d::Identity => inverse_identity(&mut t, log2h),
            }
        }
        for i in 0..h {
            residual[i * w + j] = round2(t[i], col_shift);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DCT step list must implement an orthogonal transform: a DC-only input gives a flat
    /// output equal to DC * cos(pi/4) (in the 2^12 fixed-point scale).
    #[test]
    fn dct_dc_is_flat() {
        for n in 2..=6 {
            let len = 1 << n;
            let mut t = [0i32; 64];
            t[0] = 4096;
            inverse_dct(&mut t, n, 24);
            for v in &t[..len] {
                assert_eq!(*v, 2896, "n={n}: {:?}", &t[..len]);
            }
        }
    }

    /// Compare each inverse DCT basis vector with the real-valued DCT-III.
    #[test]
    fn dct_matches_float() {
        for n in 2..=6u32 {
            let len = 1usize << n;
            for k in 0..len {
                let mut t = [0i32; 64];
                t[k] = 1 << 12;
                inverse_dct(&mut t, n, 24);
                for x in 0..len {
                    let c = if k == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                    let want = 4096.0 * c * ((2 * x + 1) as f64 * k as f64 * std::f64::consts::PI / (2 * len) as f64).cos();
                    assert!((t[x] as f64 - want).abs() < 4.0 + 0.5 * n as f64, "n={n} k={k} x={x}: {} vs {want}", t[x]);
                }
            }
        }
    }

    #[test]
    fn adst_is_close_to_sine_transform() {
        for n in 3..=4u32 {
            let len = 1usize << n;
            for k in 0..len {
                let mut t = [0i32; 64];
                t[k] = 1 << 12;
                inverse_adst(&mut t, n, 24);
                let energy: f64 = t[..len].iter().map(|&v| (v as f64 / 4096.0).powi(2)).sum();
                // like the DCT, the AV1 ADST is scaled by sqrt(N / 2)
                assert!((energy - len as f64 / 2.0).abs() < 0.02, "n={n} k={k} energy {energy}");
            }
        }
    }
}
