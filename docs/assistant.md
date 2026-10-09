# The Assistant

The Assistant is a chat panel (Window ▸ Assistant) where you describe an edit and FilmCraft does
it: "clean up this interview, cut the ums and the bit about lunch, add captions, about 3
minutes", "make it look like this reference", "give me a 30 s and a 60 s version". It plans the
edit with FilmCraft's own tools, shows you exactly what it will change, and applies it as **one
undo step into a new sequence**, so your original cut is never touched.

It is opt-in and off by default. Build with `--features assistant`
(`cargo run -p filmcraft --features assistant`); the default build and the web build show the
panel with a note that it is not available. The design decisions are in
[ADR 0002](adr/0002-assistant-llm-network.md).

## Setting it up

1. Open Window ▸ Assistant.
2. Read the consent sheet: it lists what is sent and where. Nothing is sent before you accept.
3. Choose a provider in the panel's settings:
   - **Anthropic (Claude)**: set `ANTHROPIC_API_KEY` in the environment, or paste a key in the
     settings (stored unencrypted in `<data dir>/assistant/credentials.json`, readable only by
     your user). Default model `claude-opus-5-5`.
   - **OpenAI-compatible (local)**: a local server such as Ollama (`http://localhost:11434/v1`)
     or LM Studio (`http://localhost:1234/v1`). Nothing leaves your computer. Smaller local models
     follow the tools less reliably.
4. Optionally set a budget (US$ per conversation); the panel shows tokens and an estimated cost.

## What is sent

| Sent | When |
|---|---|
| Your messages | always |
| Project structure: bins, item names, durations, sequence tracks and clips | when the Assistant looks at the project |
| Transcript text with word indices and times | when it reads a transcript |
| Numbers: silences, loudness, shot lengths, colour statistics | when it analyses media |
| Downscaled frames (contact sheets, ≤ 1568 px) | only with Vision on, when it looks at footage |

Media files themselves are never uploaded. Speech-to-text runs locally (Whisper).

## How it edits

The model never edits the timeline directly. It calls a curated set of tools (the same ones the
MCP server offers to external agents, see [agents.md](agents.md)) and proposes an **edit plan**:
which words and ranges to remove and why, captions, grade, loudness, markers. The plan card shows
the transcript with the removed words struck through and the reason for each cut; untick any cut
you want to keep, then Apply. The plan compiler never cuts inside a word and keeps a little air
around speech.

Things that always wait for your click: exports, overwriting files, deleting media, changing
preferences, downloading speech models, and any command outside the curated tools. One undo
(Cmd+Z) reverses everything the Assistant did in a turn.

### Talking-head cleanup

1. **Silences** come from the audio waveform (`audio.detectSilence`): fast, no model, and a
   quiet word that has a transcript is never cut.
2. **Words and fillers** come from Whisper, run locally, with a prompt that makes it keep "um" and
   "uh" (Whisper normally drops them), transcribing only the voiced parts.
3. **Content cuts** (false starts, repeated takes, tangents) are proposed by the model from the
   transcript, each with a reason.
4. Short noises that the waveform hears but Whisper didn't transcribe are listed as optional cuts.

### Style from a reference

Import a reference video and ask for its look. `media.analyze` measures pacing (shot lengths,
cuts per minute), colour (Oklab tone and colour statistics), loudness, speech rate and aspect; the
model also looks at a contact sheet for caption style and framing. The grade is matched with
`lumetri.matchToItem` and can be baked into a reusable `.cube` LUT with `lumetri.bakeLut`.

Limits: the grade match is statistical (overall tone and colour, not individual objects or
shots), a baked LUT can't carry spatial effects (vignette, sharpening), and LUTs assume SDR
Rec.709.

## Driving it from an agent or a test

The control channel has `assistant.send`, `assistant.state`, `assistant.approve`,
`assistant.cancel`, `assistant.reset` and `assistant.settings.get|set`
([control-protocol.md](control-protocol.md)). The key is never readable through them. The agent
loop itself is in `crates/agent` (`run_turn`); tests drive it with `ScriptedProvider`, so they need
no network and no key.
