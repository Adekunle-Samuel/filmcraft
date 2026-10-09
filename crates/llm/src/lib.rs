//! FilmCraft's LLM client layer (L5), used by the Assistant's agent loop.
//!
//! - [`types`]: provider-neutral messages and content blocks. Blocks FilmCraft does not interpret
//!   (thinking, redacted thinking, compaction…) are kept as [`ContentBlock::Opaque`] and sent back
//!   unchanged, so history stays append-only.
//! - [`LlmProvider`]: the blocking, streaming, cancellable provider trait.
//! - [`sse`]: an incremental, bounded Server-Sent Events parser.
//! - [`anthropic`]: the Claude Messages API codec (request body, headers, stream decoder).
//! - [`openai`]: the OpenAI-compatible Chat Completions codec (Ollama, LM Studio…).
//! - [`ScriptedProvider`]: a fake provider replaying canned responses, for tests.
//! - [`price`]: per-model prices and [`estimate_cost_usd`].
//! - [`transport`]: [`ApiKey`] (redacted `Debug`), the provider URL policy and the retry schedule.
//! - `http` (feature `http`): the blocking ureq + rustls transport, `AnthropicProvider` and `OpenAiCompatProvider`.
//!
//! Without `http` everything here is plain serde: no network, threads or clocks, so it builds for
//! wasm32.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod anthropic;
pub mod error;
#[cfg(feature = "http")]
pub mod http;
pub mod openai;
pub mod price;
pub mod scripted;
pub mod sse;
pub mod transport;
pub mod types;

use std::sync::atomic::AtomicBool;

pub use error::LlmError;
#[cfg(feature = "http")]
pub use http::{AnthropicProvider, OpenAiCompatProvider};
pub use price::estimate_cost_usd;
pub use scripted::{ScriptStep, ScriptedProvider};
pub use transport::ApiKey;
pub use types::{ChatRequest, ChatResponse, ContentBlock, Effort, Message, Role, StopReason, StreamEvent, SystemBlock, ToolResultContent, ToolSpec, Usage};

/// The default model.
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// What a provider supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    pub tools: bool,
    /// Image input.
    pub vision: bool,
    /// Thinking blocks (summaries, progress notes).
    pub thinking: bool,
    /// Prompt caching.
    pub caching: bool,
}

impl Capabilities {
    /// Everything supported.
    pub const fn all() -> Self {
        Self { tools: true, vision: true, thinking: true, caching: true }
    }
}

/// A chat model backend. `send` blocks (run it on a worker thread), streams progress through
/// `on_event`, checks `cancel` between reads, and returns the assembled response.
pub trait LlmProvider: Send + Sync {
    /// A short display name ("anthropic", "openai-compatible"…).
    fn name(&self) -> &str;

    /// Run one model call.
    fn send(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<ChatResponse, LlmError>;

    /// What this provider supports.
    fn capabilities(&self) -> Capabilities {
        Capabilities { tools: true, vision: true, thinking: true, caching: true }
    }
}
