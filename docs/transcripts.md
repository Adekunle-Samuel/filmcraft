# Transcripts and text-based editing

FilmCraft's Text panel ▸ **Transcript** tab shows the dialogue of the open sequence as text. Select
words to mark In/Out, then extract or lift them; remove filler words and long pauses in one step;
turn the transcript into captions. Every action is an engine command (`transcript.*`), so the CLI,
the control channel and MCP agents can do the same.

## Model

- A **transcript** belongs to a media item (`Project::transcripts`, saved in the `.fcproj` since
  schema v9). It lists **words** with media-time bounds (`Tick`s), an optional speaker index and a
  confidence, plus the speaker names and the language. Because the times are media time, the
  transcript stays valid however the clip is trimmed, moved, sped up or reused.
- The **sequence transcript** is derived, never stored (`filmcraft_edit::transcript::sequence_words`):
  audio tracks are read top first; a word is heard through the first enabled clip whose range
  covers the word's midpoint (duplicates of the same dialogue on lower tracks read once); disabled,
  reversed and frame-hold clips contribute nothing.
- Speaker names come from the clip transcripts, so renaming "Speaker 1" in every transcript renames
  it across the sequence.

## Commands

| Command | What it does |
|---|---|
| `transcript.generate` | Transcribe media items (`items`, else the Project selection, else the media of the sequence's audio clips). Params: `model` (default `whisper-base`), `language` (`auto` = detect), `diarize`, `maxSpeakers`, `keepFillers`, `prompt`, `regions`, `wait` (see [Fillers, prompts and voiced regions](#fillers-prompts-and-voiced-regions)). Runs as a background job and returns `{job, items, skipped}`; `jobs.list` shows its progress and `jobs.cancel` stops it without changing anything. The transcripts are stored in one undo step ("Transcribe") when it finishes. With `wait: true` (and always on the web) it finishes first and reports `items: [{item, words, speakers, language, source}]`. |
| `transcript.set` | Store a transcript you bring (JSON: `language`, `speakers`, `words` with `text`/`start`/`end`/`speaker`); it is sorted and made well formed. |
| `transcript.delete` | Remove transcripts. |
| `transcript.inspect` | The sequence transcript: words (index, text, sequence times, speaker, clip), paragraphs, speakers, the word at the playhead. |
| `transcript.search` | Word-index ranges matching a phrase (case and punctuation ignored; the last word may be a prefix). |
| `transcript.select` | Mark In/Out around words `from..=to` (frame-snapped outward) and move the playhead there. |
| `transcript.extract` / `transcript.lift` | Extract (ripple) or lift the words' frames on the targeted tracks. |
| `transcript.renameSpeaker` | Rename a speaker by name (every transcript) or by index in one `item`. |
| `transcript.removeFillers` | Ripple-delete filler words (`fillers`, default um/uh/erm/…; phrases such as "you know" allowed). |
| `transcript.removePauses` | Ripple-delete pauses longer than `minSeconds`, keeping `keepSeconds` of air on both sides. |
| `transcript.createCaptions` | Lay the words out as captions on a new caption track (`maxChars`, `lines`, `minSeconds`, `maxSeconds`, `gapFrames`). |
| `transcript.models` / `transcript.downloadModel` | List the speech models (size, licence, installed) / download one. |

### Waveform silence (no transcript needed)

These work on the sequence mix itself (muted tracks, clip gain and effects count as they sound), so
they run on any build, before or without speech-to-text. The Assistant's talking-head cleanup runs
them first, then uses Whisper for filler words and content.

| Command | What it does |
|---|---|
| `audio.detectSilence` | Silences of the mix: 10 ms level envelope, threshold `thresholdDb` or derived from the recording (between its noise floor and speech level), gaps of at least `minSeconds` (0.5) padded by `padSeconds` (0.08) next to the voice and snapped inward to frames. With a transcript, every word (± pad) is cut out of the silences, so a quiet word is never removed (`respectTranscript: false` turns that off). Returns the silences, the voiced regions (sequence seconds) and the threshold used. Read-only. `startSeconds` / `endSeconds` limit the span (at most 4 h per call). |
| `audio.removeSilence` | The same ranges, ripple-deleted on every unlocked track in one undo step. |
| `audio.loudness` | EBU R128 report of the mix (or a span): integrated LUFS, loudness range, max momentary / short-term, sample peak, true peak. Digital silence reports `null`. |

To combine word cuts, fillers, pauses, silences and captions into one reviewed, undoable edit (the
way the assistant edits), write an edit plan: see [edit-plans.md](edit-plans.md).

## Speech recognition

Recognition goes through the `Transcriber` trait (`crates/speech`). The built-in recogniser is
OpenAI's Whisper, run in pure Rust on [candle](https://github.com/huggingface/candle) on the CPU,
with timestamp decoding, language detection and word times from cross-attention alignment (see the
`filmcraft_speech::whisper` module docs). Speakers are labelled by clustering per-chunk MFCC
statistics (`filmcraft_speech::diarize`); no model is involved.

Both are **optional features**, off by default and never built for the web:

- `whisper` (on `filmcraft-speech`, `filmcraft-engine`, and the `filmcraft` / `filmcraft-cli`
  apps, where it also enables downloads): candle inference.
- `download` (`speech-download` on the engine): HTTPS downloads with rustls + RustCrypto and the
  operating system's certificate verifier.

Without `whisper`, and with no recogniser installed, `transcript.generate` and Transcribe Sequence
are disabled, with "speech-to-text is not available in this build" as the reason (`describe`,
`command_list {"enabled_only": true}` and the menus show it); with Automatically transcribe clips
on, `file.import` reports the same reason as a `transcription: …` entry in its `errors`.
Without `speech-download`, `transcript.downloadModel` is disabled the same way. Transcripts can
still be imported with `transcript.set` and edited with every other command. Hosts and tests can
install any recogniser in `Session::transcriber`, which enables transcription in any build.

### Fillers, prompts and voiced regions

- **`keepFillers: true`** conditions Whisper on a disfluent prompt
  (`filmcraft_speech::FILLER_PROMPT`, "Umm, so, uh, I was like, hmm…"). Whisper tends to leave
  "um" and "uh" out of its text; after this prompt it writes them out with word times, so
  `transcript.removeFillers` can find them. Off unless set.
- **`prompt`** (text, at most 2000 characters) is any other initial prompt: names and terms to
  spell right, or a style. It wins over `keepFillers`. Whisper places it before every 30-second
  window as `<|startofprev|>` followed by the prompt's last 223 tokens, ahead of
  `<|startoftranscript|>` (each window gets the same prompt, not the previous window's text). Word
  alignment runs without it.
- **`regions`** (`[[startSeconds, endSeconds], …]`, media seconds of one item, at most 10 000)
  transcribes only those spans, for example the voiced regions a silence pass found. Each region
  gets 0.3 s of air on both sides; regions are clamped to the media and merged where they
  overlap, and the spans are transcribed laid end to end. Word times are mapped back to media
  time: a word belongs to the region its start falls in and ends by that region's end. Regions
  that are not numbers, run backwards or lie outside the media are refused.

### Models

Weights are **never** bundled or committed. They are downloaded on request into
`<data dir>/models/<id>/` (see `filmcraft_engine::autosave::default_data_dir`), each file pinned to a
revision of OpenAI's Hugging Face repositories and checked against its SHA-256:

| Id | Languages | Licence |
|---|---|---|
| `whisper-tiny` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-base` (default) | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |
| `whisper-small` | multilingual | MIT (OpenAI); HF conversion Apache-2.0 |

### Testing

Unit and engine tests use a fake `Transcriber` (`FixedTranscriber`), so CI needs no model. The
end-to-end test `crates/speech/tests/whisper_model.rs` (feature `whisper`) runs only when weights are
in `target/models/<id>/` (or `$FILMCRAFT_MODELS_DIR`) and speech samples (mono 16 kHz f32 with a
reference `.txt`) are in `target/fixtures/speech/`; it reports the word error rate and otherwise
prints SKIPPED. Measured on 2026-10-01: `whisper-tiny`, English, 12.2 % WER over 797 words of
the local speech samples (LibriSpeech read speech and dialogue clips) (about 9.5 minutes for the run in a release build on an
Apple-silicon laptop CPU).

## Limits

- Media longer than 4 hours is refused (its 16 kHz mono audio is held in memory); transcribe it in
  parts.
- One media item can't be in two transcription jobs at once.
- Track items that refer to a subclip are looked up by the subclip's id, so a transcript made for
  the parent media is not shown through subclip clips yet.
