//! [`ScriptedProvider`]: a fake provider that replays canned responses, for tests of the agent
//! loop and the Assistant UI. It records every request so tests can assert that history is
//! append-only, and streams events like a real provider.

use crate::types::{ChatRequest, ChatResponse, ContentBlock, StreamEvent};
use crate::{Capabilities, LlmError, LlmProvider};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

/// One scripted reply.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptStep {
    /// Return this response, streaming its text, thinking and tool input as events first.
    Response(ChatResponse),
    /// Decode this Anthropic SSE transcript with the real decoder (events and all).
    AnthropicSse(String),
    /// Fail with this error.
    Error(LlmError),
}

/// Replays [`ScriptStep`]s in order; fails with [`LlmError::Unsupported`] once they run out.
#[derive(Debug)]
pub struct ScriptedProvider {
    name: String,
    steps: Mutex<VecDeque<ScriptStep>>,
    requests: Mutex<Vec<ChatRequest>>,
    capabilities: Capabilities,
}

impl ScriptedProvider {
    /// Replay these responses.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self::from_steps(responses.into_iter().map(ScriptStep::Response).collect())
    }

    /// Replay these steps.
    pub fn from_steps(steps: Vec<ScriptStep>) -> Self {
        Self { name: "scripted".into(), steps: Mutex::new(steps.into()), requests: Mutex::new(Vec::new()), capabilities: Capabilities::all() }
    }

    /// Report these capabilities instead of all of them.
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Steps not yet replayed.
    pub fn remaining(&self) -> usize {
        self.steps.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

/// Text is streamed in pieces of at most this many characters.
const TEXT_PIECE: usize = 8;

fn stream_response(r: &ChatResponse, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<(), LlmError> {
    on_event(StreamEvent::Usage(r.usage));
    for (index, b) in r.content.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }
        match b {
            ContentBlock::Text { text } => {
                let chars: Vec<char> = text.chars().collect();
                for piece in chars.chunks(TEXT_PIECE) {
                    on_event(StreamEvent::TextDelta(piece.iter().collect()));
                }
            }
            ContentBlock::ToolUse { id, name, input } => {
                on_event(StreamEvent::ToolUseStarted { id: id.clone(), name: name.clone() });
                on_event(StreamEvent::ToolInputDelta { id: id.clone(), partial_json: input.to_string() });
            }
            ContentBlock::Opaque { raw } => {
                if let Some(t) = raw.get("thinking").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                    on_event(StreamEvent::ThinkingDelta(t.to_string()));
                }
            }
            ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {}
        }
        on_event(StreamEvent::BlockStop { index });
    }
    on_event(StreamEvent::Usage(r.usage));
    on_event(StreamEvent::Done);
    Ok(())
}

impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn send(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<ChatResponse, LlmError> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).push(req.clone());
        if cancel.load(Ordering::Relaxed) {
            return Err(LlmError::Cancelled);
        }
        let step = self.steps.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        match step {
            Some(ScriptStep::Response(r)) => {
                stream_response(&r, on_event, cancel)?;
                Ok(r)
            }
            Some(ScriptStep::AnthropicSse(s)) => crate::anthropic::decode_stream(s.as_bytes(), on_event),
            Some(ScriptStep::Error(e)) => Err(e),
            None => Err(LlmError::Unsupported("the scripted provider has no more responses".into())),
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, StopReason, Usage};
    use serde_json::json;

    #[test]
    fn replays_records_and_streams() {
        let tool = ChatResponse {
            content: vec![
                ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "Need silences.", "signature": "s"}) },
                ContentBlock::text("Checking the audio for silences."),
                ContentBlock::ToolUse { id: "t1".into(), name: "find_silences".into(), input: json!({}) },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            model: None,
        };
        let p = ScriptedProvider::new(vec![tool.clone(), ChatResponse::text("Done.")]);
        let cancel = AtomicBool::new(false);
        let mut req = ChatRequest { messages: vec![Message::user_text("cut silences")], ..ChatRequest::default() };
        let mut evs = Vec::new();
        let r1 = p.send(&req, &mut |e| evs.push(e), &cancel).unwrap();
        assert_eq!(r1, tool);
        let text: String = evs.iter().filter_map(|e| if let StreamEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(text, "Checking the audio for silences.");
        assert!(evs.iter().filter(|e| matches!(e, StreamEvent::TextDelta(_))).count() > 1);
        assert!(evs.contains(&StreamEvent::ThinkingDelta("Need silences.".into())));
        assert!(evs.contains(&StreamEvent::ToolUseStarted { id: "t1".into(), name: "find_silences".into() }));
        assert_eq!(evs.last(), Some(&StreamEvent::Done));

        req.messages.push(r1.to_message());
        req.messages.push(Message { role: crate::Role::User, content: vec![ContentBlock::tool_result("t1", "none")] });
        assert_eq!(p.send(&req, &mut |_| {}, &cancel).unwrap().joined_text(), "Done.");
        assert!(matches!(p.send(&req, &mut |_| {}, &cancel), Err(LlmError::Unsupported(_))));

        let reqs = p.requests();
        assert_eq!(reqs.len(), 3);
        // Append-only: each request's history is a prefix of the next.
        assert!(reqs[1].messages.starts_with(&reqs[0].messages));
        assert_eq!(p.remaining(), 0);
    }

    #[test]
    fn errors_cancel_and_sse_steps() {
        let p = ScriptedProvider::from_steps(vec![
            ScriptStep::Error(LlmError::Overloaded),
            ScriptStep::AnthropicSse(crate::anthropic::tests::TRANSCRIPT.into()),
            ScriptStep::Response(ChatResponse::text("never")),
        ]);
        let req = ChatRequest::default();
        let cancel = AtomicBool::new(false);
        assert_eq!(p.send(&req, &mut |_| {}, &cancel), Err(LlmError::Overloaded));
        let mut n = 0;
        let r = p.send(&req, &mut |_| n += 1, &cancel).unwrap();
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert!(n > 10);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(p.send(&req, &mut |_| {}, &cancel), Err(LlmError::Cancelled));
        assert_eq!(p.requests().len(), 3);
    }
}
