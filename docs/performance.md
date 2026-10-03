# Performance

`cargo xtask bench` measures the whole app headlessly and writes `target/bench/bench-<label>.json`
and `.md` (options and sections: [testing.md](testing.md) §5). Agents read live counters with
`perf.stats` ([control-protocol.md](control-protocol.md)).

**Read the numbers with the load average.** These results come from a shared Apple M4 Pro
(14 cores, 48 GB, macOS 15 / Darwin 24.6) with parallel agent builds running: load average
**75–200** during every run below. Wall-clock columns (fps, shown/dropped frames, seek latency)
swing by 2–5× between back-to-back runs at that load and are only comparable within one run. CPU
time per frame, samples decoded and skipped, and peak RSS are much less sensitive, and are what
the before/after comparison relies on. Each milestone below alternated base / after runs (2 rounds
each); the most load-independent figure is the decoder's own cycle count (`proc_pid_rusage`
instructions and cycles of a process, all threads summed).

## Results (M4.9: 4K H.264, before → after)

Baseline = commit `3d14099` (M3.12, unchanged decoder); after = M4.9. Interleaved runs on
2026-10-02/03, load average 92–178 (the playback rows say which load each run saw).

### Decoder work per frame (load-independent)

`h264dec` example, one decoder, CPU cycles and instructions counted by the kernel (all threads),
output bit-exact with ffmpeg:

| stream | Mcycles / frame before | after | after, draft mode |
|---|---|---|---|
| 2160p (`a2160.mp4`, 170 Mbit/s, first 48 frames) | 505 | **390** (−23 %) | **356** (−30 %) |
| 1080p (`dec_h264_1080.mp4`, 120 frames) | 132 | **100** (−24 %) | **90** (−32 %) |

The cycle count is the same with 1, 4 or 14 frame threads (no spinning or contention overhead):
at ~4.5 GHz a 4K frame is ~85 ms of one core, so 23.976 fps needs about two cores.

### Decode (every frame through the media stack)

| codec | size | CPU ms/frame before (2 rounds) | after (2 rounds) |
|---|---|---|---|
| H.264 | 1080p | 35.8, 39.5 | **33.0, 29.4** |
| H.264 | 2160p | 138.9, 152.1 | **125.7, 112.3** |

(A run at load ~190 before any change gave 41.6 / 160.2 ms; an intermediate build 32.2 / 118.3.)

### Cold seeks on the 4K fixture (12 random targets, GOP 250, fresh source per seek)

| mode | CPU ms per seek before | after | wall p50 ms before → after |
|---|---|---|---|
| full decode from the keyframe | 7529, 7418 | **5531, 5268** (−27 %) | 2757, 1848 → 924, 739 |
| keep 2 s (scrub) | 7488, 7373 | **5442, 5397** | 2408, 1995 → 872, 795 |
| late (playback catch-up) | 5597, 5487 | **4079, 3949** | 2014, 1248 → 735, 526 |

Samples decoded per seek are identical (~48, ~34 in catch-up): the same pictures, decoded with
less work. The cold seek is a chain of reference pictures, so it shows the per-picture cost
directly.

### Program-monitor playback, 2160p (8 s, GPU path)

| case | before: shown/dropped (load) | after: shown/dropped (load) | CPU ms/frame before → after |
|---|---|---|---|
| Full | 192/0 (92), 9/183 (167) | 113/79 (171), **192/0** (139) | 159 (no skips), 133 (17 skipped) → 131, 130 |
| 1/2 | 12/180 (95), 5/187 (169) | 173/19 (172), **192/0** (124) | 143, 127 → 133, 132 |
| 1/2 draft | – | 183/9 (175), **192/0** (119) | – → 144, 134 |
| 1/4 | – | 186/6 (177), **192/0** (111) | – → 134, 125 |
| 1/4 draft | – | **192/0** (178), 190/2 (101) | – → 132, 131 |
| 1080p Full | 192/0, 192/0 | 192/0, 192/0 | 41, 48 → 39, 36 |

Before, the 4K clip kept up in one run (Full at load 92; 1/2 dropped 94 % at load 95) and dropped
95–97 % at load ~168. After, every 4K case plays 192/0 at load 100–140; at load 171–178 Full drops
41 %, 1/2 10 % (5 % with draft decoding), 1/4 3 % (none with draft decoding). CPU per
displayed frame (whole process: decode, frame workers, compositing) drops ~15 % where no frames
were skipped; draft decoding at 1/2 and 1/4 is within run-to-run noise in this column (its saving
is the ~9 % of decoder cycles above, plus a quarter / sixteenth of the texture upload), while the
frame jobs wait for the decoder less (decode ms/job 212 → 67 at 1/2, 98 → 44 at 1/4 in the busy
round). On an idle machine the 4K clip needs about two cores.

## What M4.9 changed

1. **Deblocking** (25–30 % of 4K decode time before): the edge filter runs on all 16 luma / 8
   chroma lines of an edge at once with per-lane masks and min/max clipping (no `clamp`, whose
   bounds assert blocked vectorisation), so it compiles to NEON; edges whose alpha or beta index
   is 0 are skipped. Bit-exact with the per-line filter (random-edge unit test) and ffmpeg.
2. **CABAC**: `decode_decision` / `decode_bypass` select instead of branching on the bin value
   (the MPS/LPS outcome is close to random at high bit rates), renormalisation has no branch on
   the range, and context / state indexing has no bounds checks.
3. **Inverse transforms**: the 4x4 / 8x8 column pass and the add/clip run across all columns.
4. **Frame threading** was already in place (M2): pictures decode concurrently and wait per
   macroblock row for the reference rows their motion vectors reach, with deblocking pipelined row
   by row inside each picture's job. The conformance suite now decodes every fixture with 1, 3 and
   all threads (bit-exact each time). A separate deblocking thread was not added: a picture's rows
   are published about two rows after they are reconstructed, so a reference chain advances a few
   rows behind its predecessor and the pool already has more pictures in flight than cores
   (crates/h264/README.md, "Threading model").
5. **Draft decoding** (Settings ▸ Playback ▸ Draft decoding, off by default; `perf.stats`
   `playback.draftDecode`, `decode.draftFrames`, `decode.h264Threads`): while the Program monitor
   plays at 1/2 or 1/4, H.264 non-reference pictures skip deblocking (no other picture can change),
   and draft plans hand the GPU box-decimated Y'CbCr planes at the drawn size. Draft frames carry
   their own frame-cache keys and the GOP cache serves them to draft requests only, so pausing,
   rendering and exporting always decode exact pictures (tested through the media stack against
   ffmpeg after draft playback). The CPU renderer already converted reduced-resolution frames
   straight to the decimated size (`to_linear_f32_decimated`).

Not done: CABAC residual decoding (now 41 % of the work, ~19 cycles per bin) is inherently serial;
the remaining deblocking cost is mostly the vertical-edge transposes; motion compensation and the
per-row / per-picture copies (4 %) are unchanged.

## Results (M4.8, before → after)

Baseline = commit `457aca1` (the harness on unchanged code); after = M4.8. Runs alternated base /
after (2 rounds each, 2026-10-02).

### Decode (every frame through the media stack: container, GOP cache, decoder, conversion)

CPU ms per frame (both rounds; lower is better). fps is the best of the run at that moment's load.

| codec | size | CPU ms/frame before | after | fps before → after (same-round pairs) |
|---|---|---|---|---|
| H.264 | 1080p | 43.4 / 43.7 | 48.2 / 46.1 | 101, 105 → 51, 34 (load 99 → 127) |
| H.264 | 2160p | 169 / 166 | 171 / 178 | 17, 31 → 16, 9 |
| **HEVC** | 1080p | 80.5 / 76.4 | **31.4 / 34.5** | 25, 58 → 69, 41 |
| **HEVC** | 2160p | 325 / 314 | **122 / 136** | 8.2, 12 → 17, 11 |
| VP9 | 1080p / 2160p | 39.7 / 137.6 | 36.5 / 136.8 | 18 / 12 → 18 / 7.8 |
| AV1 | 1080p / 2160p | 20.2 / 78.2 | 19.7 / 72.6 | 106 / 23 → 50 / 9.4 |
| ProRes 422 HQ | 1080p / 2160p | 23.6 / 89.0 | 23.2 / 86.7 | 59 / 28 → 34 / 15 |

On a quieter moment earlier in the session (load ~70) the same binaries decoded H.264 at 100 fps
(1080p) / 24 fps (2160p), AV1 131 / 44 fps and ProRes 202 / 49 fps.

### Seeking (cold seek through the media stack, 12 random targets, modes interleaved, load ~150)

| fixture (GOP 250) | full decode from keyframe | skip non-reference > 2 s before (scrub) | skip all late non-reference (playback catch-up) |
|---|---|---|---|
| H.264 1080p | 1405 ms, 3.3 s CPU | 1276 ms, 3.1 s CPU | **789 ms, 2.3 s CPU** |
| H.264 2160p | 3394 ms, 8.5 s CPU | 2828 ms, 8.4 s CPU | **2274 ms, 6.3 s CPU** |
| HEVC 2160p | 4091 ms, 17.8 s CPU | 3058 ms, 12.3 s CPU | **2512 ms, 9.8 s CPU** |

The target frame is identical in every mode (only pictures nothing references are left out).

### Program-monitor playback (8 s, GPU path)

| scenario | shown/dropped before | after | CPU ms/frame before → after | non-ref samples skipped |
|---|---|---|---|---|
| H.264 1080p | 192/0, 192/0 | 191/1, 191/1 | 55 → 55 | 0 (keeps up) |
| 3 × 1080p stacked | 192/0, 190/2 | 188/4, 185/7 | 120 → 128 | 0 |
| H.264 2160p Full | 22/170, 192/0 | 0/192, 0/192 | 183 → **87** | 39–52 |
| H.264 2160p Half | 180/12, 192/0 | 0/192, 0/192 | 198 → **102** | 46–49 |
| HEVC 2160p Full | 0/192, 0/192 | 0/192, 0/192 | 164 → **90** | 88 |
| HEVC 2160p Half | 0/192, 0/192 | 0/192, 0/192 | 160 → **97** | 33–92 |

Shown/dropped at 2160p is decided by the load at the moment (base round 2 happened to play 192/0
at a quiet minute; a later three-way run at load 175–195 gave 0–21 shown for every binary, and
143/49 for HEVC once). CPU per displayed second halved at 2160p: frames that are already late are
no longer fully decoded, and HEVC decodes 2.5× cheaper.

### Scrubbing (24 random seeks + 8 playhead drags through the monitor's request path)

| scenario | seek p50 / p95 ms before | after | drag-stop p50 ms before → after |
|---|---|---|---|
| H.264 1080p | 357 / 940, 582 / 1535 | 840 / 1567, 770 / 1389 | 27, 62 → 281, 103 |
| H.264 2160p | 1063 / 2377, 1499 / 3028 | 2445 / 5357, 2390 / 4562 | 288, 513 → 560, 834 |
| HEVC 2160p | 130 / 4100 (9 timeouts), 318 / 1397 | 2466 / 6441, 2476 / 7596 (0 timeouts) | 9, 3872 → 462, 1893 |

These runs coincided with load rising from 99 to 170 for "after". A later interleaved three-way
run (base / after with catch-up disabled / after) at load 80–200 gave H.264 1080p seek p50 345 /
712 / 572 ms then 379 / 389 / 461 ms, and H.264 2160p 913 / 1312 / 2105 then 1946 / 931 / 840 ms:
within the noise. The cold-seek table above is the load-independent comparison.

### Timeline UI (1000 clips on 20 tracks, 1920×1080 window, `egui_kittest`)

| view | update p50 / p95 ms before | after | tessellate p50 ms | wgpu render p50 ms (incl. readback) | vertices |
|---|---|---|---|---|---|
| fit (all 1000 clips) | 3.1 / 4.9, 3.4 / 5.7 | 2.9 / 4.5, 2.9 / 3.3 | 0.3 | 15 | 35.7 k |
| zoomed, scrolling | 2.8 / 3.8, 2.8 / 4.8 | 2.9 / 4.9, 2.6 / 6.8 | 0.2 | 15 | 29.2 k |

The timeline is not a bottleneck: the whole app frame (all panels) costs ~3 ms with 1000 clips
visible; it already culls clips outside the view. The waveform display gain no longer rescans the
whole source's peaks for every clip every frame (cached per peak list).

### Export (8 s of 1080p23.976 H.264 + AAC source)

| format | fps before → after | CPU ms/frame before → after |
|---|---|---|
| H.264 + AAC | 10.6, 5.8 → 10.2, 11.9 | 256 → 250 |
| ProRes 422 | 4.7, 3.0 → 6.3, 9.6 | 249 → 232 |

Profile (`sample`): H.264 export is ~45 % encoder, ~30 % decoding the noisy 45 Mbit/s source,
~25 % CPU compositing; ProRes export spends most in `prores::encode::slice_bits` /
`choose_quantisers` (rate search), then source decode and compositing. Not changed in M4.8.

### Project save / open (5 sequences × 1000 clips, 3.7 MB)

save 24 / 68 → 36 / 21 ms, open 26 / 82 → 21 / 19 ms (p50 of 3; noise).

### Peak RSS per section (MB)

decode 2642 / 2925 → 2506 / 2064; playback 5246 / 7903 → 3379 / 3121; scrub 4205 / 5022 →
5390 / 4183; timeline 626 / 650 → 660 / 675; export 2367 / 2286 → 2606 / 2619; project 114 / 136 →
124 / 133.

## What M4.8 changed

1. **HEVC inverse transform** (largest hotspot: >50 % of HEVC decode CPU). Sums contiguous basis
   rows scaled by non-zero coefficients, size as a const generic so loops vectorise; bit-exact
   (exact integer sums), tested against the direct matrix product on random blocks and by the
   ffmpeg conformance fixtures. **SAO edge offset** gets a check-free inner loop. HEVC CPU per
   frame −60 %.
2. **No redundant plane copies**: H.264 / HEVC / VP9 pictures are moved into `VideoFrame`s instead
   of copied (12 MB per 2160p frame; HEVC 10-bit also lost a per-sample widening pass).
3. **Catch-up decoding**: frame jobs carry how far before their frame the playhead is
   (`filmcraft_media::cancel::with_catch_up`); the GOP cache skips non-reference samples of late
   frames (`VideoDecoder::is_disposable`: H.264 `nal_ref_idc` 0, HEVC sub-layer non-reference
   pictures of the top temporal layer). Scrub requests keep the last 2 s fully decoded so dragging
   back stays cached. The wanted frame is decoded exactly as before; a later request for a skipped
   frame re-seeks. ~50 % of the fixtures' pictures are non-reference.
4. **`perf.stats`** (engine query + UI/control method): decode counters (cache hit rate, seeks,
   samples decoded / skipped, decoder ms), playback shown / dropped, frame-worker decode / render
   ms (p50 / p95), request hit rate, cache use, UI fps, process CPU.
5. **Timeline**: waveform source peak cached.

Not feasible / not done: reduced-resolution decode for H.264 / HEVC (inter prediction needs
full-resolution references, so it cannot be bit-exact; ½/¼ playback already decimates after
decode); AV1 internals untouched (measured only); export encoders not optimised.
