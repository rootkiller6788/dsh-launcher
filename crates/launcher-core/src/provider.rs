use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::{write_json_atomic, AppPaths};

pub const DEFAULT_PROVIDER_ID: &str = "default";
const KEYRING_SERVICE: &str = "ai-harness-launcher";

/// A curated provider preset — the "auto-fill" table. DSH's LLM layer is an
/// OpenAI-compatible client (`POST {base_url}/chat/completions`), so any
/// endpoint that speaks that protocol works: cloud vendors, aggregators, and
/// local runtimes (Ollama / vLLM / llama.cpp). `models` is the pre-filled model
/// catalog (a LiteLLM-style subset) shown to the user; `needs_key` marks local
/// endpoints that need no API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub needs_key: bool,
    pub models: Vec<String>,
}

/// The built-in preset library (~20 OpenAI-compatible providers).
pub fn provider_presets() -> Vec<ProviderPreset> {
    let mut out = vec![
        preset(
            "deepseek",
            "DeepSeek",
            "https://api.deepseek.com",
            true,
            &["deepseek-chat", "deepseek-reasoner"],
        ),
        preset(
            "openai",
            "OpenAI",
            "https://api.openai.com/v1",
            true,
            &[
                "gpt-4o",
                "gpt-4o-mini",
                "gpt-4.1",
                "gpt-4.1-mini",
                "o3-mini",
            ],
        ),
        preset(
            "gemini",
            "Google Gemini (OpenAI-compatible)",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            true,
            &["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.0-flash"],
        ),
        preset(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            true,
            &[
                "openai/gpt-4o",
                "anthropic/claude-3.5-sonnet",
                "google/gemini-2.5-pro",
            ],
        ),
        preset(
            "groq",
            "Groq",
            "https://api.groq.com/openai/v1",
            true,
            &["llama-3.3-70b-versatile", "deepseek-r1-distill-llama-70b"],
        ),
        preset(
            "mistral",
            "Mistral AI",
            "https://api.mistral.ai/v1",
            true,
            &["mistral-large-latest", "mistral-small-latest"],
        ),
        preset(
            "together",
            "Together AI",
            "https://api.together.xyz/v1",
            true,
            &["meta-llama/Llama-3.3-70B-Instruct-Turbo"],
        ),
        preset(
            "xai",
            "xAI Grok",
            "https://api.x.ai/v1",
            true,
            &["grok-2-latest"],
        ),
        preset(
            "siliconflow",
            "SiliconFlow",
            "https://api.siliconflow.cn/v1",
            true,
            &["deepseek-ai/DeepSeek-V3", "Qwen/Qwen2.5-72B-Instruct"],
        ),
        preset(
            "moonshot",
            "Moonshot Kimi",
            "https://api.moonshot.cn/v1",
            true,
            &["moonshot-v1-8k"],
        ),
        preset(
            "zhipu",
            "Zhipu GLM",
            "https://open.bigmodel.cn/api/paas/v4",
            true,
            &["glm-4-plus"],
        ),
        preset(
            "dashscope",
            "Alibaba DashScope",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            true,
            &["qwen-max", "qwen-plus"],
        ),
        preset(
            "ollama",
            "Ollama (local)",
            "http://localhost:11434/v1",
            false,
            &["llama3.2", "qwen2.5"],
        ),
        preset(
            "vllm",
            "vLLM (local)",
            "http://localhost:8000/v1",
            false,
            &[],
        ),
        preset(
            "lmstudio",
            "LM Studio (local)",
            "http://localhost:1234/v1",
            false,
            &[],
        ),
        preset(
            "llamacpp",
            "llama.cpp server (local)",
            "http://localhost:8080/v1",
            false,
            &[],
        ),
        preset(
            "anthropic",
            "Anthropic (experimental)",
            "https://api.anthropic.com/v1",
            true,
            &["claude-sonnet-4-5"],
        ),
    ];
    out.push(ProviderPreset {
        id: "custom".into(),
        name: "Custom (OpenAI-compatible)".into(),
        base_url: String::new(),
        needs_key: true,
        models: vec![],
    });
    out
}

fn preset(
    id: &str,
    name: &str,
    base_url: &str,
    needs_key: bool,
    models: &[&str],
) -> ProviderPreset {
    ProviderPreset {
        id: id.into(),
        name: name.into(),
        base_url: base_url.into(),
        needs_key,
        models: models.iter().map(|s| s.to_string()).collect(),
    }
}

/// Provider metadata. The API key is NEVER part of this struct — it lives in
/// the OS credential store (Windows Credential Manager / DPAPI via `keyring`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Model catalog shown to DSH (a LiteLLM-style subset). Empty means "DSH's
    /// own defaults".
    #[serde(default)]
    pub models: Vec<String>,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: DEFAULT_PROVIDER_ID.into(),
            name: "DeepSeek".into(),
            base_url: None,
            model: None,
            models: vec![],
        }
    }
}

/// A provider with its credential resolved from the OS keyring.
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub profile: ProviderProfile,
    pub api_key: String,
}

/// Vault over the provider metadata file + OS credential store.
#[derive(Clone)]
pub struct ProviderVault {
    paths: AppPaths,
}

impl ProviderVault {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    fn metadata_path(&self) -> PathBuf {
        self.paths.providers.clone()
    }

    fn load_all(&self) -> Result<Vec<ProviderProfile>> {
        let file = self.metadata_path();
        if !file.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&file)?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        let arr = value
            .get("providers")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));
        Ok(serde_json::from_value(arr)?)
    }

    fn save_all(&self, profiles: &[ProviderProfile]) -> Result<()> {
        let value = serde_json::json!({ "providers": profiles });
        write_json_atomic(&self.metadata_path(), &value)
    }

    /// All stored provider profiles (metadata only, no keys).
    pub fn list(&self) -> Result<Vec<ProviderProfile>> {
        self.load_all()
    }

    pub fn get(&self, id: &str) -> Result<ProviderProfile> {
        Ok(self
            .load_all()?
            .into_iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| ProviderProfile {
                id: id.into(),
                ..Default::default()
            }))
    }

    /// Persist profile metadata and, when a non-empty key is supplied, upsert
    /// the API key in the OS credential store. `None`/empty key leaves the
    /// stored key untouched.
    pub fn set(&self, profile: &ProviderProfile, api_key: Option<&str>) -> Result<()> {
        let mut all = self.load_all()?;
        match all.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile.clone(),
            None => all.push(profile.clone()),
        }
        self.save_all(&all)?;

        if let Some(key) = api_key {
            if !key.trim().is_empty() {
                let entry = keyring::Entry::new(KEYRING_SERVICE, &key_account(&profile.id))
                    .map_err(|e| anyhow!("credential store unavailable: {e}"))?;
                entry
                    .set_password(key)
                    .map_err(|e| anyhow!("failed to store API key: {e}"))?;
            }
        }
        Ok(())
    }

    pub fn has_key(&self, id: &str) -> bool {
        match keyring::Entry::new(KEYRING_SERVICE, &key_account(id)) {
            Ok(entry) => entry.get_password().is_ok(),
            Err(_) => false,
        }
    }

    /// Resolve a provider to its profile + key, erroring when no key is stored.
    pub fn resolve(&self, id: &str) -> Result<ResolvedProvider> {
        let profile = self.get(id)?;
        let api_key = keyring::Entry::new(KEYRING_SERVICE, &key_account(id))
            .and_then(|e| e.get_password())
            .map_err(|_| anyhow!("no API key stored for provider '{id}' — set it in Settings"))?;
        Ok(ResolvedProvider { profile, api_key })
    }

    pub fn delete_key(&self, id: &str) -> Result<()> {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &key_account(id)) {
            entry.delete_credential().ok();
        }
        Ok(())
    }

    /// Remove a provider entirely: its metadata row and its stored key.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut all = self.load_all()?;
        all.retain(|p| p.id != id);
        self.save_all(&all)?;
        self.delete_key(id)
    }
}

fn key_account(id: &str) -> String {
    format!("provider:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_unique_and_sane() {
        let presets = provider_presets();
        assert!(
            presets.len() >= 15,
            "expected a rich preset library, got {}",
            presets.len()
        );
        let mut ids = std::collections::HashSet::new();
        for p in &presets {
            assert!(ids.insert(p.id.as_str()), "duplicate preset id {}", p.id);
            if p.id != "custom" {
                assert!(!p.base_url.is_empty(), "{} has empty base_url", p.id);
            }
            // Cloud vendors ship a model catalog; local runtimes (Ollama/
            // vLLM/…) leave it empty for the user to fill with models they pull.
            if p.needs_key && p.id != "custom" {
                assert!(!p.models.is_empty(), "{} has no models", p.id);
            }
        }
        assert!(
            presets.iter().any(|p| p.id == "deepseek"),
            "deepseek preset missing"
        );
    }

    #[test]
    fn profile_deserializes_without_models_field() {
        // An existing providers.json predates the `models` field — it must
        // deserialize with an empty catalog, not error.
        let json = r#"{"id":"default","name":"DeepSeek","baseUrl":null,"model":null}"#;
        let p: ProviderProfile = serde_json::from_str(json).expect("legacy provider parses");
        assert!(p.models.is_empty());
    }
}
