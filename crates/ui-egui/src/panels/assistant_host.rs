//! The Assistant's worker thread and its [`ToolHost`].
//!
//! The agent loop ([`filmcraft_agent::run_turn`]) blocks on the network, so it runs on a worker
//! thread (desktop only; there are no threads on the web). The engine [`Session`] belongs to the UI
//! thread, so the worker's [`UiHost`] never touches it: every tool call, job poll and approval is
//! an [`AssistantRequest`] sent over a channel, answered by the UI thread between frames
//! (`panels::assistant::drain`, called from `FilmcraftApp::logic`). The worker waits for the reply
//! in 100 ms slices so Cancel always gets through.
//!
//! Approvals are decided here, by the host, never by the model: tools whose `tools.list` entry says
//! `approval: "ask"` (or `"askIfOverwrite"`), and escape-hatch commands that are not pure queries,
//! wait for the user's Allow / Deny click.
//!
//! [`Session`]: filmcraft_engine::Session

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

use filmcraft_agent::{Authorization, ToolCall, ToolHost, ToolImage, ToolOutcome, ToolProgress};
use filmcraft_llm::ToolSpec;
use serde_json::{Value, json};

/// How often a blocked worker looks at the cancel flag.
pub const WAIT_SLICE: Duration = Duration::from_millis(100);
/// How often a running job is polled.
pub const POLL_EVERY: Duration = Duration::from_millis(250);
/// The longest a tool's job may run before the host gives up on it (6 h: a long transcription).
pub const JOB_LIMIT: Duration = Duration::from_secs(6 * 3600);

/// A request from the worker to the UI thread.
pub enum AssistantRequest {
    /// Run a tool (`tools.call`).
    Call { call: ToolCall, reply: Sender<Result<CallStep, String>> },
    /// Read a job's progress (`jobs.list`).
    PollJob { job: u64, reply: Sender<Result<JobState, String>> },
    /// Stop a job (`jobs.cancel`), after the user pressed Cancel.
    CancelJob { job: u64 },
    /// Ask the user to allow a call; `true` = Allow.
    Approve { call: ToolCall, reason: String, reply: Sender<bool> },
}

/// What a tool call did on the UI thread.
#[derive(Debug)]
pub enum CallStep {
    Done(ToolOutcome),
    /// The tool started a background job; `started` is its immediate result.
    Job {
        job: u64,
        started: Value,
    },
}

/// A job's state as `jobs.list` reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobState {
    pub fraction: Option<f32>,
    pub status: String,
    pub finished: bool,
    /// The job's result once finished (`Err` text when it failed).
    pub result: Option<Result<Value, String>>,
}

impl JobState {
    /// Read one `jobs.list` entry (`{id, progress, status, finished, result}`).
    pub fn from_json(j: &Value) -> Self {
        let finished = j.get("finished").and_then(Value::as_bool).unwrap_or(false);
        let result = finished.then(|| {
            let r = j.get("result").cloned().unwrap_or(Value::Null);
            match r.get("error").and_then(Value::as_str) {
                Some(e) if r.as_object().is_some_and(|o| o.len() == 1) => Err(e.to_string()),
                _ => Ok(r),
            }
        });
        JobState {
            fraction: j.get("progress").and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0) as f32),
            status: j.get("status").and_then(Value::as_str).unwrap_or("").chars().take(200).collect(),
            finished,
            result,
        }
    }
}

/// The tool list for the model, from `tools.list` (`[{name, description, inputSchema, …}]`).
/// Schemas go through [`filmcraft_engine::tools::schema::for_strict_api`] (strict tool use rejects
/// `minimum`, `maxItems`… which the engine still enforces). Malformed entries are skipped.
pub fn tools_from_list(list: &Value) -> Vec<ToolSpec> {
    let entries = list.as_array().or_else(|| list.get("tools").and_then(Value::as_array));
    let mut specs = Vec::new();
    for t in entries.into_iter().flatten().take(256) {
        let Some(name) = t.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) else { continue };
        let schema = t.get("inputSchema").or_else(|| t.get("input_schema")).filter(|s| s.is_object()).cloned().unwrap_or_else(|| json!({"type": "object"}));
        let description = t.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        let input_schema = filmcraft_engine::tools::schema::for_strict_api(&schema);
        specs.push(ToolSpec { name: name.to_string(), description, input_schema, strict: true, eager_input_streaming: false });
    }
    specs
}

/// Who decides which calls need the user's click: the engine catalogue's
/// [`filmcraft_engine::tools::approval_for`] (each tool's approval; for the escape hatch, the
/// policy of the commands it names), plus the "apply without asking" setting.
#[derive(Clone, Debug, Default)]
pub struct ToolPolicy {
    /// Settings ▸ Apply plans into a new sequence without asking.
    pub auto_apply_new_sequence: bool,
}

impl ToolPolicy {
    /// `Some(why)` when `call` must wait for the user's Allow.
    pub fn ask_reason(&self, call: &ToolCall) -> Option<String> {
        use filmcraft_engine::tools::Approval;
        if call.name == "apply_edit_plan" && self.auto_apply_new_sequence && plan_targets_new_sequence(&call.input) {
            return None;
        }
        let approval = filmcraft_engine::tools::approval_for(&call.name, &call.input);
        if approval == Approval::Never {
            return None;
        }
        Some(match call.name.as_str() {
            "command_run" | "command_batch" => {
                let ids = command_ids(call);
                let shown: Vec<&str> = ids.iter().take(8).map(String::as_str).collect();
                if shown.is_empty() { "Run engine commands".to_string() } else { format!("Run {}", shown.join(", ")) }
            }
            "export_variations" => export_reason(&call.input),
            name if approval == Approval::AskIfOverwrite => format!("{} may write or overwrite a file", pretty_tool_name(name)),
            name => format!("{} changes the project", pretty_tool_name(name)),
        })
    }
}

/// What an `export_variations` call writes: `Export 3 sequences as "YouTube 1080p Full HD" to
/// /path` (and whether it replaces files). Hostile values are cut short.
fn export_reason(input: &Value) -> String {
    let short = |key: &str| -> String {
        let v: String = input.get(key).and_then(Value::as_str).unwrap_or("?").chars().take(160).collect();
        v
    };
    let n = input.get("sequences").and_then(Value::as_array).map_or(0, Vec::len);
    let replace = if input.get("overwrite").and_then(Value::as_bool) == Some(true) { ", replacing files that exist" } else { "" };
    format!("Export {n} sequence{} as \"{}\" to {}{replace}", if n == 1 { "" } else { "s" }, short("preset"), short("folder"))
}

/// The command ids an escape-hatch call names (`command_run {id}`, `command_batch {steps:[{id}]}`).
fn command_ids(call: &ToolCall) -> Vec<String> {
    let one = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_string);
    if call.name == "command_batch" {
        call.input.get("steps").and_then(Value::as_array).into_iter().flatten().take(256).filter_map(one).collect()
    } else {
        one(&call.input).into_iter().collect()
    }
}

/// Whether an `apply_edit_plan` input writes into a new sequence (the plan's `output.mode` is
/// absent or `newSequence`). Unreadable plans count as not new, so they ask.
pub fn plan_targets_new_sequence(input: &Value) -> bool {
    let plan = match input.get("plan") {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => return false,
        },
        Some(v @ Value::Object(_)) => v.clone(),
        _ => return false,
    };
    match plan.get("output").map(|o| o.get("mode").unwrap_or(o)) {
        None | Some(Value::Null) => true,
        Some(Value::String(m)) => m.replace(['_', '-'], "").eq_ignore_ascii_case("newsequence"),
        Some(Value::Object(o)) => !o.contains_key("mode"),
        Some(_) => false,
    }
}

/// `find_silences` → `Find silences`.
pub fn pretty_tool_name(name: &str) -> String {
    let s = name.replace(['_', '.'], " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Convert a `tools.call` result (`{result, images: [{png, width, height, mimeType}], job}`) into a
/// step.
pub fn call_step(v: Value) -> CallStep {
    let job = v.get("job").and_then(Value::as_u64).or_else(|| v.get("pendingJob").and_then(|j| j.as_u64().or_else(|| j.get("id").and_then(Value::as_u64))));
    let images: Vec<ToolImage> = v
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|i| {
            let data = i.get("png").or_else(|| i.get("pngBase64")).or_else(|| i.get("data")).and_then(Value::as_str)?;
            let media_type = i.get("mimeType").or_else(|| i.get("mediaType")).and_then(Value::as_str).unwrap_or("image/png").to_string();
            Some(ToolImage { media_type, data_base64: data.to_string() })
        })
        .collect();
    let json = match v.get("result").or_else(|| v.get("json")) {
        Some(r) => r.clone(),
        None => v.clone(),
    };
    match job {
        Some(job) => CallStep::Job { job, started: json },
        None => CallStep::Done(ToolOutcome { json, images }),
    }
}

/// The worker's [`ToolHost`]: forwards everything to the UI thread.
pub struct UiHost {
    pub tools: Vec<ToolSpec>,
    pub policy: ToolPolicy,
    pub tx: Sender<AssistantRequest>,
    pub cancel: Arc<AtomicBool>,
    /// Wakes the UI so it answers promptly.
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

/// Wait for a reply, giving up when `cancel` is set or the UI side went away.
fn wait<T>(rx: &std::sync::mpsc::Receiver<T>, cancel: &AtomicBool, limit: Duration) -> Result<T, String> {
    let mut waited = Duration::ZERO;
    loop {
        match rx.recv_timeout(WAIT_SLICE) {
            Ok(v) => return Ok(v),
            Err(RecvTimeoutError::Disconnected) => return Err("the app stopped this tool call".into()),
            Err(RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled by the user".into());
                }
                waited = waited.saturating_add(WAIT_SLICE);
                if waited >= limit {
                    return Err("timed out waiting for the app".into());
                }
            }
        }
    }
}

impl UiHost {
    fn send(&self, req: AssistantRequest) -> Result<(), String> {
        self.tx.send(req).map_err(|_| "the Assistant panel closed".to_string())?;
        (self.wake)();
        Ok(())
    }

    fn follow_job(&mut self, job: u64, started: Value, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String> {
        let mut waited = Duration::ZERO;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = self.send(AssistantRequest::CancelJob { job });
                return Err("cancelled by the user".into());
            }
            let (tx, rx) = channel();
            self.send(AssistantRequest::PollJob { job, reply: tx })?;
            let st = wait(&rx, cancel, Duration::from_secs(60))??;
            if let Some(r) = st.result {
                return r.map(|result| ToolOutcome::json(json!({"started": started, "result": result})));
            }
            progress(ToolProgress { fraction: st.fraction, status: st.status });
            // pace the polls; a cancel lands within one slice
            let (_keep, idle) = channel::<()>();
            match idle.recv_timeout(POLL_EVERY) {
                Ok(()) | Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
            }
            waited = waited.saturating_add(POLL_EVERY);
            if waited >= JOB_LIMIT {
                let _ = self.send(AssistantRequest::CancelJob { job });
                return Err("the job ran too long and was stopped".into());
            }
        }
    }
}

impl ToolHost for UiHost {
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools.clone()
    }

    fn authorize(&mut self, call: &ToolCall) -> Authorization {
        let Some(reason) = self.policy.ask_reason(call) else { return Authorization::Allow };
        let (tx, rx) = channel();
        if let Err(e) = self.send(AssistantRequest::Approve { call: call.clone(), reason, reply: tx }) {
            return Authorization::Deny(e);
        }
        // the user may take their time: no limit but Cancel
        match wait(&rx, &self.cancel, Duration::MAX) {
            Ok(true) => Authorization::Allow,
            Ok(false) => Authorization::Deny("the user declined".into()),
            Err(e) => Authorization::Deny(e),
        }
    }

    fn call(&mut self, call: &ToolCall, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String> {
        let (tx, rx) = channel();
        self.send(AssistantRequest::Call { call: call.clone(), reply: tx })?;
        match wait(&rx, cancel, Duration::from_secs(15 * 60))?? {
            CallStep::Done(o) => Ok(o),
            CallStep::Job { job, started } => self.follow_job(job, started, cancel, progress),
        }
    }
}

/// What the worker sends the UI.
pub enum WorkerMsg {
    Event(filmcraft_agent::AgentEvent),
    /// The turn is over; the conversation now includes it.
    Finished {
        conversation: filmcraft_agent::Conversation,
        end: filmcraft_agent::TurnEnd,
    },
    /// The worker panicked (a bug): the turn is lost, the app keeps running.
    Panicked(String),
}

/// Everything one turn needs.
pub struct TurnArgs {
    pub provider: Arc<dyn filmcraft_llm::LlmProvider>,
    pub config: filmcraft_agent::AgentConfig,
    pub conversation: filmcraft_agent::Conversation,
    pub user: Vec<filmcraft_llm::ContentBlock>,
    pub host: UiHost,
    pub events: Sender<WorkerMsg>,
}

/// Run one turn on this thread (the worker body), under `catch_unwind`.
pub fn run_turn_guarded(args: TurnArgs) {
    let TurnArgs { provider, config, mut conversation, user, mut host, events } = args;
    let cancel = host.cancel.clone();
    let wake = host.wake.clone();
    let tx = events.clone();
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut on_event = |e: filmcraft_agent::AgentEvent| {
            let _ = tx.send(WorkerMsg::Event(e));
            wake();
        };
        filmcraft_agent::run_turn(&config, &mut conversation, provider.as_ref(), &mut host, user, &mut on_event, &cancel)
    }));
    let msg = match ran {
        Ok(end) => WorkerMsg::Finished { conversation, end },
        Err(_) => WorkerMsg::Panicked(crate::crash::take_last().unwrap_or_else(|| "internal error in the Assistant".into())),
    };
    let _ = events.send(msg);
    (host.wake)();
}

/// Start a turn on a worker thread.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn(args: TurnArgs) -> Result<(), String> {
    std::thread::Builder::new()
        .name("assistant".into())
        .spawn(move || run_turn_guarded(args))
        .map(|_| ())
        .map_err(|e| format!("could not start the Assistant: {e}"))
}

/// No threads on the web.
#[cfg(target_arch = "wasm32")]
pub fn spawn(_args: TurnArgs) -> Result<(), String> {
    Err("The Assistant runs in the desktop app only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: name.into(), input }
    }

    #[test]
    fn tool_list_and_policy() {
        let list = json!([
            {"name": "project_overview", "description": "d", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}, "approval": "never"},
            {"name": "export", "inputSchema": {"type": "object"}, "approval": "askIfOverwrite"},
            {"name": "apply_edit_plan", "approval": "ask"},
            {"description": "no name"},
            7
        ]);
        let specs = tools_from_list(&list);
        assert_eq!(specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["project_overview", "export", "apply_edit_plan"]);
        assert!(specs.iter().all(|s| s.strict && s.input_schema.is_object()));
        let mut p = ToolPolicy { auto_apply_new_sequence: false };
        assert_eq!(p.ask_reason(&call("project_overview", json!({}))), None);
        assert!(p.ask_reason(&call("export", json!({"path": "/x.mp4"}))).is_some());
        let plan = json!({"plan": "{\"cuts\": []}", "source_hash": null});
        assert!(p.ask_reason(&call("apply_edit_plan", plan.clone())).is_some());
        p.auto_apply_new_sequence = true;
        assert_eq!(p.ask_reason(&call("apply_edit_plan", plan)), None);
        let in_place = json!({"plan": "{\"output\": {\"mode\": \"inPlace\"}}"});
        assert!(p.ask_reason(&call("apply_edit_plan", in_place)).is_some());
        assert!(p.ask_reason(&call("apply_edit_plan", json!({"plan": "{broken"}))).is_some());
        // escape hatch: queries run, edits ask, unknown tools ask
        assert_eq!(p.ask_reason(&call("command_run", json!({"id": "jobs.list"}))), None);
        assert!(p.ask_reason(&call("command_run", json!({"id": "sequence.addEdit"}))).unwrap().contains("sequence.addEdit"));
        assert!(p.ask_reason(&call("command_batch", json!({"steps": [{"id": "jobs.list"}, {"id": "edit.undo"}]}))).is_some());
        assert_eq!(p.ask_reason(&call("command_batch", json!({"steps": [{"id": "jobs.list"}]}))), None);
        assert!(p.ask_reason(&call("no_such_tool", json!({}))).is_some());
        // exporting variations always asks, and says where the files go
        let export = json!({"sequences": [4, 5], "preset": "YouTube 1080p Full HD", "folder": "/Users/a/Out", "overwrite": null});
        assert_eq!(p.ask_reason(&call("export_variations", export)).as_deref(), Some("Export 2 sequences as \"YouTube 1080p Full HD\" to /Users/a/Out"));
        let replace = json!({"sequences": [4], "preset": "x", "folder": "y".repeat(10_000), "overwrite": true});
        let why = p.ask_reason(&call("export_variations", replace)).unwrap();
        assert!(why.ends_with("replacing files that exist") && why.len() < 300, "{why}");
        assert_eq!(pretty_tool_name("find_silences"), "Find silences");
        assert_eq!(pretty_tool_name(""), "");
    }

    #[test]
    fn call_results_and_jobs_parse() {
        match call_step(json!({"result": {"a": 1}, "images": [{"png": "iVBO", "width": 2, "height": 2, "mimeType": "image/png"}, {"width": 1}], "job": null})) {
            CallStep::Done(o) => {
                assert_eq!(o.json, json!({"a": 1}));
                assert_eq!(o.images.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(call_step(json!({"result": {}, "images": [], "job": 4})), CallStep::Job { job: 4, .. }));
        let running = JobState::from_json(&json!({"id": 4, "progress": 7.0, "status": "Transcribing", "finished": false, "result": null}));
        assert_eq!(running.fraction, Some(1.0));
        assert!(!running.finished && running.result.is_none());
        let failed = JobState::from_json(&json!({"finished": true, "result": {"error": "no audio"}}));
        assert_eq!(failed.result, Some(Err("no audio".into())));
        let done = JobState::from_json(&json!({"finished": true, "result": {"words": 3}}));
        assert_eq!(done.result, Some(Ok(json!({"words": 3}))));
        assert_eq!(JobState::from_json(&json!("junk")), JobState::default());
    }

    #[test]
    fn host_round_trips_and_cancels() {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut host = UiHost { tools: Vec::new(), policy: ToolPolicy::default(), tx, cancel: cancel.clone(), wake: Arc::new(|| {}) };
        let ui = std::thread::spawn(move || {
            let mut polls = 0;
            while let Ok(req) = rx.recv() {
                match req {
                    AssistantRequest::Call { reply, .. } => {
                        let _ = reply.send(Ok(CallStep::Job { job: 9, started: json!({"job": 9}) }));
                    }
                    AssistantRequest::PollJob { reply, .. } => {
                        polls += 1;
                        let st = if polls < 2 {
                            JobState { fraction: Some(0.5), status: "half".into(), ..Default::default() }
                        } else {
                            JobState { finished: true, result: Some(Ok(json!(1))), ..Default::default() }
                        };
                        let _ = reply.send(Ok(st));
                    }
                    AssistantRequest::Approve { reply, .. } => {
                        let _ = reply.send(false);
                    }
                    AssistantRequest::CancelJob { .. } => {}
                }
            }
            polls
        });
        let mut seen = Vec::new();
        let flag = AtomicBool::new(false);
        let out = host.call(&call("transcribe", json!({})), &flag, &mut |p| seen.push(p)).unwrap();
        assert_eq!(out.json["result"], json!(1));
        assert_eq!(seen.len(), 1);
        assert_eq!(host.authorize(&call("command_run", json!({"id": "edit.undo"}))), Authorization::Deny("the user declined".into()));
        // cancel while waiting for an answer that never comes
        let (tx2, _rx2) = channel();
        let mut stuck = UiHost { tools: Vec::new(), policy: ToolPolicy::default(), tx: tx2, cancel: cancel.clone(), wake: Arc::new(|| {}) };
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(stuck.authorize(&call("command_run", json!({"id": "edit.undo"}))), Authorization::Deny(_)));
        assert!(stuck.call(&call("x", json!({})), &cancel, &mut |_| {}).is_err());
        drop(host);
        assert_eq!(ui.join().unwrap(), 2);
    }
}
