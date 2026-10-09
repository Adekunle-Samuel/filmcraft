//! [`ToolHost`]: where the agent's tool calls run.
//!
//! The agent loop never touches the project itself. A host owns (or reaches) the engine session:
//! the desktop app's host forwards calls to the UI thread between frames, the CLI and tests use a
//! host that owns a `Session`. The host also decides approvals, so the rule "destructive actions
//! need a click" is enforced outside the model, whatever it asks for.

use std::sync::atomic::AtomicBool;

use filmcraft_llm::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool call from the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The host's decision on a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authorization {
    Allow,
    /// Not run; the reason goes back to the model as an error result.
    Deny(String),
}

/// An image a tool returns (a frame, a contact sheet).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolImage {
    /// `image/png` or `image/jpeg`.
    pub media_type: String,
    pub data_base64: String,
}

/// What a tool returned.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub json: Value,
    pub images: Vec<ToolImage>,
}

impl ToolOutcome {
    pub fn json(json: Value) -> Self {
        Self { json, images: Vec::new() }
    }
}

/// Progress of a long tool call (a transcription or export job).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolProgress {
    /// 0…1 when known.
    pub fraction: Option<f32>,
    pub status: String,
}

/// Runs tool calls for the agent.
pub trait ToolHost {
    /// The tools offered to the model, in a stable order (the order is part of the prompt cache
    /// prefix, so it must not change between calls of one conversation).
    fn tools(&self) -> Vec<ToolSpec>;

    /// Decide whether `call` may run. Hosts ask the user here for calls that need approval
    /// (exports, overwrites, commands outside the curated tools); this may block.
    fn authorize(&mut self, call: &ToolCall) -> Authorization;

    /// Run an authorized call. Long calls report `progress` and stop early when `cancel` is set.
    /// An `Err` is shown to the model as an error result so it can correct itself.
    fn call(&mut self, call: &ToolCall, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String>;
}
