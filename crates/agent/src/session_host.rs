//! [`SessionHost`]: a [`ToolHost`] that owns an engine [`Session`] directly, for the CLI
//! (`filmcraft-cli assistant`), tests and evals. The desktop app uses its own host that forwards
//! calls to the UI thread instead; both run the same engine tool catalogue
//! ([`filmcraft_engine::tools`]) and the same approval rules.
//!
//! Background jobs (transcription, analysis, export, the `export_variations` batch, which is one
//! job standing for several export-queue items) are followed until they finish: the host polls
//! the session (which applies finished results), reports progress, and cancels the job when the
//! user cancels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_engine::tools::{self, Approval};
use filmcraft_llm::ToolSpec;
use serde_json::{Value, json};

use crate::host::{Authorization, ToolCall, ToolHost, ToolImage, ToolOutcome, ToolProgress};

/// Asks the user about a call that needs approval (`true` = allow). The string says why it asks.
pub type Approver<'a> = Box<dyn FnMut(&ToolCall, &str) -> bool + 'a>;

/// Runs the engine tool catalogue on a session it borrows.
pub struct SessionHost<'a> {
    pub session: &'a mut Session,
    approve: Approver<'a>,
    /// Longest wait for one background job.
    pub job_timeout: Duration,
    poll: Duration,
}

impl<'a> SessionHost<'a> {
    /// A host that asks `approve` for every call that needs approval.
    pub fn new(session: &'a mut Session, approve: Approver<'a>) -> Self {
        Self { session, approve, job_timeout: Duration::from_secs(4 * 3600), poll: Duration::from_millis(100) }
    }

    /// A host that declines every call needing approval (safe default for unattended runs).
    pub fn read_mostly(session: &'a mut Session) -> Self {
        Self::new(session, Box::new(|_, _| false))
    }

    fn job_state(&mut self, job: u64) -> Option<Value> {
        let jobs = self.session.execute("jobs.list", json!({})).ok()?;
        jobs.as_array()?.iter().find(|j| j["id"].as_u64() == Some(job)).cloned()
    }

    /// Follow `job` until it finishes; `Err` on failure, cancel or timeout.
    fn wait_job(&mut self, job: u64, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<Value, String> {
        let start = Instant::now();
        loop {
            self.session.poll_persistence();
            let Some(state) = self.job_state(job) else {
                // jobs that apply their result on completion may leave the list
                return Ok(Value::Null);
            };
            let fraction = state["progress"].as_f64().map(|f| f.clamp(0.0, 1.0) as f32);
            progress(ToolProgress { fraction, status: state["status"].as_str().unwrap_or_default().to_string() });
            if state["finished"].as_bool() == Some(true) {
                // let the session store the job's result (transcripts, analyses)
                self.session.poll_persistence();
                let result = state["result"].clone();
                if let Some(e) = result.get("error").and_then(Value::as_str) {
                    return Err(e.to_string());
                }
                return Ok(result);
            }
            if cancel.load(Ordering::Relaxed) || start.elapsed() > self.job_timeout {
                let _ = self.session.execute("jobs.cancel", json!({"job": job}));
                for _ in 0..600 {
                    self.session.poll_persistence();
                    if self.job_state(job).is_none_or(|j| j["finished"].as_bool() == Some(true)) {
                        break;
                    }
                    std::thread::sleep(self.poll);
                }
                return Err(if cancel.load(Ordering::Relaxed) { "cancelled by the user".into() } else { "the job took too long and was stopped".into() });
            }
            std::thread::sleep(self.poll);
        }
    }
}

/// The catalogue tools available in this build, as the model sees them.
pub fn tool_specs(session: &Session) -> Vec<ToolSpec> {
    tools::catalogue_available(session)
        .into_iter()
        .map(|d| ToolSpec {
            name: d.name.to_string(),
            description: d.description.to_string(),
            input_schema: tools::schema::for_strict_api(&d.schema_value()),
            strict: true,
            eager_input_streaming: false,
        })
        .collect()
}

/// Convert an engine tool output into the agent's outcome.
pub fn outcome_of(out: tools::ToolOutput) -> ToolOutcome {
    let v = out.to_json();
    let images = v["images"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    Some(ToolImage { media_type: i["mimeType"].as_str().unwrap_or("image/png").to_string(), data_base64: i["png"].as_str()?.to_string() })
                })
                .collect()
        })
        .unwrap_or_default();
    ToolOutcome { json: out.json, images }
}

impl ToolHost for SessionHost<'_> {
    fn tools(&self) -> Vec<ToolSpec> {
        tool_specs(self.session)
    }

    fn authorize(&mut self, call: &ToolCall) -> Authorization {
        match tools::approval_for(&call.name, &call.input) {
            Approval::Never => Authorization::Allow,
            a => {
                let why = match a {
                    Approval::AskIfOverwrite => "it may overwrite a file",
                    // export_variations: a batch of files, refused over existing ones unless asked
                    _ if tools::find(&call.name).is_some_and(|d| d.destructive) => "it writes files",
                    _ => "it changes the project or writes files",
                };
                if (self.approve)(call, why) { Authorization::Allow } else { Authorization::Deny("the user declined".into()) }
            }
        }
    }

    fn call(&mut self, call: &ToolCall, cancel: &AtomicBool, progress: &mut dyn FnMut(ToolProgress)) -> Result<ToolOutcome, String> {
        let out = tools::call(self.session, &call.name, &call.input).map_err(|e| e.to_string())?;
        let Some(job) = out.pending_job else { return Ok(outcome_of(out)) };
        let finished = self.wait_job(job, cancel, progress)?;
        // a finished analysis is read back so the model gets the profile, not just "done"
        if call.name == "analyze_media"
            && let Some(item) = call.input.get("item")
            && let Ok(profile) = self.session.execute("media.analysis", json!({"item": item}))
        {
            return Ok(ToolOutcome::json(tools::cap(json!({"job": job, "finished": true, "profile": profile}))));
        }
        Ok(ToolOutcome::json(json!({"job": job, "finished": true, "result": finished, "started": out.json})))
    }
}
