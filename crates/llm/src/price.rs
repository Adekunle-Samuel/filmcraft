//! Price table: an estimate of what a call cost, for the Assistant's cost meter and budgets.
//!
//! First-party Claude API list prices in US dollars per million tokens (cached 2026-10). Cache
//! writes are the 5-minute TTL rate (1.25 × input). An estimate only: the provider's bill is the
//! source of truth.

use crate::types::Usage;

/// Per-million-token prices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Price {
    const fn new(input: f64, output: f64, cache_read: f64) -> Self {
        Self { input, output, cache_read, cache_write: input * 1.25 }
    }
}

/// Claude Haiku 5.5 prices rise above this many prompt tokens.
const HAIKU_LONG_PROMPT: u64 = 100_000;

/// The price of `model` for a call whose prompt is `prompt_tokens` long, if known.
pub fn price(model: &str, prompt_tokens: u64) -> Option<Price> {
    let model = model.strip_prefix("anthropic.").unwrap_or(model);
    match model {
        "claude-opus-5-5" => Some(Price::new(4.0, 20.0, 0.20)),
        "claude-sonnet-5-5" => Some(Price::new(2.0, 10.0, 0.20)),
        "claude-haiku-5-5" if prompt_tokens > HAIKU_LONG_PROMPT => Some(Price::new(0.50, 2.50, 0.05)),
        "claude-haiku-5-5" => Some(Price::new(0.10, 0.50, 0.01)),
        _ => None,
    }
}

/// Estimated cost of one call in US dollars, or `None` for an unknown model (local models are
/// free; unknown hosted ones are not guessed).
pub fn estimate_cost_usd(model: &str, usage: &Usage) -> Option<f64> {
    let p = price(model, usage.prompt_tokens())?;
    let m = |tokens: u64, per_mtok: f64| tokens as f64 * per_mtok / 1_000_000.0;
    Some(
        m(usage.input_tokens, p.input)
            + m(usage.output_tokens, p.output)
            + m(usage.cache_read_input_tokens, p.cache_read)
            + m(usage.cache_creation_input_tokens, p.cache_write),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn opus_prices() {
        let u = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, cache_read_input_tokens: 1_000_000, cache_creation_input_tokens: 1_000_000 };
        assert!(close(estimate_cost_usd("claude-opus-5-5", &u).unwrap(), 4.0 + 20.0 + 0.20 + 5.0));
        assert!(close(estimate_cost_usd("claude-sonnet-5-5", &u).unwrap(), 2.0 + 10.0 + 0.20 + 2.5));
    }

    #[test]
    fn haiku_and_unknown() {
        let small = Usage { input_tokens: 10_000, output_tokens: 1_000, ..Default::default() };
        assert!(close(estimate_cost_usd("claude-haiku-5-5", &small).unwrap(), 0.001 + 0.0005));
        let big = Usage { input_tokens: 200_000, output_tokens: 0, ..Default::default() };
        assert!(close(estimate_cost_usd("claude-haiku-5-5", &big).unwrap(), 0.1));
        assert!(estimate_cost_usd("llama3.2", &small).is_none());
        assert!(estimate_cost_usd("anthropic.claude-opus-5-5", &small).is_some());
        assert_eq!(estimate_cost_usd("claude-opus-5-5", &Usage::default()), Some(0.0));
    }
}
