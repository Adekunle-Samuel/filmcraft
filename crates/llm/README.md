# filmcraft-llm

The LLM client layer behind FilmCraft's Assistant (L5). It knows nothing about the engine or the
UI: the agent loop (`filmcraft-agent`) builds a `ChatRequest`, a provider sends it, and the
response comes back as a `ChatResponse` plus a stream of `StreamEvent`s for the UI.

- `types`: provider-neutral `Message` / `ContentBlock` (text, image, tool use, tool result and
  `Opaque`), `ToolSpec`, `ChatRequest`, `ChatResponse`, `StopReason`, `Usage`, `StreamEvent`.
- `LlmProvider`: `send(&req, on_event, cancel) -> Result<ChatResponse, LlmError>`, blocking and
  cancellable, plus `capabilities()` (tools, vision, thinking, caching).
- `LlmError`: auth, rate limits (with `retry-after`), overload, bad request, too large, network,
  cancelled, decode, unsupported; every message says what the user can do about it.
- `sse`: an incremental Server-Sent Events parser fed arbitrary byte chunks.
- `anthropic`: the Claude Messages API codec.
- `ScriptedProvider`: replays canned responses (or raw SSE transcripts) and records every request.
- `price`: per-model prices and `estimate_cost_usd`.

Without the `http` feature the crate is plain serde / serde_json (no threads, clocks or sockets)
and builds for wasm32 (`cargo xtask wasm` checks it).

## Append-only history

Thinking, redacted thinking, compaction, server-tool and any unknown blocks are kept as
`ContentBlock::Opaque { raw }`: the block exactly as the provider streamed it (thinking text and
signature reassembled from their deltas) and sent back verbatim. The agent appends each response's
content to the history unchanged and never edits earlier turns, so thinking signatures stay valid
and the prompt cache keeps hitting. `ScriptedProvider::requests()` lets tests assert this.

## Anthropic request policy

`anthropic::encode_request` produces the `POST /v1/messages` body:

- `stream: true`, `thinking: {type: "adaptive"}` with an optional `display` (`summarized`,
  `omitted`, `updates`). Never `budget_tokens`, and thinking is never disabled; effort
  (`output_config.effort`) is the only depth control.
- `tool_choice: {type: "auto"}` when tools are present: forced tool choice returns a 400 on current
  models. Tools carry `strict` and `eager_input_streaming` only when set.
- `fallbacks: "default"` (beta `server-side-fallback-2026-07-01`) on every model except Haiku.
- Prompt-cache breakpoints (`cache_control: {type: "ephemeral"}`) on the last system block marked
  `cache` and, with `cache_last_user`, on the last block of the last user message. At most two, so
  well under the API's limit of four.
- Mid-conversation `Role::System` messages are sent as `{"role": "system", "content": "…"}`.
- Deterministic: objects are built field by field with `serde_json::Map` (sorted keys), so the same
  request always encodes to the same bytes (tested), which prompt caching depends on.

`anthropic::headers` gives `anthropic-version: 2023-06-01` and `anthropic-beta` (the fallback beta,
`thinking-display-updates-2026-08-18` when `display` is `updates`, then `extra_betas`, deduplicated).

## Streaming decoder

`anthropic::StreamDecoder` takes the response body in chunks of any size and handles
`message_start`, `content_block_start` / `_delta` / `_stop` (text, input JSON, thinking and
signature deltas; unknown deltas are folded into the opaque block), `message_delta` (stop reason,
`stop_details` for refusals, usage), `message_stop`, `ping` and `error`. Tool input JSON is parsed
when its block stops; invalid or truncated JSON becomes `{"__invalid_json": "<raw>"}` so the agent
can return an `is_error` tool result instead of running the tool. A stream that ends before
`message_stop` is a network error.

Hostile input is bounded: a line (or one event's data) over 8 MiB, a body over 64 MiB, or more than
4096 content blocks gives `LlmError::TooLarge`. Tests decode a synthetic transcript split at every
byte offset (same result each time) and run mutation fuzzing (truncation, bit flips, inserted junk,
deleted spans, giant lines) under `catch_unwind`.

## Prices

`estimate_cost_usd(model, &usage)` uses first-party list prices per million tokens; cache writes
are the 5-minute rate (1.25 × input):

| Model | Input | Output | Cache read |
|---|---|---|---|
| `claude-opus-5-5` | $4 | $20 | $0.20 |
| `claude-sonnet-5-5` | $2 | $10 | $0.20 |
| `claude-haiku-5-5` | $0.10 ($0.50 over 100k prompt tokens) | $0.50 ($2.50) | 0.1 × input |

Unknown models (including local ones) return `None`.
