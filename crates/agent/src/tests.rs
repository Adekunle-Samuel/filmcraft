//! Golden conversations: a scripted provider and a fake host.

use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_llm::{ChatResponse, ContentBlock, LlmError, Message, Role, ScriptStep, ScriptedProvider, StopReason, ToolResultContent, ToolSpec, Usage};
use serde_json::{Value, json};

use super::*;

#[derive(Default)]
struct FakeHost {
    calls: Vec<ToolCall>,
    deny: Vec<&'static str>,
    images: usize,
    fail: Vec<&'static str>,
    /// Set this flag when a call runs (simulates the user pressing Cancel mid-call).
    cancel_on: Option<&'static str>,
}

impl ToolHost for FakeHost {
    fn tools(&self) -> Vec<ToolSpec> {
        ["find_silences", "read_transcript", "export"]
            .iter()
            .map(|n| ToolSpec {
                name: n.to_string(),
                description: format!("{n} tool"),
                input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
                strict: true,
                eager_input_streaming: false,
            })
            .collect()
    }
    fn authorize(&mut self, call: &ToolCall) -> Authorization {
        if self.deny.contains(&call.name.as_str()) { Authorization::Deny("the user declined".into()) } else { Authorization::Allow }
    }
    fn call(&mut self, call: &ToolCall, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String> {
        self.calls.push(call.clone());
        progress(ToolProgress { fraction: Some(0.5), status: "half".into() });
        if self.cancel_on == Some(call.name.as_str()) {
            cancel.store(true, Ordering::Relaxed);
        }
        if self.fail.contains(&call.name.as_str()) {
            return Err("no sequence".into());
        }
        let images = (0..self.images).map(|_| ToolImage { media_type: "image/png".into(), data_base64: "iVBORw0KGgo=".into() }).collect();
        Ok(ToolOutcome { json: json!({"tool": call.name, "ok": true}), images })
    }
}

fn tool_use(calls: &[(&str, &str, Value)]) -> ChatResponse {
    ChatResponse {
        content: std::iter::once(ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "plan", "signature": "c2ln"}) })
            .chain(calls.iter().map(|(id, name, input)| ContentBlock::ToolUse { id: id.to_string(), name: name.to_string(), input: input.clone() }))
            .collect(),
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 1000, output_tokens: 100, ..Default::default() },
        model: None,
    }
}

fn run(cfg: &AgentConfig, conv: &mut Conversation, p: &ScriptedProvider, h: &mut FakeHost, text: &str) -> (TurnEnd, Vec<AgentEvent>) {
    let mut ev = Vec::new();
    let cancel = AtomicBool::new(false);
    let end = run_turn(cfg, conv, p, h, vec![ContentBlock::text(text)], &mut |e| ev.push(e), &cancel);
    (end, ev)
}

/// Every request's messages extend the previous request's messages (append-only history).
fn assert_append_only(p: &ScriptedProvider) {
    let reqs = p.requests();
    for w in reqs.windows(2) {
        let (a, b) = (&w[0].messages, &w[1].messages);
        assert!(b.len() > a.len(), "history shrank");
        assert_eq!(&b[..a.len()], &a[..], "history was edited");
        assert_eq!(w[0].tools, w[1].tools, "tool list changed mid-conversation");
        assert_eq!(w[0].system, w[1].system, "system prompt changed mid-conversation");
    }
}

/// Every tool_use in an assistant message is answered by the next message, in one user message.
fn assert_tool_results_paired(msgs: &[Message]) {
    for (i, m) in msgs.iter().enumerate() {
        if m.role != Role::Assistant {
            continue;
        }
        let ids: Vec<&str> = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            continue;
        }
        let Some(next) = msgs.get(i + 1) else { panic!("tool calls without results at the end") };
        assert_eq!(next.role, Role::User);
        let got: Vec<&str> = next
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(got, ids, "results must answer every call, in order, in one message");
    }
}

#[test]
fn text_only_turn() {
    let p = ScriptedProvider::new(vec![ChatResponse::text("Hi! Drop a clip in.")]);
    let mut conv = Conversation::default();
    let (end, ev) = run(&AgentConfig::default(), &mut conv, &p, &mut FakeHost::default(), "hello");
    assert_eq!(end, TurnEnd::Done);
    assert_eq!(conv.messages.len(), 2);
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Stream(filmcraft_llm::StreamEvent::TextDelta(_)))));
    assert!(matches!(ev.last(), Some(AgentEvent::TurnEnded(TurnEnd::Done))));
    let req = &p.requests()[0];
    assert_eq!(req.model, filmcraft_llm::DEFAULT_MODEL);
    assert_eq!(req.tools.len(), 3);
    assert!(req.cache_last_user);
    assert!(req.system.last().is_some_and(|b| b.cache));
}

#[test]
fn parallel_tool_calls_answered_in_one_message_and_history_append_only() {
    let p = ScriptedProvider::new(vec![
        tool_use(&[("t1", "find_silences", json!({})), ("t2", "read_transcript", json!({"offset": 0}))]),
        tool_use(&[("t3", "read_transcript", json!({"offset": 50}))]),
        ChatResponse::text("Found 12 s of silence."),
    ]);
    let mut conv = Conversation::default();
    let mut h = FakeHost::default();
    let (end, ev) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "clean this up");
    assert_eq!(end, TurnEnd::Done);
    assert_eq!(h.calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["t1", "t2", "t3"]);
    assert_append_only(&p);
    assert_tool_results_paired(&conv.messages);
    // thinking blocks are kept verbatim in history
    assert_eq!(conv.messages[1].content[0].opaque_type(), Some("thinking"));
    assert_eq!(ev.iter().filter(|e| matches!(e, AgentEvent::ToolFinished { ok: true, .. })).count(), 3);
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolProgress { .. })));
    assert_eq!(conv.usage.input_tokens, 2000);
    assert!(conv.cost_usd.is_some_and(|c| c > 0.0));
}

#[test]
fn denied_failed_and_invalid_calls_become_error_results() {
    let p = ScriptedProvider::new(vec![
        tool_use(&[
            ("t1", "export", json!({"path": "/tmp/x.mp4"})),
            ("t2", "find_silences", json!({"__invalid_json": "{\"min"})),
            ("t3", "read_transcript", json!({})),
            ("t4", "find_silences", json!([1, 2])),
        ]),
        ChatResponse::text("ok"),
    ]);
    let mut conv = Conversation::default();
    let mut h = FakeHost { deny: vec!["export"], fail: vec!["read_transcript"], ..Default::default() };
    let (end, ev) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "go");
    assert_eq!(end, TurnEnd::Done);
    assert_eq!(h.calls.len(), 1, "only the valid, allowed call ran (and failed)");
    let results = &conv.messages[2].content;
    let errs: Vec<bool> = results.iter().map(|b| matches!(b, ContentBlock::ToolResult { is_error: true, .. })).collect();
    assert_eq!(errs, [true, true, true, true]);
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolDenied { name, .. } if name == "export")));
    assert_tool_results_paired(&conv.messages);
}

#[test]
fn max_tokens_with_a_tool_call_runs_nothing_and_asks_again() {
    let mut cut = tool_use(&[("t1", "find_silences", json!({}))]);
    cut.stop_reason = StopReason::MaxTokens;
    let p = ScriptedProvider::new(vec![cut, ChatResponse::text("smaller now")]);
    let mut conv = Conversation::default();
    let mut h = FakeHost::default();
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "go");
    assert_eq!(end, TurnEnd::Done);
    assert!(h.calls.is_empty());
    assert_tool_results_paired(&conv.messages);
    assert_append_only(&p);
}

#[test]
fn refusal_leaves_pending_results_for_the_next_turn() {
    let mut r = tool_use(&[("t1", "find_silences", json!({}))]);
    r.stop_reason = StopReason::Refusal { category: Some("cyber".into()), explanation: Some("no".into()) };
    let p = ScriptedProvider::new(vec![r, ChatResponse::text("sure")]);
    let mut conv = Conversation::default();
    let mut h = FakeHost::default();
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "go");
    assert!(matches!(end, TurnEnd::Refused { .. }));
    assert!(h.calls.is_empty());
    assert_eq!(conv.pending_results.len(), 1);
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "something else");
    assert_eq!(end, TurnEnd::Done);
    assert!(conv.pending_results.is_empty());
    assert_tool_results_paired(&conv.messages);
    // the owed result comes first, then the new text
    let first_user_of_turn2 = &conv.messages[2].content;
    assert!(matches!(first_user_of_turn2[0], ContentBlock::ToolResult { .. }));
    assert!(matches!(first_user_of_turn2[1], ContentBlock::Text { .. }));
}

#[test]
fn cancel_mid_tools_keeps_history_valid() {
    let p = ScriptedProvider::new(vec![
        tool_use(&[("t1", "find_silences", json!({})), ("t2", "read_transcript", json!({})), ("t3", "read_transcript", json!({}))]),
        ChatResponse::text("resumed"),
    ]);
    let mut conv = Conversation::default();
    let mut h = FakeHost { cancel_on: Some("find_silences"), ..Default::default() };
    let cancel = AtomicBool::new(false);
    let end = run_turn(&AgentConfig::default(), &mut conv, &p, &mut h, vec![ContentBlock::text("go")], &mut |_| {}, &cancel);
    assert_eq!(end, TurnEnd::Cancelled);
    assert_eq!(h.calls.len(), 1);
    assert_eq!(conv.pending_results.len(), 3, "one real result and two cancelled");
    h.cancel_on = None;
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut h, "continue");
    assert_eq!(end, TurnEnd::Done);
    assert_tool_results_paired(&conv.messages);
    assert_append_only(&p);
}

#[test]
fn cancelled_before_the_call_returns_cancelled() {
    let p = ScriptedProvider::new(vec![ChatResponse::text("never")]);
    let mut conv = Conversation::default();
    let cancel = AtomicBool::new(true);
    let end = run_turn(&AgentConfig::default(), &mut conv, &p, &mut FakeHost::default(), vec![ContentBlock::text("x")], &mut |_| {}, &cancel);
    assert_eq!(end, TurnEnd::Cancelled);
    assert!(p.requests().is_empty());
}

#[test]
fn limits_stop_runaway_loops() {
    let steps: Vec<ChatResponse> = (0..10).map(|i| tool_use(&[(&format!("t{i}"), "find_silences", json!({}))])).collect();
    let p = ScriptedProvider::new(steps);
    let mut conv = Conversation::default();
    let cfg = AgentConfig { max_model_calls: 3, ..Default::default() };
    let (end, _) = run(&cfg, &mut conv, &p, &mut FakeHost::default(), "loop");
    assert!(matches!(end, TurnEnd::Limit(_)), "{end:?}");
    assert_eq!(p.requests().len(), 3);
    assert_tool_results_paired(&conv.messages);

    let many: Vec<(String, &str, Value)> = (0..5).map(|i| (format!("m{i}"), "find_silences", json!({}))).collect();
    let refs: Vec<(&str, &str, Value)> = many.iter().map(|(a, b, c)| (a.as_str(), *b, c.clone())).collect();
    let p = ScriptedProvider::new(vec![tool_use(&refs)]);
    let mut conv = Conversation::default();
    let mut h = FakeHost::default();
    let cfg = AgentConfig { max_tool_calls: 2, ..Default::default() };
    let (end, _) = run(&cfg, &mut conv, &p, &mut h, "loop");
    assert!(matches!(end, TurnEnd::Limit(_)));
    assert_eq!(h.calls.len(), 2);
    assert_eq!(conv.pending_results.len(), 5);
}

#[test]
fn budget_stops_before_the_next_call() {
    let p = ScriptedProvider::new(vec![tool_use(&[("t1", "find_silences", json!({}))]), ChatResponse::text("x")]);
    let mut conv = Conversation::default();
    let cfg = AgentConfig { budget_usd: Some(0.000_001), ..Default::default() };
    let (end, _) = run(&cfg, &mut conv, &p, &mut FakeHost::default(), "go");
    assert!(matches!(end, TurnEnd::Limit(ref m) if m.contains("budget")), "{end:?}");
    assert_eq!(p.requests().len(), 1);
    assert_tool_results_paired(&conv.messages);
}

#[test]
fn images_are_capped_per_call_and_dropped_without_vision() {
    let p = ScriptedProvider::new(vec![tool_use(&[("t1", "find_silences", json!({})), ("t2", "find_silences", json!({}))]), ChatResponse::text("x")]);
    let mut conv = Conversation::default();
    let mut h = FakeHost { images: 4, ..Default::default() };
    let cfg = AgentConfig { max_images: 6, ..Default::default() };
    run(&cfg, &mut conv, &p, &mut h, "look");
    let n: usize = conv.messages[2]
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::ToolResult { content, .. } => content.iter().filter(|c| matches!(c, ToolResultContent::Image { .. })).count(),
            _ => 0,
        })
        .sum();
    assert_eq!(n, 6);

    let p = ScriptedProvider::new(vec![tool_use(&[("t1", "find_silences", json!({}))]), ChatResponse::text("x")]);
    let mut conv = Conversation::default();
    let cfg = AgentConfig { vision: false, ..Default::default() };
    run(&cfg, &mut conv, &p, &mut h, "look");
    assert!(format!("{:?}", conv.messages[2]).find("Image").is_none());
}

#[test]
fn long_results_are_truncated_at_char_boundaries() {
    struct Big;
    impl ToolHost for Big {
        fn tools(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn authorize(&mut self, _: &ToolCall) -> Authorization {
            Authorization::Allow
        }
        fn call(&mut self, _: &ToolCall, _: &AtomicBool, _: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String> {
            Ok(ToolOutcome::json(json!({"text": "é".repeat(50_000)})))
        }
    }
    let p = ScriptedProvider::new(vec![tool_use(&[("t1", "read_transcript", json!({}))]), ChatResponse::text("x")]);
    let mut conv = Conversation::default();
    let mut ev = Vec::new();
    let cfg = AgentConfig { max_result_chars: 1000, ..Default::default() };
    run_turn(&cfg, &mut conv, &p, &mut Big, vec![ContentBlock::text("x")], &mut |e| ev.push(e), &AtomicBool::new(false));
    let ContentBlock::ToolResult { content, .. } = &conv.messages[2].content[0] else { panic!() };
    let ToolResultContent::Text { text } = &content[0] else { panic!() };
    assert!(text.chars().count() < 1200 && text.contains("truncated"));
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Notice(_))));
}

#[test]
fn provider_errors_end_the_turn() {
    let p = ScriptedProvider::from_steps(vec![ScriptStep::Error(LlmError::Auth)]);
    let mut conv = Conversation::default();
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut FakeHost::default(), "x");
    assert!(matches!(end, TurnEnd::Failed(ref m) if m.contains("API key")), "{end:?}");
    // the script ran out: a clear failure, not a panic
    let (end, _) = run(&AgentConfig::default(), &mut conv, &p, &mut FakeHost::default(), "again");
    assert!(matches!(end, TurnEnd::Failed(_)));
}

#[test]
fn notes_and_conversations_serialize() {
    let mut conv = Conversation::default();
    conv.push_note("the user edited sequence 3");
    let s = serde_json::to_string(&conv).unwrap();
    let back: Conversation = serde_json::from_str(&s).unwrap();
    assert_eq!(back, conv);
    assert_eq!(back.messages[0].role, Role::System);
}
