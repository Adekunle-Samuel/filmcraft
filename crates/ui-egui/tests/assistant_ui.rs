//! Headless tests of the Assistant panel: a scripted LLM provider installed through
//! `HostHooks::assistant_provider`, driven over the control channel (`assistant.*`) and by
//! automation id. The worker thread's tool calls and approvals round-trip through the UI-thread
//! drain.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write
//! `assistant-*.png`.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_llm::{ChatResponse, ContentBlock, LlmProvider, Role, ScriptedProvider, StopReason, ToolResultContent, Usage};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new(provider: Option<Arc<ScriptedProvider>>, session: Session) -> Self {
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        if let Some(p) = provider {
            app.hooks.assistant_provider = Some(Box::new(move |_s| Ok(p.clone() as Arc<dyn LlmProvider>)));
        }
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d.ok("ui.panel.show", json!({"panel": "assistant"}));
        d.frames(3);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn state(&mut self) -> Value {
        self.ok("assistant.state", json!({}))
    }

    /// Step frames (the worker runs meanwhile) until `f` holds for `assistant.state`.
    fn wait_for(&mut self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..2000 {
            let s = self.state();
            if f(&s) {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("timed out waiting for {what}: {}", self.state());
    }

    fn wait_idle(&mut self) -> Value {
        self.wait_for("the turn to end", |s| s["running"] == json!(false))
    }

    fn consent(&mut self) {
        self.click("assistant.consent.accept");
        assert_eq!(self.ok("assistant.settings.get", json!({}))["consented"], json!(true));
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(3);
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(img) = self.harness.render() {
            let _ = img.save(dir.join(format!("assistant-{name}.png")));
        }
    }
}

fn texts(s: &Value, role: &str) -> Vec<String> {
    s["messages"].as_array().unwrap().iter().filter(|m| m["role"] == role).map(|m| m["text"].as_str().unwrap_or("").to_string()).collect()
}

fn tool_use(id: &str, name: &str, input: Value) -> ChatResponse {
    ChatResponse {
        content: vec![ContentBlock::text("Let me check."), ContentBlock::ToolUse { id: id.into(), name: name.into(), input }],
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 1200, output_tokens: 80, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 },
        model: None,
    }
}

/// The tool result block answering `id` in the request's last user message: (text, is_error).
fn result_for(p: &ScriptedProvider, request: usize, id: &str) -> (String, bool) {
    let reqs = p.requests();
    let msg = reqs[request].messages.iter().rev().find(|m| m.role == Role::User).unwrap();
    msg.content
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, content, is_error } if tool_use_id == id => {
                let text = content.iter().filter_map(|c| if let ToolResultContent::Text { text } = c { Some(text.as_str()) } else { None }).collect();
                Some((text, *is_error))
            }
            _ => None,
        })
        .unwrap()
}

fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-assistant-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn consent_then_scripted_text_streams_into_the_chat() {
    let p = Arc::new(ScriptedProvider::new(vec![ChatResponse::text("Hello from the scripted model. Drop a clip in.")]));
    let mut d = Driver::new(Some(p.clone()), Session::default());
    assert!(!d.ok("ui.elements", json!({"prefix": "assistant.consent.accept"})).as_array().unwrap().is_empty());
    d.snapshot("consent");
    // nothing is sent before consent
    let r = d.call("assistant.send", json!({"text": "hi"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("consent"), "{r}");
    assert!(d.call("assistant.settings.set", json!({"consented": true}))["ok"] == json!(false));
    assert!(p.requests().is_empty());

    d.consent();
    d.snapshot("empty");
    // typing in the composer and pressing Send
    d.click("assistant.input");
    d.ok("ui.type", json!({"text": "hi"}));
    d.frames(2);
    d.click("assistant.send");
    let s = d.wait_idle();
    assert_eq!(texts(&s, "user"), ["hi"]);
    assert_eq!(texts(&s, "assistant"), ["Hello from the scripted model. Drop a clip in."]);
    assert_eq!(p.requests().len(), 1);
    assert_eq!(s["usage"]["inputTokens"], json!(0));
    // the composer was cleared and the message is on screen
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["panels"]["assistant"]["draft"], json!(""));
    d.frames(3);
    assert!(!d.ok("ui.elements", json!({"prefix": "assistant.msg.1"})).as_array().unwrap().is_empty());
    d.snapshot("chat");

    // New conversation clears it
    d.click("assistant.new");
    assert!(d.state()["messages"].as_array().unwrap().is_empty());
}

#[test]
fn tool_calls_round_trip_through_the_ui_thread() {
    let p = Arc::new(ScriptedProvider::new(vec![tool_use("t1", "project_overview", json!({})), ChatResponse::text("Your project has one sequence.")]));
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut d = Driver::new(Some(p.clone()), session);
    d.consent();
    d.ok("assistant.send", json!({"text": "What's in my project?"}));
    let s = d.wait_idle();
    let tools = s["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{s}");
    assert_eq!(tools[0]["name"], json!("project_overview"));
    assert_eq!(tools[0]["running"], json!(false));
    let (text, is_error) = result_for(&p, 1, "t1");
    if filmcraft_engine::find_command("tools.call").is_some() {
        assert!(!is_error, "{text}");
        assert_eq!(tools[0]["ok"], json!(true));
    } else {
        // this build has no tool catalogue: the drain answers with an error result
        assert!(is_error);
        assert!(text.contains("tool catalogue not available"), "{text}");
        assert_eq!(tools[0]["ok"], json!(false));
    }
    assert_eq!(texts(&s, "assistant"), ["Let me check.", "Your project has one sequence."]);
    assert_eq!(s["usage"]["inputTokens"], json!(1200));
    d.frames(3);
    assert!(!d.ok("ui.elements", json!({"prefix": "assistant.tool.t1"})).as_array().unwrap().is_empty());
    d.click("assistant.tool.t1.raw");
    d.snapshot("tool");
}

#[test]
fn edits_wait_for_approval_and_deny_returns_an_error_result() {
    let p = Arc::new(ScriptedProvider::new(vec![
        tool_use("t1", "command_run", json!({"id": "sequence.addEdit", "params": {}})),
        ChatResponse::text("OK, I won't."),
    ]));
    let mut d = Driver::new(Some(p.clone()), Session::default());
    d.consent();
    d.ok("assistant.send", json!({"text": "cut at the playhead"}));
    let s = d.wait_for("an approval", |s| !s["approvals"].as_array().unwrap().is_empty());
    assert_eq!(s["approvals"][0]["tool"], json!("command_run"));
    assert!(s["approvals"][0]["reason"].as_str().unwrap().contains("sequence.addEdit"));
    assert_eq!(s["running"], json!(true));
    let k = s["approvals"][0]["id"].as_u64().unwrap();
    d.frames(3);
    d.snapshot("approval");
    d.click(&format!("assistant.approval.{k}.deny"));
    let s = d.wait_idle();
    assert!(s["approvals"].as_array().unwrap().is_empty());
    assert_eq!(s["tools"][0]["denied"], json!(true));
    assert_eq!(s["tools"][0]["ok"], json!(false));
    let (text, is_error) = result_for(&p, 1, "t1");
    assert!(is_error);
    assert!(text.contains("the user declined"), "{text}");
    assert_eq!(texts(&s, "assistant").last().map(String::as_str), Some("OK, I won't."));
}

#[test]
fn cancel_stops_a_waiting_turn() {
    let p = Arc::new(ScriptedProvider::new(vec![tool_use("t1", "command_run", json!({"id": "sequence.addEdit"})), ChatResponse::text("never")]));
    let mut d = Driver::new(Some(p.clone()), Session::default());
    d.consent();
    d.ok("assistant.send", json!({"text": "do it"}));
    d.wait_for("an approval", |s| !s["approvals"].as_array().unwrap().is_empty());
    assert!(!d.ok("ui.elements", json!({"prefix": "assistant.cancel"})).as_array().unwrap().is_empty());
    d.ok("assistant.cancel", json!({}));
    let s = d.wait_idle();
    assert!(s["approvals"].as_array().unwrap().is_empty());
    assert_eq!(texts(&s, "notice").last().map(String::as_str), Some("Stopped."), "{s}");
    assert_eq!(p.requests().len(), 1, "no model call after the cancel");
    // the next turn starts cleanly
    let r = d.call("assistant.approve", json!({"index": 0, "allow": true}));
    assert_eq!(r["ok"], json!(false));
}

#[test]
fn without_a_provider_the_panel_says_not_available() {
    let mut d = Driver::new(None, Session::default());
    let els = d.ok("ui.elements", json!({"prefix": "assistant.unavailable"}));
    let label = els[0]["label"].as_str().unwrap().to_string();
    assert!(label.contains("not available in this build"), "{label}");
    assert!(label.contains("--features assistant"));
    let r = d.call("assistant.send", json!({"text": "hi"}));
    assert_eq!(r["ok"], json!(false));
    assert!(r["error"].as_str().unwrap().contains("not available"));
    assert_eq!(d.state()["available"], json!(false));
    // Window ▸ Assistant
    let menu = d.ok("ui.menu.list", json!({})).to_string();
    assert!(menu.contains("\"Assistant\""), "Window ▸ Assistant missing");
    d.snapshot("unavailable");
    d.ok("ui.panel.close", json!({"panel": "assistant"}));
}

#[test]
fn the_api_key_never_reaches_inspect_and_conversations_persist() {
    const SECRET: &str = "sk-ant-test-SECRET-7f3c9";
    let dir = temp_data_dir("key");
    let mut session = Session::default();
    session.prefs_path = Some(dir.join("preferences.json"));
    let p = Arc::new(ScriptedProvider::new(vec![ChatResponse::text("Saved reply.")]));
    let mut d = Driver::new(Some(p.clone()), session);
    d.click("assistant.settings");
    d.click("assistant.settings.key");
    d.ok("ui.type", json!({"text": SECRET}));
    d.frames(3);
    d.snapshot("settings");
    let leaks = |d: &mut Driver| {
        let inspect = d.ok("ui.inspect", json!({})).to_string();
        let elements = d.ok("ui.elements", json!({})).to_string();
        let settings = d.ok("assistant.settings.get", json!({})).to_string();
        let state = d.state().to_string();
        [inspect, elements, settings, state].iter().any(|s| s.contains(SECRET) || s.contains("SECRET"))
    };
    assert!(!leaks(&mut d), "the key being typed leaked");
    d.click("assistant.settings.key.save");
    let stored = std::fs::read_to_string(dir.join("assistant").join("credentials.json")).unwrap();
    assert!(stored.contains(SECRET));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("assistant").join("credentials.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(!leaks(&mut d), "the stored key leaked");
    assert!(!d.ok("assistant.settings.get", json!({}))["keySource"].is_null());
    let r = d.call("assistant.settings.set", json!({"apiKey": "x"}));
    assert_eq!(r["ok"], json!(false));
    // settings over the control channel, persisted beside the key
    let s = d.ok("assistant.settings.set", json!({"effort": "medium", "budgetUsd": 3.5, "vision": false}));
    assert_eq!((s["effort"].clone(), s["budgetUsd"].clone(), s["vision"].clone()), (json!("medium"), json!(3.5), json!(false)));
    d.click("assistant.settings.done");
    d.frames(2);
    let saved: Value = serde_json::from_slice(&std::fs::read(dir.join("assistant").join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["effort"], json!("medium"));
    assert!(!saved.to_string().contains(SECRET));

    d.consent();
    d.ok("assistant.send", json!({"text": "remember me"}));
    d.wait_idle();
    let conv = dir.join("assistant").join("conversations").join("untitled.json");
    let saved = std::fs::read_to_string(&conv).unwrap();
    assert!(saved.contains("remember me") && saved.contains("Saved reply."));
    assert!(!saved.contains(SECRET));
    // the request used the settings
    let req = &p.requests()[0];
    assert_eq!(req.effort, Some(filmcraft_llm::Effort::Medium));
    // a new conversation forgets it
    d.ok("assistant.reset", json!({}));
    assert!(!conv.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plan_cards_show_the_diff_and_reject_notes_it() {
    use filmcraft_ui_egui::panels::assistant::{ChatItem, PlanStatus, ToolCard};
    let p = Arc::new(ScriptedProvider::new(vec![]));
    let mut d = Driver::new(Some(p), Session::default());
    d.consent();
    let preview = json!({
        "durationBefore": 182.4, "durationAfter": 151.0,
        "transcriptDiff": [{"text": "So"}, {"text": "um,", "removed": true, "reason": "filler"}, {"text": "today we look at"}, {"text": "(long pause)", "removed": true, "reason": "silence"}, {"text": "the new edit."}],
        "warnings": ["One cut is shorter than 0.3 s and was dropped."],
    });
    let rt = &mut d.harness.state_mut().assistant;
    rt.items.push(ChatItem::User("Remove the silences and ums".into()));
    rt.items.push(ChatItem::Assistant("Here is a plan.".into()));
    rt.items.push(ChatItem::Tool(ToolCard {
        id: "t9".into(),
        name: "propose_edit_plan".into(),
        input: json!({"plan": "{}"}),
        running: false,
        progress: None,
        ok: Some(true),
        denied: false,
        summary: "2 cuts, 31.4 s shorter".into(),
        result: Some(preview),
        expanded: false,
        plan: Some(PlanStatus::Proposed),
    }));
    d.frames(3);
    assert!(!d.ok("ui.elements", json!({"prefix": "assistant.plan.0.apply"})).as_array().unwrap().is_empty());
    d.snapshot("plan");
    d.click("assistant.plan.0.reject");
    let s = d.state();
    assert_eq!(s["tools"][0]["plan"], json!("rejected"));
    let conv = &d.harness.state().assistant.conversation;
    assert!(conv.messages.last().is_some_and(|m| m.role == Role::System && m.text().contains("rejected")));
}

#[test]
fn conversations_reload_after_a_restart() {
    let dir = temp_data_dir("reload");
    let session = || {
        let mut s = Session::default();
        s.prefs_path = Some(dir.join("preferences.json"));
        s
    };
    let p = Arc::new(ScriptedProvider::new(vec![ChatResponse::text("First answer.")]));
    let mut d = Driver::new(Some(p), session());
    d.consent();
    d.ok("assistant.send", json!({"text": "first question"}));
    d.wait_idle();
    drop(d);
    let mut d = Driver::new(Some(Arc::new(ScriptedProvider::new(vec![]))), session());
    // consent and settings persisted too
    assert_eq!(d.ok("assistant.settings.get", json!({}))["consented"], json!(true));
    let s = d.state();
    assert_eq!(texts(&s, "user"), ["first question"]);
    assert_eq!(texts(&s, "assistant"), ["First answer."]);
    let _ = std::fs::remove_dir_all(&dir);
}
