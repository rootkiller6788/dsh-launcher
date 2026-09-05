//! Cost model — a curated, approximate public list-price table.
//!
//! DSH's LLM layer is an OpenAI-compatible client, so a request is only
//! attributable to a *provider* (the profile id, recorded as `api_key_alias`)
//! and a *model* string. This module turns that pair into a per-token price.
//!
//! **Honesty contract:** these are maintainer-curated *approximate* public
//! list prices (USD per 1M tokens), not a live bill. They drift as vendors
//! change pricing, so they live in one place for easy updating. `cost_known`
//! in the ledger means only "we have a price to look up", not "this is your
//! exact invoice". When we cannot price a model we return `None` and the caller
//! records the cost as *unknown* rather than fabricating a flat estimate.

/// USD price per 1,000,000 tokens (input and output).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Providers whose runtime is local — no per-token charge. Priced as `$0`
/// (a *known* zero, not "unknown").
const FREE_PROVIDERS: &[&str] = &["ollama", "vllm", "lmstudio", "llamacpp"];

/// `(model substring, input $/1M, output $/1M)`. Matched case-insensitively
/// against the recorded model; the longest matching entry wins so a more
/// specific tier (`gpt-4o-mini`) beats its parent (`gpt-4o`).
const MODEL_PRICES: &[(&str, f64, f64)] = &[
    // DeepSeek
    ("deepseek-chat", 0.27, 1.10),
    ("deepseek-reasoner", 0.55, 2.19),
    // OpenAI
    ("gpt-4o-mini", 0.15, 0.60),
    ("gpt-4o", 2.50, 10.00),
    ("gpt-4.1-mini", 0.40, 1.60),
    ("gpt-4.1", 2.00, 8.00),
    ("o3-mini", 1.10, 4.40),
    // Google Gemini (OpenAI-compatible endpoint)
    ("gemini-2.5-flash", 0.30, 2.50),
    ("gemini-2.5-pro", 1.25, 10.00),
    ("gemini-2.0-flash", 0.10, 0.40),
    // Anthropic
    ("claude-opus", 15.00, 75.00),
    ("claude-sonnet", 3.00, 15.00),
    ("claude-haiku", 0.80, 4.00),
    // Groq
    ("deepseek-r1-distill-llama-70b", 0.75, 0.99),
    ("llama-3.3-70b", 0.59, 0.79),
    // Mistral
    ("mistral-large", 2.00, 6.00),
    ("mistral-small", 0.20, 0.60),
    // xAI
    ("grok-2", 2.00, 10.00),
    // Moonshot Kimi
    ("moonshot-v1", 1.67, 1.67),
    // Zhipu GLM
    ("glm-4-plus", 7.14, 7.14),
    ("glm-4-flash", 0.71, 0.71),
    // Alibaba DashScope
    ("qwen-max", 3.35, 10.05),
    ("qwen-plus", 0.55, 2.20),
    ("qwen2.5", 0.55, 2.20),
];

/// Resolve a price for `(provider_id, model)`, or `None` when unpriced.
///
/// Local providers are `$0`. Otherwise the recorded model is matched against
/// the curated table by longest substring (an OpenRouter-style `vendor/model`
/// is normalized by dropping the `vendor/` prefix first).
pub fn lookup(provider_id: &str, model: &str) -> Option<Price> {
    let provider = provider_id.to_ascii_lowercase();
    if FREE_PROVIDERS.contains(&provider.as_str()) {
        return Some(Price {
            input_per_mtok: 0.0,
            output_per_mtok: 0.0,
        });
    }
    let normalized = normalize_model(model);
    MODEL_PRICES
        .iter()
        .filter(|(pat, _, _)| normalized.contains(pat))
        .max_by_key(|(pat, _, _)| pat.len())
        .map(|(_, input, output)| Price {
            input_per_mtok: *input,
            output_per_mtok: *output,
        })
}

/// Total USD cost for a request, or `None` when it cannot be priced.
pub fn cost_for(provider_id: &str, model: &str, input: u64, output: u64) -> Option<f64> {
    let price = lookup(provider_id, model)?;
    let input_cost = input as f64 * price.input_per_mtok / 1_000_000.0;
    let output_cost = output as f64 * price.output_per_mtok / 1_000_000.0;
    Some(input_cost + output_cost)
}

/// Lowercase and drop an optional `vendor/` prefix (OpenRouter / LiteLLM style).
fn normalize_model(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    match lower.split_once('/') {
        Some((_vendor, rest)) => rest.trim().to_string(),
        None => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_hits_exact_then_prefix() {
        let exact = lookup("openai", "gpt-4o").unwrap();
        assert_eq!(exact.input_per_mtok, 2.50);
        // `gpt-4o-mini` contains `gpt-4o`; the longer entry must win.
        let mini = lookup("openai", "gpt-4o-mini").unwrap();
        assert_eq!(mini.input_per_mtok, 0.15);
    }

    #[test]
    fn lookup_normalizes_vendor_prefix() {
        // OpenRouter-style "openai/gpt-4o" resolves to the gpt-4o tier.
        let p = lookup("openrouter", "openai/gpt-4o").unwrap();
        assert_eq!(p.input_per_mtok, 2.50);
    }

    #[test]
    fn lookup_returns_none_for_unpriced() {
        assert!(lookup("openai", "some-unknown-model").is_none());
        assert!(lookup("acme", "acme-llm").is_none());
    }

    #[test]
    fn local_providers_are_free() {
        let p = lookup("ollama", "llama3.2").unwrap();
        assert_eq!(p.input_per_mtok, 0.0);
        assert_eq!(p.output_per_mtok, 0.0);
    }

    #[test]
    fn cost_for_multiplies_per_mtok() {
        // 1M input + 1M output of gpt-4o-mini = 0.15 + 0.60 = 0.75.
        let c = cost_for("openai", "gpt-4o-mini", 1_000_000, 1_000_000).unwrap();
        assert!((c - 0.75).abs() < 1e-9, "got {c}");
        assert!(cost_for("openai", "nope", 1, 1).is_none());
    }
}
