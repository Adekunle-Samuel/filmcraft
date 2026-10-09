//! The Anthropic Messages API codec (`POST /v1/messages`, streaming): pure functions from
//! [`ChatRequest`] to the request body and headers, and an incremental decoder from the SSE
//! response to [`ChatResponse`] plus [`StreamEvent`]s.
//!
//! Request policy (see the crate README): adaptive thinking (never `budget_tokens`, never
//! disabled), explicit `output_config.effort`, `tool_choice: auto` (forced tool choice is rejected
//! by current models), server-side fallbacks (`fallbacks: "default"`) except on Haiku, and
//! prompt-cache breakpoints on the last cached system block and the last user message.
//! Encoding is deterministic: the same request always gives the same bytes.

use crate::LlmError;
use crate::error::classify_stream_error;
use crate::sse::{SseEvent, SseParser};
use crate::types::{ChatRequest, ChatResponse, ContentBlock, Message, Role, StopReason, StreamEvent, ToolResultContent, Usage, parse_tool_input};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The `anthropic-version` header value.
pub const API_VERSION: &str = "2023-06-01";
/// The beta enabling `fallbacks: "default"`.
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// The beta needed for `thinking.display: "updates"` (progress notes between tool calls).
pub const THINKING_UPDATES_BETA: &str = "thinking-display-updates-2026-08-18";
/// The default API host.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// The request path.
pub const MESSAGES_PATH: &str = "/v1/messages";
/// The API accepts at most this many `cache_control` breakpoints per request.
pub const MAX_CACHE_BREAKPOINTS: usize = 4;
/// Most content blocks accepted in one response.
pub const MAX_BLOCKS: usize = 4096;

/// Whether `model` gets server-side fallbacks (`fallbacks: "default"`); Haiku has none.
pub fn uses_fallbacks(model: &str) -> bool {
    !model.starts_with("claude-haiku")
}

/// The request body for `req`.
pub fn encode_request(req: &ChatRequest) -> Value {
    let mut breakpoints = 0usize;
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(req.max_tokens));
    body.insert("stream".into(), json!(true));

    if !req.system.is_empty() {
        let last_cached = req.system.iter().rposition(|b| b.cache);
        let blocks: Vec<Value> = req
            .system
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let mut o = Map::new();
                o.insert("type".into(), json!("text"));
                o.insert("text".into(), json!(b.text));
                if Some(i) == last_cached && breakpoints < MAX_CACHE_BREAKPOINTS {
                    o.insert("cache_control".into(), cache_control());
                    breakpoints += 1;
                }
                Value::Object(o)
            })
            .collect();
        body.insert("system".into(), Value::Array(blocks));
    }

    let last_user = if req.cache_last_user { req.messages.iter().rposition(|m| m.role == Role::User) } else { None };
    let messages: Vec<Value> = req
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let cache = Some(i) == last_user && breakpoints < MAX_CACHE_BREAKPOINTS;
            let (v, placed) = encode_message(m, cache);
            if placed {
                breakpoints += 1;
            }
            v
        })
        .collect();
    body.insert("messages".into(), Value::Array(messages));

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut o = Map::new();
                o.insert("name".into(), json!(t.name));
                o.insert("description".into(), json!(t.description));
                o.insert("input_schema".into(), t.input_schema.clone());
                if t.strict {
                    o.insert("strict".into(), json!(true));
                }
                if t.eager_input_streaming {
                    o.insert("eager_input_streaming".into(), json!(true));
                }
                Value::Object(o)
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!({"type": "auto"}));
    }

    let mut thinking = Map::new();
    thinking.insert("type".into(), json!("adaptive"));
    if let Some(d) = &req.thinking_display {
        thinking.insert("display".into(), json!(d));
    }
    body.insert("thinking".into(), Value::Object(thinking));
    if let Some(e) = req.effort {
        body.insert("output_config".into(), json!({"effort": e.as_str()}));
    }
    if uses_fallbacks(&req.model) {
        body.insert("fallbacks".into(), json!("default"));
    }
    Value::Object(body)
}

/// The request body as bytes (deterministic).
pub fn encode_request_bytes(req: &ChatRequest) -> Result<Vec<u8>, LlmError> {
    serde_json::to_vec(&encode_request(req)).map_err(|e| LlmError::Decode(e.to_string()))
}

/// The protocol headers for `req` (not including the API key or `content-type`).
pub fn headers(req: &ChatRequest) -> Vec<(&'static str, String)> {
    let mut betas: Vec<&str> = Vec::new();
    if uses_fallbacks(&req.model) {
        betas.push(FALLBACK_BETA);
    }
    if req.thinking_display.as_deref() == Some("updates") {
        betas.push(THINKING_UPDATES_BETA);
    }
    for b in &req.extra_betas {
        let b = b.trim();
        if !b.is_empty() && !betas.contains(&b) {
            betas.push(b);
        }
    }
    let mut h = vec![("anthropic-version", API_VERSION.to_string())];
    if !betas.is_empty() {
        h.push(("anthropic-beta", betas.join(",")));
    }
    h
}

fn cache_control() -> Value {
    json!({"type": "ephemeral"})
}

/// Encode one message; with `cache`, the last non-opaque block gets a breakpoint. Returns whether
/// one was placed.
fn encode_message(m: &Message, cache: bool) -> (Value, bool) {
    if m.role == Role::System {
        // Mid-conversation system message: plain text content.
        let text: String =
            m.content.iter().filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n");
        return (json!({"role": "system", "content": text}), false);
    }
    let role = if m.role == Role::User { "user" } else { "assistant" };
    let mut blocks: Vec<Value> = m.content.iter().map(encode_block).collect();
    // The breakpoint goes on the last block FilmCraft built (opaque blocks are sent verbatim).
    let mut placed = false;
    if cache
        && let Some(i) = m.content.iter().rposition(|b| !matches!(b, ContentBlock::Opaque { .. }))
        && let Some(Value::Object(o)) = blocks.get_mut(i)
    {
        o.insert("cache_control".into(), cache_control());
        placed = true;
    }
    (json!({"role": role, "content": Value::Array(blocks)}), placed)
}

fn image(media_type: &str, data: &str) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}})
}

fn encode_block(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Image { media_type, data_base64 } => image(media_type, data_base64),
        ContentBlock::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
            let content: Vec<Value> = content
                .iter()
                .map(|c| match c {
                    ToolResultContent::Text { text } => json!({"type": "text", "text": text}),
                    ToolResultContent::Image { media_type, data_base64 } => image(media_type, data_base64),
                })
                .collect();
            let mut o = Map::new();
            o.insert("type".into(), json!("tool_result"));
            o.insert("tool_use_id".into(), json!(tool_use_id));
            o.insert("content".into(), Value::Array(content));
            if *is_error {
                o.insert("is_error".into(), json!(true));
            }
            Value::Object(o)
        }
        ContentBlock::Opaque { raw } => raw.clone(),
    }
}

/// A content block being assembled.
#[derive(Debug)]
enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
        initial: Value,
    },
    /// Thinking, redacted thinking, compaction, server tools, text with citations, unknown types:
    /// the start object, updated by its deltas.
    Opaque {
        raw: Map<String, Value>,
        json: String,
    },
}

/// Incremental decoder of an Anthropic streaming response.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    sse: SseParser,
    open: BTreeMap<u64, Block>,
    done: BTreeMap<u64, ContentBlock>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    model: Option<String>,
    stopped: bool,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw response bytes.
    pub fn push(&mut self, chunk: &[u8], on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
        if self.stopped {
            return Ok(());
        }
        for ev in self.sse.push(chunk)? {
            self.event(&ev, on_event)?;
        }
        Ok(())
    }

    /// True once `message_stop` arrived.
    pub fn is_complete(&self) -> bool {
        self.stopped
    }

    /// End of the byte stream: the assembled response.
    pub fn finish(mut self, on_event: &mut dyn FnMut(StreamEvent)) -> Result<ChatResponse, LlmError> {
        if !self.stopped {
            for ev in self.sse.finish()? {
                self.event(&ev, on_event)?;
            }
        }
        if !self.stopped {
            return Err(LlmError::Network("the response stream ended early".into()));
        }
        Ok(ChatResponse {
            content: self.done.into_values().collect(),
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            usage: self.usage,
            model: self.model,
        })
    }

    fn event(&mut self, ev: &SseEvent, on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
        if self.stopped {
            return Ok(());
        }
        let v: Value = serde_json::from_str(&ev.data).map_err(|e| LlmError::Decode(format!("bad event JSON: {e}")))?;
        let kind = v.get("type").and_then(Value::as_str).unwrap_or(ev.event.as_str());
        match kind {
            "message_start" => {
                let msg = v.get("message");
                self.model = msg.and_then(|m| m.get("model")).and_then(Value::as_str).map(str::to_string);
                if let Some(u) = msg.and_then(|m| m.get("usage")) {
                    merge_usage(&mut self.usage, u);
                }
                on_event(StreamEvent::Usage(self.usage));
            }
            "content_block_start" => {
                let index = index_of(&v)?;
                if self.open.contains_key(&index) || self.done.contains_key(&index) {
                    return Err(LlmError::Decode(format!("content block {index} started twice")));
                }
                if self.open.len().saturating_add(self.done.len()) >= MAX_BLOCKS {
                    return Err(LlmError::TooLarge);
                }
                let Some(Value::Object(cb)) = v.get("content_block") else {
                    return Err(LlmError::Decode("content_block_start without a block".into()));
                };
                let block = start_block(cb.clone());
                if let Block::ToolUse { id, name, .. } = &block {
                    on_event(StreamEvent::ToolUseStarted { id: id.clone(), name: name.clone() });
                }
                if let Block::Text(t) = &block
                    && !t.is_empty()
                {
                    on_event(StreamEvent::TextDelta(t.clone()));
                }
                self.open.insert(index, block);
            }
            "content_block_delta" => {
                let index = index_of(&v)?;
                let Some(block) = self.open.get_mut(&index) else {
                    return Err(LlmError::Decode(format!("delta for unknown content block {index}")));
                };
                let Some(Value::Object(delta)) = v.get("delta") else {
                    return Err(LlmError::Decode("content_block_delta without a delta".into()));
                };
                apply_delta(block, delta, on_event)?;
            }
            "content_block_stop" => {
                let index = index_of(&v)?;
                let Some(block) = self.open.remove(&index) else {
                    return Err(LlmError::Decode(format!("stop for unknown content block {index}")));
                };
                self.done.insert(index, finish_block(block));
                on_event(StreamEvent::BlockStop { index: usize::try_from(index).unwrap_or(usize::MAX) });
            }
            "message_delta" => {
                if let Some(d) = v.get("delta")
                    && let Some(r) = d.get("stop_reason").and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason(r, d.get("stop_details").or_else(|| v.get("stop_details"))));
                }
                if let Some(u) = v.get("usage") {
                    merge_usage(&mut self.usage, u);
                }
                on_event(StreamEvent::Usage(self.usage));
            }
            "message_stop" => {
                // Blocks left open by a broken stream are closed as they are.
                for (i, b) in std::mem::take(&mut self.open) {
                    self.done.insert(i, finish_block(b));
                }
                self.stopped = true;
                on_event(StreamEvent::Done);
            }
            "error" => return Err(classify_stream_error(&v)),
            // `ping` and future event types.
            _ => {}
        }
        Ok(())
    }
}

/// Decode a complete SSE transcript (tests, scripted replays).
pub fn decode_stream(bytes: &[u8], on_event: &mut dyn FnMut(StreamEvent)) -> Result<ChatResponse, LlmError> {
    let mut d = StreamDecoder::new();
    d.push(bytes, on_event)?;
    d.finish(on_event)
}

fn index_of(v: &Value) -> Result<u64, LlmError> {
    v.get("index").and_then(Value::as_u64).ok_or_else(|| LlmError::Decode("event without a block index".into()))
}

fn start_block(cb: Map<String, Value>) -> Block {
    let kind = cb.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        // Plain text (no citations or other extras) is interpreted; anything richer stays opaque.
        "text" if cb.keys().all(|k| k == "type" || k == "text" || (k == "citations" && cb.get(k).is_some_and(Value::is_null))) => {
            Block::Text(cb.get("text").and_then(Value::as_str).unwrap_or_default().to_string())
        }
        "tool_use" => Block::ToolUse {
            id: cb.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
            name: cb.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
            json: String::new(),
            initial: cb.get("input").cloned().unwrap_or_else(|| Value::Object(Map::new())),
        },
        _ => Block::Opaque { raw: cb, json: String::new() },
    }
}

fn apply_delta(block: &mut Block, delta: &Map<String, Value>, on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
    let kind = delta.get("type").and_then(Value::as_str).unwrap_or_default();
    let s = |k: &str| delta.get(k).and_then(Value::as_str).unwrap_or_default();
    match (kind, &mut *block) {
        ("text_delta", Block::Text(t)) => {
            push_capped(t, s("text"))?;
            on_event(StreamEvent::TextDelta(s("text").to_string()));
        }
        ("input_json_delta", Block::ToolUse { id, json, .. }) => {
            push_capped(json, s("partial_json"))?;
            on_event(StreamEvent::ToolInputDelta { id: id.clone(), partial_json: s("partial_json").to_string() });
        }
        ("input_json_delta", Block::Opaque { json, .. }) => push_capped(json, s("partial_json"))?,
        ("thinking_delta", Block::Opaque { raw, .. }) => {
            append_str(raw, "thinking", s("thinking"))?;
            if !s("thinking").is_empty() {
                on_event(StreamEvent::ThinkingDelta(s("thinking").to_string()));
            }
        }
        ("signature_delta", Block::Opaque { raw, .. }) => append_str(raw, "signature", s("signature"))?,
        (_, Block::Text(t)) => {
            // An unknown delta on a text block (citations…): keep the block opaque from now on.
            let mut raw = Map::new();
            raw.insert("type".into(), json!("text"));
            raw.insert("text".into(), Value::String(std::mem::take(t)));
            *block = Block::Opaque { raw, json: String::new() };
            return apply_delta(block, delta, on_event);
        }
        (_, Block::Opaque { raw, .. }) => {
            if kind == "text_delta" {
                on_event(StreamEvent::TextDelta(s("text").to_string()));
            }
            merge_unknown_delta(raw, delta)?;
        }
        (_, Block::ToolUse { .. }) => {}
    }
    Ok(())
}

/// Text accumulated in one block is capped like an SSE line.
fn push_capped(s: &mut String, more: &str) -> Result<(), LlmError> {
    if s.len().saturating_add(more.len()) > crate::sse::MAX_TOTAL {
        return Err(LlmError::TooLarge);
    }
    s.push_str(more);
    Ok(())
}

fn append_str(raw: &mut Map<String, Value>, key: &str, more: &str) -> Result<(), LlmError> {
    match raw.get_mut(key) {
        Some(Value::String(s)) => push_capped(s, more),
        _ => {
            raw.insert(key.into(), Value::String(more.to_string()));
            Ok(())
        }
    }
}

/// Fold an unrecognised delta into an opaque block: string fields are appended to the same key,
/// other values are pushed onto an array at the key (or its plural, `citation` → `citations`) or
/// replace it.
fn merge_unknown_delta(raw: &mut Map<String, Value>, delta: &Map<String, Value>) -> Result<(), LlmError> {
    for (k, v) in delta {
        if k == "type" {
            continue;
        }
        if let Value::String(more) = v {
            append_str(raw, k, more)?;
            continue;
        }
        let plural = format!("{k}s");
        if let Some(Value::Array(a)) = raw.get_mut(k) {
            a.push(v.clone());
        } else if let Some(Value::Array(a)) = raw.get_mut(&plural) {
            a.push(v.clone());
        } else if raw.get(&plural).is_some_and(Value::is_null) {
            raw.insert(plural, Value::Array(vec![v.clone()]));
        } else {
            raw.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}

fn finish_block(b: Block) -> ContentBlock {
    match b {
        Block::Text(text) => ContentBlock::Text { text },
        Block::ToolUse { id, name, json, initial } => {
            let input = if json.is_empty() { initial } else { parse_tool_input(&json) };
            ContentBlock::ToolUse { id, name, input }
        }
        Block::Opaque { mut raw, json } => {
            if !json.is_empty() {
                raw.insert("input".into(), parse_tool_input(&json));
            }
            ContentBlock::Opaque { raw: Value::Object(raw) }
        }
    }
}

fn stop_reason(r: &str, details: Option<&Value>) -> StopReason {
    match r {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => {
            let field = |k: &str| details.and_then(|d| d.get(k)).and_then(Value::as_str).map(str::to_string);
            StopReason::Refusal { category: field("category"), explanation: field("explanation") }
        }
        other => StopReason::Other(other.to_string()),
    }
}

fn merge_usage(u: &mut Usage, v: &Value) {
    let get = |k: &str| v.get(k).and_then(Value::as_u64);
    if let Some(n) = get("input_tokens") {
        u.input_tokens = n;
    }
    if let Some(n) = get("output_tokens") {
        u.output_tokens = n;
    }
    if let Some(n) = get("cache_read_input_tokens") {
        u.cache_read_input_tokens = n;
    }
    if let Some(n) = get("cache_creation_input_tokens") {
        u.cache_creation_input_tokens = n;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{Effort, SystemBlock, ToolSpec};

    fn request() -> ChatRequest {
        ChatRequest {
            system: vec![SystemBlock::new("You are FilmCraft's assistant."), SystemBlock::cached("Skills…"), SystemBlock::new("volatile")],
            messages: vec![
                Message::user_text("clean up this interview"),
                Message::assistant(vec![
                    ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "plan", "signature": "c2ln"}) },
                    ContentBlock::ToolUse { id: "t1".into(), name: "find_silences".into(), input: json!({"min_s": 0.4}) },
                ]),
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::ToolResult {
                            tool_use_id: "t1".into(),
                            content: vec![
                                ToolResultContent::Text { text: "3 silences".into() },
                                ToolResultContent::Image { media_type: "image/png".into(), data_base64: "iVBO".into() },
                            ],
                            is_error: false,
                        },
                        ContentBlock::tool_error("t2", "no such clip"),
                    ],
                },
                Message::system_text("The playhead is at 00:01:02:03."),
            ],
            tools: vec![
                ToolSpec {
                    name: "find_silences".into(),
                    description: "Find silences".into(),
                    input_schema: json!({"type": "object", "properties": {"min_s": {"type": "number"}}, "required": ["min_s"], "additionalProperties": false}),
                    strict: true,
                    eager_input_streaming: false,
                },
                ToolSpec {
                    name: "propose_edit_plan".into(),
                    description: "Plan".into(),
                    input_schema: json!({"type": "object"}),
                    strict: true,
                    eager_input_streaming: true,
                },
            ],
            effort: Some(Effort::High),
            thinking_display: Some("summarized".into()),
            cache_last_user: true,
            extra_betas: vec!["compact-2026-01-12".into(), FALLBACK_BETA.into()],
            ..ChatRequest::default()
        }
    }

    #[test]
    fn encodes_the_messages_api_body() {
        let b = encode_request(&request());
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["max_tokens"], 64000);
        assert_eq!(b["stream"], true);
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert!(b["thinking"].get("budget_tokens").is_none());
        assert_eq!(b["output_config"], json!({"effort": "high"}));
        assert_eq!(b["tool_choice"], json!({"type": "auto"}));
        assert_eq!(b["fallbacks"], "default");
        // Tools carry `strict` / `eager_input_streaming` only when set.
        assert_eq!(b["tools"][0]["strict"], true);
        assert!(b["tools"][0].get("eager_input_streaming").is_none());
        assert_eq!(b["tools"][1]["eager_input_streaming"], true);
        // Cache breakpoints: the last cached system block and the last user message.
        assert!(b["system"][0].get("cache_control").is_none());
        assert_eq!(b["system"][1]["cache_control"], json!({"type": "ephemeral"}));
        assert!(b["system"][2].get("cache_control").is_none());
        let msgs = b["messages"].as_array().unwrap();
        assert!(msgs[0]["content"][0].get("cache_control").is_none());
        assert_eq!(msgs[2]["content"][1]["cache_control"], json!({"type": "ephemeral"}));
        assert!(msgs[2]["content"][0].get("cache_control").is_none());
        // Opaque blocks verbatim; tool results with images and is_error.
        assert_eq!(msgs[1]["content"][0], json!({"type": "thinking", "thinking": "plan", "signature": "c2ln"}));
        assert_eq!(msgs[2]["content"][0]["content"][1]["source"], json!({"type": "base64", "media_type": "image/png", "data": "iVBO"}));
        assert!(msgs[2]["content"][0].get("is_error").is_none());
        assert_eq!(msgs[2]["content"][1]["is_error"], true);
        assert_eq!(msgs[3], json!({"role": "system", "content": "The playhead is at 00:01:02:03."}));
        let n = serde_json::to_string(&b).unwrap().matches("cache_control").count();
        assert!(n <= MAX_CACHE_BREAKPOINTS);
    }

    #[test]
    fn encoding_is_byte_deterministic() {
        let r = request();
        let a = encode_request_bytes(&r).unwrap();
        let b = encode_request_bytes(&r.clone()).unwrap();
        assert_eq!(a, b);
        // Also across a serde round trip of the request (as when a conversation is reloaded).
        let back: ChatRequest = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(encode_request_bytes(&back).unwrap(), a);
    }

    #[test]
    fn haiku_has_no_fallbacks_and_minimal_request_is_minimal() {
        let r = ChatRequest { model: "claude-haiku-5-5".into(), messages: vec![Message::user_text("hi")], ..ChatRequest::default() };
        let b = encode_request(&r);
        assert!(b.get("fallbacks").is_none());
        assert!(b.get("tools").is_none() && b.get("tool_choice").is_none() && b.get("system").is_none() && b.get("output_config").is_none());
        assert_eq!(b["thinking"], json!({"type": "adaptive"}));
        assert_eq!(headers(&r), vec![("anthropic-version", API_VERSION.to_string())]);
    }

    #[test]
    fn headers_list_betas_once() {
        let mut r = request();
        assert_eq!(
            headers(&r),
            vec![("anthropic-version", "2023-06-01".to_string()), ("anthropic-beta", "server-side-fallback-2026-07-01,compact-2026-01-12".to_string())]
        );
        r.thinking_display = Some("updates".into());
        assert!(headers(&r)[1].1.contains(THINKING_UPDATES_BETA));
    }

    pub(crate) const TRANSCRIPT: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-opus-5-5\",\"stop_reason\":null,\"usage\":{\"input_tokens\":120,\"output_tokens\":1,\"cache_read_input_tokens\":900,\"cache_creation_input_tokens\":30}}}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Find the \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"silences first.\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqQBCkYIARgCIkD/é+==\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque-bytes\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"Looking for \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"silences… 🎬\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"find_silences\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"min_s\\\": \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"0.4}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":4,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"read_transcript\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":4,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"page\\\": \"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":4}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":5,\"content_block\":{\"type\":\"compaction\",\"content\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":5,\"delta\":{\"type\":\"compaction_delta\",\"content\":\"Summary of \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":5,\"delta\":{\"type\":\"compaction_delta\",\"content\":\"earlier turns.\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":5}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null,\"stop_details\":null},\"usage\":{\"output_tokens\":87}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );

    fn decode(bytes: &[u8]) -> (Result<ChatResponse, LlmError>, Vec<StreamEvent>) {
        let mut evs = Vec::new();
        let r = decode_stream(bytes, &mut |e| evs.push(e));
        (r, evs)
    }

    #[test]
    fn decodes_a_full_transcript() {
        let (r, evs) = decode(TRANSCRIPT.as_bytes());
        let r = r.unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.usage, Usage { input_tokens: 120, output_tokens: 87, cache_read_input_tokens: 900, cache_creation_input_tokens: 30 });
        assert_eq!(r.content.len(), 6);
        // Thinking reassembled exactly: type, text, signature.
        assert_eq!(
            r.content[0],
            ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "Find the silences first.", "signature": "EqQBCkYIARgCIkD/é+=="}) }
        );
        assert_eq!(r.content[1], ContentBlock::Opaque { raw: json!({"type": "redacted_thinking", "data": "opaque-bytes"}) });
        assert_eq!(r.content[2], ContentBlock::text("Looking for silences… 🎬"));
        assert_eq!(r.content[3], ContentBlock::ToolUse { id: "toolu_1".into(), name: "find_silences".into(), input: json!({"min_s": 0.4}) });
        assert_eq!(
            r.content[4],
            ContentBlock::ToolUse { id: "toolu_2".into(), name: "read_transcript".into(), input: json!({"__invalid_json": "{\"page\": "}) }
        );
        assert_eq!(r.content[5], ContentBlock::Opaque { raw: json!({"type": "compaction", "content": "Summary of earlier turns."}) });
        assert!(evs.contains(&StreamEvent::ThinkingDelta("Find the ".into())));
        assert!(evs.contains(&StreamEvent::TextDelta("silences… 🎬".into())));
        assert!(evs.contains(&StreamEvent::ToolUseStarted { id: "toolu_1".into(), name: "find_silences".into() }));
        assert!(evs.contains(&StreamEvent::ToolInputDelta { id: "toolu_1".into(), partial_json: "0.4}".into() }));
        assert!(evs.contains(&StreamEvent::BlockStop { index: 5 }));
        assert_eq!(evs.last(), Some(&StreamEvent::Done));
        // The assembled response goes back to the API unchanged (append-only history).
        let req = ChatRequest { messages: vec![Message::user_text("go"), r.to_message()], ..ChatRequest::default() };
        let body = encode_request(&req);
        assert_eq!(body["messages"][1]["content"][0], json!({"type": "thinking", "thinking": "Find the silences first.", "signature": "EqQBCkYIARgCIkD/é+=="}));
        assert_eq!(body["messages"][1]["content"][1], json!({"type": "redacted_thinking", "data": "opaque-bytes"}));
    }

    #[test]
    fn every_split_point_gives_the_same_result() {
        let bytes = TRANSCRIPT.as_bytes();
        let (whole, whole_evs) = decode(bytes);
        let whole = whole.unwrap();
        for cut in 0..=bytes.len() {
            let mut d = StreamDecoder::new();
            let mut evs = Vec::new();
            d.push(&bytes[..cut], &mut |e| evs.push(e)).unwrap();
            d.push(&bytes[cut..], &mut |e| evs.push(e)).unwrap();
            assert_eq!(d.finish(&mut |e| evs.push(e)).unwrap(), whole, "cut at {cut}");
            assert_eq!(evs, whole_evs, "cut at {cut}");
        }
        // CRLF line endings and one-byte chunks.
        let crlf = TRANSCRIPT.replace('\n', "\r\n");
        let mut d = StreamDecoder::new();
        for b in crlf.as_bytes().chunks(1) {
            d.push(b, &mut |_| {}).unwrap();
        }
        assert_eq!(d.finish(&mut |_| {}).unwrap(), whole);
    }

    #[test]
    fn refusal_errors_and_truncation() {
        let refusal = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\",\"explanation\":\"Declined.\"}},\"usage\":{\"output_tokens\":0}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let r = decode(refusal.as_bytes()).0.unwrap();
        assert_eq!(r.stop_reason, StopReason::Refusal { category: Some("cyber".into()), explanation: Some("Declined.".into()) });
        assert!(r.content.is_empty());

        let err = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert_eq!(decode(err.as_bytes()).0, Err(LlmError::Overloaded));

        let cut = &TRANSCRIPT[..TRANSCRIPT.find("event: message_delta").unwrap()];
        assert!(matches!(decode(cut.as_bytes()).0, Err(LlmError::Network(_))));

        let stop = |r: &str| {
            decode(format!("data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{r}\"}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n").as_bytes())
                .0
                .unwrap()
                .stop_reason
        };
        assert_eq!(stop("max_tokens"), StopReason::MaxTokens);
        assert_eq!(stop("pause_turn"), StopReason::PauseTurn);
        assert_eq!(stop("end_turn"), StopReason::EndTurn);
        assert_eq!(stop("brand_new"), StopReason::Other("brand_new".into()));
    }

    #[test]
    fn unknown_deltas_and_citations_stay_opaque() {
        let s = concat!(
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Quoted\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"cited_text\":\"x\"}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" text\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"s1\",\"name\":\"web_search\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (r, evs) = decode(s.as_bytes());
        let r = r.unwrap();
        assert_eq!(r.content[0], ContentBlock::Opaque { raw: json!({"type": "text", "text": "Quoted text", "citation": {"cited_text": "x"}}) });
        assert_eq!(r.content[1], ContentBlock::Opaque { raw: json!({"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"q": 1}}) });
        assert!(evs.contains(&StreamEvent::TextDelta(" text".into())));
    }

    #[test]
    fn protocol_violations_are_errors() {
        let bad = [
            "data: {not json}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":9,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"content_block\":{}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        ];
        for b in bad {
            assert!(matches!(decode(b.as_bytes()).0, Err(LlmError::Decode(_))), "{b}");
        }
    }

    /// A tiny deterministic PRNG (xorshift) so the fuzz test needs no dependency.
    pub(crate) struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[test]
    fn mutation_fuzz_never_panics() {
        let base = TRANSCRIPT.as_bytes();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..3000 {
            let mut b = base.to_vec();
            match round % 4 {
                0 => b.truncate(rng.below(b.len())),
                1 => {
                    for _ in 0..1 + rng.below(8) {
                        let i = rng.below(b.len());
                        b[i] ^= 1 << rng.below(8);
                    }
                }
                2 => {
                    let i = rng.below(b.len());
                    let junk: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
                    b.splice(i..i, junk);
                }
                _ => {
                    let i = rng.below(b.len());
                    let j = (i + rng.below(200)).min(b.len());
                    b.drain(i..j);
                }
            }
            let chunk = 1 + rng.below(64);
            let res = std::panic::catch_unwind(|| {
                let mut d = StreamDecoder::new();
                for c in b.chunks(chunk) {
                    if d.push(c, &mut |_| {}).is_err() {
                        return;
                    }
                }
                let _ = d.finish(&mut |_| {});
            });
            assert!(res.is_ok(), "panic in round {round}");
        }
        // A giant line is rejected, not buffered forever.
        let res = std::panic::catch_unwind(|| {
            let mut d = StreamDecoder::new();
            let mut line = b"data: {\"type\":\"ping\",\"x\":\"".to_vec();
            line.extend(vec![b'a'; crate::sse::MAX_LINE + 1]);
            d.push(&line, &mut |_| {})
        });
        assert_eq!(res.unwrap(), Err(LlmError::TooLarge));
    }
}
