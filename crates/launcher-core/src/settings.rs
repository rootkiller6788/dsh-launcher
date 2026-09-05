use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{write_json_atomic, AppPaths};

/// Application-level settings, persisted as JSON at the app root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    /// Absolute path to the DSH CLI entry (e.g. `.../apps/cli/lib/bin.js`).
    /// When unset the adapter falls back to bundled / managed runtimes / PATH.
    pub dsh_path: Option<String>,
    /// Absolute path to a Node executable override. When unset the adapter
    /// prefers bundled / managed Node, then PATH.
    pub node_path: Option<String>,
    /// The last selected instance id (reserved for Phase 2).
    pub last_instance: Option<String>,
    /// UI language: `"en"` (default) or `"zh"`. `None` = English.
    pub language: Option<String>,
    /// Theme preference: `"light"`, `"dark"`, or `"system"` (default).
    /// Synced with the running DSH's `ui-theme.preference`.
    pub theme: Option<String>,
    /// Crash-telemetry consent. Default off (#602): no crash data leaves the
    /// machine unless the user opts in here. When true, a panic also writes a
    /// minimal `crash-<ts>.json` sidecar that the next launch may upload.
    #[serde(default)]
    pub telemetry_enabled: bool,
    /// User-owned crash-ingest URL (`https://…`). When unset, consent alone
    /// enables nothing — there is nowhere to send.
    pub telemetry_endpoint: Option<String>,
}

impl AppSettings {
    pub fn load(paths: &AppPaths) -> Self {
        let file = &paths.settings;
        if file.exists() {
            if let Ok(text) = std::fs::read_to_string(file) {
                if let Ok(s) = serde_json::from_str(&text) {
                    return s;
                }
            }
        }
        Self::default()
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let value = serde_json::to_value(self)?;
        write_json_atomic(&paths.settings, &value)
    }
}
