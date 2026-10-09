//! Talking-head cleanup, end to end and offline: the real agent loop, the real engine tool
//! catalogue and edit-plan executor, and a scripted "model" that reacts to the tool results the way
//! the skill tells a real model to (find silences → read the transcript → propose a plan → apply).
//!
//! The footage is synthetic speech with a known script, so the eval can measure what matters:
//! every silence ≥ 0.5 s gone, the filler gone, the off-topic passage gone, no kept word cut, the
//! original sequence untouched, captions over every kept word, and one undo restoring it all.
//!
//! A live run against a real model is opt-in (`FILMCRAFT_EVAL_LIVE=1` plus a key) because it costs
//! money; this scripted run is the CI gate.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use filmcraft_agent::{AgentConfig, AgentEvent, Conversation, SessionHost, TurnEnd, run_turn};
use filmcraft_engine::Session;
use filmcraft_llm::{ChatRequest, ChatResponse, ContentBlock, LlmError, LlmProvider, Role, StopReason, StreamEvent, ToolResultContent, Usage};
use filmcraft_project::{Transcript, Word};
use filmcraft_time::Tick;
use serde_json::{Value, json};

const SR: u32 = 48_000;

/// (word, start, end) in seconds. Words 7..=9 are the off-topic lunch aside; word 2 is a filler.
const SCRIPT: &[(&str, f64, f64)] = &[
    ("Hello", 0.10, 0.60),
    ("there.", 0.65, 1.10),
    // 1.5 s of dead air
    ("um", 2.60, 3.00),
    ("This", 3.10, 3.40),
    ("is", 3.45, 3.60),
    ("the", 3.65, 3.80),
    ("point.", 3.85, 4.30),
    // 1.2 s
    ("Lunch", 5.50, 6.00),
    ("was", 6.05, 6.30),
    ("great.", 6.35, 6.80),
    // 0.8 s
    ("Thanks.", 7.60, 8.10),
];
const LENGTH_S: f64 = 8.5;

fn voiced(t: f64) -> bool {
    SCRIPT.iter().any(|(_, a, b)| t >= *a && t < *b)
}

/// A voiced buzz under a syllable envelope wherever a word is spoken.
fn audio() -> Vec<f32> {
    let n = (LENGTH_S * SR as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            if !voiced(t) {
                return 0.0;
            }
            let mut v = 0.0;
            for k in 1..20 {
                v += (2.0 * std::f64::consts::PI * 140.0 * k as f64 * t).sin() / k as f64;
            }
            let syl = 0.7 - 0.3 * (2.0 * std::f64::consts::PI * 5.0 * t).cos();
            (v * syl * 0.2) as f32
        })
        .collect()
}

fn session() -> (Session, filmcraft_project::ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Interview", "audio": 2, "video": 1, "fps": 25})).unwrap();
    let inter: Vec<f32> = audio().iter().flat_map(|v| [*v, *v]).collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, SR).into();
    let item = filmcraft_engine::commands::import_bytes(&mut s, "/interview.wav", bytes, None).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let mut t = Transcript { language: "en".into(), ..Default::default() };
    for (w, a, b) in SCRIPT {
        t.words.push(Word::new(*w, Tick::from_seconds_f64(*a), Tick::from_seconds_f64(*b)));
    }
    t.normalize();
    s.execute("transcript.set", json!({"item": item.0, "transcript": serde_json::to_value(&t).unwrap()})).unwrap();
    s.execute("edit.deselectAll", json!({})).unwrap();
    (s, item)
}

/// A scripted model that decides each step from the previous tool result, like the skill says.
struct Editor {
    step: Mutex<usize>,
}

fn last_result(req: &ChatRequest) -> Value {
    let Some(m) = req.messages.iter().rev().find(|m| m.role == Role::User) else { return Value::Null };
    for b in &m.content {
        if let ContentBlock::ToolResult { content, is_error, .. } = b {
            assert!(!is_error, "a tool failed: {content:?}");
            if let Some(ToolResultContent::Text { text }) = content.first() {
                return serde_json::from_str(text).unwrap_or(Value::Null);
            }
        }
    }
    Value::Null
}

fn call(id: &str, name: &str, input: Value) -> ChatResponse {
    ChatResponse {
        content: vec![ContentBlock::text("Working on it."), ContentBlock::ToolUse { id: id.into(), name: name.into(), input }],
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 2000, output_tokens: 200, ..Default::default() },
        model: None,
    }
}

thread_local! {
    static PLAN: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

impl LlmProvider for Editor {
    fn name(&self) -> &str {
        "scripted-editor"
    }
    fn send(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), _: &AtomicBool) -> Result<ChatResponse, LlmError> {
        // the tools the skill relies on are offered
        let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
        for need in ["find_silences", "read_transcript", "propose_edit_plan", "apply_edit_plan"] {
            assert!(names.contains(&need), "{need} not offered: {names:?}");
        }
        let mut step = self.step.lock().unwrap();
        *step += 1;
        on_event(StreamEvent::TextDelta(format!("step {step}")));
        Ok(match *step {
            1 => {
                call("c1", "find_silences", json!({"min_seconds": 0.5, "pad_seconds": null, "threshold_db": null, "start_seconds": null, "end_seconds": null}))
            }
            2 => {
                let sil = last_result(req);
                let silences: Vec<Value> =
                    sil["silences"].as_array().unwrap().iter().map(|r| json!({"startS": r["startSeconds"], "endS": r["endSeconds"]})).collect();
                assert_eq!(silences.len(), 3, "three gaps of at least 0.5 s: {sil}");
                let plan = json!({
                    "version": 1,
                    "title": "Tight interview",
                    "rationale": "dead air, the um and the lunch aside removed",
                    "cleanup": {"fillers": [], "silences": silences},
                    "cuts": {"removeWords": [{"from": 7, "to": 9, "reason": "off-topic: lunch"}]},
                    "captions": {"maxChars": 32, "lines": 2}
                })
                .to_string();
                PLAN.with(|p| *p.borrow_mut() = plan.clone());
                call("c2", "read_transcript", json!({"offset": 0, "limit": 40}))
            }
            3 => {
                let tr = last_result(req).to_string();
                assert!(tr.contains("Lunch") && tr.contains("um"), "the transcript shows the filler and the aside: {tr}");
                call("c3", "propose_edit_plan", json!({"plan": PLAN.with(|p| p.borrow().clone())}))
            }
            4 => {
                let preview = last_result(req);
                let hash = preview["sourceHash"].as_str().unwrap().to_string();
                call("c4", "apply_edit_plan", json!({"plan": PLAN.with(|p| p.borrow().clone()), "source_hash": hash}))
            }
            _ => ChatResponse::text("Done: a new sequence without the silences, the um and the lunch aside, with captions."),
        })
    }
}

fn sequence_words(s: &mut Session) -> Vec<String> {
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    r["words"].as_array().unwrap().iter().map(|w| w["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn talking_head_cleanup_end_to_end() {
    let (mut s, _) = session();
    let original = s.state.active_sequence.unwrap();
    let original_seq = s.project.sequence(original).unwrap().clone();
    let before = original_seq.duration().seconds();
    let undo_before = s.history.undo.len();

    let provider = Editor { step: Mutex::new(0) };
    let mut conv = Conversation::default();
    let mut events = Vec::new();
    let mut asked = Vec::new();
    let end = {
        let mut host = SessionHost::new(
            &mut s,
            Box::new(|c, _| {
                asked.push(c.name.clone());
                true
            }),
        );
        run_turn(
            &AgentConfig::default(),
            &mut conv,
            &provider,
            &mut host,
            vec![ContentBlock::text("Clean this up: remove the silences and ums, cut the bit about lunch, add captions.")],
            &mut |e| events.push(e),
            &AtomicBool::new(false),
        )
    };
    assert_eq!(end, TurnEnd::Done, "{events:#?}");
    assert_eq!(asked, ["apply_edit_plan"], "only the apply needed the user's click");
    assert!(events.iter().all(|e| !matches!(e, AgentEvent::ToolFinished { ok: false, .. })), "{events:#?}");

    // a new sequence is active; the original is untouched
    let new = s.state.active_sequence.unwrap();
    assert_ne!(new, original);
    assert_eq!(s.project.sequence(original).unwrap(), &original_seq);

    // content: filler and aside gone, every other word kept whole
    let words = sequence_words(&mut s);
    assert_eq!(words, ["Hello", "there.", "This", "is", "the", "point.", "Thanks."]);
    let r = s.execute("transcript.inspect", json!({})).unwrap();
    for (w, orig) in r["words"].as_array().unwrap().iter().zip(SCRIPT.iter().filter(|(t, _, _)| !["um", "Lunch", "was", "great."].contains(t))) {
        let dur = (w["end"].as_i64().unwrap() - w["start"].as_i64().unwrap()) as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
        assert!((dur - (orig.2 - orig.1)).abs() < 0.045, "word {} was cut: {dur} s", orig.0);
    }

    // no silence of 0.5 s or more is left
    let left = s.execute("audio.detectSilence", json!({"minSeconds": 0.5})).unwrap();
    assert_eq!(left["count"], 0, "residual silence: {left}");

    // duration: the speech that was kept, plus a little air
    let after = s.project.sequence(new).unwrap().duration().seconds();
    let kept_speech: f64 = SCRIPT.iter().filter(|(t, _, _)| !["um", "Lunch", "was", "great."].contains(t)).map(|(_, a, b)| b - a).sum();
    assert!(after < before - 3.0 && after > kept_speech, "{before} s → {after} s (speech {kept_speech} s)");

    // captions cover every kept word
    let seq = s.project.sequence(new).unwrap();
    let caps: Vec<String> = seq.caption_tracks.iter().flat_map(|t| t.captions.iter().map(|c| c.text.clone())).collect();
    let all = caps.join(" ");
    for w in &words {
        assert!(all.contains(w.trim_end_matches('.')), "caption missing {w}: {caps:?}");
    }

    // the request history stayed append-only and every call got its result
    assert_eq!(conv.messages.iter().filter(|m| m.role == Role::Assistant).count(), 5);
    assert!(conv.cost_usd.is_some_and(|c| c > 0.0));

    // one undo puts everything back
    let added = s.history.undo.len() - undo_before;
    assert_eq!(added, 1, "the whole edit is one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.sequence(new).is_none());
    assert_eq!(s.project.sequence(original).unwrap(), &original_seq);
}

#[test]
fn unattended_hosts_decline_edits() {
    let (mut s, _) = session();
    let undo_before = s.history.undo.len();
    struct ApplyOnly(Mutex<usize>);
    impl LlmProvider for ApplyOnly {
        fn name(&self) -> &str {
            "apply-only"
        }
        fn send(&self, _: &ChatRequest, _: &mut dyn FnMut(StreamEvent), _: &AtomicBool) -> Result<ChatResponse, LlmError> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            Ok(if *n == 1 { call("x1", "command_run", json!({"id": "edit.rippleDelete", "params": "{}"})) } else { ChatResponse::text("ok") })
        }
    }
    let mut conv = Conversation::default();
    let mut ev = Vec::new();
    {
        let mut host = SessionHost::read_mostly(&mut s);
        run_turn(
            &AgentConfig::default(),
            &mut conv,
            &ApplyOnly(Mutex::new(0)),
            &mut host,
            vec![ContentBlock::text("x")],
            &mut |e| ev.push(e),
            &AtomicBool::new(false),
        );
    }
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolDenied { .. })), "{ev:#?}");
    assert_eq!(s.history.undo.len(), undo_before);
}
