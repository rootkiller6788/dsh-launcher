//! Environment package (`.dshenv`) schema.
//!
//! A `.dshenv` export is an *install manifest*, never a file bundle: it records
//! which plugins / skins / skills / MCP servers an instance has, together with
//! the provenance + version to re-install them from, but deliberately carries no
//! API keys, logs, `node_modules`, or workspace state. The manifest lives in
//! `launcher-core` (not the Tauri command layer) so the install job store can
//! persist the full plan for Install Center visibility and retry.

use serde::{Deserialize, Serialize};

use crate::market::{ContentKind, RegistryPlugin};

pub const ENVIRONMENT_FORMAT: &str = "dsh.environment";
pub const ENVIRONMENT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSource {
    pub instance_id: String,
    pub instance_name: String,
    pub runtime: String,
}

/// Per-item provenance captured at export time so the import preview can show
/// where each resource will be re-installed from and which version it had.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedItem {
    /// Stable identity (`owner/name`, else `name`) — matches the install plan.
    pub key: String,
    pub kind: ContentKind,
    pub name: String,
    /// Download provenance: `npm:<pkg>` / `github:owner/repo` / `tarball:<url>`.
    pub source: String,
    /// Installed version at export time (best-effort; `None` when unknown).
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentManifest {
    pub format: String,
    pub format_version: u32,
    pub exported_at: u64,
    pub name: String,
    pub description: String,
    /// DSH runtime version the environment was exported from; the import can
    /// surface a compatibility warning against it.
    #[serde(default)]
    pub compatible_with: String,
    pub source: EnvironmentSource,
    /// Install plan (full `RegistryPlugin` entries resolved against the merged
    /// catalog at export time).
    pub items: Vec<RegistryPlugin>,
    /// Provenance + versions for the import preview / audit.
    #[serde(default)]
    pub exports: Vec<ExportedItem>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::RegistryPlugin;

    #[test]
    fn manifest_roundtrips_new_fields_in_camel_case() {
        let manifest = EnvironmentManifest {
            format: ENVIRONMENT_FORMAT.into(),
            format_version: ENVIRONMENT_FORMAT_VERSION,
            exported_at: 12345,
            name: "demo".into(),
            description: "desc".into(),
            compatible_with: "0.1.0".into(),
            source: EnvironmentSource {
                instance_id: "i1".into(),
                instance_name: "demo".into(),
                runtime: "0.1.0".into(),
            },
            items: vec![RegistryPlugin {
                name: "toolbox".into(),
                ..Default::default()
            }],
            exports: vec![ExportedItem {
                key: "acme/toolbox".into(),
                kind: ContentKind::Plugin,
                name: "toolbox".into(),
                source: "npm:@acme/toolbox".into(),
                version: Some("1.2.3".into()),
            }],
        };
        let json = serde_json::to_string(&manifest).expect("serialize");
        assert!(json.contains("\"compatibleWith\":\"0.1.0\""));
        assert!(json.contains("\"source\":\"npm:@acme/toolbox\""));
        let back: EnvironmentManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.compatible_with, "0.1.0");
        assert_eq!(back.exports.len(), 1);
        assert_eq!(back.exports[0].version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn legacy_manifest_without_new_fields_defaults_empty() {
        // A pre-"固化" v1 export carried no compatible_with / exports; the new
        // schema must still accept it (serde defaults).
        let legacy = r#"{
            "format": "dsh.environment",
            "formatVersion": 1,
            "exportedAt": 1,
            "name": "old",
            "description": "",
            "source": { "instanceId": "i", "instanceName": "old", "runtime": "0.0.9" },
            "items": [ { "kind": "plugin", "name": "x", "owner": "", "spec": "x" } ]
        }"#;
        let manifest: EnvironmentManifest =
            serde_json::from_str(legacy).expect("deserialize legacy");
        assert_eq!(manifest.compatible_with, "");
        assert!(manifest.exports.is_empty());
        assert_eq!(manifest.items.len(), 1);
    }
}
