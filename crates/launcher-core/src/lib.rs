//! Launcher Core — the harness-agnostic heart of AI Harness Launcher.
//!
//! This crate deliberately knows nothing about DSH specifically: it owns the
//! app data layout (`paths`), the portable JSON manifests (`instance`), the
//! credential vault (`provider`), the process supervisor (`process`), and the
//! small `RuntimeAdapter` contract that DSH-specific behavior plugs into.

pub mod bundle;
pub mod capabilities;
pub mod crash;
pub mod diagnose;
pub mod diagnostics;
pub mod download;
pub mod environment;
pub mod error_code;
pub mod github;
pub mod history;
pub mod instance;
pub mod jobs;
pub mod llm;
pub mod mcp_config;
pub mod market;
pub mod paths;
pub mod pricing;
pub mod process;
pub mod provider;
pub mod redact;
pub mod runtime;
pub mod settings;
pub mod telemetry;
pub mod usage;

/// OS credential-store service name. The provider vault and the per-MCP config
/// store share one service and separate the entries by account name.
pub const KEYRING_SERVICE: &str = "ai-harness-launcher";

pub use bundle::{BundleItemResult, BundleManifest, BundleSummary};
pub use capabilities::{
    capability_for, CacheSource, ContentCapability, InstallAuthority, StateAuthority,
    CONTENT_CAPABILITIES,
};
pub use download::{download_file, file_sha256, part_path, sha256_hex, DownloadOutcome};
pub use environment::{EnvironmentManifest, EnvironmentSource, ExportedItem};
pub use error_code::{CodedError, ErrorCode};
pub use history::{LaunchHistory, LaunchSession};
pub use instance::{
    load_runtime, save_runtime, InstanceManifest, McpRuntimeState, McpServerRecord, RuntimeRef,
    SkillRecord, SkinPackage, MCP_STATE_DEGRADED, MCP_STATE_ERROR, MCP_STATE_OK, MCP_STATE_UNTESTED,
};
pub use jobs::{Job, JobKind, JobPlan, JobStatus, JobStore};
pub use market::{
    McpEnvRequirement, McpInstallManifest, McpLaunchSpec, RecommendPlan, RecommendResult, Registry,
    RegistryPlugin,
};
pub use mcp_config::{McpConfigStore, McpConfigVar};
pub use paths::AppPaths;
pub use pricing::{cost_for, lookup, Price};
pub use process::{
    ChildHandle, ExitSink, LogLevel, LogLine, LogSink, LogStream, ProcessState, ProcessStatus,
};
pub use provider::{ProviderPreset, ProviderProfile, ProviderVault, ResolvedProvider};
pub use redact::{redact_secrets, MASK};
pub use runtime::{RuntimeAdapter, RuntimeInfo};
pub use settings::AppSettings;
pub use usage::{
    NewUsageRecord, UsageBucket, UsageDimension, UsageLedger, UsageModelTotal, UsageRecord,
    UsageSummary,
};

/// Atomically write a JSON value to disk (temp file + rename).
pub fn write_json_atomic(path: &std::path::Path, value: &serde_json::Value) -> anyhow::Result<()> {
    use anyhow::Context;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
