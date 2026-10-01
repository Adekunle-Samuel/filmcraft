# FilmCraft Roadmap

Progress toward feature parity with Adobe Premiere Pro, with estimates. Updated as milestones land.

**Last updated:** 2026-10-01 · **Overall parity:** ~45% · **Code:** ~125k lines of Rust in 31 crates, 800+ tests

## Estimate to parity

Throughput observed so far: ~25% parity in the first ~5 wall-clock hours (Sep 30, 05:22 → 10:00), with
3–4 coding agents running in parallel and one integrator. The early percentage points were the cheap
ones; the remaining work is broader, with a long tail of commands, dialogs and edge cases.

| | Agent-hours | Wall-clock (4–6 parallel agents, 24/7) |
|---|---|---|
| Feature parity by checklist | ~350–500 | **~100–150 h (4–6 days)** |
| Robust on real-world material (codec edge cases, 4K/8K performance, pro workflows) | +150–300 | **+1–2 weeks** |

Limits on speed: machine load (builds and benchmarks slow down under many agents), disk space, and a
single integrator merging and checking each agent's work. With one agent and no parallelism, multiply
wall-clock by ~3–4.

Not reachable clean-room and locally: **Generative Extend** (needs a large video-generation model).
**Enhance Speech** and **Auto Reframe** are feasible only with openly licensed models we can ship.

## Milestones

Status: ✅ done · 🟡 in progress · ⬜ not started. Estimates are remaining agent-hours.

| # | Milestone | Status | Done | Remaining | Est. |
|---|---|---|---|---|---|
| M0 | Skeleton + visual shell | ✅ | Workspace, 20 crates, dock/workspaces, Premiere 26 look, native menus, control channel, MCP, xtask gates (layers, wasm) | — | — |
| M1 | Media I/O | ✅ | MP4/MOV demux+mux, WAV, stills, MJPEG, symphonia audio (MP3/FLAC/ALAC/Vorbis), GOP seek + frame cache, import | Media Browser polish | 2 |
| M2 | H.264 decoder | ✅ | Own decoder, bit-exact on 37+ streams, 500–600 fps 1080p | — | — |
| M3 | Editing core | 🟡 | Edit algebra (insert/overwrite/razor/lift/extract/ripple/roll/slip/slide/rate-stretch/nest/paste), tools, markers, trim mode + Trim Monitor + dynamic J/K/L trimming, Keyboard Shortcuts editor with FilmCraft/Premiere/FCP/Avid presets | Multicam, subclip polish, ~220 Premiere default shortcuts whose commands don't exist yet | 6–10 |
| M4 | Playback | 🟡 | Audio-clock master, prefetch, J/K/L, dropped-frame stats, playback resolution | Render previews, render bar, 4K/8K tuning | 8–12 |
| M5 | Effects, keyframes, GPU | 🟡 | ~60 CPU effects, 30 transitions, keyframes + value/velocity graphs, wgpu compositor | Full ~150-effect catalogue, WGSL parity for all, masks + tracking, adjustment layers, presets, Warp Stabilizer, Morph Cut | 30–45 |
| M6 | Export | ✅ | Own H.264 encoder (High/Main/Baseline, B-frames, VBR/CBR/2-pass) → MP4 + own AAC; ProRes, MJPEG, PNG, GIF, WAV; background jobs | Preset library, queue UI, smart render | 6–8 |
| M7 | Audio | 🟡 | Mixer graph (tracks → submixes → Mix, pre/post-fader inserts and sends, latency-compensated, sample-accurate, ~6× realtime for 24 tracks × 3 effects on one core), Audio Track Mixer + Audio Clip Mixer panels, track automation (Off/Read/Latch/Touch/Write, recorded live while playing, thinned to keyframes, timeline lanes with pen editing), solo/solo-safe, channel mapping basics, peak + BS.1770 loudness meters (match ffmpeg), DSP crate with 12 clip/track effects, Audio Gain (set/adjust/normalize), Constant Power / Constant Gain / Exponential Fade | Essential Sound, 5.1 panner and multichannel buses, voice-over record, effect editor windows (EQ curve), remaining effects (multiband, convolution reverb), clip-mixer automation recording | 6–9 |
| M8 | Colour | 🟡 | Lumetri: basic, creative + looks, RGB & hue curves, wheels, HSL secondary, vignette; basic scopes | LUT import UI, colour match, colour management + HDR (PQ/HLG, log, tone mapping) | 8–12 |
| M9 | More codecs | 🟡 | ProRes decode+encode, AAC decode+encode, HEVC Main/Main 10 decoder (bit-exact on 41 fixtures, ~225 fps 1080p), VP9 decoder (profiles 0–3, 8/10/12-bit, bit-exact on 50+ fixtures; WebM/MKV `V_VP9` and MP4 `vp09` import with key-frame-checked seeking), Matroska/WebM import (H.264/HEVC/VP9/ProRes/MJPEG + AAC/Opus/FLAC/MP3/Vorbis/PCM), Opus decoder (SILK/CELT/hybrid, 5.1/7.1 multistream; all RFC 8251 vectors range-exact; WebM/MKV/MP4) | VP9 frame threading, Ogg Opus files, AV1 (rav1d), DNxHR, MXF, hardware decode | 12–20 |
| M10 | Graphics & captions | 🟡 | Caption tracks (Subtitle/CEA-608/708/Teletext formats, track style), SRT/WebVTT/SCC import+export (frame-exact, property-tested), caption editing (add/split/merge/trim/move, sync-locked insert/extract), Text panel Captions tab, burn-in in Program monitor and export; text engine (`crates/text`: bundled + system fonts, harfrust shaping, bidi, line breaking, paragraph layout, glyph cache; 3-line 1080p title ≈ 0.15 ms warm); graphic clips with text + shape layers (fill, 2 strokes, background, shadow, keyframable transform), Type tool with on-monitor editing, shape/pen tools, Properties/Essential Graphics editor, align/distribute | Responsive design pins, rolls/crawls, per-character styles, motion graphics templates, MCC/STL/TTML, 608/708 embedding, speech-to-text | 12–18 |
| M11 | Interchange & project management | 🟡 | `.fcproj` schema versions + migrations, atomic saves, Save a Copy/Revert, auto-save ring + crash-recovery journal (Preferences ▸ Auto Save, recovery prompt), FCP7 XML, FCPXML, EDL, OTIO | Relink/offline, project manager, proxies | 8 |
| M12–M16 | Web (WASM), platform, long tail | 🟡 | L0–L4 crates compile to wasm32 | Web app shell (file access, WebCodecs, audio), ~850 remaining commands and dialogs, performance hardening | 40–65 |

## Running now

- Playback: fix a UI hang after preview renders, then a load-robust playback benchmark and profiling (target: 3 × 1080p streams with 0 dropped frames)

## Log

- **2026-10-01 (later):** text engine (shaping, bidi, line breaking) + Type/Shape/Pen tools + graphic clips + graphics panel; Trim Monitor + dynamic trimming; Keyboard Shortcuts editor; audio mixer with automation. Fixed: MP4 muxer wrote unreadable all-empty sample tables; system font scan race.

- **2026-10-01:** Audio mixing (M7.2/M7.5/M7.6 basics): mixer graph with submixes, sends, inserts and latency compensation; Audio Track / Clip Mixer panels; Latch/Touch/Write automation recorded live; timeline track keyframes; Audio Gain dialog.
- **2026-10-01:** recovered from a machine crash with no lost work. Merged: VP9 decoder (profiles 0–3, bit-exact, WebM/MKV/MP4), Opus decoder (RFC 8251 range-exact; WebM/MKV/MP4), captions (SRT/VTT/SCC, burn-in), render bar + render previews, project schema versioning + atomic saves + auto-save + crash-recovery journal, test infrastructure (golden images, loudness oracle vs ffmpeg, headless scripted UI tests; fixed a GPU stale-texture bug), public contributor docs and licence files.

- **2026-09-30 (evening):** Opus decoder (RFC 6716/8251, range-exact on every conformance vector, ~80–110× realtime 48 kHz stereo) wired into WebM/MKV and MP4 import.
- **2026-09-30 (afternoon):** Matroska/WebM import; LUFS meters; clip audio effects on the DSP crate.
- **2026-09-30 (midday):** HEVC decoder, Matroska/WebM demuxer and audio DSP merged; HEVC import wired; asset rules (AGENTS.md, ATTRIBUTION.md, `cargo xtask assets`); README with hero screenshot and the Craft family.
- **2026-09-30 (late morning):** keyframe value/velocity graphs; xtask gates; H.264 MP4 export; Lumetri curves, wheels, looks, HSL secondary.
- **2026-09-30 (early morning):** H.264 decoder, ProRes, AAC; GPU compositor; MCP; Premiere 26 visual fidelity pass.
