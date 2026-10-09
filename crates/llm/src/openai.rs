//! The OpenAI-compatible Chat Completions codec (`POST /v1/chat/completions`, streaming), for
//! local servers such as Ollama and LM Studio.
//!
//! Tools are sent as `function` tools and streamed `tool_calls` deltas are assembled by index;
//! images become `image_url` data URLs; the system prompt is the first message and tool results
//! are `tool` messages. There is no thinking or prompt caching: opaque blocks are dropped (they
//! belong to another provider). Reasoning text some servers stream (`reasoning_content`,
//! `reasoning`) is shown as thinking progress but not kept.

use crate::LlmError;
use crate::sse::SseParser;
use crate::types::{ChatRequest, ChatResponse, ContentBlock, Message, Role, StopReason, StreamEvent, ToolResultContent, Usage, parse_tool_input};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The default base URL (Ollama on this machine).
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";
/// The request path below the base URL.
pub const CHAT_PATH: &str = "/chat/completions";
/// Most tool calls accepted in one response.
pub const MAX_TOOL_CALLS: usize = 256;

fn data_url(media_type: &str, data: &str) -> Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:{media_type};base64,{data}")}})
}

/// The request body for `req`.
pub fn encode_request(req: &ChatRequest) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        let text = req.system.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n\n");
        messages.push(json!({"role": "system", "content": text}));
    }
    for m in &req.messages {
        encode_message(m, &mut messages);
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(req.max_tokens));
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert("messages".into(), Value::Array(messages));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut f = Map::new();
                f.insert("name".into(), json!(t.name));
                f.insert("description".into(), json!(t.description));
                f.insert("parameters".into(), t.input_schema.clone());
                if t.strict {
                    f.insert("strict".into(), json!(true));
                }
                json!({"type": "function", "function": Value::Object(f)})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!("auto"));
    }
    Value::Object(body)
}

/// The request body as bytes (deterministic).
pub fn encode_request_bytes(req: &ChatRequest) -> Result<Vec<u8>, LlmError> {
    serde_json::to_vec(&encode_request(req)).map_err(|e| LlmError::Decode(e.to_string()))
}

/// Plain string content when every part is text, else content parts.
fn content_value(parts: Vec<Value>) -> Value {
    if parts.iter().all(|p| p.get("type").and_then(Value::as_str) == Some("text")) {
        let text: Vec<&str> = parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect();
        Value::String(text.join("\n\n"))
    } else {
        Value::Array(parts)
    }
}

fn encode_message(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::System => {
            let text: Vec<&str> = m.content.iter().filter_map(|b| if let ContentBlock::Text { text } = b { Some(text.as_str()) } else { None }).collect();
            if !text.is_empty() {
                out.push(json!({"role": "system", "content": text.join("\n\n")}));
            }
        }
        Role::Assistant => {
            let mut text = String::new();
            let mut calls = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::ToolUse { id, name, input } => {
                        calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}));
                    }
                    _ => {}
                }
            }
            let mut o = Map::new();
            o.insert("role".into(), json!("assistant"));
            o.insert("content".into(), if text.is_empty() && !calls.is_empty() { Value::Null } else { Value::String(text) });
            if !calls.is_empty() {
                o.insert("tool_calls".into(), Value::Array(calls));
            }
            out.push(Value::Object(o));
        }
        Role::User => {
            // Tool results must directly follow the assistant's tool calls, so they go first.
            // `tool` messages carry text only: their images follow in the user message.
            let mut parts = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let mut text = String::new();
                        if *is_error {
                            text.push_str("Error: ");
                        }
                        for c in content {
                            match c {
                                ToolResultContent::Text { text: t } => text.push_str(t),
                                ToolResultContent::Image { media_type, data_base64 } => parts.push(data_url(media_type, data_base64)),
                            }
                        }
                        out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": text}));
                    }
                    ContentBlock::Text { text } => parts.push(json!({"type": "text", "text": text})),
                    ContentBlock::Image { media_type, data_base64 } => parts.push(data_url(media_type, data_base64)),
                    ContentBlock::ToolUse { .. } | ContentBlock::Opaque { .. } => {}
                }
            }
            if !parts.is_empty() {
                out.push(json!({"role": "user", "content": content_value(parts)}));
            }
        }
    }
}

#[derive(Debug, Default)]
struct Call {
    id: String,
    name: String,
    args: String,
    started: bool,
}

/// Incremental decoder of a streamed chat completion.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    sse: SseParser,
    text: String,
    calls: BTreeMap<u64, Call>,
    usage: Usage,
    finish: Option<StopReason>,
    model: Option<String>,
    done: bool,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw response bytes.
    pub fn push(&mut self, chunk: &[u8], on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
        if self.done {
            return Ok(());
        }
        for ev in self.sse.push(chunk)? {
            self.data(&ev.data, on_event)?;
            if self.done {
                break;
            }
        }
        Ok(())
    }

    /// True once `data: [DONE]` arrived.
    pub fn is_complete(&self) -> bool {
        self.done
    }

    /// End of the byte stream: the assembled response.
    pub fn finish(mut self, on_event: &mut dyn FnMut(StreamEvent)) -> Result<ChatResponse, LlmError> {
        if !self.done {
            for ev in self.sse.finish()? {
                self.data(&ev.data, on_event)?;
            }
        }
        // Servers that omit `[DONE]` still end with a finish reason.
        if !self.done && self.finish.is_none() {
            return Err(LlmError::Network("the response stream ended early".into()));
        }
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }
        for (i, c) in self.calls {
            let id = if c.id.is_empty() { format!("call_{i}") } else { c.id };
            content.push(ContentBlock::ToolUse { id, name: c.name, input: parse_tool_input(&c.args) });
        }
        let has_calls = content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        let stop_reason = match self.finish {
            // Some servers report `stop` even when they called tools.
            Some(StopReason::EndTurn) | None if has_calls => StopReason::ToolUse,
            Some(r) => r,
            None => StopReason::EndTurn,
        };
        on_event(StreamEvent::Done);
        Ok(ChatResponse { content, stop_reason, usage: self.usage, model: self.model })
    }

    fn data(&mut self, data: &str, on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| LlmError::Decode(format!("bad chunk JSON: {e}")))?;
        if let Some(err) = v.get("error") {
            let msg = err.get("message").and_then(Value::as_str).or_else(|| err.as_str()).unwrap_or("unknown error");
            return Err(LlmError::BadRequest(crate::error::truncate_chars(msg, 500)));
        }
        if self.model.is_none() {
            self.model = v.get("model").and_then(Value::as_str).map(str::to_string);
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let get = |k: &str| u.get(k).and_then(Value::as_u64);
            let cached = u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")).and_then(Value::as_u64).unwrap_or(0);
            if let Some(p) = get("prompt_tokens") {
                self.usage.input_tokens = p.saturating_sub(cached);
                self.usage.cache_read_input_tokens = cached;
            }
            if let Some(c) = get("completion_tokens") {
                self.usage.output_tokens = c;
            }
            on_event(StreamEvent::Usage(self.usage));
        }
        let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) else {
            return Ok(());
        };
        if let Some(delta) = choice.get("delta") {
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    on_event(StreamEvent::ThinkingDelta(t.to_string()));
                }
            }
            if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                if self.text.len().saturating_add(t.len()) > crate::sse::MAX_TOTAL {
                    return Err(LlmError::TooLarge);
                }
                self.text.push_str(t);
                on_event(StreamEvent::TextDelta(t.to_string()));
            }
            for tc in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                self.tool_call(tc, on_event)?;
            }
        }
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(match r {
                "stop" => StopReason::EndTurn,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal { category: Some("content_filter".into()), explanation: None },
                other => StopReason::Other(other.to_string()),
            });
        }
        Ok(())
    }

    fn tool_call(&mut self, tc: &Value, on_event: &mut dyn FnMut(StreamEvent)) -> Result<(), LlmError> {
        let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
        if !self.calls.contains_key(&index) && self.calls.len() >= MAX_TOOL_CALLS {
            return Err(LlmError::TooLarge);
        }
        let call = self.calls.entry(index).or_default();
        if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            call.id = id.to_string();
        }
        let f = tc.get("function");
        if let Some(name) = f.and_then(|f| f.get("name")).and_then(Value::as_str) {
            call.name.push_str(name);
        }
        if !call.started && !call.name.is_empty() {
            call.started = true;
            let id = if call.id.is_empty() { format!("call_{index}") } else { call.id.clone() };
            on_event(StreamEvent::ToolUseStarted { id, name: call.name.clone() });
        }
        if let Some(args) = f.and_then(|f| f.get("arguments")) {
            // Most servers stream a string; some send the whole object at once.
            let piece = match args {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            if call.args.len().saturating_add(piece.len()) > crate::sse::MAX_TOTAL {
                return Err(LlmError::TooLarge);
            }
            call.args.push_str(&piece);
            if !piece.is_empty() {
                let id = if call.id.is_empty() { format!("call_{index}") } else { call.id.clone() };
                on_event(StreamEvent::ToolInputDelta { id, partial_json: piece });
            }
        }
        Ok(())
    }
}

/// Decode a complete SSE transcript.
pub fn decode_stream(bytes: &[u8], on_event: &mut dyn FnMut(StreamEvent)) -> Result<ChatResponse, LlmError> {
    let mut d = StreamDecoder::new();
    d.push(bytes, on_event)?;
    d.finish(on_event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SystemBlock, ToolSpec};

    #[test]
    fn encodes_chat_completions() {
        let req = ChatRequest {
            model: "llama3.2".into(),
            system: vec![SystemBlock::new("Be brief."), SystemBlock::cached("Skills.")],
            messages: vec![
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::text("What is in this frame?"),
                        ContentBlock::Image { media_type: "image/jpeg".into(), data_base64: "/9j/".into() },
                    ],
                },
                Message::assistant(vec![
                    ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "x", "signature": "y"}) },
                    ContentBlock::ToolUse { id: "c1".into(), name: "contact_sheet".into(), input: json!({"n": 4}) },
                ]),
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::ToolResult {
                            tool_use_id: "c1".into(),
                            content: vec![
                                ToolResultContent::Text { text: "sheet".into() },
                                ToolResultContent::Image { media_type: "image/png".into(), data_base64: "iVBO".into() },
                            ],
                            is_error: false,
                        },
                        ContentBlock::tool_error("c2", "boom"),
                    ],
                },
                Message::system_text("Note."),
                Message::user_text("thanks"),
            ],
            tools: vec![ToolSpec {
                name: "contact_sheet".into(),
                description: "Frames".into(),
                input_schema: json!({"type": "object"}),
                strict: true,
                eager_input_streaming: true,
            }],
            ..ChatRequest::default()
        };
        let b = encode_request(&req);
        assert_eq!(b["stream"], true);
        assert!(b.get("thinking").is_none() && b.get("fallbacks").is_none());
        assert_eq!(
            b["tools"][0],
            json!({"type": "function", "function": {"name": "contact_sheet", "description": "Frames", "parameters": {"type": "object"}, "strict": true}})
        );
        assert_eq!(b["tool_choice"], "auto");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({"role": "system", "content": "Be brief.\n\nSkills."}));
        assert_eq!(m[1]["content"][1], json!({"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,/9j/"}}));
        assert_eq!(
            m[2],
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "contact_sheet", "arguments": "{\"n\":4}"}}]})
        );
        assert_eq!(m[3], json!({"role": "tool", "tool_call_id": "c1", "content": "sheet"}));
        assert_eq!(m[4], json!({"role": "tool", "tool_call_id": "c2", "content": "Error: boom"}));
        assert_eq!(m[5]["role"], "user");
        assert_eq!(m[5]["content"][0]["image_url"]["url"], "data:image/png;base64,iVBO");
        assert_eq!(m[6], json!({"role": "system", "content": "Note."}));
        assert_eq!(m[7], json!({"role": "user", "content": "thanks"}));
        assert_eq!(m.len(), 8);
        assert!(!serde_json::to_string(&b).unwrap().contains("signature"));
        assert_eq!(encode_request_bytes(&req).unwrap(), encode_request_bytes(&req.clone()).unwrap());
    }

    const TRANSCRIPT: &str = concat!(
        "data: {\"id\":\"c\",\"model\":\"llama3.2\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let me \"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"look.\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"find_silences\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"read_transcript\",\"arguments\":\"{\\\"page\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"min_s\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"0.5}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"2}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":20,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn decodes_streamed_tool_calls() {
        let mut evs = Vec::new();
        let r = decode_stream(TRANSCRIPT.as_bytes(), &mut |e| evs.push(e)).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.model.as_deref(), Some("llama3.2"));
        assert_eq!(r.usage, Usage { input_tokens: 40, output_tokens: 20, cache_read_input_tokens: 10, cache_creation_input_tokens: 0 });
        assert_eq!(
            r.content,
            vec![
                ContentBlock::text("Let me look."),
                ContentBlock::ToolUse { id: "call_a".into(), name: "find_silences".into(), input: json!({"min_s": 0.5}) },
                ContentBlock::ToolUse { id: "call_b".into(), name: "read_transcript".into(), input: json!({"page": 2}) },
            ]
        );
        assert!(evs.contains(&StreamEvent::ThinkingDelta("hmm".into())));
        assert!(evs.contains(&StreamEvent::ToolUseStarted { id: "call_b".into(), name: "read_transcript".into() }));
        assert_eq!(evs.last(), Some(&StreamEvent::Done));
        for cut in 0..=TRANSCRIPT.len() {
            let mut d = StreamDecoder::new();
            d.push(&TRANSCRIPT.as_bytes()[..cut], &mut |_| {}).unwrap();
            d.push(&TRANSCRIPT.as_bytes()[cut..], &mut |_| {}).unwrap();
            assert_eq!(d.finish(&mut |_| {}).unwrap(), r, "cut at {cut}");
        }
    }

    #[test]
    fn odd_servers_and_failures() {
        // No [DONE], `stop` with a tool call, id-less call with object arguments, bad JSON.
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"x\",\"arguments\":{\"a\":1}}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"name\":\"y\",\"arguments\":\"{oops\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let r = decode_stream(s.as_bytes(), &mut |_| {}).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(r.content[0], ContentBlock::ToolUse { id: "call_0".into(), name: "x".into(), input: json!({"a": 1}) });
        assert!(crate::types::is_invalid_tool_input(&r.tool_uses().nth(1).unwrap().2.clone()));

        let fin = |reason: &str| {
            decode_stream(format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"a\"}},\"finish_reason\":\"{reason}\"}}]}}\n\n").as_bytes(), &mut |_| {})
                .unwrap()
                .stop_reason
        };
        assert_eq!(fin("length"), StopReason::MaxTokens);
        assert_eq!(fin("stop"), StopReason::EndTurn);
        assert!(matches!(fin("content_filter"), StopReason::Refusal { .. }));

        assert!(matches!(decode_stream(b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n", &mut |_| {}), Err(LlmError::Network(_))));
        assert_eq!(decode_stream(b"data: {\"error\":{\"message\":\"model not found\"}}\n\n", &mut |_| {}), Err(LlmError::BadRequest("model not found".into())));
        assert!(matches!(decode_stream(b"data: {nope\n\n", &mut |_| {}), Err(LlmError::Decode(_))));
    }

    #[test]
    fn mutation_fuzz_never_panics() {
        let base = TRANSCRIPT.as_bytes();
        let mut rng = crate::anthropic::tests::Rng(0xDEAD_BEEF_CAFE_F00D);
        for round in 0..3000 {
            let mut b = base.to_vec();
            match round % 3 {
                0 => b.truncate(rng.below(b.len())),
                1 => {
                    for _ in 0..1 + rng.below(8) {
                        let i = rng.below(b.len());
                        b[i] ^= 1 << rng.below(8);
                    }
                }
                _ => {
                    let i = rng.below(b.len());
                    let junk: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
                    b.splice(i..i, junk);
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
        // Thousands of distinct tool-call indices are refused.
        let mut s = String::new();
        for i in 0..MAX_TOOL_CALLS + 1 {
            s.push_str(&format!("data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{i},\"function\":{{\"name\":\"t\"}}}}]}}}}]}}\n\n"));
        }
        assert_eq!(decode_stream(s.as_bytes(), &mut |_| {}), Err(LlmError::TooLarge));
    }
}
