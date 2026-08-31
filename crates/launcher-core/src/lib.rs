//! Launcher Core — the harness-agnostic heart of AI Harness Launcher.
//!
//! This crate deliberately knows nothing about DSH specifically: it owns the
//! app data layout (`paths`), the portable JSON manifests (`instance`), the
//! credential vault (`provider`), the process supervisor (`process`), and the
//! small `RuntimeAdapter` contract that DSH-specific behavior plugs into.

pub mod diagnostics;
pub mod history;
pub mod instance;
pub mod market;
pub mod paths;
pub mod process;
pub mod provider;
pub mod runtime;
pub mod settings;

pub use history::{LaunchHistory, LaunchSession};
pub use instance::{InstanceManifest, RuntimeRef};
pub use market::{RecommendPlan, RecommendResult, Registry, RegistryPlugin};
pub use paths::AppPaths;
pub use process::{ChildHandle, LogLine, LogSink, LogStream, ProcessState, ProcessStatus};
pub use provider::{ProviderProfile, ProviderVault, ResolvedProvider};
pub use runtime::{RuntimeAdapter, RuntimeInfo};
pub use settings::AppSettings;

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
