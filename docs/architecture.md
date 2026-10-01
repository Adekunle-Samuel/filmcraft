# Architecture

FilmCraft is a Cargo workspace of small crates with strictly enforced layering. The engine is
headless: every feature can be reached without a window, and the egui UI is one client among the
CLI, the JSON control channel and the MCP server.

Design principles:

1. **Engine-first.** Project-changing actions go through `Session::execute(id, params)`.
2. **Everything is a command.** Stable id, label, menu path, shortcut, parameter doc, `enabled()`
   predicate with a human-readable reason, and `run()`.
3. **Exact time.** Integer ticks, rational frame rates. No `f64` seconds in edit math.
4. **Copy-on-write snapshots.** The project is an `Arc<Project>`. Undo is a stack of snapshots, and
   background readers (playback, export) hold a snapshot without locking.
5. **CPU reference, GPU fast path.** The CPU compositor is the oracle. The GPU path is tested
   against it.
6. **Pure Rust, clean-room.** Codecs and containers are written from public specifications
   (see [AGENTS.md](../AGENTS.md)).

## 1. Layers

```text
 L6  apps/filmcraft · apps/filmcraft-cli
 L5  ui-egui · automation
 L4  engine
 L3  render · gpu · export · golden (test-only)
 L2  edit · codecs · interchange
 L1  frame · media · project · audio-dsp
 L0  foundation: time · geom · color · bitstream · testkit (dev-dependency only)
     codecs/containers: isobmff · matroska · h264 · h264enc · hevc · prores · aac
```

Crates are named `filmcraft-<dir>` (`crates/time` is `filmcraft-time`). The apps are `filmcraft`
and `filmcraft-cli`.

| Crate | Layer | Purpose |
|---|---|---|
| `time` | L0 | `Tick`, `FrameRate`, `TimeRange`, timecode parse/format (NDF/DF, frames, feet+frames, samples) |
| `geom` | L0 | `Vec2`, `Rect`, `Affine`, Motion-transform composition |
| `color` | L0 | colour spaces, transfer functions, YUV↔RGB matrices, LUTs |
| `bitstream` | L0 | bit reader/writer, Exp-Golomb, emulation prevention |
| `isobmff` | L0 | MP4/MOV demux and mux |
| `matroska` | L0 | MKV/WebM demux |
| `h264`, `h264enc` | L0 | H.264 decoder; H.264 encoder |
| `hevc` | L0 | H.265 Main/Main 10 decoder |
| `prores` | L0 | ProRes decoder and encoder |
| `aac` | L0 | AAC-LC decoder and encoder |
| `testkit` | L0 | test-only helpers, used only as a dev-dependency: ffmpeg/ffprobe discovery, fixture dirs, golden images ([testing.md](testing.md)) |
| `frame` | L1 | `VideoFrame` (planar YUV / RGBA8 / linear RGBA f32, colour metadata), `AudioBuffer` |
| `media` | L1 | `MediaSource` trait, probing/openers, frame cache, generators, stills, WAV |
| `project` | L1 | document model, effect definitions, keyframes |
| `audio-dsp` | L1 | loudness metering (BS.1770 / R128) and audio effects; no dependencies |
| `edit` | L2 | pure edit algebra (insert, overwrite, razor, ripple, roll, slip, slide, rate stretch…) |
| `codecs` | L2 | container + codec hub: MP4/MOV and MKV sources, GOP-aware seeking, decoder registry, audio decoding |
| `interchange` | L2 | EDL, FCP7 XML, FCPXML and OTIO import/export (no file I/O) |
| `render` | L3 | sequence evaluation, CPU compositor, video effects, transitions, audio mix |
| `gpu` | L3 | wgpu compositor (WGSL) |
| `golden` | L3 | test-only: golden-image tests of the CPU renderer and GPU-vs-CPU parity; empty library, dev-dependencies only |
| `export` | L3 | render → encode → mux pipeline, progress/cancel |
| `engine` | L4 | `Session`, command registry, undo history, media pool, jobs, interchange glue |
| `ui-egui` | L5 | the egui frontend: docking, panels, timeline, monitors, playback, control-channel handlers |
| `automation` | L5 | MCP server (`rmcp`, stdio), headless or bridged to the running app |
| `filmcraft` | L6 | desktop binary: eframe/wgpu window, cpal audio output, file dialogs, native macOS menu, TCP control server |
| `filmcraft-cli` | L6 | headless CLI: `probe`, `commands`, `render`, `run`, `mcp` |

### What `cargo xtask layers` enforces

The table of layers lives in `xtask/src/main.rs` (`LAYERS`). It also reserves names for planned
crates. The check reads `cargo metadata` and looks at normal and build dependencies (dev-dependencies
are exempt):

| Rule | Detail |
|---|---|
| Every crate has a layer | A new crate fails the check until it is added to `LAYERS`. |
| Only downward edges | A crate may not depend on a crate in a higher layer. |
| Same-layer edges are listed | From L1 up, a same-layer edge must be in `SAME_LAYER`: `media→frame`, `project→media`, `project→frame`, `gpu→render`, `export→render`, `cli→filmcraft`, plus a few reserved for planned crates. |
| L0 codecs stay standalone | L0 crates other than `time`, `geom`, `color`, `bitstream`, `testkit` may depend on no workspace crate except `filmcraft-bitstream`. External crates such as `thiserror` and `rayon` are allowed. |
| No UI/OS crates below L5 | `egui`, `eframe`, `egui-wgpu`, `winit`, `rfd`, `cpal`, `muda` are allowed only in L5 and L6. |

`cargo xtask wasm` runs `cargo check --target wasm32-unknown-unknown` on every L0–L4 crate, so
everything up to the engine stays web-portable. `unsafe_code = "deny"` applies workspace-wide.

## 2. Time base

All time is `filmcraft_time::Tick(i64)` at `TICKS_PER_SECOND = 254_016_000_000`.

That number divides evenly into the frame duration of every broadcast rate (23.976, 24, 25, 29.97,
30, 48, 50, 59.94, 60, 120…) and the sample duration of every common audio rate (8 kHz to 192 kHz,
including the 44.1 kHz family). So frame and sample positions are exact integers, edits never drift
at 29.97, and audio and video line up to the sample.

| Type | Use |
|---|---|
| `Tick` | timeline and media positions and durations |
| `FrameRate { num, den }` | `frame_duration()`, `tick_of(frame)`, `frame_at(tick)`, `snap(tick)` |
| `TimeRange` | half-open `start + duration` |
| Timecode | display only (SMPTE NDF/DF, frames, feet+frames, samples); `parse_timecode` and `format_time` |

Commands take time as `time` (ticks), `frame`, `seconds` or `timecode`; the engine converts once at
the boundary.

## 3. Data model (`filmcraft-project`)

```text
Project
├─ root: Bin                         tree of bins
├─ items: map ItemId → ProjectItem   flat
│    kind: Media(MediaClip) | Sequence(Sequence) | Subclip{..} | AdjustmentLayer{..}
└─ next_id

Sequence
├─ settings: frame rate, size, sample rate, …
├─ video_tracks / audio_tracks: Vec<Track>
├─ markers, mark_in / mark_out
Track
├─ locked, sync lock, targeting, mute/solo/visibility
├─ items: Vec<TrackItem>             sorted, never overlapping
├─ transitions: Vec<Transition>
└─ audio: volume_db, pan, effects (mixer inserts), mixer: MixerStrip
     (automation mode + lanes, sends, output, record arm, solo safe, input map)
Sequence (audio) ─ submix_tracks: Vec<Track>, master_volume_db / master_effects / master_mixer
TrackItem (a clip instance)
├─ item: ItemId, start (timeline ticks), source_in (media ticks), duration, speed
├─ link group, label, enabled
└─ effects: Vec<EffectInstance>      intrinsic Motion/Opacity/Volume… first, then standard effects
EffectInstance
└─ effect id, enabled, params: id → constant value or keyframe track
```

- Everything is plain serde data. `Sequence::check()` validates the invariants (no overlaps, unique
  ids), and the engine runs it after every sequence edit.
- Timeline positions are sequence ticks; `source_in` and keyframes are in media time, so trims and
  splits never move keyframes.
- **Effect definitions are data.** `project::effect::effect_defs()` lists every video effect,
  audio effect and transition with its parameter schema. The Effects panel tree, the Effect
  Controls rows and the parameter docs agents see are all generated from it.
- **Project files** (`.fcproj`) are the project serialised as JSON. Saves are atomic: write a
  temporary sibling file, then rename it over the target.

## 4. Command system (`filmcraft-engine`)

```rust
pub struct CommandSpec {
    pub id: &'static str,                 // "sequence.addEdit"
    pub label: &'static str,              // "Add Edit"
    pub menu: &'static [&'static str],    // ["Sequence"]; empty = not in menus
    pub shortcut: Option<&'static str>,   // "Cmd+K" (Cmd = ⌘ on macOS, Ctrl elsewhere)
    pub params: &'static str,             // r#"{"time":ticks?}"#, shown to agents
    pub enabled: fn(&Session) -> Result<(), String>,  // Err carries the reason
    pub run: fn(&mut Session, &Value) -> Result<Value>,
    pub journal: bool,                    // false for read-only queries
}
```

- All commands are in `crates/engine/src/commands.rs` (`cmd!` for actions, `query!` for read-only
  queries such as `project.inspect`, `sequence.inspect`, `effects.list`, `jobs.list`). Ids follow
  the menu structure: `file.*`, `edit.*`, `clip.*`, `sequence.*`, `markers.*`, `timeline.*`,
  `effects.*`…
- `Session::execute(id, params)` finds the spec, checks `enabled`, runs it and appends it to the
  journal.
- **Undo.** Edits go through `Session::edit(label, |project, state| …)` or
  `Session::edit_sequence(label, |seq, ctx, state| …)`. These clone the project, apply the closure,
  and on success push the old `Arc<Project>` with the label onto the undo stack (200 entries).
  On error nothing changes. `edit.undo` and `edit.redo` swap snapshots. Thanks to structural sharing
  a snapshot costs little.
- **Editor state** (`EditorState`: active sequence, playheads, selection, targeting, edit points…)
  is serde, so agents can read it with `state.inspect`.
- **Events** (`ProjectChanged`, `Toast`, `OpenSequence`, `OpenSource`) are drained by frontends
  each frame.
- **UI-only commands** (tools, playback, zoom, panels, workspaces) live in
  `crates/ui-egui/src/menus.rs` (`UI_COMMANDS`). The menu bar is built from the engine registry plus
  this table, and `menus::invoke` is the single entry point for menus, shortcuts and the control
  channel.

## 5. Media, render and playback pipeline

```text
file ──► codecs (MP4/MOV, MKV, audio)        demux + decode, GOP-aware seek
          │   decoder registry: h264, hevc, prores, mjpeg (+ any registered first)
          ▼
        media::MediaSource ──► frame cache (byte-budgeted LRU, shared)
          ▼
        render::render_sequence(project, seq, t, scale)        CPU reference
          per track bottom→top: map timeline t → media t (speed), fetch frame,
          standard effects → Motion → Opacity/blend, transitions, composite
          in linear-light premultiplied f32
          │
          └─ render::plan::plan_frame → gpu::GpuCompositor     GPU path
               layers = decoded YUV/RGBA frames + matrix + opacity;
               anything the shaders don't cover is pre-rendered on the CPU
          ▼
        ui-egui frames.rs worker pool ──► monitors (program/source), thumbnails, prefetch
```

- **Sources.** `media::MediaSource` yields `video_frame(FrameRequest)` and
  `audio(start, frames, rate)` in media time. Sources are `Send + Sync` and shared by monitors,
  thumbnails, playback and export. The engine's `MediaPool` creates one per project item, lazily,
  through registered openers (`codecs::openers()`: MP4/MOV, MKV/WebM, audio files).
- **Seeking.** `codecs::Mp4Source` seeks to the preceding sync sample and decodes forward, caching
  every frame of the GOP. Sequential playback reuses the decoder. Decoders implement
  `codecs::VideoDecoder`. `register_video_decoder` puts a factory in front of the built-in ones, so a
  hardware decoder can take precedence.
- **Compositor.** `render` is the reference for monitors, thumbnails and export. `render::plan`
  turns a frame into GPU layers. Non-Normal blend modes, standard effects, adjustment layers, nested
  sequences and non-dissolve transitions are rendered on the CPU for that layer or frame and handed to
  the GPU as an image, so both paths give the same picture. Setting `FILMCRAFT_CPU_COMPOSITE=1`
  forces the CPU path in the desktop app.
- **Frame scheduling.** `crates/ui-egui/src/frames.rs` runs a small pool of worker threads with
  prioritised jobs: the frame on screen first, then playback prefetch, then thumbnails. The UI never
  decodes. It shows the exact frame when it is ready and holds the nearest cached frame meanwhile.
- **Audio clock.** The desktop app passes a cpal output (`apps/filmcraft/src/audio.rs`) to the UI as
  `AudioOut`. While playing, the samples played by the sound card drive the playhead and video follows.
  Without an audio device, playback falls back to the wall clock. Dropped frames are counted.
  Sequence audio goes through the mixer graph (`render::mixer`, §5.1); clip audio effects run on
  `audio-dsp` via `render::audio_fx`.

### 5.1 Audio mixer

```text
clip: gain → clip effects → Volume / Channel Volume / Panner (clip keyframes, media time)
      → audio transitions → summed per track                           render::audio::track_input
track / submix strip:  input map, mono fold → pre-fader inserts → pre-fader sends → mute
      → fader (volume) → meter → post-fader inserts → post-fader sends → pan / balance → output
Mix:  bus sum → pre-fader inserts → fader → meter → post-fader inserts → out    render::mixer
```

- **Model** (`project::mixer`). Every audio track, submix and the Mix has a `MixerStrip`. Static
  values stay in `Track::volume_db`, `pan`, `muted` and the send/effect parameters; automation is
  keyframes in sequence ticks: lanes `volume`, `pan`, `mute` (hold), `send.<i>.level` in
  `MixerStrip::lanes`, and insert parameters (`fx.<slot>.<param>`) in the effect's own keyframes.
  Up to 5 inserts (`EffectInstance::post_fader` picks the side) and 5 sends per strip. Submixes feed
  the Mix or a submix after them (no feedback). All fields have serde defaults, so older projects
  load unchanged.
- **Graph** (`render::mixer::mix_graph`). Lanes are evaluated per sample; effect parameters update
  on an absolute 64-sample grid, so the output does not depend on how callers cut the timeline into
  requests (export batches and device callbacks give identical samples). Inserts that report
  latency delay their strip; each route into a bus gets a compensation delay and the graph is read
  ahead by its total latency. Graph state (DSP, delay lines) is cached per structure and continued by
  sequential readers; other requests start fresh with a pre-roll (effect tails, ≤ 3 s). Tracks run
  in parallel (rayon), buses in order. Mono tracks pan with the −3 dB constant-power law; stereo
  tracks and sends use balance. Solo keeps soloed and solo-safe strips plus everything feeding them
  or fed by them. 24 tracks × (EQ + Dynamics + Studio Reverb) + a compressed submix renders at
  ~6× realtime on one core (release).
- **Automation modes** (Premiere semantics). Off ignores lanes; Read plays them; Latch records from
  the first touch and holds the last value until playback stops; Touch records while held and ramps
  back to the existing automation over the **automatch time** (Preferences ▸ Audio, 1 s); Write
  records every control from playback start (then switches to Touch unless "Switch to Touch after
  Write" is off).
- **Recording** (`engine::mixer`). Playback start runs `mixer.recordStart`, stop runs
  `mixer.recordStop`. Fader and knob drags send `mixer.touch` (value, playhead) while held and
  `mixer.release` when let go. Held values go to `render::mixer::LiveMix`, which the playing mix
  reads, so moves are heard at once. At stop each gesture stream is thinned (linear keyframe
  thinning, optional minimum time interval) and written over its time range as one undo step,
  with boundary keyframes that keep the automation outside the range unchanged.
- **Live state.** `PreviewStore::live` (`LiveMix`) also carries per-strip meter peaks posted by the
  mix (Track Mixer and Audio Meters read them) and the newest project snapshot, which the audio
  callback uses, so edits made during playback are heard.

## 6. Export jobs (`filmcraft-export`)

```text
file.exportMedia {path, format, scale, audio, quality}
  → engine creates a Job {id, label, progress, result} and runs it on a background thread
  → export: render frames in parallel batches → encode in order → mux; audio mixed per batch
  → jobs.list shows progress; jobs.cancel sets the shared cancel flag
```

| Format | Encoder | Container |
|---|---|---|
| `h264` | `filmcraft-h264enc` + `filmcraft-aac` | MP4 (`isobmff`) |
| `prores` | `filmcraft-prores` | MOV |
| `mjpeg` | built in | MOV |
| `png`, `gif`, `wav` | built in | image sequence / GIF / WAV |

Video encoders implement `export::VideoEncoder`. Codec crates plug in with `register_encoder` and
`register_audio_encoder`.

Timelines can be exchanged as EDL, FCP7 XML, FCPXML or OTIO. `file.import` detects these formats and
merges the result into the project as one undoable step, and `file.exportInterchange`,
`file.exportEdl`, `file.exportFcpxml` and `file.exportOtio` write them.

## 7. Automation surfaces

All of these dispatch the same command ids.

| Surface | Where | Scope |
|---|---|---|
| UI | `ui-egui` menus, shortcuts, panels | `menus::invoke` → engine or UI command |
| CLI | `filmcraft-cli` | `commands`, `run script.jsonl` (one `{"id","params"}` per line), `render`, `probe` |
| Control channel | `filmcraft --control <port>` | JSON lines on loopback TCP: engine commands plus synthetic input, inspection and screenshots of the live UI |
| MCP | `filmcraft-cli mcp` | stdio MCP server: headless in-process session, or `--bridge` to the control channel |

- **Automation ids.** Every interactive widget calls `app.auto.add(id, rect, label)` each frame
  (`crates/ui-egui/src/automation.rs`). Agents click by id, e.g. `tools.Razor`,
  `timeline.clip.<id>`, `effects.item.gaussian_blur`, `panel.Timeline`.
- **UI state** that is not project data (tool, workspace, dock layout, zoom, scroll, monitor
  settings) is in `crates/ui-egui/src/state.rs` as serde structs, so `ui.inspect` and `ui.set` can
  read and write it.

Protocol reference: [control-protocol.md](control-protocol.md). Agent guide: [agents.md](agents.md).

## 8. Not built yet

The layer table reserves names for crates that don't exist yet: `riff`, `mjpeg`, `dnx`,
`keyframe`, `effects`, `text`, `audio`, `captions`, `scopes`, `playback`, `format` and `platform`.
Until they exist, that work lives elsewhere: keyframes and effect definitions in `project`, effects
and the audio mix in `render`, scopes and playback in `ui-egui`, and OS integration (cpal, rfd,
native menus) in `apps/filmcraft`. [ROADMAP.md](../ROADMAP.md) has the milestone status.
