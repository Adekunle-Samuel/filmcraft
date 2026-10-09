//! The curated, LLM-facing tool surface: a small set of tools with strict JSON schemas, shared by
//! the in-app Assistant and the MCP server (`tools.list` / `tools.call`).
//!
//! Tools are thin: each one validates its input against its schema ([`schema::validate`]) and runs
//! engine commands through [`Session::execute`], so undo, the journal and the never-crash guard
//! apply as for any other caller. The escape hatch (`command_run`, `command_batch`) reaches every
//! command, filtered by [`policy`]. Approvals are the host's job: a tool's [`Approval`] (and, for
//! the escape hatch, [`approval_for`]) says when to ask the user before calling it.
//!
//! Some tools are backed by commands that are still being built; they carry the command id in
//! [`ToolDef::requires`] and are left out of [`catalogue_available`] until it is registered.

pub mod schema;

mod defs;

use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// When the host asks the user before running a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// Runs without asking (read-only, or an undoable edit).
    Never,
    /// Always asks.
    Ask,
    /// Asks when the call would overwrite a file (the path in the input exists).
    AskIfOverwrite,
}

impl Approval {
    pub fn as_str(self) -> &'static str {
        match self {
            Approval::Never => "never",
            Approval::Ask => "ask",
            Approval::AskIfOverwrite => "askIfOverwrite",
        }
    }
}

/// A PNG a tool returns (for a vision model).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What a tool returns: JSON for the model, images, and a background job still running.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolOutput {
    pub json: Value,
    pub images: Vec<ToolImage>,
    /// A job (`jobs.list` id) the call started: the host follows its progress and can cancel it.
    pub pending_job: Option<u64>,
}

impl ToolOutput {
    pub fn json(json: Value) -> Self {
        Self { json, ..Default::default() }
    }

    /// The output as one JSON value (images as base64 PNG), as `tools.call` returns it.
    pub fn to_json(&self) -> Value {
        json!({
            "result": self.json,
            "images": self.images.iter().map(|i| json!({"png": crate::frames::base64(&i.png), "width": i.width, "height": i.height, "mimeType": "image/png"})).collect::<Vec<_>>(),
            "job": self.pending_job,
        })
    }
}

type Run = fn(&mut Session, &Value) -> Result<ToolOutput>;

/// One tool.
pub struct ToolDef {
    /// snake_case, unique.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// JSON Schema of the input, in the strict subset ([`schema::check_strict`]).
    pub schema: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
    pub approval: Approval,
    /// The engine command the tool needs; the tool is unavailable while it is not registered.
    pub requires: &'static str,
    pub run: Run,
}

impl ToolDef {
    /// The input schema as JSON (`{}` if the literal were broken; a test parses every one).
    pub fn schema_value(&self) -> Value {
        serde_json::from_str(self.schema).unwrap_or_else(|_| json!({}))
    }

    /// Whether the build has the command the tool needs.
    pub fn available(&self) -> bool {
        crate::commands::find(self.requires).is_some()
    }

    /// The `tools.list` entry: name, title, description, schema, MCP annotations, approval.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.schema_value(),
            "annotations": {
                "title": self.title,
                "readOnlyHint": self.read_only,
                "destructiveHint": self.destructive,
                "idempotentHint": self.idempotent,
                "openWorldHint": self.open_world,
            },
            "approval": self.approval.as_str(),
        })
    }
}

/// Every tool, available or not, in a fixed order (prompt caches depend on it).
pub fn catalogue() -> &'static [ToolDef] {
    defs::TOOLS
}

/// The tools this build can run (their backing commands exist).
pub fn catalogue_available(_s: &Session) -> Vec<&'static ToolDef> {
    catalogue().iter().filter(|t| t.available()).collect()
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    catalogue().iter().find(|t| t.name == name)
}

/// Most characters of JSON a tool returns; longer results are cut and say so.
pub const MAX_RESULT_CHARS: usize = 16_000;

fn tool_err(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: "tools.call".into(), msg: msg.into() }
}

/// Check `input` against tool `name`'s schema, without running it.
pub fn validate(name: &str, input: &Value) -> Result<()> {
    let def = find(name).ok_or_else(|| unknown_tool(name))?;
    schema::validate(&def.schema_value(), input).map_err(|e| tool_err(format!("{name}: {e}")))
}

fn unknown_tool(name: &str) -> EngineError {
    let names: Vec<&str> = catalogue().iter().filter(|t| t.available()).map(|t| t.name).collect();
    tool_err(format!("unknown tool `{name}`; the tools are: {}", names.join(", ")))
}

/// Run tool `name` with `input` (validated strictly against its schema first). Approval is not
/// checked here: the host asks the user before calling (see [`ToolDef::approval`], [`approval_for`]).
pub fn call(s: &mut Session, name: &str, input: &Value) -> Result<ToolOutput> {
    let def = find(name).ok_or_else(|| unknown_tool(name))?;
    if !def.available() {
        return Err(tool_err(format!("tool `{name}` needs the command `{}`, which this build does not have yet", def.requires)));
    }
    schema::validate(&def.schema_value(), input).map_err(|e| tool_err(format!("{name}: {e}")))?;
    let mut out = (def.run)(s, input)?;
    out.json = cap(out.json);
    Ok(out)
}

/// A result longer than [`MAX_RESULT_CHARS`] becomes its first part with `truncated: true`.
pub fn cap(v: Value) -> Value {
    cap_to(v, MAX_RESULT_CHARS)
}

/// [`cap`] to `limit` characters.
pub fn cap_to(v: Value, limit: usize) -> Value {
    let text = v.to_string();
    if text.len() <= limit {
        return v;
    }
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    json!({"truncated": true, "chars": text.len(), "text": text.get(..end).unwrap_or_default()})
}

/// What the escape hatch may do with a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Runs without asking (queries).
    Allow,
    /// Asks the user first (edits).
    Ask,
    /// Never runs from a tool, with the reason.
    Deny(String),
}

/// Commands the escape hatch never runs (app-level, irreversible, or needing their own consent).
const DENY_EXACT: &[&str] = &["file.quit", "media.makeOffline", "project.removeUnused", "transcript.downloadModel", "tools.call"];
const DENY_PREFIX: &[&str] = &["app.", "prefs.", "shortcuts.", "file.close"];

/// The escape hatch's policy for command `id`: queries run, denylisted commands never run,
/// everything else asks first.
pub fn policy(id: &str) -> Policy {
    if DENY_EXACT.contains(&id) || DENY_PREFIX.iter().any(|p| id.starts_with(p)) {
        return Policy::Deny(format!("`{id}` changes the app or loses work; ask the user to do it themselves"));
    }
    match crate::commands::find(id) {
        None => Policy::Deny(format!("unknown command `{id}`; use command_search to find ids")),
        Some(c) if !c.journal => Policy::Allow,
        Some(_) => Policy::Ask,
    }
}

impl Policy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Policy::Allow => "allow",
            Policy::Ask => "ask",
            Policy::Deny(_) => "deny",
        }
    }
}

/// The approval one call needs: the tool's own, except for the escape hatch, where it follows the
/// [`policy`] of the command(s) named in `input` (a denied command is refused when it runs).
pub fn approval_for(name: &str, input: &Value) -> Approval {
    let Some(def) = find(name) else { return Approval::Ask };
    let ids: Vec<&str> = match name {
        "command_run" => input.get("id").and_then(Value::as_str).into_iter().collect(),
        "command_batch" => input.get("steps").and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.get("id").and_then(Value::as_str)).collect(),
        _ => return def.approval,
    };
    if ids.iter().all(|id| policy(id) == Policy::Allow) { Approval::Never } else { Approval::Ask }
}

pub(crate) fn commands() -> Vec<crate::CommandSpec> {
    vec![
        crate::CommandSpec {
            id: "tools.list",
            label: "List Agent Tools",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            run: |s, _| Ok(Value::Array(catalogue_available(s).iter().map(|t| t.to_json()).collect())),
            journal: false,
        },
        // Not journaled itself: the commands a tool runs are journaled as they run (replaying both
        // would apply each edit twice).
        crate::CommandSpec {
            id: "tools.call",
            label: "Call Agent Tool",
            menu: &[],
            shortcut: None,
            params: r#"{"name":str,"input":{}}"#,
            enabled: crate::commands::always,
            run: |s, p| {
                let name = p.get("name").and_then(Value::as_str).ok_or_else(|| tool_err("need `name`"))?;
                let input = p.get("input").cloned().unwrap_or_else(|| json!({}));
                call(s, name, &input).map(|o| o.to_json())
            },
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests;
