<h1 align="center">FilmCraft</h1>

<p align="center">
  <b>Non-linear video editing, reimagined in pure Rust.</b><br>
  An open-source, clean-room take on the Adobe Premiere Pro workflow: native on macOS, Windows and Linux, and in the browser via WebAssembly.<br>
  <i>By the artcraft team.</i>
</p>

<p align="center">
  <img src="docs/images/filmcraft-hero.png" alt="FilmCraft cutting a trailer for Night of the Living Dead (1968): Effect Controls, the Program monitor on the ghouls advancing, the Project bin of public-domain films, and a multi-track timeline with dissolves, markers, a Carnival of Souls insert, a Chopin score and loudness meters" width="100%">
</p>

<table>
  <tr>
    <td width="50%"><img src="docs/images/filmcraft-color.png" alt="Color workspace: a Lumetri grade on Charade (1963) with curves, colour sliders, waveform and vectorscope"><br><sub><b>Colour.</b> Grading <i>Charade</i> (1963) with Lumetri-style curves and scopes.</sub></td>
    <td width="50%"><img src="docs/images/filmcraft-keyframes.png" alt="Effect Controls with the value and velocity graphs of an eased Scale push-in on the Night of the Living Dead title"><br><sub><b>Keyframes.</b> An eased push-in on the title, with value and velocity graphs.</sub></td>
  </tr>
  <tr>
    <td width="50%"><img src="docs/images/filmcraft-export.png" alt="Export mode with H.264 MP4 settings"><br><sub><b>Export.</b> H.264 MP4 with our own encoder.</sub></td>
    <td width="50%"><sub>Footage: <i>Night of the Living Dead</i> (1968), <i>Carnival of Souls</i> (1962) and <i>Charade</i> (1963), all in the US public domain; music: Chopin performed for Musopen (CC0). The media is not in this repository; full credits in <a href="ATTRIBUTION.md">ATTRIBUTION.md</a>.</sub></td>
  </tr>
</table>

## Highlights

- **Own codecs, in Rust.** H.264 decoder (bit-exact against ffmpeg, 500+ fps at 1080p) and encoder (High profile, CABAC, B-frames, 2-pass), HEVC Main/Main 10 decoder, ProRes decode and encode, AAC-LC decode and encode, MP4/MOV and Matroska/WebM containers. No FFmpeg inside.
- **Real editing.** Insert, overwrite, ripple, roll, slip, slide, rate stretch, razor, lift/extract, nesting and markers, all built on an exact integer time base (254,016,000,000 ticks per second), so frame math never drifts.
- **GPU compositing.** A wgpu compositor samples YUV directly with footprint supersampling and blends in linear light, with a matching CPU path. Playback runs in real time at 1080p, using the audio clock as master.
- **Colour.** Lumetri-style grading: basic correction, creative looks, RGB and hue curves, colour wheels, HSL secondary and vignette, plus waveform and vectorscope.
- **Keyframes.** Linear, Bezier, auto/continuous Bezier, hold and ease, with value and velocity graphs.
- **Audio.** EBU R128 / BS.1770 loudness meters (momentary, short-term, integrated, true peak), and clip effects (EQ, filters, dynamics, limiter, delay, reverb, DeNoise, DeHum, pitch shift) on our own DSP library.
- **Export.** H.264 MP4 with AAC, ProRes and MJPEG MOV, PNG sequences, GIF and WAV, run as background jobs.
- **Agent-drivable.** Every action is a command. The UI, a CLI, a JSON control channel and an MCP server all drive the same engine, so Claude or any agent can edit, grade and export.

## Status

Early but moving fast: roughly 30% of the way to Premiere Pro parity. See **[ROADMAP.md](ROADMAP.md)** for milestone status and estimates.

## Quick start

```sh
cargo run --release -p filmcraft                           # desktop app with the demo project
cargo run --release -p filmcraft -- --control 9876         # plus the JSON-lines control server
cargo run --release -p filmcraft-cli -- commands           # list every engine command
cargo run --release -p filmcraft-cli -- mcp                # MCP server (headless)
```

The control protocol is documented in [docs/control-protocol.md](docs/control-protocol.md).

## Architecture

A layered Cargo workspace: codecs and containers at the bottom, then frames and media, project model and edit algebra, rendering and export, the engine (command registry, session, undo), and finally the swappable UI (`ui-egui`). Nothing below the UI layer depends on a UI toolkit or OS APIs, and everything up to the engine compiles to `wasm32`. `cargo xtask ci` checks formatting, lints, tests, the layering rules and the wasm build.

## Crafting Apps

Open-source, clean-room, pure-Rust creative tools. Each runs natively on macOS, Windows and Linux, and on the web.

<table>
  <tr>
    <td width="20%" align="center"><a href="https://github.com/storytold/photocraft"><b>PhotoCraft</b></a></td>
    <td>Image editing and compositing, Photoshop-class: layers, masks, adjustments and byte-exact PSD/PSB round trips.</td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/storytold/drawcraft"><b>DrawCraft</b></a></td>
    <td>Vector illustration, Illustrator-class: paths, shapes, type and artboards.</td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/storytold/filmcraft"><b>FilmCraft</b></a></td>
    <td>Non-linear video editing, Premiere Pro-class: timeline editing, own codecs, GPU compositing, colour and export. <i>You are here.</i></td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/storytold/lightcraft"><b>LightCraft</b></a></td>
    <td>Photo library and non-destructive raw developer, Lightroom-class: catalog, culling, develop and export.</td>
  </tr>
  <tr>
    <td align="center"><a href="https://github.com/storytold/printcraft"><b>PrintCraft</b></a></td>
    <td>PDF viewing and editing, Acrobat-class: pages, annotations, forms and document tools.</td>
  </tr>
</table>

## Clean room

FilmCraft is an independent implementation. It contains no Adobe code, assets, presets or LUTs, and no GPL/LGPL code; codecs are implemented from the public ITU-T and ISO specifications. ffmpeg is used only as an external test oracle. Adobe and Premiere Pro are trademarks of Adobe Inc.; FilmCraft is not affiliated with Adobe.
