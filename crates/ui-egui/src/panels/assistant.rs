//! The Assistant panel: chat with an LLM agent that edits through the engine's tool catalogue.
//!
//! - **State.** [`AssistantPanelState`] (`UiState::panels.assistant`, serde) holds the draft and
//!   the [`AssistantSettings`]; the settings also persist in `<data dir>/assistant/settings.json`.
//!   The runtime ([`AssistantRuntime`], `FilmcraftApp::assistant`) is not serialized: the chat as
//!   shown, tool cards, pending approvals, usage and the worker. The API key is never in either:
//!   it comes from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` or `<data dir>/assistant/credentials.json`
//!   (0600, unencrypted) and only the app's provider factory reads it.
//! - **Turns.** Send starts a worker thread ([`super::assistant_host`]) that runs
//!   [`filmcraft_agent::run_turn`]. Its tool calls come back here as requests and run on the UI
//!   thread in [`drain`] (via `tools.call`), between frames. The turn's edits fold into one undo
//!   step ("Assistant: …") with `edit.historyMark` / `edit.collapseSince` when the engine has them.
//! - **Conversations** are saved after each turn to
//!   `<data dir>/assistant/conversations/<project hash | untitled>.json` and reloaded on open.
//! - **Provider.** The host installs [`crate::HostHooks::assistant_provider`] (the desktop app with
//!   `--features assistant`; tests install a scripted provider). Without it the panel says the
//!   Assistant is not available in this build.
//! - **Consent.** Nothing is sent until the user accepted the consent sheet for the current
//!   provider (changing provider or URL asks again).
//!
//! Control methods (`assistant.*`, see [`control`]) and automation ids (`assistant.*`) make all of
//! it agent-drivable.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use filmcraft_agent::{AgentConfig, AgentEvent, Conversation, ToolCall, ToolProgress, TurnEnd};
use filmcraft_llm::{ContentBlock, LlmProvider, Role, StreamEvent, ToolResultContent, Usage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::assistant_host::{self, AssistantRequest, CallStep, JobState, ToolPolicy, UiHost, WorkerMsg};
use crate::FilmcraftApp;

/// Builds the LLM provider for the current settings (see [`crate::HostHooks::assistant_provider`]).
pub type ProviderFactory = Box<dyn Fn(&AssistantSettings) -> Result<Arc<dyn LlmProvider>, String>>;

/// Shown when the build has no provider factory.
pub const NOT_AVAILABLE: &str = "The Assistant is not available in this build (build with --features assistant)";
/// The default base URL of the private, local option (Ollama's OpenAI-compatible server).
pub const OLLAMA_URL: &str = filmcraft_llm::openai::DEFAULT_BASE_URL;
/// Efforts offered in the settings.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Most chat items kept on screen (older ones scroll away; the conversation keeps everything).
const MAX_ITEMS: usize = 2000;
/// Longest text of one chat item.
const MAX_ITEM_CHARS: usize = 200_000;
/// Largest conversation file read back.
const MAX_CONVERSATION_BYTES: u64 = 64 << 20;
/// Time per frame spent answering the worker's requests.
const FRAME_BUDGET_MS: u128 = 40;

/// Assistant settings (Settings sheet). Never holds the API key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssistantSettings {
    /// `"anthropic"` or `"openai"` (any OpenAI-compatible server: Ollama, LM Studio…).
    pub provider: String,
    pub model: String,
    /// `low`, `medium`, `high`, `xhigh` or `max` (Anthropic); empty = provider default.
    pub effort: String,
    /// Provider URL; empty = the provider's default (`https://api.anthropic.com`, Ollama on this
    /// machine).
    pub base_url: String,
    /// Send downscaled frames (contact sheets) to the model.
    pub vision: bool,
    /// Stop a conversation once its estimated cost reaches this (US$).
    pub budget_usd: Option<f64>,
    /// Apply edit plans into a new sequence without asking.
    pub auto_apply_new_sequence: bool,
    /// The user accepted the consent sheet for this provider and URL.
    pub consented: bool,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: filmcraft_llm::DEFAULT_MODEL.into(),
            effort: "high".into(),
            base_url: String::new(),
            vision: true,
            budget_usd: None,
            auto_apply_new_sequence: false,
            consented: false,
        }
    }
}

impl AssistantSettings {
    pub fn is_openai(&self) -> bool {
        self.provider.eq_ignore_ascii_case("openai")
    }

    /// The URL requests go to.
    pub fn effective_base_url(&self) -> String {
        let u = self.base_url.trim();
        if !u.is_empty() {
            u.to_string()
        } else if self.is_openai() {
            OLLAMA_URL.to_string()
        } else {
            filmcraft_llm::anthropic::DEFAULT_BASE_URL.to_string()
        }
    }

    /// The host part of [`Self::effective_base_url`] (what the consent sheet names).
    pub fn host(&self) -> String {
        let u = self.effective_base_url();
        let rest = u.split_once("://").map_or(u.as_str(), |(_, r)| r);
        rest.split(['/', '?', '#']).next().unwrap_or(rest).to_string()
    }

    /// Requests stay on this machine.
    pub fn is_local(&self) -> bool {
        let h = self.host().to_ascii_lowercase();
        let h = h.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map_or(h.as_str(), |(h, _)| h).to_string();
        matches!(h.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    }

    /// The agent configuration for these settings (limits from the defaults).
    pub fn agent_config(&self) -> AgentConfig {
        let effort = serde_json::from_value(Value::String(self.effort.trim().to_ascii_lowercase())).ok();
        AgentConfig {
            model: if self.model.trim().is_empty() { filmcraft_llm::DEFAULT_MODEL.into() } else { self.model.trim().into() },
            effort,
            budget_usd: self.budget_usd.filter(|b| b.is_finite() && *b > 0.0),
            vision: self.vision,
            ..AgentConfig::default()
        }
    }

    /// Normalize hostile values (from `ui.set`, `assistant.settings.set` or the settings file).
    fn sanitize(&mut self) {
        if !self.is_openai() {
            self.provider = "anthropic".into();
        } else {
            self.provider = "openai".into();
        }
        self.model = self.model.chars().filter(|c| !c.is_control()).take(200).collect();
        self.effort = self.effort.trim().to_ascii_lowercase();
        if !self.effort.is_empty() && !EFFORTS.contains(&self.effort.as_str()) {
            self.effort = "high".into();
        }
        self.base_url = self.base_url.trim().chars().take(2048).collect();
        self.budget_usd = self.budget_usd.filter(|b| b.is_finite() && *b > 0.0).map(|b| b.min(1.0e6));
    }
}

/// `UiState::panels.assistant`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AssistantPanelState {
    /// The composer's text.
    pub draft: String,
    pub settings: AssistantSettings,
    /// Scroll the chat to the end on the next frame.
    pub scroll_to_bottom: bool,
    /// The settings sheet is open.
    pub show_settings: bool,
}

/// One row of the chat.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatItem {
    User(String),
    Assistant(String),
    /// Thinking summary / progress notes (dim).
    Thinking(String),
    Notice {
        text: String,
        error: bool,
    },
    Tool(ToolCard),
}

/// A plan card's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanStatus {
    Proposed,
    Applying,
    Rejected,
}

/// A tool call as shown in the chat.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCard {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub running: bool,
    pub progress: Option<ToolProgress>,
    /// `None` while running.
    pub ok: Option<bool>,
    pub denied: bool,
    pub summary: String,
    /// The full result (JSON) when known.
    pub result: Option<Value>,
    pub expanded: bool,
    /// Set for a successful `propose_edit_plan`.
    pub plan: Option<PlanStatus>,
}

impl ToolCard {
    fn new(id: &str, name: &str, input: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
            running: true,
            progress: None,
            ok: None,
            denied: false,
            summary: String::new(),
            result: None,
            expanded: false,
            plan: None,
        }
    }
}

/// A call waiting for the user's Allow / Deny.
pub struct PendingApproval {
    pub k: u64,
    pub call: ToolCall,
    pub reason: String,
    reply: Sender<bool>,
}

struct Worker {
    cancel: Arc<AtomicBool>,
    events: Receiver<WorkerMsg>,
    requests: Receiver<AssistantRequest>,
    /// `edit.historyMark` result, folded at the end.
    mark: Option<Value>,
    label: String,
}

/// The Assistant's runtime state (`FilmcraftApp::assistant`; not serialized).
#[derive(Default)]
pub struct AssistantRuntime {
    pub items: Vec<ChatItem>,
    pub conversation: Conversation,
    pub approvals: Vec<PendingApproval>,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
    /// The API key being typed in the settings sheet (masked; cleared once stored).
    pub key_draft: String,
    /// Result of the last key save: (message, is error).
    pub key_message: Option<(String, bool)>,
    /// The last error (send refused, provider missing…).
    pub error: Option<String>,
    worker: Option<Worker>,
    backlog: VecDeque<AssistantRequest>,
    results: HashMap<String, Value>,
    open_text: bool,
    next_k: u64,
    /// The conversation file key the chat was loaded for.
    loaded_key: Option<String>,
    settings_loaded: bool,
    saved_settings: Option<AssistantSettings>,
}

impl AssistantRuntime {
    pub fn running(&self) -> bool {
        self.worker.is_some()
    }

    fn push(&mut self, item: ChatItem) {
        self.items.push(item);
        if self.items.len() > MAX_ITEMS {
            let n = self.items.len() - MAX_ITEMS;
            self.items.drain(..n);
        }
    }

    fn append_text(&mut self, thinking: bool, t: &str) {
        let target = match self.items.last_mut() {
            Some(ChatItem::Assistant(s)) if !thinking && self.open_text => Some(s),
            Some(ChatItem::Thinking(s)) if thinking && self.open_text => Some(s),
            _ => None,
        };
        match target {
            Some(s) => {
                if s.len() < MAX_ITEM_CHARS {
                    s.push_str(t);
                }
            }
            None => {
                self.push(if thinking { ChatItem::Thinking(t.to_string()) } else { ChatItem::Assistant(t.to_string()) });
                self.open_text = true;
            }
        }
    }

    fn card_mut(&mut self, id: &str) -> Option<&mut ToolCard> {
        self.items.iter_mut().rev().find_map(|i| match i {
            ChatItem::Tool(c) if c.id == id => Some(c),
            _ => None,
        })
    }

    /// Plan cards in order (their index is the `k` in `assistant.plan.<k>.*`).
    pub fn plan_cards(&self) -> Vec<&ToolCard> {
        self.items
            .iter()
            .filter_map(|i| match i {
                ChatItem::Tool(c) if c.plan.is_some() => Some(c),
                _ => None,
            })
            .collect()
    }

    fn deny_all(&mut self) {
        for a in self.approvals.drain(..) {
            let _ = a.reply.send(false);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// files

/// `<data dir>` (beside `preferences.json`), when the session persists anything.
pub fn data_dir(app: &FilmcraftApp) -> Option<PathBuf> {
    app.session.prefs_path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf)
}

/// `<data dir>/assistant/credentials.json`.
pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join("assistant").join("credentials.json")
}

/// The environment variable holding a provider's key.
pub fn key_env(provider: &str) -> &'static str {
    if provider.eq_ignore_ascii_case("openai") { "OPENAI_API_KEY" } else { "ANTHROPIC_API_KEY" }
}

/// The stored key for `provider` (`anthropic` / `openai`) from the credentials file.
pub fn read_api_key(data_dir: &Path, provider: &str) -> Option<String> {
    let path = credentials_path(data_dir);
    if std::fs::metadata(&path).ok()?.len() > 1 << 20 {
        return None;
    }
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    v.get(provider.to_ascii_lowercase()).and_then(Value::as_str).map(str::trim).filter(|k| !k.is_empty()).map(str::to_string)
}

/// The key for `provider`: the environment variable, else the credentials file.
pub fn api_key(data_dir: Option<&Path>, provider: &str) -> Option<String> {
    std::env::var(key_env(provider)).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).or_else(|| data_dir.and_then(|d| read_api_key(d, provider)))
}

/// Where the key would come from: `"environment"`, `"file"` or nothing.
pub fn key_source(data_dir: Option<&Path>, provider: &str) -> Option<&'static str> {
    if std::env::var(key_env(provider)).is_ok_and(|k| !k.trim().is_empty()) {
        Some("environment")
    } else if data_dir.and_then(|d| read_api_key(d, provider)).is_some() {
        Some("file")
    } else {
        None
    }
}

/// Store (or, with an empty key, remove) `provider`'s key in the credentials file (0600 on unix).
pub fn store_api_key(data_dir: &Path, provider: &str, key: &str) -> Result<(), String> {
    let provider = if provider.eq_ignore_ascii_case("openai") { "openai" } else { "anthropic" };
    let path = credentials_path(data_dir);
    let mut v: serde_json::Map<String, Value> =
        std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let key = key.trim();
    if key.is_empty() {
        v.remove(provider);
    } else {
        filmcraft_llm::ApiKey::new(key).map_err(|e| e.to_string())?;
        v.insert(provider.into(), Value::String(key.into()));
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(v)).map_err(|e| e.to_string())?;
    write_atomic(&path, &bytes, true).map_err(|e| format!("could not store the key: {e}"))
}

/// Write via a temporary file and a rename, so a crash never leaves half a file. `private`: owner
/// read/write only (unix).
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// The conversation file's key: a hash of the project path, or `untitled`.
pub fn conversation_key(project_path: Option<&str>) -> String {
    match project_path.filter(|p| !p.is_empty()) {
        None => "untitled".into(),
        Some(p) => {
            // FNV-1a 64: stable across runs and platforms
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in p.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            format!("{h:016x}")
        }
    }
}

fn conversation_path(app: &FilmcraftApp, key: &str) -> Option<PathBuf> {
    data_dir(app).map(|d| d.join("assistant").join("conversations").join(format!("{key}.json")))
}

fn settings_path(app: &FilmcraftApp) -> Option<PathBuf> {
    data_dir(app).map(|d| d.join("assistant").join("settings.json"))
}

fn save_conversation(app: &mut FilmcraftApp) {
    let Some(key) = app.assistant.loaded_key.clone() else { return };
    let Some(path) = conversation_path(app, &key) else { return };
    let r = if app.assistant.conversation.messages.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        serde_json::to_vec(&app.assistant.conversation).map_err(std::io::Error::other).and_then(|b| write_atomic(&path, &b, true))
    };
    if let Err(e) = r {
        log::warn!("assistant: could not save the conversation: {e}");
    }
}

/// Load the conversation for the open project when it changed (not while a turn runs).
fn sync_conversation(app: &mut FilmcraftApp) {
    if app.assistant.running() {
        return;
    }
    let key = conversation_key(app.session.path.as_deref());
    if app.assistant.loaded_key.as_deref() == Some(key.as_str()) {
        return;
    }
    let conv = conversation_path(app, &key)
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_CONVERSATION_BYTES))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Conversation>(&b).ok())
        .unwrap_or_default();
    let rt = &mut app.assistant;
    rt.deny_all();
    rt.items = items_from_conversation(&conv);
    rt.usage = conv.usage;
    rt.cost_usd = conv.cost_usd;
    rt.conversation = conv;
    rt.results.clear();
    rt.loaded_key = Some(key);
}

fn sync_settings(app: &mut FilmcraftApp) {
    if !app.assistant.settings_loaded {
        app.assistant.settings_loaded = true;
        if let Some(path) = settings_path(app)
            && let Ok(b) = std::fs::read(&path)
            && let Ok(mut s) = serde_json::from_slice::<AssistantSettings>(&b)
        {
            s.sanitize();
            app.ui.panels.assistant.settings = s;
        }
        app.assistant.saved_settings = Some(app.ui.panels.assistant.settings.clone());
        return;
    }
    if app.assistant.saved_settings.as_ref() == Some(&app.ui.panels.assistant.settings) {
        return;
    }
    app.ui.panels.assistant.settings.sanitize();
    let s = app.ui.panels.assistant.settings.clone();
    if let Some(path) = settings_path(app)
        && let Ok(b) = serde_json::to_vec_pretty(&s)
        && let Err(e) = write_atomic(&path, &b, false)
    {
        log::warn!("assistant: could not save settings: {e}");
    }
    app.assistant.saved_settings = Some(s);
}

/// The chat as shown, rebuilt from a saved conversation.
pub fn items_from_conversation(conv: &Conversation) -> Vec<ChatItem> {
    let mut items: Vec<ChatItem> = Vec::new();
    for m in &conv.messages {
        match m.role {
            Role::System => {}
            Role::User => {
                for b in &m.content {
                    match b {
                        ContentBlock::Text { text } if !text.trim().is_empty() => items.push(ChatItem::User(text.clone())),
                        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                            let text: String = content
                                .iter()
                                .filter_map(|c| match c {
                                    ToolResultContent::Text { text } => Some(text.as_str()),
                                    ToolResultContent::Image { .. } => None,
                                })
                                .collect();
                            let card = items.iter_mut().rev().find_map(|i| match i {
                                ChatItem::Tool(c) if c.id == *tool_use_id => Some(c),
                                _ => None,
                            });
                            if let Some(c) = card {
                                c.running = false;
                                c.ok = Some(!is_error);
                                c.denied = *is_error && text.starts_with("not run:");
                                c.summary = text.chars().take(160).collect();
                                c.result = serde_json::from_str(&text).ok();
                                if !is_error && c.name == "propose_edit_plan" {
                                    c.plan = Some(PlanStatus::Proposed);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Role::Assistant => {
                for b in &m.content {
                    match b {
                        ContentBlock::Text { text } if !text.trim().is_empty() => items.push(ChatItem::Assistant(text.clone())),
                        ContentBlock::ToolUse { id, name, input } => {
                            let mut c = ToolCard::new(id, name, input.clone());
                            c.running = false;
                            items.push(ChatItem::Tool(c));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for i in &mut items {
        if let ChatItem::Tool(c) = i
            && c.ok.is_none()
        {
            c.ok = Some(false);
            c.summary = "no result (the turn was interrupted)".into();
        }
    }
    let n = items.len().saturating_sub(MAX_ITEMS);
    items.drain(..n);
    items
}

// ---------------------------------------------------------------------------------------------
// turns

/// Whether the build can talk to a model.
pub fn available(app: &FilmcraftApp) -> bool {
    app.hooks.assistant_provider.is_some()
}

/// Send `text` as the next user message and start a turn.
pub fn send(app: &mut FilmcraftApp, ctx: &egui::Context, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("type a message first".into());
    }
    if text.chars().count() > 100_000 {
        return Err("the message is too long (100 000 characters at most)".into());
    }
    if app.assistant.running() {
        return Err("the Assistant is still working: wait for it, or press Cancel".into());
    }
    sync_settings(app);
    sync_conversation(app);
    let settings = app.ui.panels.assistant.settings.clone();
    let Some(factory) = app.hooks.assistant_provider.as_ref() else { return Err(NOT_AVAILABLE.into()) };
    if !settings.consented {
        return Err(format!("the Assistant needs your consent first: it sends project data to {} (see the Assistant panel)", settings.host()));
    }
    let provider = factory(&settings).inspect_err(|e| app.assistant.error = Some(e.clone()))?;

    // the tools and their approvals
    let tools = if filmcraft_engine::find_command("tools.list").is_some() {
        app.session.execute("tools.list", json!({})).map(|v| assistant_host::tools_from_list(&v)).unwrap_or_default()
    } else {
        Default::default()
    };
    let mark = if filmcraft_engine::find_command("edit.historyMark").is_some() { app.session.execute("edit.historyMark", json!({})).ok() } else { None };
    let label = format!("Assistant: {}", text.chars().take(40).collect::<String>());

    let cancel = Arc::new(AtomicBool::new(false));
    let (req_tx, req_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let wake_ctx = ctx.clone();
    let host = UiHost {
        tools,
        policy: ToolPolicy { auto_apply_new_sequence: settings.auto_apply_new_sequence },
        tx: req_tx,
        cancel: cancel.clone(),
        wake: Arc::new(move || wake_ctx.request_repaint()),
    };
    let args = assistant_host::TurnArgs {
        provider,
        config: settings.agent_config(),
        conversation: app.assistant.conversation.clone(),
        user: vec![ContentBlock::text(text)],
        host,
        events: ev_tx,
    };
    assistant_host::spawn(args).inspect_err(|e| app.assistant.error = Some(e.clone()))?;
    let rt = &mut app.assistant;
    rt.error = None;
    rt.open_text = false;
    rt.results.clear();
    rt.push(ChatItem::User(text.to_string()));
    rt.worker = Some(Worker { cancel, events: ev_rx, requests: req_rx, mark, label });
    app.ui.panels.assistant.scroll_to_bottom = true;
    ctx.request_repaint();
    Ok(())
}

/// Stop the running turn (pending approvals are denied).
pub fn cancel(app: &mut FilmcraftApp) {
    if let Some(w) = &app.assistant.worker {
        w.cancel.store(true, Ordering::Relaxed);
    }
    app.assistant.deny_all();
}

/// New conversation: cancel, clear the chat and forget the saved conversation.
pub fn reset(app: &mut FilmcraftApp) {
    cancel(app);
    // a running worker finishes in the background; its result is dropped
    app.assistant.worker = None;
    app.assistant.backlog.clear();
    let rt = &mut app.assistant;
    rt.items.clear();
    rt.conversation = Conversation::default();
    rt.usage = Usage::default();
    rt.cost_usd = None;
    rt.results.clear();
    rt.error = None;
    rt.open_text = false;
    if rt.loaded_key.is_none() {
        rt.loaded_key = Some(conversation_key(app.session.path.as_deref()));
    }
    save_conversation(app);
}

/// Answer approval `k`.
pub fn approve(app: &mut FilmcraftApp, k: u64, allow: bool) -> Result<(), String> {
    let i = app.assistant.approvals.iter().position(|a| a.k == k).ok_or_else(|| format!("no pending approval {k}"))?;
    let a = app.assistant.approvals.remove(i);
    let _ = a.reply.send(allow);
    Ok(())
}

/// Apply (`true`) or reject plan card `k`: answers a pending `apply_edit_plan` approval when there
/// is one, else asks the model to apply it (or notes the rejection for the next turn).
pub fn plan_action(app: &mut FilmcraftApp, ctx: &egui::Context, k: usize, apply: bool) -> Result<(), String> {
    let id = app.assistant.plan_cards().get(k).map(|c| c.id.clone()).ok_or_else(|| format!("no plan {k}"))?;
    if let Some(a) = app.assistant.approvals.iter().find(|a| a.call.name == "apply_edit_plan").map(|a| a.k) {
        approve(app, a, apply)?;
    } else if apply {
        send(app, ctx, "Apply the plan.")?;
    } else if !app.assistant.running() {
        app.assistant.conversation.push_note("The user rejected the proposed edit plan; do not apply it.");
        save_conversation(app);
    }
    if let Some(c) = app.assistant.card_mut(&id) {
        c.plan = Some(if apply { PlanStatus::Applying } else { PlanStatus::Rejected });
    }
    Ok(())
}

/// Called every frame from `FilmcraftApp::logic`: load settings and the conversation, answer the
/// worker's requests (tool calls run here, on the UI thread) and show its events.
pub fn drain(app: &mut FilmcraftApp, ctx: &egui::Context) {
    sync_settings(app);
    sync_conversation(app);
    let Some(w) = app.assistant.worker.as_ref() else { return };
    let mut gone = false;
    loop {
        match w.requests.try_recv() {
            Ok(r) => app.assistant.backlog.push_back(r),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    let mut msgs = Vec::new();
    loop {
        match w.events.try_recv() {
            Ok(m) => msgs.push(m),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                gone = true;
                break;
            }
        }
    }
    let t0 = web_time::Instant::now();
    while let Some(req) = app.assistant.backlog.pop_front() {
        answer(app, req);
        if t0.elapsed().as_millis() > FRAME_BUDGET_MS {
            ctx.request_repaint();
            break;
        }
    }
    let mut finished = false;
    for m in msgs {
        match m {
            WorkerMsg::Event(e) => apply_event(app, e),
            WorkerMsg::Finished { conversation, end } => {
                finish(app, Some(conversation), &end);
                finished = true;
            }
            WorkerMsg::Panicked(why) => {
                finish(app, None, &TurnEnd::Failed(format!("internal error: {why}")));
                finished = true;
            }
        }
    }
    if gone && !finished && app.assistant.worker.is_some() {
        finish(app, None, &TurnEnd::Failed("the Assistant stopped unexpectedly".into()));
    }
}

/// Run one worker request on the UI thread.
fn answer(app: &mut FilmcraftApp, req: AssistantRequest) {
    match req {
        AssistantRequest::Call { call, reply } => {
            let r = run_tool(app, &call);
            if let Ok(step) = &r {
                let shown = match step {
                    CallStep::Done(o) => o.json.clone(),
                    CallStep::Job { started, .. } => started.clone(),
                };
                app.assistant.results.insert(call.id.clone(), shown);
            }
            let _ = reply.send(r);
        }
        AssistantRequest::PollJob { job, reply } => {
            let r = app.session.execute("jobs.list", json!({})).map_err(|e| e.to_string()).and_then(|v| {
                v.as_array()
                    .and_then(|a| a.iter().find(|j| j.get("id").and_then(Value::as_u64) == Some(job)))
                    .map(JobState::from_json)
                    .ok_or_else(|| format!("job {job} is gone"))
            });
            if let Ok(JobState { result: Some(Ok(v)), .. }) = &r {
                // the card shows the job's result
                let id = app.assistant.items.iter().rev().find_map(|i| match i {
                    ChatItem::Tool(c) if c.running => Some(c.id.clone()),
                    _ => None,
                });
                if let Some(id) = id {
                    app.assistant.results.insert(id, v.clone());
                }
            }
            let _ = reply.send(r);
        }
        AssistantRequest::CancelJob { job } => {
            let _ = app.session.execute("jobs.cancel", json!({"job": job}));
        }
        AssistantRequest::Approve { call, reason, reply } => {
            let k = app.assistant.next_k;
            app.assistant.next_k += 1;
            app.assistant.approvals.push(PendingApproval { k, call, reason, reply });
            app.ui.panels.assistant.scroll_to_bottom = true;
        }
    }
}

/// `tools.call` on the UI thread.
fn run_tool(app: &mut FilmcraftApp, call: &ToolCall) -> Result<CallStep, String> {
    if filmcraft_engine::find_command("tools.call").is_none() {
        return Err("tool catalogue not available in this build".into());
    }
    app.session.execute("tools.call", json!({"name": call.name, "input": call.input})).map(assistant_host::call_step).map_err(|e| e.to_string())
}

fn apply_event(app: &mut FilmcraftApp, e: AgentEvent) {
    let rt = &mut app.assistant;
    match e {
        AgentEvent::Stream(StreamEvent::TextDelta(t)) => rt.append_text(false, &t),
        AgentEvent::Stream(StreamEvent::ThinkingDelta(t)) => rt.append_text(true, &t),
        AgentEvent::Stream(_) => {}
        AgentEvent::ModelCall { .. } => rt.open_text = false,
        AgentEvent::ToolStarted { id, name, input } => {
            rt.open_text = false;
            rt.push(ChatItem::Tool(ToolCard::new(&id, &name, input)));
        }
        AgentEvent::ToolProgress { id, progress } => {
            if let Some(c) = rt.card_mut(&id) {
                c.progress = Some(progress);
            }
        }
        AgentEvent::ToolFinished { id, name, ok, summary, .. } => {
            let result = rt.results.remove(&id);
            if let Some(c) = rt.card_mut(&id) {
                c.running = false;
                c.ok = Some(ok);
                c.summary = summary;
                c.result = if ok { result } else { None };
                if ok && name == "propose_edit_plan" {
                    c.plan = Some(PlanStatus::Proposed);
                }
            }
            rt.open_text = false;
        }
        AgentEvent::ToolDenied { id, name, reason } => {
            rt.open_text = false;
            let mut c = ToolCard::new(&id, &name, Value::Null);
            c.running = false;
            c.ok = Some(false);
            c.denied = true;
            c.summary = format!("not run: {reason}");
            if let Some(existing) = rt.card_mut(&id) {
                *existing = c;
            } else {
                rt.push(ChatItem::Tool(c));
            }
        }
        AgentEvent::Usage { total, cost_usd } => {
            rt.usage = total;
            rt.cost_usd = cost_usd;
        }
        AgentEvent::Notice(text) => {
            rt.open_text = false;
            rt.push(ChatItem::Notice { text, error: false });
        }
        AgentEvent::TurnEnded(_) => {}
    }
    app.ui.panels.assistant.scroll_to_bottom = true;
}

fn finish(app: &mut FilmcraftApp, conversation: Option<Conversation>, end: &TurnEnd) {
    let Some(w) = app.assistant.worker.take() else { return };
    app.assistant.backlog.clear();
    app.assistant.deny_all();
    if let Some(c) = conversation {
        app.assistant.usage = c.usage;
        app.assistant.cost_usd = c.cost_usd;
        app.assistant.conversation = c;
    }
    // tool cards left running (a cancel mid-call)
    for i in &mut app.assistant.items {
        if let ChatItem::Tool(c) = i
            && c.running
        {
            c.running = false;
            c.ok = Some(false);
            c.summary = "stopped".into();
        }
    }
    let notice = match end {
        TurnEnd::Done => None,
        TurnEnd::Cancelled => Some(("Stopped.".to_string(), false)),
        TurnEnd::Refused { explanation, .. } => {
            Some((format!("The model declined{}", explanation.as_deref().map(|e| format!(": {e}")).unwrap_or_default()), true))
        }
        TurnEnd::Limit(why) => Some((why.clone(), false)),
        TurnEnd::Failed(why) => Some((why.clone(), true)),
    };
    if let Some((text, error)) = notice {
        app.assistant.push(ChatItem::Notice { text, error });
    }
    app.assistant.open_text = false;
    // fold the turn's edits into one undo step
    if let Some(mark) = w.mark
        && filmcraft_engine::find_command("edit.collapseSince").is_some()
    {
        let mut p = if mark.is_object() { mark } else { json!({"mark": mark}) };
        p["label"] = Value::String(w.label);
        let _ = app.session.execute("edit.collapseSince", p);
    }
    save_conversation(app);
    app.ui.panels.assistant.scroll_to_bottom = true;
}

// ---------------------------------------------------------------------------------------------
// control channel

/// What `assistant.state` returns.
pub fn state_json(app: &FilmcraftApp) -> Value {
    let rt = &app.assistant;
    let mut plan_k = 0usize;
    let messages: Vec<Value> = rt
        .items
        .iter()
        .map(|i| match i {
            ChatItem::User(t) => json!({"role": "user", "text": t}),
            ChatItem::Assistant(t) => json!({"role": "assistant", "text": t}),
            ChatItem::Thinking(t) => json!({"role": "thinking", "text": t}),
            ChatItem::Notice { text, error } => json!({"role": if *error { "error" } else { "notice" }, "text": text}),
            ChatItem::Tool(c) => {
                let mut v = json!({"role": "tool", "id": c.id, "name": c.name, "text": c.summary});
                if c.plan.is_some() {
                    v["plan"] = json!(plan_k);
                    plan_k += 1;
                }
                v
            }
        })
        .collect();
    let tools: Vec<Value> = rt
        .items
        .iter()
        .filter_map(|i| match i {
            ChatItem::Tool(c) => Some(json!({
                "id": c.id,
                "name": c.name,
                "input": c.input,
                "running": c.running,
                "ok": c.ok,
                "denied": c.denied,
                "summary": c.summary,
                "progress": c.progress.as_ref().map(|p| json!({"fraction": p.fraction, "status": p.status})),
                "plan": c.plan,
            })),
            _ => None,
        })
        .collect();
    let approvals: Vec<Value> =
        rt.approvals.iter().enumerate().map(|(i, a)| json!({"index": i, "id": a.k, "tool": a.call.name, "input": a.call.input, "reason": a.reason})).collect();
    json!({
        "available": available(app),
        "consented": app.ui.panels.assistant.settings.consented,
        "running": rt.running(),
        "messages": messages,
        "tools": tools,
        "approvals": approvals,
        "usage": {
            "inputTokens": rt.usage.input_tokens,
            "outputTokens": rt.usage.output_tokens,
            "cacheReadTokens": rt.usage.cache_read_input_tokens,
            "cacheWriteTokens": rt.usage.cache_creation_input_tokens,
            "costUsd": rt.cost_usd,
        },
        "error": rt.error,
    })
}

/// Settings as `assistant.settings.get` returns them: never the key, only where one comes from.
pub fn settings_json(app: &FilmcraftApp) -> Value {
    let s = &app.ui.panels.assistant.settings;
    let mut v = serde_json::to_value(s).unwrap_or_default();
    v["host"] = Value::String(s.host());
    v["keySource"] = json!(key_source(data_dir(app).as_deref(), &s.provider));
    v
}

/// The `assistant.*` control methods.
pub fn control(app: &mut FilmcraftApp, ctx: &egui::Context, method: &str, p: &Value) -> Result<Value, String> {
    match method {
        "assistant.send" => {
            let text = p.get("text").and_then(Value::as_str).ok_or("need `text`")?;
            send(app, ctx, text)?;
            Ok(json!({"started": true}))
        }
        "assistant.state" => Ok(state_json(app)),
        "assistant.approve" => {
            let allow = p.get("allow").and_then(Value::as_bool).ok_or("need `allow` (true or false)")?;
            let k = match (p.get("index").and_then(Value::as_u64), p.get("id").and_then(Value::as_u64)) {
                (_, Some(id)) => id,
                (Some(i), None) => app
                    .assistant
                    .approvals
                    .get(usize::try_from(i).unwrap_or(usize::MAX))
                    .map(|a| a.k)
                    .ok_or_else(|| format!("no pending approval at index {i}"))?,
                (None, None) => app.assistant.approvals.first().map(|a| a.k).ok_or("no pending approval")?,
            };
            approve(app, k, allow)?;
            Ok(Value::Null)
        }
        "assistant.plan" => {
            let k = p.get("index").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).ok_or("need `index`")?;
            let apply = p.get("apply").and_then(Value::as_bool).ok_or("need `apply` (true or false)")?;
            plan_action(app, ctx, k, apply)?;
            Ok(Value::Null)
        }
        "assistant.cancel" => {
            cancel(app);
            Ok(Value::Null)
        }
        "assistant.reset" => {
            reset(app);
            Ok(Value::Null)
        }
        "assistant.settings.get" => Ok(settings_json(app)),
        "assistant.settings.set" => {
            let o = p.as_object().ok_or("params must be an object of settings")?;
            for k in o.keys() {
                let lower = k.to_ascii_lowercase();
                if lower.contains("key") || lower.contains("secret") || lower.contains("token") {
                    return Err("the API key is never set over the control channel: use the environment variable or Assistant ▸ Settings".into());
                }
                if k == "consented" {
                    return Err("consent is given by the user in the Assistant panel (assistant.consent.accept)".into());
                }
            }
            let mut cur = serde_json::to_value(&app.ui.panels.assistant.settings).map_err(|e| e.to_string())?;
            if let Some(c) = cur.as_object_mut() {
                for (k, v) in o {
                    c.insert(k.clone(), v.clone());
                }
            }
            let mut s: AssistantSettings = serde_json::from_value(cur).map_err(|e| format!("bad settings: {e}"))?;
            s.sanitize();
            set_settings(app, s);
            Ok(settings_json(app))
        }
        m => Err(format!("unknown method `{m}`")),
    }
}

/// Replace the settings; consent is withdrawn when the destination (provider or URL) changes.
pub fn set_settings(app: &mut FilmcraftApp, mut s: AssistantSettings) {
    let cur = &app.ui.panels.assistant.settings;
    if s.provider != cur.provider || s.effective_base_url() != cur.effective_base_url() {
        s.consented = false;
    }
    app.ui.panels.assistant.settings = s;
}

/// Switch to the private option: a local OpenAI-compatible server (Ollama).
pub fn use_local_model(app: &mut FilmcraftApp) {
    let mut s = app.ui.panels.assistant.settings.clone();
    s.provider = "openai".into();
    s.base_url = OLLAMA_URL.into();
    if s.model == filmcraft_llm::DEFAULT_MODEL || s.model.trim().is_empty() {
        s.model = "llama3.1".into();
    }
    s.effort = String::new();
    set_settings(app, s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_llm::Message;

    #[test]
    fn settings_hosts_and_sanitize() {
        let mut s = AssistantSettings::default();
        assert_eq!(s.host(), "api.anthropic.com");
        assert!(!s.is_local());
        s.provider = "openai".into();
        assert_eq!(s.host(), "localhost:11434");
        assert!(s.is_local());
        s.base_url = "https://example.com/v1".into();
        assert_eq!(s.host(), "example.com");
        s.provider = "weird".into();
        s.effort = "LOUD".into();
        s.budget_usd = Some(f64::NAN);
        s.sanitize();
        assert_eq!((s.provider.as_str(), s.effort.as_str(), s.budget_usd), ("anthropic", "high", None));
        let cfg = AssistantSettings { effort: "low".into(), budget_usd: Some(2.0), ..Default::default() }.agent_config();
        assert_eq!(cfg.effort, Some(filmcraft_llm::Effort::Low));
        assert_eq!(cfg.budget_usd, Some(2.0));
        assert_eq!(AssistantSettings { effort: String::new(), ..Default::default() }.agent_config().effort, None);
    }

    #[test]
    fn conversation_keys_are_stable() {
        assert_eq!(conversation_key(None), "untitled");
        assert_eq!(conversation_key(Some("")), "untitled");
        let a = conversation_key(Some("/a/b.fcproj"));
        assert_eq!(a, conversation_key(Some("/a/b.fcproj")));
        assert_ne!(a, conversation_key(Some("/a/c.fcproj")));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn credentials_are_private_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("filmcraft-assistant-cred-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read_api_key(&dir, "anthropic"), None);
        store_api_key(&dir, "anthropic", "  sk-test-1  ").unwrap();
        store_api_key(&dir, "openai", "ol-2").unwrap();
        assert_eq!(read_api_key(&dir, "anthropic").as_deref(), Some("sk-test-1"));
        assert_eq!(read_api_key(&dir, "openai").as_deref(), Some("ol-2"));
        assert!(store_api_key(&dir, "anthropic", "bad\nkey").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(credentials_path(&dir)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store_api_key(&dir, "openai", "").unwrap();
        assert_eq!(read_api_key(&dir, "openai"), None);
        assert_eq!(read_api_key(&dir, "anthropic").as_deref(), Some("sk-test-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chat_rebuilds_from_a_saved_conversation() {
        let conv = Conversation {
            messages: vec![
                Message::user_text("cut the silences"),
                Message::assistant(vec![
                    ContentBlock::Opaque { raw: json!({"type": "thinking", "thinking": "x"}) },
                    ContentBlock::text("Looking."),
                    ContentBlock::ToolUse { id: "t1".into(), name: "propose_edit_plan".into(), input: json!({"plan": "{}"}) },
                    ContentBlock::ToolUse { id: "t2".into(), name: "export".into(), input: json!({}) },
                ]),
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::tool_result("t1", "{\"durationBefore\": 10}"), ContentBlock::tool_error("t2", "not run: the user declined")],
                },
                Message::system_text("note"),
                Message::assistant(vec![
                    ContentBlock::text("Done."),
                    ContentBlock::ToolUse { id: "t3".into(), name: "find_silences".into(), input: json!({}) },
                ]),
            ],
            ..Default::default()
        };
        let items = items_from_conversation(&conv);
        assert_eq!(items.len(), 6, "{items:?}");
        assert_eq!(items[0], ChatItem::User("cut the silences".into()));
        let ChatItem::Tool(plan) = &items[2] else { panic!() };
        assert_eq!(plan.plan, Some(PlanStatus::Proposed));
        assert_eq!(plan.result, Some(json!({"durationBefore": 10})));
        let ChatItem::Tool(denied) = &items[3] else { panic!() };
        assert!(denied.denied && denied.ok == Some(false));
        let ChatItem::Tool(open) = &items[5] else { panic!() };
        assert_eq!(open.ok, Some(false));
    }

    #[test]
    fn streaming_text_appends_and_splits() {
        let mut rt = AssistantRuntime::default();
        rt.append_text(false, "Hel");
        rt.append_text(false, "lo");
        rt.append_text(true, "hmm");
        rt.append_text(false, "Next");
        assert_eq!(rt.items, vec![ChatItem::Assistant("Hello".into()), ChatItem::Thinking("hmm".into()), ChatItem::Assistant("Next".into())]);
        rt.open_text = false;
        rt.append_text(false, "New");
        assert_eq!(rt.items.len(), 4);
    }
}
