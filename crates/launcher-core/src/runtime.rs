use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{AppSettings, ChildHandle, ExitSink, InstanceManifest, LogSink, ResolvedProvider};

/// The detected facts about a runtime installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub id: String,
    pub version: String,
    /// Absolute path to the harness CLI entry.
    pub bin_path: String,
    pub node_version: String,
    /// Absolute path to the Node executable that launched it, when known.
    pub node_path: Option<String>,
    /// Where the runtime came from: `override` | `bundled` | `managed` | `dev` | `path`.
    pub source: String,
}

/// The adapter contract. Deliberately tiny — first version ships a single
/// `DshAdapter`; other harnesses plug in later without redesigning the core.
#[allow(async_fn_in_trait)]
pub trait RuntimeAdapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// Locate the runtime and report what was found.
    fn detect(&self, settings: &AppSettings) -> Result<RuntimeInfo>;

    /// Build the child-process environment (API key, base URL, DSH_HOME, …).
    fn build_env(
        &self,
        provider: &ResolvedProvider,
        instance: &InstanceManifest,
    ) -> Result<HashMap<String, String>>;

    /// Spawn the harness as a managed child process.
    async fn launch(
        &self,
        settings: &AppSettings,
        instance: &InstanceManifest,
        env: &HashMap<String, String>,
        on_log: LogSink,
        on_exit: Option<ExitSink>,
    ) -> Result<ChildHandle>;
}
