//! The Assistant's LLM provider (feature `assistant`): Anthropic, or an OpenAI-compatible server
//! (Ollama, LM Studio…), keyed from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` or from
//! `<data dir>/assistant/credentials.json` (written by Assistant ▸ Settings, 0600). The key is read
//! when a turn starts and goes only into the provider (whose `Debug` redacts it).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_llm::{AnthropicProvider, ApiKey, LlmProvider, OpenAiCompatProvider};
use filmcraft_ui_egui::panels::assistant::{self, AssistantSettings, ProviderFactory};

/// The provider factory installed in `HostHooks::assistant_provider`.
pub fn factory(data_dir: Option<PathBuf>) -> ProviderFactory {
    Box::new(move |s: &AssistantSettings| provider(s, data_dir.as_deref()))
}

fn provider(s: &AssistantSettings, data_dir: Option<&Path>) -> Result<Arc<dyn LlmProvider>, String> {
    let key = assistant::api_key(data_dir, &s.provider);
    if s.is_openai() {
        let key = key.map(ApiKey::new).transpose().map_err(|e| e.to_string())?;
        let p = OpenAiCompatProvider::new(&s.effective_base_url(), key).map_err(|e| e.to_string())?;
        return Ok(Arc::new(p));
    }
    let Some(key) = key else {
        return Err(format!("no Anthropic API key: add one in Assistant ▸ Settings, or set {}", assistant::key_env(&s.provider)));
    };
    let key = ApiKey::new(key).map_err(|e| e.to_string())?;
    let p = if s.base_url.trim().is_empty() { AnthropicProvider::new(key) } else { AnthropicProvider::with_base_url(key, s.base_url.trim()) };
    Ok(Arc::new(p.map_err(|e| e.to_string())?))
}
