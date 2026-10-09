//! Provider-neutral chat types.
//!
//! These are FilmCraft's own serde shapes (used for conversation logs and tests). The codecs in
//! [`crate::anthropic`] and [`crate::openai`] translate them to and from each provider's wire
//! format explicitly, so a provider API change never changes what is stored on disk.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// A mid-conversation operator instruction (volatile context placed after the cached prefix).
    System,
}

/// One turn of the conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A user message holding one text block.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::text(text)] }
    }

    /// A mid-conversation system message holding one text block.
    pub fn system_text(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: vec![ContentBlock::text(text)] }
    }

    /// An assistant message with the given blocks (normally a response's content, unchanged).
    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self { role: Role::Assistant, content }
    }

    /// The concatenated text blocks.
    pub fn text(&self) -> String {
        join_text(&self.content)
    }
}

/// A content block of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// A base64-encoded image (`image/png`, `image/jpeg`, `image/webp`, `image/gif`).
    Image {
        media_type: String,
        data_base64: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    /// A provider block FilmCraft does not interpret (thinking, redacted thinking, compaction,
    /// server tool blocks, anything new). It is kept exactly as the provider sent it and sent back
    /// verbatim, so history stays append-only and thinking signatures stay valid.
    Opaque {
        raw: Value,
    },
}

impl ContentBlock {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// A successful text tool result.
    pub fn tool_result(tool_use_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::ToolResult { tool_use_id: tool_use_id.into(), content: vec![ToolResultContent::Text { text: text.into() }], is_error: false }
    }

    /// A failed tool result (`is_error: true`), so the model can correct itself.
    pub fn tool_error(tool_use_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::ToolResult { tool_use_id: tool_use_id.into(), content: vec![ToolResultContent::Text { text: text.into() }], is_error: true }
    }

    /// The provider block type of an opaque block (`"thinking"`, `"compaction"`…).
    pub fn opaque_type(&self) -> Option<&str> {
        match self {
            Self::Opaque { raw } => raw.get("type").and_then(Value::as_str),
            _ => None,
        }
    }
}

/// Content of a tool result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text { text: String },
    Image { media_type: String, data_base64: String },
}

/// A tool the model may call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the input object.
    pub input_schema: Value,
    /// Ask the provider to guarantee schema-valid input (needs `additionalProperties: false`).
    #[serde(default)]
    pub strict: bool,
    /// Stream the input as it is generated (the client then owns validation).
    #[serde(default)]
    pub eager_input_streaming: bool,
}

/// A block of the top-level system prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    /// Place a prompt-cache breakpoint after this block (only the last such block gets one).
    #[serde(default)]
    pub cache: bool,
}

impl SystemBlock {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: false }
    }

    pub fn cached(text: impl Into<String>) -> Self {
        Self { text: text.into(), cache: true }
    }
}

/// How much effort (thinking depth and token spend) the model puts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// One model call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default)]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    #[serde(default)]
    pub effort: Option<Effort>,
    /// Thinking display mode (`"summarized"`, `"omitted"`, `"updates"`); `None` = provider default.
    #[serde(default)]
    pub thinking_display: Option<String>,
    /// Put a rolling prompt-cache breakpoint on the last user message.
    #[serde(default)]
    pub cache_last_user: bool,
    /// Extra `anthropic-beta` values.
    #[serde(default)]
    pub extra_betas: Vec<String>,
}

/// The default `max_tokens` (streaming, so no HTTP timeout concern).
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

impl Default for ChatRequest {
    fn default() -> Self {
        Self {
            model: crate::DEFAULT_MODEL.to_string(),
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: DEFAULT_MAX_TOKENS,
            effort: None,
            thinking_display: None,
            cache_last_user: false,
            extra_betas: Vec::new(),
        }
    }
}

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    /// The response holds tool calls to run.
    ToolUse,
    /// Cut off by `max_tokens`: a trailing tool call may be incomplete and must not run.
    MaxTokens,
    /// Declined; show the explanation and run nothing.
    Refusal {
        category: Option<String>,
        explanation: Option<String>,
    },
    /// A long server-side turn paused; send the conversation again to continue.
    PauseTurn,
    Other(String),
}

/// Token usage of one call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// Add another call's usage (saturating).
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(o.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(o.output_tokens);
        self.cache_read_input_tokens = self.cache_read_input_tokens.saturating_add(o.cache_read_input_tokens);
        self.cache_creation_input_tokens = self.cache_creation_input_tokens.saturating_add(o.cache_creation_input_tokens);
    }

    /// Every prompt token: uncached + cache reads + cache writes.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.cache_read_input_tokens).saturating_add(self.cache_creation_input_tokens)
    }
}

/// A finished model call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
    /// The model that actually answered (may differ from the request after a server-side fallback).
    #[serde(default)]
    pub model: Option<String>,
}

impl ChatResponse {
    /// A plain text answer that ended the turn (handy for tests).
    pub fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], stop_reason: StopReason::EndTurn, usage: Usage::default(), model: None }
    }

    /// The concatenated text blocks.
    pub fn joined_text(&self) -> String {
        join_text(&self.content)
    }

    /// The tool calls, in order: `(id, name, input)`.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    /// The assistant message to append to the history (content unchanged).
    pub fn to_message(&self) -> Message {
        Message::assistant(self.content.clone())
    }
}

/// Streaming progress for the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StreamEvent {
    TextDelta(String),
    /// Thinking summary or progress-note text.
    ThinkingDelta(String),
    ToolUseStarted {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        partial_json: String,
    },
    /// Content block `index` is complete.
    BlockStop {
        index: usize,
    },
    /// Usage so far.
    Usage(Usage),
    /// The response is complete.
    Done,
}

/// The marker key of a tool input that was not valid JSON (`{"__invalid_json": "<raw>"}`).
pub const INVALID_JSON_KEY: &str = "__invalid_json";

/// Parse streamed tool-input JSON; empty input is `{}`, invalid or truncated input becomes
/// `{"__invalid_json": "<raw>"}` so the agent can answer with an `is_error` tool result.
pub fn parse_tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        Err(_) => {
            let mut m = serde_json::Map::new();
            m.insert(INVALID_JSON_KEY.into(), Value::String(raw.to_string()));
            Value::Object(m)
        }
    }
}

/// True for a tool input produced by [`parse_tool_input`] from invalid JSON.
pub fn is_invalid_tool_input(input: &Value) -> bool {
    input.get(INVALID_JSON_KEY).is_some()
}

fn join_text(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        if let ContentBlock::Text { text } = b {
            s.push_str(text);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_blocks_round_trip_through_serde() {
        let blocks = vec![
            ContentBlock::text("hi"),
            ContentBlock::Image { media_type: "image/png".into(), data_base64: "AAAA".into() },
            ContentBlock::ToolUse { id: "t1".into(), name: "find_silences".into(), input: json!({"min_s": 0.5}) },
            ContentBlock::tool_error("t1", "bad"),
            ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "hmm", "signature": "sig=="}) },
        ];
        let s = serde_json::to_string(&blocks).unwrap();
        let back: Vec<ContentBlock> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, blocks);
        assert_eq!(back[4].opaque_type(), Some("thinking"));
    }

    #[test]
    fn tool_input_parsing_never_fails() {
        assert_eq!(parse_tool_input(""), json!({}));
        assert_eq!(parse_tool_input("{\"a\":1}"), json!({"a": 1}));
        let bad = parse_tool_input("{\"a\":");
        assert!(is_invalid_tool_input(&bad));
        assert_eq!(bad[INVALID_JSON_KEY], "{\"a\":");
    }

    #[test]
    fn usage_adds_saturating() {
        let mut u = Usage { input_tokens: u64::MAX, output_tokens: 1, ..Default::default() };
        u.add(&Usage { input_tokens: 5, output_tokens: 2, cache_read_input_tokens: 3, cache_creation_input_tokens: 4 });
        assert_eq!(u.input_tokens, u64::MAX);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.prompt_tokens(), u64::MAX);
    }
}
