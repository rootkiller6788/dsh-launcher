//! DSH LLM provider config over the host settings RPC.
//!
//! DSH's `llm-deepseek` plugin is an OpenAI-compatible client
//! (`POST {baseURL}/chat/completions`). Its provider facts — `baseURL`,
//! `apiKeyEnv`, and the `models` catalog — live in Host settings namespace
//! `llm-deepseek` and are resolved *per request* (a change reaches the very
//! next request without restart). `baseURL` and the key already flow through
//! the `DEEPSEEK_BASE_URL` / `DEEPSEEK_API_KEY` env vars the launcher sets; the
//! The launcher also stamps `baseURL` after boot when it needs to route traffic
//! through the local usage proxy.

use anyhow::Result;
use serde_json::{json, Value};

use crate::theme::{ensure_ok, host_rpc};

/// Set `llm-deepseek.models` — the model ids DSH's selector shows and sends on
/// the wire. `models` are plain ids (name/contextWindow/maxTokens are optional
/// in DSH's catalog schema). No-op when empty (DSH keeps its own defaults).
pub async fn set_models(port: u16, models: &[String]) -> Result<()> {
    if models.is_empty() {
        return Ok(());
    }
    let catalog: Vec<Value> = models.iter().map(|id| json!({ "id": id })).collect();
    let payload = json!({
        "ns": "llm-deepseek",
        "ops": [{ "op": "set", "path": ["models"], "value": catalog }],
    });
    let value = host_rpc(port, "settings.mutate", payload).await?;
    ensure_ok(&value, "settings.mutate")
}

/// Set `llm-deepseek.baseURL` so DSH resolves the next request through the
/// launcher's local usage proxy instead of a persisted provider URL.
pub async fn set_base_url(port: u16, base_url: &str) -> Result<()> {
    let payload = json!({
        "ns": "llm-deepseek",
        "ops": [{ "op": "set", "path": ["baseURL"], "value": base_url }],
    });
    let value = host_rpc(port, "settings.mutate", payload).await?;
    ensure_ok(&value, "settings.mutate")
}
