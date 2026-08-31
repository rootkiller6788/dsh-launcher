use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::{write_json_atomic, AppPaths};

pub const DEFAULT_PROVIDER_ID: &str = "default";
const KEYRING_SERVICE: &str = "ai-harness-launcher";

/// Provider metadata. The API key is NEVER part of this struct — it lives in
/// the OS credential store (Windows Credential Manager / DPAPI via `keyring`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl Default for ProviderProfile {
    fn default() -> Self {
        Self {
            id: DEFAULT_PROVIDER_ID.into(),
            name: "DeepSeek".into(),
            base_url: None,
            model: None,
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
        let arr = value.get("providers").cloned().unwrap_or_else(|| serde_json::json!([]));
        Ok(serde_json::from_value(arr)?)
    }

    fn save_all(&self, profiles: &[ProviderProfile]) -> Result<()> {
        let value = serde_json::json!({ "providers": profiles });
        write_json_atomic(&self.metadata_path(), &value)
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
}

fn key_account(id: &str) -> String {
    format!("provider:{id}")
}
