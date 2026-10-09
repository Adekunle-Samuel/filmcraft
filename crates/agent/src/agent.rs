//! The agent loop: one user turn = model calls and tool calls until the model ends its turn, a
//! limit is hit, the user cancels or something fails.
//!
//! History rules (they keep prompt caching and thinking-block replay valid):
//!
//! - The conversation is **append-only**. Responses are stored exactly as received (thinking and
//!   other provider blocks included, as [`ContentBlock::Opaque`]); nothing earlier is ever edited
//!   or pruned.
//! - Every tool call the model makes gets exactly one result, and all results of one response go
//!   back in **one** user message (`is_error` for failures, denials and invalid input).
//! - When a turn stops with calls unanswered (cancel, refusal, a limit), their results are kept
//!   as *pending* and open the next user message, so the history stays valid.
//!
//! Tool results are capped ([`AgentConfig::max_result_chars`] of JSON, at most
//! [`AgentConfig::max_images`] images per model call) so one call can't flood the context.

use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_llm::{ChatRequest, ContentBlock, Effort, LlmError, LlmProvider, Message, Role, StopReason, StreamEvent, ToolResultContent, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::{Authorization, ToolCall, ToolHost, ToolProgress};

/// Limits and model settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentConfig {
    pub model: String,
    pub effort: Option<Effort>,
    pub max_tokens: u32,
    /// Thinking display (`"summarized"` shows a readable summary while the model works).
    pub thinking_display: Option<String>,
    /// Most model calls in one user turn.
    pub max_model_calls: u32,
    /// Most tool calls in one user turn.
    pub max_tool_calls: u32,
    /// Stop the turn once the conversation's estimated cost reaches this (US$).
    pub budget_usd: Option<f64>,
    /// Longest JSON text of one tool result (characters).
    pub max_result_chars: usize,
    /// Most images sent back per model call.
    pub max_images: usize,
    /// Send images at all (vision).
    pub vision: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: filmcraft_llm::DEFAULT_MODEL.into(),
            effort: Some(Effort::High),
            max_tokens: 32_000,
            thinking_display: Some("summarized".into()),
            max_model_calls: 25,
            max_tool_calls: 60,
            budget_usd: None,
            max_result_chars: 16_000,
            max_images: 6,
            vision: true,
        }
    }
}

/// Why a turn ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnEnd {
    /// The model finished.
    Done,
    Cancelled,
    /// The model declined.
    Refused {
        category: Option<String>,
        explanation: Option<String>,
    },
    /// A limit was reached (calls, tools, budget).
    Limit(String),
    /// The provider failed.
    Failed(String),
}

/// What the agent reports while it works (for the chat UI, the CLI and logs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentEvent {
    /// Streaming output of the current model call.
    Stream(StreamEvent),
    /// Model call `n` (1-based) of this turn starts.
    ModelCall {
        n: u32,
    },
    ToolStarted {
        id: String,
        name: String,
        input: Value,
    },
    ToolProgress {
        id: String,
        progress: ToolProgress,
    },
    ToolFinished {
        id: String,
        name: String,
        ok: bool,
        summary: String,
        images: usize,
    },
    /// The host refused a call (the user declined, or policy).
    ToolDenied {
        id: String,
        name: String,
        reason: String,
    },
    /// Totals so far.
    Usage {
        total: Usage,
        cost_usd: Option<f64>,
    },
    /// Something the user should know (a limit, a truncated result).
    Notice(String),
    TurnEnded(TurnEnd),
}

/// The conversation: messages plus totals. Serializable, so it can be saved and resumed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Conversation {
    pub messages: Vec<Message>,
    /// Tool results owed to the last assistant message (see the module docs).
    pub pending_results: Vec<ContentBlock>,
    pub usage: Usage,
    /// Estimated cost so far; `None` when a model's price is unknown.
    pub cost_usd: Option<f64>,
}

impl Conversation {
    /// Append a mid-conversation system note (volatile context: "the user edited sequence X").
    /// It is sent after the cached prefix and never edits earlier history.
    pub fn push_note(&mut self, text: impl Into<String>) {
        self.messages.push(Message::system_text(text));
    }
}

/// Run one user turn. `user` is the user's message content (text, images). Returns how the turn
/// ended; the same value is also sent as [`AgentEvent::TurnEnded`].
pub fn run_turn(
    cfg: &AgentConfig,
    conv: &mut Conversation,
    provider: &dyn LlmProvider,
    host: &mut dyn ToolHost,
    user: Vec<ContentBlock>,
    on_event: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> TurnEnd {
    let end = turn(cfg, conv, provider, host, user, on_event, cancel);
    on_event(AgentEvent::TurnEnded(end.clone()));
    end
}

fn turn(
    cfg: &AgentConfig,
    conv: &mut Conversation,
    provider: &dyn LlmProvider,
    host: &mut dyn ToolHost,
    user: Vec<ContentBlock>,
    on_event: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> TurnEnd {
    let mut first = std::mem::take(&mut conv.pending_results);
    first.extend(user);
    if first.is_empty() {
        return TurnEnd::Done;
    }
    conv.messages.push(Message { role: Role::User, content: first });

    let caps = provider.capabilities();
    let tools = if caps.tools { host.tools() } else { Vec::new() };
    let system = crate::skills::system_blocks();
    let mut tool_calls: u32 = 0;
    for n in 1..=cfg.max_model_calls.max(1) {
        if cancel.load(Ordering::Relaxed) {
            return TurnEnd::Cancelled;
        }
        if let (Some(b), Some(c)) = (cfg.budget_usd, conv.cost_usd)
            && c >= b
        {
            return TurnEnd::Limit(format!("the conversation reached its budget (${b:.2}); raise it in Assistant settings to continue"));
        }
        on_event(AgentEvent::ModelCall { n });
        let req = ChatRequest {
            model: cfg.model.clone(),
            system: system.clone(),
            messages: conv.messages.clone(),
            tools: tools.clone(),
            max_tokens: cfg.max_tokens.max(1024),
            effort: cfg.effort,
            thinking_display: if caps.thinking { cfg.thinking_display.clone() } else { None },
            cache_last_user: caps.caching,
            extra_betas: Vec::new(),
        };
        let resp = match provider.send(&req, &mut |e| on_event(AgentEvent::Stream(e)), cancel) {
            Ok(r) => r,
            Err(LlmError::Cancelled) => return TurnEnd::Cancelled,
            Err(e) => return TurnEnd::Failed(e.to_string()),
        };
        conv.usage.add(&resp.usage);
        let model = resp.model.clone().unwrap_or_else(|| cfg.model.clone());
        conv.cost_usd = match (conv.cost_usd, filmcraft_llm::estimate_cost_usd(&model, &resp.usage)) {
            (Some(a), Some(b)) => Some(a + b),
            (None, Some(b)) if conv.messages.iter().filter(|m| m.role == Role::Assistant).count() == 0 => Some(b),
            _ => None,
        };
        on_event(AgentEvent::Usage { total: conv.usage, cost_usd: conv.cost_usd });

        let calls: Vec<ToolCall> = resp.tool_uses().map(|(id, name, input)| ToolCall { id: id.into(), name: name.into(), input: input.clone() }).collect();
        if !resp.content.is_empty() {
            conv.messages.push(resp.to_message());
        }
        match resp.stop_reason {
            StopReason::ToolUse => {}
            StopReason::PauseTurn => continue,
            StopReason::MaxTokens if !calls.is_empty() => {
                // a trailing call may be truncated: run none, ask for smaller input
                let results = calls.iter().map(|c| ContentBlock::tool_error(&c.id, "not run: your response was cut off by max_tokens, so this call may be incomplete. Send a smaller input (fewer cuts per plan, or page through).")).collect();
                conv.messages.push(Message { role: Role::User, content: results });
                continue;
            }
            StopReason::Refusal { category, explanation } => {
                conv.pending_results = owed(&calls, "not run: the model declined this turn");
                return TurnEnd::Refused { category, explanation };
            }
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::Other(_) => {
                if !calls.is_empty() {
                    conv.pending_results = owed(&calls, "not run: the turn ended");
                }
                return TurnEnd::Done;
            }
        }
        if calls.is_empty() {
            return TurnEnd::Done;
        }

        // run the calls; every call gets exactly one result, all in one message
        let mut results: Vec<ContentBlock> = Vec::with_capacity(calls.len());
        let mut images_left = if cfg.vision && caps.vision { cfg.max_images } else { 0 };
        for (k, c) in calls.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                results.extend(owed(calls.get(k..).unwrap_or(&[]), "not run: cancelled by the user"));
                conv.pending_results = results;
                return TurnEnd::Cancelled;
            }
            if tool_calls >= cfg.max_tool_calls {
                results.extend(owed(calls.get(k..).unwrap_or(&[]), "not run: too many tool calls in one turn"));
                conv.pending_results = results;
                return TurnEnd::Limit(format!("stopped after {} tool calls in one turn; say \"continue\" to go on", cfg.max_tool_calls));
            }
            tool_calls += 1;
            results.push(run_call(cfg, host, c, &mut images_left, on_event, cancel));
        }
        conv.messages.push(Message { role: Role::User, content: results });
    }
    TurnEnd::Limit(format!("stopped after {} model calls in one turn; say \"continue\" to go on", cfg.max_model_calls.max(1)))
}

/// Error results for calls that won't run.
fn owed(calls: &[ToolCall], why: &str) -> Vec<ContentBlock> {
    calls.iter().map(|c| ContentBlock::tool_error(&c.id, why)).collect()
}

fn run_call(
    cfg: &AgentConfig,
    host: &mut dyn ToolHost,
    c: &ToolCall,
    images_left: &mut usize,
    on_event: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> ContentBlock {
    if filmcraft_llm::types::is_invalid_tool_input(&c.input) {
        return ContentBlock::tool_error(&c.id, "the tool input was not valid JSON; send it again as one JSON object");
    }
    if !c.input.is_object() {
        return ContentBlock::tool_error(&c.id, "the tool input must be a JSON object");
    }
    if let Authorization::Deny(reason) = host.authorize(c) {
        on_event(AgentEvent::ToolDenied { id: c.id.clone(), name: c.name.clone(), reason: reason.clone() });
        return ContentBlock::tool_error(&c.id, format!("not run: {reason}"));
    }
    on_event(AgentEvent::ToolStarted { id: c.id.clone(), name: c.name.clone(), input: c.input.clone() });
    let id = c.id.clone();
    let out = host.call(c, cancel, &mut |p| on_event(AgentEvent::ToolProgress { id: id.clone(), progress: p }));
    match out {
        Ok(o) => {
            let (text, truncated) = cap_json(&o.json, cfg.max_result_chars);
            let mut content = vec![ToolResultContent::Text { text: text.clone() }];
            let mut sent = 0;
            for img in o.images {
                if *images_left == 0 {
                    break;
                }
                *images_left -= 1;
                sent += 1;
                content.push(ToolResultContent::Image { media_type: img.media_type, data_base64: img.data_base64 });
            }
            if truncated {
                on_event(AgentEvent::Notice(format!("{} returned a long result; it was shortened", c.name)));
            }
            on_event(AgentEvent::ToolFinished { id: c.id.clone(), name: c.name.clone(), ok: true, summary: summary(&text), images: sent });
            ContentBlock::ToolResult { tool_use_id: c.id.clone(), content, is_error: false }
        }
        Err(e) => {
            let (e, _) = cap_str(&e, cfg.max_result_chars);
            on_event(AgentEvent::ToolFinished { id: c.id.clone(), name: c.name.clone(), ok: false, summary: summary(&e), images: 0 });
            ContentBlock::tool_error(&c.id, e)
        }
    }
}

/// The JSON as text, cut to `max` characters (at a char boundary) with a note.
fn cap_json(v: &Value, max: usize) -> (String, bool) {
    let s = v.to_string();
    cap_str(&s, max)
}

fn cap_str(s: &str, max: usize) -> (String, bool) {
    let max = max.max(256);
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let cut: String = s.chars().take(max).collect();
    (format!("{cut}… [truncated: the result was longer than {max} characters; ask for less, e.g. a page with offset/limit]"), true)
}

/// A one-line summary of a result for the UI.
fn summary(s: &str) -> String {
    let line: String = s.chars().take(160).collect();
    if s.chars().count() > 160 { format!("{line}…") } else { line }
}
