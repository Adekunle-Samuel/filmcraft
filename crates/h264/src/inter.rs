//! Inter prediction sample interpolation (8.4.2.2) and weighted sample prediction (8.4.2.3).

/// A reference plane.
#[derive(Clone, Copy)]
pub struct PlaneRef<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

const WIN: usize = 16 + 5;

/// Copy the (bw+5)x(bh+5) window whose top-left is (x-2, y-2) into `win` (stride WIN), clamping
/// coordinates to the picture.
#[inline]
fn fetch_window(p: PlaneRef, x: i32, y: i32, bw: usize, bh: usize, win: &mut [u8; WIN * WIN]) {
    let x0 = x - 2;
    let y0 = y - 2;
    let ww = bw + 5;
    let wh = bh + 5;
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    if x0 >= 0 && y0 >= 0 && x0 + ww as i32 - 1 <= maxx && y0 + wh as i32 - 1 <= maxy {
        for r in 0..wh {
            let src = (y0 as usize + r) * p.stride + x0 as usize;
            win[r * WIN..r * WIN + ww].copy_from_slice(&p.data[src..src + ww]);
        }
    } else {
        for r in 0..wh {
            let yy = (y0 + r as i32).clamp(0, maxy) as usize;
            let row = &p.data[yy * p.stride..yy * p.stride + p.width];
            for c in 0..ww {
                let xx = (x0 + c as i32).clamp(0, maxx) as usize;
                win[r * WIN + c] = row[xx];
            }
        }
    }
}

#[inline(always)]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

#[inline(always)]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Luma sample interpolation for a bw x bh block. (x, y) is the integer sample position
/// (block position + (mv >> 2)); (fx, fy) the quarter-sample fraction. Output stride is `os`.
#[allow(clippy::too_many_arguments)]
pub fn mc_luma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
    let mut win = [0u8; WIN * WIN];
    fetch_window(p, x, y, bw, bh, &mut win);
    // sample G at (r, c) in block coordinates is win[(r + 2) * WIN + c + 2]
    let g = |r: usize, c: usize| win[(r + 2) * WIN + c + 2] as i32;
    // unclipped horizontal half-sample b1 at block row r (-2..bh+3 via offset) and column c
    let b1 = |r: isize, c: usize| {
        let base = ((r + 2) as usize) * WIN + c;
        tap6(win[base] as i32, win[base + 1] as i32, win[base + 2] as i32, win[base + 3] as i32, win[base + 4] as i32, win[base + 5] as i32)
    };
    let h1 = |r: usize, c: isize| {
        let col = (c + 2) as usize;
        tap6(
            win[r * WIN + col] as i32,
            win[(r + 1) * WIN + col] as i32,
            win[(r + 2) * WIN + col] as i32,
            win[(r + 3) * WIN + col] as i32,
            win[(r + 4) * WIN + col] as i32,
            win[(r + 5) * WIN + col] as i32,
        )
    };
    match (fx, fy) {
        (0, 0) => {
            for r in 0..bh {
                out[r * os..r * os + bw].copy_from_slice(&win[(r + 2) * WIN + 2..(r + 2) * WIN + 2 + bw]);
            }
        }
        (_, 0) => {
            // a, b, c
            for r in 0..bh {
                for c in 0..bw {
                    let b = clip1((b1(r as isize, c) + 16) >> 5) as i32;
                    out[r * os + c] = match fx {
                        1 => ((g(r, c) + b + 1) >> 1) as u8,
                        2 => b as u8,
                        _ => ((g(r, c + 1) + b + 1) >> 1) as u8,
                    };
                }
            }
        }
        (0, _) => {
            // d, h, n
            for r in 0..bh {
                for c in 0..bw {
                    let h = clip1((h1(r, c as isize) + 16) >> 5) as i32;
                    out[r * os + c] = match fy {
                        1 => ((g(r, c) + h + 1) >> 1) as u8,
                        2 => h as u8,
                        _ => ((g(r + 1, c) + h + 1) >> 1) as u8,
                    };
                }
            }
        }
        (2, _) | (_, 2) => {
            // j-based: j needs b1 for rows -2..bh+3
            let mut bcol = [0i32; WIN * WIN];
            for r in 0..bh + 5 {
                for c in 0..bw {
                    bcol[r * WIN + c] = b1(r as isize - 2, c);
                }
            }
            for r in 0..bh {
                for c in 0..bw {
                    let j1 = tap6(
                        bcol[r * WIN + c],
                        bcol[(r + 1) * WIN + c],
                        bcol[(r + 2) * WIN + c],
                        bcol[(r + 3) * WIN + c],
                        bcol[(r + 4) * WIN + c],
                        bcol[(r + 5) * WIN + c],
                    );
                    let j = clip1((j1 + 512) >> 10) as i32;
                    let v = match (fx, fy) {
                        (2, 2) => j,
                        (2, 1) => (clip1((bcol[(r + 2) * WIN + c] + 16) >> 5) as i32 + j + 1) >> 1, // f
                        (2, 3) => (clip1((bcol[(r + 3) * WIN + c] + 16) >> 5) as i32 + j + 1) >> 1, // q
                        (1, 2) => (clip1((h1(r, c as isize) + 16) >> 5) as i32 + j + 1) >> 1,       // i
                        _ => (clip1((h1(r, c as isize + 1) + 16) >> 5) as i32 + j + 1) >> 1,        // k
                    };
                    out[r * os + c] = v as u8;
                }
            }
        }
        _ => {
            // e, g, p, r: average of a horizontal half-sample (b or s) and a vertical one (h or m)
            for r in 0..bh {
                for c in 0..bw {
                    let hr = if fy == 1 { r as isize } else { r as isize + 1 };
                    let vc = if fx == 1 { c as isize } else { c as isize + 1 };
                    let bh_ = clip1((b1(hr, c) + 16) >> 5) as i32;
                    let vv = clip1((h1(r, vc) + 16) >> 5) as i32;
                    out[r * os + c] = ((bh_ + vv + 1) >> 1) as u8;
                }
            }
        }
    }
}

/// Chroma sample interpolation (8.4.2.2.2) for a bw x bh block at integer position (x, y) with
/// eighth-sample fraction (fx, fy).
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma(p: PlaneRef, x: i32, y: i32, fx: u32, fy: u32, bw: usize, bh: usize, out: &mut [u8], os: usize) {
    let maxx = p.width as i32 - 1;
    let maxy = p.height as i32 - 1;
    let fx = fx as i32;
    let fy = fy as i32;
    let w00 = (8 - fx) * (8 - fy);
    let w10 = fx * (8 - fy);
    let w01 = (8 - fx) * fy;
    let w11 = fx * fy;
    let inside = x >= 0 && y >= 0 && x + bw as i32 <= maxx && y + bh as i32 <= maxy;
    if inside {
        let (x, y) = (x as usize, y as usize);
        for r in 0..bh {
            let r0 = &p.data[(y + r) * p.stride + x..];
            let r1 = &p.data[(y + r + 1) * p.stride + x..];
            for c in 0..bw {
                let v = w00 * r0[c] as i32 + w10 * r0[c + 1] as i32 + w01 * r1[c] as i32 + w11 * r1[c + 1] as i32;
                out[r * os + c] = ((v + 32) >> 6) as u8;
            }
        }
    } else {
        let at = |xx: i32, yy: i32| p.data[yy.clamp(0, maxy) as usize * p.stride + xx.clamp(0, maxx) as usize] as i32;
        for r in 0..bh as i32 {
            for c in 0..bw as i32 {
                let v = w00 * at(x + c, y + r) + w10 * at(x + c + 1, y + r) + w01 * at(x + c, y + r + 1) + w11 * at(x + c + 1, y + r + 1);
                out[(r as usize) * os + c as usize] = ((v + 32) >> 6) as u8;
            }
        }
    }
}

/// Weights for one prediction direction or pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    /// Default: copy (single list) or rounded average (bi).
    Default,
    /// Explicit/implicit weighting: log2 denominator, weights and offsets for L0 and L1.
    Weighted { log_wd: i32, w0: i32, w1: i32, o0: i32, o1: i32 },
}

/// Combine prediction(s) into `dst` (stride `ds`). `p0`/`p1` have stride `ps`.
#[allow(clippy::too_many_arguments)]
pub fn weighted_store(dst: &mut [u8], ds: usize, p0: Option<&[u8]>, p1: Option<&[u8]>, ps: usize, bw: usize, bh: usize, w: Weight) {
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = ((x as u32 + y as u32 + 1) >> 1) as u8;
                }
            }
        }
        (Some(a), Some(b), Weight::Weighted { log_wd, w0, w1, o0, o1 }) => {
            let round = 1 << log_wd;
            let off = (o0 + o1 + 1) >> 1;
            for r in 0..bh {
                for c in 0..bw {
                    let v = ((a[r * ps + c] as i32 * w0 + b[r * ps + c] as i32 * w1 + round) >> (log_wd + 1)) + off;
                    dst[r * ds + c] = clip1(v);
                }
            }
        }
        (Some(a), None, w) | (None, Some(a), w) => {
            let (wt, o) = match (w, p0.is_some()) {
                (Weight::Default, _) => {
                    for r in 0..bh {
                        dst[r * ds..r * ds + bw].copy_from_slice(&a[r * ps..r * ps + bw]);
                    }
                    return;
                }
                (Weight::Weighted { w0, o0, .. }, true) => (w0, o0),
                (Weight::Weighted { w1, o1, .. }, false) => (w1, o1),
            };
            let log_wd = match w {
                Weight::Weighted { log_wd, .. } => log_wd,
                Weight::Default => 0,
            };
            for r in 0..bh {
                for c in 0..bw {
                    let s = a[r * ps + c] as i32;
                    let v = if log_wd >= 1 { ((s * wt + (1 << (log_wd - 1))) >> log_wd) + o } else { s * wt + o };
                    dst[r * ds + c] = clip1(v);
                }
            }
        }
        (None, None, _) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..w * h).map(|i| f(i % w, i / w)).collect()
    }

    #[test]
    fn full_sample_copy_and_clamp() {
        let d = plane(16, 16, |x, y| (x + 16 * y) as u8);
        let p = PlaneRef { data: &d, width: 16, height: 16, stride: 16 };
        let mut out = [0u8; 16];
        mc_luma(p, 2, 3, 0, 0, 4, 4, &mut out, 4);
        assert_eq!(&out[..4], &[50, 51, 52, 53]);
        // clamped: far outside to the top-left gives sample (0,0)
        mc_luma(p, -40, -40, 0, 0, 4, 4, &mut out, 4);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn half_sample_constant_is_constant() {
        let d = vec![100u8; 32 * 32];
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        for fx in 0..4 {
            for fy in 0..4 {
                let mut out = [0u8; 64];
                mc_luma(p, 8, 8, fx, fy, 8, 8, &mut out, 8);
                assert!(out.iter().all(|&v| v == 100), "{fx},{fy}");
                mc_chroma(p, 8, 8, fx * 2, fy * 2, 8, 8, &mut out, 8);
                assert!(out.iter().all(|&v| v == 100));
            }
        }
    }

    #[test]
    fn half_sample_horizontal_ramp() {
        // linear ramp: 6-tap filter of a linear function is exact: b = (G + H) / 2 (rounded)
        let d = plane(32, 32, |x, _| (x * 4) as u8);
        let p = PlaneRef { data: &d, width: 32, height: 32, stride: 32 };
        let mut out = [0u8; 16];
        mc_luma(p, 8, 8, 2, 0, 4, 4, &mut out, 4);
        assert_eq!(&out[..4], &[34, 38, 42, 46]);
        mc_luma(p, 8, 8, 1, 0, 4, 4, &mut out, 4);
        assert_eq!(&out[..4], &[33, 37, 41, 45]);
    }

    #[test]
    fn weighting() {
        let a = [100u8; 4];
        let b = [50u8; 4];
        let mut d = [0u8; 4];
        weighted_store(&mut d, 4, Some(&a), Some(&b), 4, 4, 1, Weight::Default);
        assert_eq!(d, [75; 4]);
        weighted_store(&mut d, 4, Some(&a), None, 4, 4, 1, Weight::Weighted { log_wd: 5, w0: 16, w1: 0, o0: 3, o1: 0 });
        assert_eq!(d, [53; 4]);
    }
}
