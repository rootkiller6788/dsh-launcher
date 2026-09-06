//! Minimal OpenAI-compatible chat client, shared by the market recommender
//! ([`crate::market::recommend`]) and the MCP local-install AI fallback
//! (dsh-adapter's `mcp_local`, roadmap Phase 4).
//!
//! The endpoint speaks `POST {base_url}/chat/completions` (the same protocol
//! DSH's own LLM layer uses), so any configured provider works — cloud vendors,
//! aggregators, and local runtimes (Ollama / vLLM / llama.cpp). The key comes
//! from the caller as a [`ResolvedProvider`], which the app resolves out of the
//! OS credential vault — nothing here ever sees disk, let alone stores a key.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::ResolvedProvider;

const DEFAULT_BASE: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const LLM_TIMEOUT: Duration = Duration::from_secs(90);
/// Guard against a hostile/looping model reply — never hold more than this.
const MAX_CONTENT: usize = 400_000;

/// POST one chat completion and return `choices[0].message.content`.
///
/// `system` and `user` are the two messages; the request is non-streaming.
/// Errors are readable and one-shot (no retry — callers decide policy). The
/// returned string is the *raw* model reply — callers that need JSON apply their
/// own fence-strip + schema validation (the recommender validates against the
/// candidate set; the MCP AI fallback validates against a toolchain allow-list).
pub async fn chat(provider: &ResolvedProvider, system: &str, user: &str) -> Result<String> {
    let base = provider
        .profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASE)
        .trim_end_matches('/');
    let model = provider
        .profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL);

    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "max_tokens": 1500,
        "stream": false,
    });

    let client = reqwest::Client::builder()
        .timeout(LLM_TIMEOUT)
        .build()?;
    let resp = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("LLM request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.context("read LLM response")?;
    if !status.is_success() {
        return Err(anyhow!(
            "LLM error {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| anyhow!("LLM returned non-JSON: {e}"))?;
    let raw = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if raw.trim().is_empty() {
        return Err(anyhow!("LLM returned an empty completion"));
    }
    if raw.chars().count() > MAX_CONTENT {
        return Err(anyhow!("LLM completion too large — refusing to process"));
    }
    Ok(raw)
}
