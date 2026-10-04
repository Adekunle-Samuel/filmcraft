//! Long exports over MCP (docs/agents.md § Long exports): `file.exportMedia` with `wait: true`
//! runs as a background engine job; the server reports its progress as MCP
//! `notifications/progress` (when the request carried a `progressToken`) and stops it on
//! `notifications/cancelled`, deleting the partial output. The session lock is only held for
//! short polls, so other requests are answered while the export runs.

use std::path::Path;
use std::time::{Duration, SystemTime};

use rmcp::RoleServer;
use rmcp::model::{CallToolResult, ContentBlock as Content, ProgressNotificationParam, ProgressToken};
use rmcp::service::{Peer, RequestContext};
use serde_json::{Value, json};

use crate::server::FilmcraftMcp;

/// Engine commands that block until a render is written when called with `wait: true`.
pub const LONG_COMMANDS: &[&str] = &["file.exportMedia"];

/// How often the job is polled (and at most how often progress is reported).
const POLL: Duration = Duration::from_millis(100);

/// Whether `command_run {id, params}` is a long call this module runs.
pub fn is_long(id: &str, params: &Value) -> bool {
    LONG_COMMANDS.contains(&id) && params.get("wait").and_then(Value::as_bool) == Some(true)
}

/// Sends `notifications/progress` for one request; progress only ever increases.
struct Reporter {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    last: f64,
}

impl Reporter {
    async fn report(&mut self, done: f64, total: f64, message: &str) {
        let Some(token) = self.token.clone() else { return };
        if total <= 0.0 || done <= self.last {
            return;
        }
        self.last = done;
        let mut p = ProgressNotificationParam::new(token, done).with_total(total);
        if !message.is_empty() {
            p = p.with_message(message);
        }
        let _ = self.peer.notify_progress(p).await;
    }
}

impl FilmcraftMcp {
    /// Run `id` (a [`LONG_COMMANDS`] entry, `wait: true`) as a job, reporting progress and
    /// honouring cancellation.
    pub(crate) async fn run_long(&self, id: &str, mut params: Value, context: &RequestContext<RoleServer>) -> CallToolResult {
        let started = SystemTime::now();
        if let Some(p) = params.as_object_mut() {
            p.insert("wait".into(), json!(false));
        }
        let start = match self.run(id, params).await {
            Ok(v) => v,
            Err(e) => return CallToolResult::error(vec![Content::text(e.to_string())]),
        };
        let Some(job) = start.get("job").and_then(Value::as_u64) else {
            return CallToolResult::error(vec![Content::text(format!("{id} started no job: {start}"))]);
        };
        let path = start.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut rep = Reporter { peer: context.peer.clone(), token: context.meta.get_progress_token(), last: 0.0 };
        loop {
            let state = self.job_state(job).await;
            let finished = state.as_ref().is_none_or(|j| j["finished"].as_bool() == Some(true));
            if let Some(j) = &state {
                let (done, total) = (j["done"].as_f64().unwrap_or(0.0), j["total"].as_f64().unwrap_or(0.0));
                rep.report(done, total, j["status"].as_str().unwrap_or_default()).await;
            }
            if finished {
                let result = state.map(|j| j["result"].clone()).unwrap_or(Value::Null);
                if let Some(e) = result.get("error").and_then(Value::as_str) {
                    return CallToolResult::error(vec![Content::text(format!("export failed: {e}"))]);
                }
                let mut out = start;
                if let Some(o) = out.as_object_mut() {
                    o.insert("result".into(), result);
                }
                return CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&out).unwrap_or_default())]);
            }
            tokio::select! {
                _ = context.ct.cancelled() => break,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
        // Cancelled: stop the encode at the next batch, wait for the worker, delete the partial file.
        let _ = self.run("jobs.cancel", json!({"job": job})).await;
        for _ in 0..600 {
            if self.job_state(job).await.is_none_or(|j| j["finished"].as_bool() == Some(true)) {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        remove_partial(&path, started);
        CallToolResult::error(vec![Content::text("cancelled")])
    }

    /// The job's `jobs.list` entry.
    async fn job_state(&self, job: u64) -> Option<Value> {
        let jobs = self.run("jobs.list", json!({})).await.ok()?;
        jobs.as_array()?.iter().find(|j| j["id"].as_u64() == Some(job)).cloned()
    }
}

/// Delete what an interrupted export wrote: the output file and, for image sequences and caption
/// sidecars, the files next to it named `<stem>[digits].<ext>` — only those written since `since`.
pub fn remove_partial(path: &str, since: SystemTime) {
    let p = Path::new(path);
    let (Some(dir), Some(stem)) = (p.parent(), p.file_stem().map(|s| s.to_string_lossy().to_string())) else { return };
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // file times are coarser than the clock
    let since = since.checked_sub(Duration::from_secs(1)).unwrap_or(since);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let sibling = name.strip_prefix(&stem).is_some_and(|rest| rest.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with('.'));
        let ours = sibling && e.metadata().is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|t| t >= since));
        if ours {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
