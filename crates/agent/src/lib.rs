//! The FilmCraft Assistant's agent loop (layer L5).
//!
//! - [`run_turn`]: one user turn: model calls through a [`filmcraft_llm::LlmProvider`], tool calls
//!   through a [`ToolHost`], until the model ends its turn, a limit is hit or the user cancels.
//! - [`ToolHost`]: where tools run and who approves them (the desktop app forwards calls to the UI
//!   thread; the CLI and tests own an engine session).
//! - [`skills`]: the system prompt and the workflow skills (talking-head cleanup, style from a
//!   reference), compiled in from `crates/agent/skills/*.md`.
//!
//! The loop is blocking and has no threads, clocks or network of its own (the provider has those),
//! so it builds for wasm32 and runs the same in tests with a scripted provider.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod agent;
pub mod host;
pub mod skills;

pub use agent::{AgentConfig, AgentEvent, Conversation, TurnEnd, run_turn};
pub use host::{Authorization, ToolCall, ToolHost, ToolImage, ToolOutcome, ToolProgress};

#[cfg(test)]
mod tests;
