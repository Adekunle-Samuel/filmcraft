# Testing

```sh
cargo test --workspace                          # everything; oracle tests skip without ffmpeg
cargo test -p filmcraft-h264                    # one crate
cargo test --release -p filmcraft-hevc          # codec tests are much faster in release
cargo xtask ci                                  # all gates (fmt, clippy, tests, layers, assets, wasm)
```

## 1. Kinds of tests

| Kind | Where | Examples |
|---|---|---|
| Unit | `src/` `#[cfg(test)]` modules, `src/tests.rs` | CABAC engines, VLC tables (prefix-freeness, Kraft sums), colour matrices, frame cache |
| Property (`proptest`) | `time`, `bitstream`, `edit`, `isobmff/tests/roundtrip.rs`, `interchange/tests/*` | tick↔frame round trips for every rate, DF timecode, edit invariants (no overlaps, durations conserved), mux→demux round trips, interchange export→import |
| Synthetic streams | `h264/src/synth_tests.rs`, `hevc/src/synth_tests.rs` | hand-built bitstreams for features the reference encoders don't emit; expected output known exactly |
| Engine / command | `crates/engine/src/tests.rs` | run commands on the demo project, assert the sequence, undo/redo, disabled cases |
| Render | `crates/render/src/tests.rs` | compositing, opacity, Motion, cross dissolve midpoint, ½-res vs full, GPU plan vs reference, audio mix, audio-effect continuity |
| Oracle | `crates/*/tests/*oracle*.rs`, `conformance.rs` | compare with ffmpeg/ffprobe (§2) |
| Robustness / fuzz | `*/tests/robustness.rs`, `*/tests/fuzz.rs` | seeded mutation and truncation of real and synthetic files; nothing may panic |
| Performance | `*/tests/perf.rs` (`#[ignore]`) | §5 |

Commit `*.proptest-regressions` files so failing cases are re-run.

## 2. ffmpeg oracle tests

ffmpeg and ffprobe are **external processes** used to generate fixtures and to check results. They
are never linked or shipped ([AGENTS.md](../AGENTS.md) §2).

- Each crate's `tests/common/mod.rs` finds the tools in `/opt/homebrew/bin`, `/usr/local/bin` or
  `/usr/bin`. If they are missing, the test prints a message and returns (it passes).
- Fixtures are generated on first use into `<repo>/target/fixtures/<crate>/` and reused afterwards.
  Delete that directory to regenerate them. Never commit media.
- Fixture sources are synthetic: `testsrc2`, `mandelbrot`, SMPTE bars, noise and fades, sine tones.
  H.264 and HEVC fixtures need ffmpeg built with libx264 and libx265. VideoToolbox fixtures are
  generated only on macOS.

### Pass criteria per codec

| Crate | Oracle check | Criterion |
|---|---|---|
| `h264` | decode every fixture single-threaded and frame-threaded; compare with `ffmpeg -f rawvideo -pix_fmt yuv420p` | **bit-exact**, every frame |
| `hevc` | same, `yuv420p` / `yuv420p10le` | **bit-exact** (8- and 10-bit) |
| `h264enc` | `ffmpeg -ec 0` decodes our stream | no errors, exact frame count, **bit-identical to the encoder's reconstruction**; B-frame order checked with ffprobe; rate-control targets |
| `prores` decode | ffmpeg `prores_ks` / `prores_aw` fixtures, compared at native depth | within **±1 LSB** (different integer IDCT); alpha bit-exact |
| `prores` encode | ffmpeg decodes with `-xerror` | no errors; agrees with our decoder within ±1 |
| `aac` decode | ffmpeg's decode of the same stream | max abs error ~1e-7 (float) |
| `aac` encode | ffmpeg decodes our stream | no errors, no clipping, CBR within ±5% of target; SNR reported |
| `isobmff` | `ffprobe -show_packets` on ffmpeg-made MP4/MOV | packet offsets, sizes, pts/dts, durations, key flags and stream parameters equal; remuxed files decode with `ffmpeg -v error` silent |
| `matroska` | `ffprobe -show_packets` | every packet (stream, size, key flag, pts, duration) equal; seeks land on the latest keyframe ≤ target |
| `export` | our own demuxer/decoder reads the file back; ffprobe counts frames when present | expected size, duration, colour, audio level; exact frame count |

Each codec README has the full fixture matrix and the measured results.

## 3. Rendering: CPU reference and GPU parity

- The CPU compositor in `filmcraft-render` is the reference. Its tests check computed values
  (opacity blends, dissolve midpoints, Motion placement, ½-resolution against downsampled full
  resolution) and that `render::plan` matches `render_sequence`.
- `crates/gpu/src/tests.rs` composites the same frame plan (a YUV layer plus a transformed,
  semi-transparent RGBA layer) on the GPU and on the CPU. It requires a 99th-percentile channel
  difference of ≤ 6 and a mean of < 1.5 (8-bit levels). It skips when no GPU adapter is available.
- Effects implemented only on the CPU need no GPU test: the plan pre-renders those layers on the
  CPU.
- Golden-image files (rendered PNGs compared against committed references) are not used yet. If you
  add them, generate the references from FilmCraft itself, give each one an attribution sidecar,
  and document the tolerance (PSNR in dB, or max/percentile error).

## 4. UI: control channel and screenshots

There is no automated UI test suite yet. UI work is checked by driving the real app:

```sh
cargo run --release -p filmcraft -- --control 9876
```

Then script it over the control channel or MCP: run commands, click by automation id,
`ui.inspect` / `ui.elements` to assert state, and `ui.screenshot {"path": "…"}` to capture the window
or a single panel (`{"panel": "Timeline"}`). Look at every screenshot. Examples are in
[agents.md](agents.md). The headless path (`filmcraft-cli run script.jsonl`, MCP `--demo`) covers
engine behaviour without a window.

## 5. Performance

| Benchmark | Command |
|---|---|
| H.264 decode, 1080p, threads sweep | `cargo test --release -p filmcraft-h264 --test perf -- --ignored --nocapture` |
| HEVC decode, 1080p / 2160p | `cargo test --release -p filmcraft-hevc --test perf -- --ignored --nocapture` |
| ProRes decode/encode | `cargo test --release -p filmcraft-prores --test perf -- --ignored --nocapture` |
| Encoder speed / PSNR / bitrate | `cargo run --release -p filmcraft-h264enc --example h264enc_synth -- …` |
| Decode a real file through the media stack | `cargo run --release -p filmcraft-cli -- bench-decode file.mp4 --frames 120` |
| Coding-tool coverage of the fixtures | `cargo test --release -p filmcraft-h264 --test conformance coverage_report -- --ignored --nocapture` (same for `hevc`) |

The perf tests check bit-exactness before they time anything. Results go in the crate README's
performance table, with machine and thread count. Headline numbers go in
[ROADMAP.md](../ROADMAP.md). Measure on an idle machine: parallel agent builds distort timings.

### Playback benchmark

`cargo xtask bench-playback` (the example `crates/ui-egui/examples/bench_playback.rs`) plays
sequences headlessly through the Program monitor's own frame scheduler: the `FrameServer` worker
pool, `schedule_playback` (prefetch order and stale-job dropping), its caches, and the
`PlaybackMeter` that counts shown/dropped frames in the app. On the GPU path it also composites
each plan with `filmcraft-gpu`, as the monitor does on the UI thread.

```sh
cargo xtask bench-playback                                   # every scenario, GPU path, Full and Half
cargo xtask bench-playback --scenario stack3 --res full --cpu
cargo xtask bench-playback --json target/bench-playback.json # machine-readable results
```

| Scenario | What plays |
|---|---|
| `h264-1080` | one 1080p23.976 H.264 clip (testsrc2 + grain, ~45 Mbit/s, 250-frame GOP) |
| `stack3` | three 1080p H.264 clips on V1–V3; V2/V3 scaled, positioned, rotated, 70–85 % opacity |
| `h264-2160` | one 2160p23.976 H.264 clip |
| `demo` | the built-in demo project (procedural footage, transitions, effects) |
| `after-preview` | 1080p H.264 + Lumetri/Sharpen/Levels/Tint: a live play, Render Effects In to Out, then two plays of the green segment |
| `seek-storm` | 40 jumps to random frames 150 ms apart (scrubbing), time until the exact frame shows |

Options: `--res full,half,quarter`, `--cpu` (CPU compositor + texture conversion instead of the
GPU path), `--seconds`, `--refresh` (display Hz), `--workers`, `--repeat`, `--json <file>`.
Fixtures are made with ffmpeg in `target/fixtures/playback/` (set `FILMCRAFT_FIXTURES` to share one
set between worktrees).

Columns: **shown/drop** as counted in the app (a frame is shown when its exact picture was on
screen at a refresh while it was due; frames passed over without a refresh count as dropped);
**ontime** = due frames whose job finished before they were due; **lat** = queue→ready per job,
**svc** = worker time per job; **cpu/j**, **src/j**, **srcC/j** = worker thread CPU, source fetch
(decode) wall and thread CPU per job; **ui** = time on the UI thread to present a frame (GPU upload
and draw, or texture conversion); **cpu ms/f** = process CPU per frame and **cores** = the cores that
needs at the sequence frame rate; **seeks/dec** = decoder restarts and samples decoded; **waste** =
jobs for frames that were never due.

Wall-clock columns (shown/drop, ontime, latencies) depend on machine load, so each row prints the
load average. CPU columns (thread and process CPU time) and the structural counters (seeks,
samples decoded, wasted jobs) do not, and are what to compare between runs on a busy machine.
