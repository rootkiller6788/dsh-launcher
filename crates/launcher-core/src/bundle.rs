//! Bundles — a curated list of content entries (plugins, skins, skills, MCP
//! servers) installed in one pass. Import-only today: bundles have no catalog
//! of their own; a user pastes a JSON manifest and the launcher dispatches each
//! item to its kind's installer.

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::market::RegistryPlugin;

/// One bundle manifest. `items` reuse [`RegistryPlugin`] directly — each item's
/// `kind` selects the installer, and the kind-specific fields (`fetch`,
/// `serverName`/`transport`/`command`, …) drive it. Plugin/theme items install
/// via their derived `install_spec()` (`npm` → `tarball` → `github:owner/repo`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BundleManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub items: Vec<RegistryPlugin>,
}

impl BundleManifest {
    /// Parse a bundle manifest from JSON, refusing an empty item list.
    pub fn parse(json: &str) -> anyhow::Result<Self> {
        let manifest: Self = serde_json::from_str(json).context("parse bundle JSON")?;
        if manifest.items.is_empty() {
            anyhow::bail!("bundle has no items");
        }
        Ok(manifest)
    }
}

/// One bundle item's install outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleItemResult {
    pub name: String,
    pub kind: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Aggregate result of a bundle import — what the UI shows after the run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BundleSummary {
    pub installed: usize,
    pub failed: usize,
    #[serde(default)]
    pub results: Vec<BundleItemResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::ContentKind;

    #[test]
    fn parse_valid_bundle_preserves_kinds_and_fields() {
        let json = r#"{
            "name": "My bundle",
            "version": "1.0.0",
            "items": [
                { "name": "github-sync", "owner": "acme", "url": "https://github.com/acme/github-sync" },
                { "name": "filesystem", "owner": "modelcontextprotocol", "kind": "mcp",
                  "serverName": "filesystem", "transport": "stdio", "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-filesystem"] }
            ]
        }"#;
        let m = BundleManifest::parse(json).unwrap();
        assert_eq!(m.name, "My bundle");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.items.len(), 2);
        // kind defaults to Plugin when omitted.
        assert_eq!(m.items[0].kind, ContentKind::Plugin);
        assert_eq!(m.items[1].kind, ContentKind::Mcp);
        assert_eq!(m.items[1].server_name.as_deref(), Some("filesystem"));
        assert_eq!(m.items[1].transport.as_deref(), Some("stdio"));
        assert_eq!(m.items[1].args.as_deref().unwrap().len(), 2);
    }

    #[test]
    fn parse_rejects_empty_and_invalid() {
        assert!(BundleManifest::parse(r#"{"name":"x","items":[]}"#).is_err());
        assert!(BundleManifest::parse("not json").is_err());
    }
}
