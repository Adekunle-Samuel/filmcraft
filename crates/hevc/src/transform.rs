//! Scaling (8.6.3), transform skip and inverse transforms (8.6.4): DCT 4..32 and DST 4x4.

use crate::tables::{DST_MATRIX, TRANS_MATRIX};

/// Inverse transform of an n x n block of scaled coefficients `c` (row-major, `c[y * n + x]`) in place,
/// producing residuals after the final `bd_shift` rounding (8-299). `max_x` / `max_y` bound the
/// non-zero coefficients (inclusive). `dst`: DST-VII (intra 4x4 luma).
pub fn inverse_transform(c: &mut [i32], n: usize, dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
    let mut tmp = [0i32; 32 * 32];
    let step = 32 / n;
    // 1. columns: e[x][y] = sum_j M[j][y] * d[x][j]
    for x in 0..=max_x {
        for y in 0..n {
            let mut s = 0i32;
            for j in 0..=max_y {
                let m = if dst { DST_MATRIX[j][y] as i32 } else { TRANS_MATRIX[j * step][y] as i32 };
                s += m * c[j * n + x];
            }
            tmp[y * n + x] = ((s + 64) >> 7).clamp(-32768, 32767);
        }
    }
    // 2. rows: r[x][y] = sum_j M[j][x] * g[j][y]
    let round = 1 << (bd_shift - 1);
    for y in 0..n {
        let row = &tmp[y * n..y * n + n];
        for x in 0..n {
            let mut s = 0i32;
            for j in 0..=max_x {
                let m = if dst { DST_MATRIX[j][x] as i32 } else { TRANS_MATRIX[j * step][x] as i32 };
                s += m * row[j];
            }
            c[y * n + x] = (s + round) >> bd_shift;
        }
    }
}

/// DC-only inverse DCT: every residual sample gets the same value.
pub fn inverse_dc(dc: i32, bd_shift: u32) -> i32 {
    let e = ((64 * dc + 64) >> 7).clamp(-32768, 32767);
    (64 * e + (1 << (bd_shift - 1))) >> bd_shift
}

/// Transform skip residual (8-298, 8-299): r = (d << tsShift + round) >> bdShift.
pub fn transform_skip(c: &mut [i32], n: usize, bd_shift: u32) {
    let ts_shift = 5 + n.trailing_zeros();
    let round = 1 << (bd_shift - 1);
    for v in c[..n * n].iter_mut() {
        *v = ((*v << ts_shift) + round) >> bd_shift;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_matches_full_transform() {
        for &n in &[4usize, 8, 16, 32] {
            for dc in [-300, -1, 1, 7, 64, 1000] {
                let mut c = vec![0i32; n * n];
                c[0] = dc;
                inverse_transform(&mut c, n, false, 0, 0, 12);
                let v = inverse_dc(dc, 12);
                assert!(c.iter().all(|&x| x == v), "n {n} dc {dc}");
            }
        }
    }

    #[test]
    fn bounded_region_matches_full() {
        let n = 16;
        let mut c = vec![0i32; n * n];
        c[0] = 50;
        c[1] = -20;
        c[2 * n + 3] = 9;
        let mut a = c.clone();
        inverse_transform(&mut a, n, false, 3, 2, 12);
        let mut b = c.clone();
        inverse_transform(&mut b, n, false, n - 1, n - 1, 12);
        assert_eq!(a, b);
    }
}
