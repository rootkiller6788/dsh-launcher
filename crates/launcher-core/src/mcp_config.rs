//! Per-MCP configured variables — the "fill + inject" half of MCP config.
//!
//! Split mirrors [`crate::provider::ProviderVault`]: the *non-secret* key name
//! (+ whether the value is a secret) lives on disk under the server's mcp dir
//! (`instances/<id>/mcp/<server>/config.json`); the *value* lives only in the
//! OS credential store (`keyring`, Windows Credential Manager). A value never
//! touches disk, and `list` never returns one — the UI only ever sees whether a
//! key is configured, not what it holds.
//!
//! Injection is process-level: DSH is launched with each configured key set on
//! its environment (`commands/process.rs` folds `resolve_env` over the
//! instance's records), and the MCP server children DSH spawns inherit it.
//! This assumes configured keys are globally unique across one DSH process —
//! two servers wanting different values under the same key can't both be
//! satisfied through process env alone (out of scope for this phase).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;
use crate::write_json_atomic;

/// Same Windows Credential Manager service as the provider vault.
const KEYRING_SERVICE: &str = "ai-harness-launcher";

/// One configured variable: the key name and whether its value is a secret.
/// Serialized to `config.json`; the value is never part of this struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpConfigVar {
    pub key: String,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct ConfigFile {
    vars: Vec<McpConfigVar>,
}

/// Read/write access to per-server MCP config (names on disk, values in the OS
/// credential store).
#[derive(Clone)]
pub struct McpConfigStore {
    paths: AppPaths,
}

impl McpConfigStore {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    fn config_path(&self, instance_id: &str, server_id: &str) -> PathBuf {
        self.paths.mcp_dir(instance_id, server_id).join("config.json")
    }

    fn account(instance_id: &str, server_id: &str, key: &str) -> String {
        format!("mcp:{instance_id}:{server_id}:{key}")
    }

    /// The OS credential-store entry backing one configured variable. Every
    /// read/write of a value goes through here so the service + account naming
    /// stays in one place.
    fn entry(
        instance_id: &str,
        server_id: &str,
        key: &str,
    ) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(KEYRING_SERVICE, &Self::account(instance_id, server_id, key))
    }

    fn read(&self, instance_id: &str, server_id: &str) -> Result<ConfigFile> {
        let path = self.config_path(instance_id, server_id);
        if !path.exists() {
            return Ok(ConfigFile::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    fn write(&self, instance_id: &str, server_id: &str, file: &ConfigFile) -> Result<()> {
        write_json_atomic(&self.config_path(instance_id, server_id), &serde_json::to_value(file)?)
    }

    /// Configured variable names for a server (values never returned).
    pub fn list(&self, instance_id: &str, server_id: &str) -> Result<Vec<McpConfigVar>> {
        Ok(self.read(instance_id, server_id)?.vars)
    }

    /// Whether a key is currently configured for the server.
    pub fn has(&self, instance_id: &str, server_id: &str, key: &str) -> Result<bool> {
        Ok(self
            .read(instance_id, server_id)?
            .vars
            .iter()
            .any(|v| v.key == key))
    }

    /// Upsert a variable: write/refresh its name + secret flag on disk and,
    /// when `value` is non-empty, its value into the OS credential store.
    /// `None`/empty `value` leaves any stored secret untouched (used when only
    /// the secret flag changes or the form row was left blank).
    pub fn set(
        &self,
        instance_id: &str,
        server_id: &str,
        key: &str,
        secret: bool,
        value: Option<&str>,
    ) -> Result<()> {
        let mut file = self.read(instance_id, server_id)?;
        match file.vars.iter_mut().find(|v| v.key == key) {
            Some(existing) => existing.secret = secret,
            None => file.vars.push(McpConfigVar {
                key: key.to_string(),
                secret,
            }),
        }
        self.write(instance_id, server_id, &file)?;

        if let Some(value) = value {
            if !value.trim().is_empty() {
                let entry = Self::entry(instance_id, server_id, key)
                    .map_err(|e| anyhow::anyhow!("credential store unavailable: {e}"))?;
                entry
                    .set_password(value)
                    .with_context(|| format!("store value for {key}"))?;
            }
        }
        Ok(())
    }

    /// Drop a variable entirely: its name from disk and its value from the OS
    /// credential store.
    pub fn remove(&self, instance_id: &str, server_id: &str, key: &str) -> Result<()> {
        let mut file = self.read(instance_id, server_id)?;
        let before = file.vars.len();
        file.vars.retain(|v| v.key != key);
        if file.vars.len() != before {
            self.write(instance_id, server_id, &file)?;
        }
        if let Ok(entry) = Self::entry(instance_id, server_id, key) {
            entry.delete_credential().ok();
        }
        Ok(())
    }

    /// Resolve a server's configured variables to real values from the OS
    /// credential store. Keys whose stored value is missing/empty are skipped
    /// (a name was configured but the value was never saved — nothing to inject).
    pub fn resolve_env(&self, instance_id: &str, server_id: &str) -> HashMap<String, String> {
        let mut env = HashMap::new();
        for var in self.read(instance_id, server_id).unwrap_or_default().vars {
            let entry = Self::entry(instance_id, server_id, &var.key)
                .ok()
                .and_then(|e| e.get_password().ok());
            if let Some(value) = entry {
                if !value.trim().is_empty() {
                    env.insert(var.key, value);
                }
            }
        }
        env
    }

    /// Keys that actually hold a value in the OS store. A name on disk is not
    /// enough — a variable configured but never saved has nothing to inject or
    /// to satisfy a declared requirement with. The Library snapshot drops a
    /// declared key from "needs configuring" only when it shows up here.
    pub fn resolved_keys(&self, instance_id: &str, server_id: &str) -> HashSet<String> {
        self.resolve_env(instance_id, server_id).into_keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;

    /// A throwaway store rooted in temp; caller cleans the dir.
    fn test_store(tag: &str) -> (McpConfigStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ahl-mcpcfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = AppPaths {
            root: dir.clone(),
            portable: false,
            settings: dir.join("settings.json"),
            providers: dir.join("providers.json"),
            runtimes: dir.join("runtimes"),
            instances: dir.join("instances"),
            cache: dir.join("cache"),
            logs: dir.join("logs"),
            launcher_log: dir.join("logs").join("launcher.log"),
        };
        (McpConfigStore::new(paths), dir)
    }

    #[test]
    fn names_on_disk_values_in_credential_store() {
        let (store, dir) = test_store("split");
        assert!(store.list("i1", "ErnestoCorona/kanboard-mcp").unwrap().is_empty());

        // Non-secret URL: value stored in the OS store, name on disk only.
        store
            .set("i1", "ErnestoCorona/kanboard-mcp", "KANBOARD_URL", false, Some("https://pm.example.com"))
            .unwrap();
        let list = store.list("i1", "ErnestoCorona/kanboard-mcp").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, "KANBOARD_URL");
        assert!(!list[0].secret, "secret flag round-trips");
        // No value ever comes back through list.
        let resolved = store.resolve_env("i1", "ErnestoCorona/kanboard-mcp");
        assert_eq!(
            resolved.get("KANBOARD_URL").map(String::as_str),
            Some("https://pm.example.com")
        );
        // Instance/server isolation: a different server sees nothing.
        assert!(store.resolve_env("i1", "other/server").is_empty());
        assert!(store.resolve_env("i2", "ErnestoCorona/kanboard-mcp").is_empty());

        // remove clears both name and stored value.
        store.remove("i1", "ErnestoCorona/kanboard-mcp", "KANBOARD_URL").unwrap();
        assert!(store.list("i1", "ErnestoCorona/kanboard-mcp").unwrap().is_empty());
        assert!(store.resolve_env("i1", "ErnestoCorona/kanboard-mcp").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_value_keeps_stored_secret_flag_upsert_updates() {
        let (store, dir) = test_store("flag");
        store.set("i1", "srv", "TOKEN", true, Some("abc")).unwrap();
        // Refresh with no value → name/flag update, stored secret untouched.
        store.set("i1", "srv", "TOKEN", false, None).unwrap();
        let list = store.list("i1", "srv").unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].secret, "flag flipped without touching the value");
        assert_eq!(
            store.resolve_env("i1", "srv").get("TOKEN").map(String::as_str),
            Some("abc"),
            "stored value survives a flag-only update"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
