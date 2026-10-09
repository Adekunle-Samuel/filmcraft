# ADR 0002: the Assistant talks to an LLM provider over the network, opt-in and off by default

- **Status:** accepted (2026-10-09, plan approved by the project owner; the Assistant milestone
  "A" in the agentic-framework plan)
- **Scope:** `crates/llm`, `crates/agent`, the Assistant panel in `crates/ui-egui`, the
  `assistant` feature of `apps/filmcraft` and `apps/filmcraft-cli`

## Context

FilmCraft is local-first: until now nothing leaves the machine except optional speech-model
downloads (pinned, SHA-256 checked, user-initiated). The Assistant is a chat panel where the user
asks for an edit ("clean up this interview", "make it look like this reference", "give me three
cuts") and an LLM plans it with FilmCraft's own tools. Planning edits from a transcript and
understanding reference frames needs a capable model; today the practical choices are hosted APIs
(Claude first) or a local model served by Ollama / LM Studio.

That is the first feature that sends user content to a third party: project structure, transcript
text, numeric analysis (loudness, shot lengths, colour statistics) and, when vision is on,
downscaled frames.

## Decision

1. **Provider-neutral core, network only behind a feature.** `crates/llm` (L5) holds the message
   types, the `LlmProvider` trait, the SSE parser and the Anthropic / OpenAI-compatible wire
   codecs as pure functions. Its `http` feature (off by default) adds the blocking transport. The
   apps expose it as the `assistant` feature, which also enables `whisper` (talking-head cleanup
   needs word timings). Without it, and on the web, no provider is installed and the panel says the
   assistant is not available in this build. Layers L0–L4 gain no network access.
2. **TLS is rustls with the `rustls-rustcrypto` provider** and the platform verifier, the same stack
   as the speech-model downloads, so the product stays pure Rust. The provider is an alpha crate;
   the alternatives (`ring`, `aws-lc-rs`) are not pure Rust. Revisit when a stable pure-Rust
   provider ships. Plain `http://` is allowed only to loopback hosts (local model servers).
3. **Nothing is sent before consent.** The first use shows what is sent and to which host; the user
   must accept. The local (OpenAI-compatible) provider is offered as the private option. Vision
   (frames) is a separate switch.
4. **API keys** come from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`, or a file
   `<data dir>/assistant/credentials.json` written atomically with mode 0600 on Unix and a visible
   "stored unencrypted on this computer" warning. Keys never enter preferences, project files, the
   journal, the event log, `ui.inspect`, screenshots or error messages (redacting `Debug`). An OS
   keychain needs FFI and is deferred (it would belong in `crates/platform` under ADR 0001).
5. **The model never edits directly.** It calls a curated tool catalogue (engine `tools`), produces
   an `EditPlan` that the engine validates, previews and applies as one undo step into a new
   sequence. Destructive actions (export, overwrite, delete, preference changes, model downloads,
   non-curated commands that write) need a user click, enforced by the host whatever the model
   says. LLM output and anything it quotes (transcripts, file names) are hostile input under
   AGENTS.md §0: strict parsing, caps, no panics.
6. **Tests need no network and no key.** A `ScriptedProvider` replays canned turns; live evals run
   only on request with `FILMCRAFT_EVAL_LIVE=1`, because they cost money.

## Consequences

- Release builds decide whether to ship with `assistant`; the default build is unchanged.
- Privacy docs (`docs/assistant.md`) must list exactly what each tool sends.
- The tool catalogue is shared with the MCP server, so Claude Code and the in-app agent see the
  same tools and the same approval rules.
