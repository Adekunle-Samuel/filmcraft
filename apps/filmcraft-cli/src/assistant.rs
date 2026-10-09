//! `filmcraft-cli assistant "<prompt>"`: one Assistant turn on a headless session (feature
//! `assistant`). The model plans with the engine tool catalogue; calls that need approval are
//! asked on the terminal (or allowed with `--yes`, declined when stdin is not a terminal).

use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::AtomicBool;

use filmcraft_agent::{AgentConfig, AgentEvent, Conversation, SessionHost, TurnEnd, run_turn};
use filmcraft_engine::Session;
use filmcraft_llm::{ContentBlock, Effort, LlmProvider, StreamEvent};

pub struct Options<'a> {
    pub prompt: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub budget: Option<f64>,
    pub conversation: Option<&'a str>,
    pub yes: bool,
}

fn provider(o: &Options) -> Result<Box<dyn LlmProvider>, String> {
    match o.provider.unwrap_or("anthropic") {
        "anthropic" => {
            let p = match o.base_url {
                Some(u) => {
                    let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| "set ANTHROPIC_API_KEY".to_string())?;
                    filmcraft_llm::AnthropicProvider::with_base_url(filmcraft_llm::ApiKey::new(key).map_err(|e| e.to_string())?, u)
                }
                None => filmcraft_llm::AnthropicProvider::from_env(),
            };
            Ok(Box::new(p.map_err(|e| e.to_string())?))
        }
        "openai" | "local" => {
            let key = std::env::var("OPENAI_API_KEY").ok().and_then(|k| filmcraft_llm::ApiKey::new(k).ok());
            let url = o.base_url.unwrap_or(filmcraft_llm::openai::DEFAULT_BASE_URL);
            Ok(Box::new(filmcraft_llm::OpenAiCompatProvider::new(url, key).map_err(|e| e.to_string())?))
        }
        p => Err(format!("unknown provider `{p}` (anthropic, openai)")),
    }
}

fn effort(s: Option<&str>) -> Result<Option<Effort>, String> {
    Ok(Some(match s.unwrap_or("high") {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::Xhigh,
        "max" => Effort::Max,
        e => return Err(format!("unknown effort `{e}` (low, medium, high, xhigh, max)")),
    }))
}

/// Run one turn; returns the exit status (0 done, 1 failed or refused).
pub fn run(s: &mut Session, o: &Options) -> Result<i32, String> {
    let provider = provider(o)?;
    let mut cfg = AgentConfig { effort: effort(o.effort)?, budget_usd: o.budget, ..Default::default() };
    if let Some(m) = o.model {
        cfg.model = m.to_string();
    }
    let mut conv: Conversation = match o.conversation {
        Some(p) if std::path::Path::new(p).exists() => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{p}: not a conversation: {e}"))?
        }
        _ => Conversation::default(),
    };
    let yes = o.yes;
    let interactive = std::io::stdin().is_terminal();
    let approve = Box::new(move |c: &filmcraft_agent::ToolCall, why: &str| {
        if yes {
            return true;
        }
        if !interactive {
            eprintln!("· declined {} ({why}); pass --yes to allow", c.name);
            return false;
        }
        eprint!("Allow {} ({why})? input: {}\n[y/N] ", c.name, c.input);
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        matches!(line.trim(), "y" | "Y" | "yes")
    });
    let mut host = SessionHost::new(s, approve);
    let mut out = std::io::stdout();
    let mut on_event = |e: AgentEvent| match e {
        AgentEvent::Stream(StreamEvent::TextDelta(t)) => {
            let _ = write!(out, "{t}");
            let _ = out.flush();
        }
        AgentEvent::ToolStarted { name, .. } => eprintln!("\n· {name}…"),
        AgentEvent::ToolFinished { name, ok: false, summary, .. } => eprintln!("· {name} failed: {summary}"),
        AgentEvent::ToolDenied { name, reason, .. } => eprintln!("· {name} not run: {reason}"),
        AgentEvent::Notice(n) => eprintln!("· {n}"),
        AgentEvent::Usage { total, cost_usd: Some(c) } => {
            eprint!("\r· {} in / {} out tokens, ≈ ${c:.3}   ", total.prompt_tokens(), total.output_tokens);
        }
        _ => {}
    };
    let end = run_turn(&cfg, &mut conv, provider.as_ref(), &mut host, vec![ContentBlock::text(o.prompt)], &mut on_event, &AtomicBool::new(false));
    println!();
    if let Some(p) = o.conversation {
        let text = serde_json::to_string(&conv).map_err(|e| e.to_string())?;
        filmcraft_format::atomic_write(std::path::Path::new(p), text.as_bytes()).map_err(|e| format!("{p}: {e}"))?;
    }
    Ok(match end {
        TurnEnd::Done => 0,
        TurnEnd::Cancelled => 1,
        TurnEnd::Refused { explanation, .. } => {
            eprintln!("The model declined: {}", explanation.unwrap_or_default());
            1
        }
        TurnEnd::Limit(m) => {
            eprintln!("{m}");
            0
        }
        TurnEnd::Failed(m) => {
            eprintln!("Assistant failed: {m}");
            1
        }
    })
}
