//! SILK output resampler (RFC 6716 §4.2.9: non-normative filter, normative delay budget).
//!
//! A windowed-sinc polyphase interpolator whose group delay matches the delay allocation of
//! RFC 6716 Table 54 (and the integer delays used when no rate conversion is needed), so that the
//! SILK output lines up with the CELT layer in Hybrid mode.

#[derive(Clone)]
pub struct Resampler {
    fin: u32,
    fout: u32,
    /// Pure-delay mode (equal rates): delay line.
    copy_delay: usize,
    /// Polyphase taps per output phase.
    phases: Vec<(i64, Vec<f32>)>,
    period: u64,
    /// Total delay in units of 1/fout input samples.
    delay_q: i64,
    half: i64,
    hist: Vec<f32>,
    /// Absolute input index of `hist[0]`.
    hist_start: i64,
    n_in: i64,
    n_out: u64,
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..30 {
        term *= q / (k * k) as f64;
        sum += term;
    }
    sum
}

impl Resampler {
    pub fn new(fin: u32, fout: u32) -> Resampler {
        let mut r =
            Resampler { fin, fout, copy_delay: 0, phases: Vec::new(), period: 1, delay_q: 0, half: 0, hist: Vec::new(), hist_start: 0, n_in: 0, n_out: 0 };
        if fin == fout {
            r.copy_delay = match fin {
                8000 => 4,
                12000 => 9,
                _ => 12,
            };
            r.hist = vec![0.0; r.copy_delay];
            return r;
        }
        // Delay in milliseconds per internal bandwidth (RFC 6716 Table 54).
        let delay_ms = match fin {
            8000 => 0.538,
            12000 => 0.692,
            _ => 0.706,
        };
        let d_in = delay_ms * fin as f64 / 1000.0; // input samples
        let half = (d_in.floor() as i64).clamp(2, 8);
        r.half = half;
        r.delay_q = (d_in * fout as f64).round() as i64;
        let g = gcd(fin, fout);
        r.period = (fout / g) as u64;
        let fc = 0.92 * (fout.min(fin) as f64 / fin as f64);
        let beta = 5.0;
        for j in 0..r.period as i64 {
            let num = j * fin as i64 - r.delay_q;
            let base = num.div_euclid(fout as i64);
            let frac = num.rem_euclid(fout as i64) as f64 / fout as f64;
            // Taps for input indices base - half + 1 ..= base + half.
            let mut taps = Vec::with_capacity(2 * half as usize);
            for m in (-half + 1)..=half {
                let u = frac - m as f64; // distance from the tap to the output instant
                let x = u * fc;
                let s = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let w = u / (half as f64 + 1.0);
                let win = if w.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - w * w).sqrt()) / bessel_i0(beta) };
                taps.push(s * win);
            }
            let sum: f64 = taps.iter().sum();
            let taps: Vec<f32> = taps.iter().map(|t| (t / sum) as f32).collect();
            r.phases.push((base, taps));
        }
        r.hist = vec![0.0; (2 * half + 4) as usize];
        r.hist_start = -(2 * half + 4);
        r
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.fin, self.fout)
    }

    /// Resamples `input` (16-bit scale) and appends the output samples.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<f32>) {
        if self.fin == self.fout {
            for &v in input {
                self.hist.push(v as f32);
            }
            let n = self.hist.len() - self.copy_delay;
            out.extend_from_slice(&self.hist[..n]);
            self.hist.drain(..n);
            return;
        }
        self.hist.extend(input.iter().map(|&v| v as f32));
        self.n_in += input.len() as i64;
        let total_out = (self.n_in as u64 * self.fout as u64) / self.fin as u64;
        while self.n_out < total_out {
            let j = self.n_out;
            let cycle = (j / self.period) as i64;
            let (base0, ref taps) = self.phases[(j % self.period) as usize];
            let base = base0 + cycle * self.fin as i64 * self.period as i64 / self.fout as i64;
            let first = base - self.half + 1;
            let mut acc = 0f32;
            for (m, &t) in taps.iter().enumerate() {
                let idx = first + m as i64 - self.hist_start;
                if idx >= 0 && (idx as usize) < self.hist.len() {
                    acc += self.hist[idx as usize] * t;
                }
            }
            out.push(acc);
            self.n_out += 1;
        }
        // Trim history, keeping enough for the next taps.
        let keep_from = self.n_in - (2 * self.half + 4);
        if keep_from > self.hist_start {
            let drop = (keep_from - self.hist_start) as usize;
            self.hist.drain(..drop.min(self.hist.len()));
            self.hist_start = keep_from;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsample_sine_and_delay() {
        for &(fin, fout) in &[(16000u32, 48000u32), (8000, 48000), (12000, 48000), (16000, 24000), (16000, 8000), (12000, 16000)] {
            let mut r = Resampler::new(fin, fout);
            let f = 440.0;
            let n = fin as usize / 10;
            let x: Vec<i16> = (0..n).map(|i| ((2.0 * std::f64::consts::PI * f * i as f64 / fin as f64).sin() * 10000.0) as i16).collect();
            let mut y = Vec::new();
            for c in x.chunks(fin as usize / 50) {
                r.process(c, &mut y);
            }
            assert_eq!(y.len(), n * fout as usize / fin as usize);
            // Compare against the ideal delayed sine.
            let delay_ms = match fin {
                8000 => 0.538,
                12000 => 0.692,
                _ => 0.706,
            };
            let mut err = 0f64;
            let mut sig = 0f64;
            for (j, &v) in y.iter().enumerate().skip(fout as usize / 100) {
                let t = j as f64 / fout as f64 - delay_ms / 1000.0;
                let ideal = (2.0 * std::f64::consts::PI * f * t).sin() * 10000.0;
                err += (v as f64 - ideal).powi(2);
                sig += ideal * ideal;
            }
            let snr = 10.0 * (sig / err).log10();
            assert!(snr > 40.0, "{fin}->{fout}: {snr} dB");
        }
    }
}
