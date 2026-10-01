# Testing

```sh
cargo test --workspace                          # everything; oracle tests skip without ffmpeg
cargo test -p filmcraft-h264                    # one crate
cargo test --release -p filmcraft-hevc          # codec tests are much faster in release
cargo xtask ci                                  # all gates (fmt, clippy, tests, layers, assets, wasm)
cargo xtask fixtures                            # pre-generate the ffmpeg fixture matrix
FILMCRAFT_REQUIRE_ORACLES=1 cargo test --workspace   # CI: missing ffmpeg fails instead of skipping
```

## 1. Kinds of tests

| Kind | Where | Examples |
|---|---|---|
| Unit | `src/` `#[cfg(test)]` modules, `src/tests.rs` | CABAC engines, VLC tables (prefix-freeness, Kraft sums), colour matrices, frame cache |
| Property (`proptest`) | `time`, `bitstream`, `edit`, `isobmff/tests/roundtrip.rs`, `interchange/tests/*` | tick↔frame round trips for every rate, DF timecode, edit invariants (no overlaps, durations conserved), mux→demux round trips, interchange export→import |
| Synthetic streams | `h264/src/synth_tests.rs`, `hevc/src/synth_tests.rs` | hand-built bitstreams for features the reference encoders don't emit; expected output known exactly |
| Scripted UI | `crates/ui-egui/tests/scripted.rs` | headless app (egui_kittest) driven over the control channel: razor/undo, insert, apply effect, playback, click by automation id (§4) |
| Golden images | `crates/golden/tests/golden.rs` | CPU renders vs committed PNGs; GPU vs CPU on the same scenes (§3) |
| Engine / command | `crates/engine/src/tests.rs` | run commands on the demo project, assert the sequence, undo/redo, disabled cases |
| Render | `crates/render/src/tests.rs` | compositing, opacity, Motion, cross dissolve midpoint, ½-res vs full, GPU plan vs reference, audio mix, audio-effect continuity |
| Mixer | `crates/render/src/mixer_tests.rs`, `crates/engine/src/mixer_tests.rs` | sample-exact fader gain and pan laws, automation at block boundaries (bit-identical however requests are cut), solo/mute/solo-safe, sends and submix routing, latency-compensation alignment, render-vs-playback identity, Touch ramp-back, recorder modes (Latch/Touch/Write), thinning, Audio Gain, transition curves; `perf_24_tracks_3_effects_realtime_factor` prints the realtime factor |
| Oracle | `crates/*/tests/*oracle*.rs`, `conformance.rs` | compare with ffmpeg/ffprobe (§2) |
| Robustness / fuzz | `*/tests/robustness.rs`, `*/tests/fuzz.rs` | seeded mutation and truncation of real and synthetic files; nothing may panic |
| Performance | `*/tests/perf.rs` (`#[ignore]`) | §5 |

Commit `*.proptest-regressions` files so failing cases are re-run.

## 2. ffmpeg oracle tests

ffmpeg and ffprobe are **external processes** used to generate fixtures and to check results. They
are never linked or shipped ([AGENTS.md](../AGENTS.md) §2).

- The tools are found by `filmcraft-testkit` (a dev-dependency-only crate, `crates/testkit`), in
  this order: `FILMCRAFT_FFMPEG` / `FILMCRAFT_FFPROBE` (explicit paths; a wrong path is an error),
  then every directory on `PATH` (`ffmpeg` and `ffmpeg.exe`), then well-known install directories
  (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `C:\ffmpeg\bin`, …).
- If a tool is missing the test prints `SKIPPED (<test>): ffmpeg not found …` and passes. Set
  **`FILMCRAFT_REQUIRE_ORACLES=1`** (CI) to make every such skip a failure. In new tests use
  `let ff = filmcraft_testkit::require_ffmpeg!();` (also `require_ffprobe!`, `require_oracles!`).
- Fixtures are generated on first use into `<workspace>/target/fixtures/<crate>/`
  (`filmcraft_testkit::fixtures_dir`; independent of `CARGO_TARGET_DIR`, so agents with private
  target dirs share them; `FILMCRAFT_FIXTURES_DIR` overrides the root) and reused afterwards.
  Generators write to a per-process/thread temporary name (`testkit::temp_path`) and rename it into
  place, so concurrent tests never see half-written files. Delete the directory to regenerate.
  Never commit media.
- Every crate's `tests/common/mod.rs` delegates discovery and fixture paths to testkit, except
  `crates/codecs/src/tests.rs`, which still has its own lookup (the crate was being edited
  concurrently; migrate it when convenient).
- `cargo xtask fixtures [crate…]` pre-generates the whole fixture matrix up front (useful before a
  parallel test run or on a fresh machine). It runs each crate's ignored `generate_fixtures` test —
  the same generators the oracle tests call — for `h264`, `hevc`, `isobmff`, `matroska` and
  `prores`, and prints one `made` / `cached` / `skipped` line per fixture plus a summary. The
  `aac`, `opus` and `h264enc` oracles generate small per-test signals on demand and are not part of
  the matrix.
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
| `audio-dsp` loudness | `tests/loudness_oracle.rs`: signals generated in Rust (997 Hz sine, pink noise at 48/96 kHz, speech-like bursts at 48/44.1 kHz, stereo with silence and sub-gate passages, an fs/4 inter-sample-peak tone) written as float WAV and measured with `ffmpeg -af ebur128=peak=true:metadata=1` | momentary and short-term every 100 ms **±0.1 LU**, integrated **±0.1 LU**, LRA **±0.5 LU**, true peak **±0.2 dB** (against the analytic value when one exists). Measured: ΔM/ΔS ≤ 0.0005 LU, ΔI ≤ 0.007 LU, ΔLRA ≤ 0.04 LU, ΔTP ≤ 0.045 dB against ffmpeg. On the fs/4 tone ffmpeg's own true peak is +0.6 dB high (−0.32 vs the analytic −0.92 dBTP); ours is −0.05 dB |
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
- **Golden images** (`crates/golden`, a test-only crate): `tests/golden.rs` builds small procedural
  projects (demo-generator footage, bars, no media files) at 320×180 and renders one frame of each
  through the CPU compositor: Motion transform + opacity, four blend modes (Multiply, Screen,
  Overlay, Difference), Gaussian Blur, Lumetri basic correction, Crop, Cross Dissolve at 50 %, Dip
  to Black at 25 %, Wipe at 50 %, and Timecode / Clip Name burn-in text. Each frame is compared
  with `crates/golden/goldens/<scene>.png` by `filmcraft_testkit::golden`:
  **PSNR ≥ 45 dB, max abs ≤ 12, 99th-percentile per-pixel max channel difference ≤ 2** (8-bit
  sRGB levels; `Tolerance::RENDER`). On failure the actual frame and a ×8 difference image go to
  `<workspace>/target/golden-failures/`. `scenes_are_distinct` guards against blank or duplicate
  scenes.
- **Blessing:** `FILMCRAFT_BLESS=1 cargo test -p filmcraft-golden` rewrites the references (and
  writes a `.attribution` sidecar for any new one; it prints the `ATTRIBUTION.md` row to add).
  References are original work rendered by FilmCraft, must stay under 50 KB (enforced on bless),
  and every one needs its sidecar and index row (`cargo xtask assets`). Look at a re-blessed PNG
  before committing it.
- The same scenes run through `plan_frame` + `GpuCompositor` when a GPU adapter exists
  (`gpu_matches_cpu_on_golden_scenes`, same p99 ≤ 6 / mean < 1.5 criterion; skipped otherwise).
  This found a real bug: the GPU upload cache was keyed by pixel-buffer address without keeping
  the buffer alive, so a new frame allocated at a freed frame's address was drawn with the stale
  texture (fixed; regression test `upload_cache_keeps_buffers_alive`).

## 4. UI: control channel and screenshots

### Scripted UI tests (headless)

`crates/ui-egui/tests/scripted.rs` runs the real `FilmcraftApp` under
[`egui_kittest`](https://docs.rs/egui_kittest) (`build_eframe`): no window, no GPU, no OS event loop.
A small `Driver` opens the demo project, sends requests through the same control channel agents use
(`ControlRequest` → `control::handle`, exactly as the TCP server does) and steps egui frames until
each reply arrives. Synthetic input queued by `ui.click` / `ui.drag` / `ui.key` is moved into the
next frame by the app's own `raw_input_hook`, so clicks by automation id work headless. Covered:

| Test | Asserts |
|---|---|
| `demo_project_opens_headless_and_registers_widgets` | active sequence, > 50 registered widgets, panel and tool ids, 6 clips on V1 (`sequence.inspect`) |
| `razor_then_undo_through_the_control_channel` | `timeline.razor` adds a clip, the timeline can locate it on screen, `edit.undo` via `ui.menu.invoke`, redo |
| `insert_from_source_ripples_the_sequence` | `source.open` + marks + `source.insert` lengthens the sequence |
| `apply_effect_appears_in_effect_controls` | `effects.apply` adds Gaussian Blur in the model and `effectControls.effect.gaussian_blur` appears in the panel; undo removes it |
| `playback_toggle_and_stop` | `playback.toggle` / `ui.playback stop` and `ui.inspect` playback state |
| `clicking_a_tool_button_by_automation_id` | `ui.click {id: "tools.Razor"}` changes the tool (real egui input path) |
| `unknown_methods_and_commands_fail_cleanly` | errors come back as `{"ok": false}` |

`crates/ui-egui/tests/mixer_ui.rs` drives the Audio Track Mixer, Audio Clip Mixer, timeline track
keyframes and the Audio Gain dialog by automation id and with multi-frame pointer drags (press, move
over several frames, release). With `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` it also renders the window
offscreen through wgpu (`Harness::render`) and writes `mixer-*.png`; this works without a visible
window (for example on a locked screen, where `ui.screenshot` cannot capture).

Not covered headless: `ui.screenshot` (needs a real viewport), the wgpu monitor path (the harness
runs the CPU texture path), audio output, and wall-clock playback advance (kittest frames do not
advance real time). Use `Driver` for new UI regressions: `d.exec(command, params)`,
`d.ok(method, params)`, `d.frames(n)`.

### Interactive checks

For visual work, drive the real app:

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

Results on an M4 Pro (14 cores), GPU path, 8 s plays, median of 2 alternating runs of the M4.6
baseline (commit `5170376`) and the result of M4.6, on a machine shared with parallel agent
builds (**load average 86–207**, so wall-clock columns are pessimistic; CPU ms/frame is not):

| Scenario | shown/dropped before | after | CPU ms/frame before → after | decoder seeks |
|---|---|---|---|---|
| h264-1080 Full / Half | 106/86, 118/74 | **192/0, 192/0** | 80 → 46, 95 → 46 | 3–4 → 1 |
| stack3 (3 × 1080p) Full / Half | 185/7, 174/18 | **192/0, 192/0** | 136 → 109, 117 → 111 | 4 → 3 (one per source) |
| h264-2160 Full / Half | 0/192, 0/192 | 5/187, 19/173 | 110 → 135, 170 → 145 | 4–5 → 1–2 |
| demo Full / Half | 63/129, 124/68 | 97/95, 152/40 | 140 → 80, 93 → 40 | |
| after-preview, 1st play after render (Full) | 80/112 | **190/2** | 119 → 26 | 104 → 0 |
| after-preview, 2nd play (Full) | 145/47 | **192/0** | 76 → 26 | 132 → 0 |
| after-preview, live effects (Half) | 18/174 | 69/123 (frames skipped evenly) | 109 → 95 | |
| seek-storm (40 jumps, 150 ms each) | 0/40 shown | 6/40 | 827 → 666 per jump | |

At load ~25–60 the same final build plays h264-1080, stack3 and h264-2160 at Full with 0 dropped
in 3 of 3 runs (192/0) and render previews 192/0. The 4K fixture needs ~4 cores of decode per
real-time second (≈160 ms CPU per frame at 170 Mbit/s); the demo project's procedural footage
~190 ms per Full-resolution frame.
