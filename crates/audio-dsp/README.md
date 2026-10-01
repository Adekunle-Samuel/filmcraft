# filmcraft-audio-dsp

Real-time audio DSP for FilmCraft. Layer **L1**, **no dependencies**, compiles for
`wasm32-unknown-unknown`, no `unsafe`. Clean-room: written from the public standards
(ITU-R BS.1770-4, EBU R128 / Tech 3341 / Tech 3342) and textbook DSP (RBJ cookbook biquads,
Householder FDN, phase vocoder).

## Conventions

- Planar `f32` audio: `&mut [&mut [f32]]`, one slice per channel, equal lengths.
- Effects are built (allocating) with `(sample_rate, channels)`; `process`, `set_param` and
  `reset` never allocate.
- Parameters change between blocks and are smoothed per sample; output does not depend on how
  the stream is cut into blocks (tested for every effect with 0/1/random-size blocks).
- Recursive state is flushed below ~1e-30, so no denormals without CPU FTZ flags.

## Loudness (`loudness`)

`LoudnessMeter::new(sample_rate, channels)` → `process(&[&[f32]])` / `process_interleaved`, then
`momentary()`, `short_term()`, `integrated()`, `loudness_range()`, `sample_peak()`,
`true_peak()` / `true_peak_dbtp()`, `summary()`.

- K-weighting derived for any sample rate by bilinear transform (matches the 48 kHz BS.1770 table).
- Gated integrated loudness (−70 LUFS / −10 LU) and LRA (−70 / −20 LU, 10th–95th percentile) use a
  0.01 LU histogram with exact energy sums: fixed memory for programmes of any length.
- True peak: 4× (2× at 88.2/96 kHz) polyphase Kaiser-sinc interpolation.
- Oracle-tested against ffmpeg's `ebur128` filter (`tests/loudness_oracle.rs`, skipped without ffmpeg):
  M/S within 0.0005 LU, I within 0.007 LU, LRA within 0.04 LU and true peak within 0.045 dB (0.053 dB of the analytic value on an fs/4 tone) on
  sine, pink-noise, speech-like and gated stereo signals at 44.1/48/96 kHz.
- `normalize_gain_db(measured, target)` and `normalize_gain_db_peak_limited(...)` for Normalize /
  Essential Sound loudness auto-match.

## Effects (`effects`)

All implement `AudioEffect` (`id`, `params`, `set_param`, `param`, `reset`, `process`, `latency`).
`effects()` lists `EffectInfo { id, name, category, params, create }`; each `ParamSpec` has
id, name, range, default, `Unit`, log-scale hint and choice labels so UIs can be generated.

| id | effect |
|---|---|
| `parametric_eq` | 8 bands, each peaking / shelf / LP / HP / notch / band-pass, RBJ biquads in TDF-II |
| `simple_eq` | low shelf, mid peak, high shelf |
| `compressor` | peak/RMS detector, soft knee, attack/release, make-up; stereo-linked |
| `gate` | downward expander / gate with range and hold |
| `limiter` | 5 ms look-ahead, true-peak-aware, ceiling guaranteed (latency reported) |
| `delay` | feedback delay, ms or tempo-synced divisions, mix |
| `reverb` | 8×8 Householder FDN, pre-delay, RT60 decay, HF damping, size, mix |
| `amplify`, `channel_volume`, `balance` | gain, per-channel gain, balance / constant-power pan |
| `invert`, `swap_channels`, `fill_left`, `fill_right` | polarity and routing |
| `dehum` | 50/60 Hz notch comb with up to 10 harmonics |
| `denoise` | STFT spectral gating, minimum-statistics noise floor (latency = FFT size) |
| `pitch_shifter` | ±12 semitones, phase vocoder with peak shifting + phase locking (latency = FFT size) |

## Known limits

- Effects assume ≤ the channel count they were built with; extra channels pass through
  (channel-generic gain effects process all of them).
- DeNoise adapts continuously; a perfectly stationary tone is (by design) treated as noise after
  ~1.5 s. There is no explicit "learn noise print" mode yet.
- Filter type / choice changes switch immediately (continuous parameters are smoothed).
