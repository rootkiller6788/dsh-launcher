use std::path::PathBuf;

use anyhow::{anyhow, Result};
use directories::BaseDirs;

/// The on-disk layout for the whole launcher.
///
/// Default root is `%LOCALAPPDATA%/AIHarnessLauncher` (Windows). Set the
/// `AHL_HOME` env var to relocate it (used for dev/testing).
///
/// ```text
/// %LOCALAPPDATA%/AIHarnessLauncher/
/// ├── settings.json       # app settings (dshPath override, lastInstance)
/// ├── providers.json      # provider metadata — NEVER the API key
/// ├── runtimes/           # managed runtimes (future: Phase 4)
/// ├── instances/default/  # the (single) instance: instance.json + workspace/
/// ├── cache/
/// └── logs/launcher.log
/// ```
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub providers: PathBuf,
    pub runtimes: PathBuf,
    pub instances: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub launcher_log: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Result<Self> {
        let base = if let Ok(alt) = std::env::var("AHL_HOME") {
            PathBuf::from(alt)
        } else {
            let dirs = BaseDirs::new().ok_or_else(|| anyhow!("cannot resolve base directories"))?;
            dirs.data_local_dir().join("AIHarnessLauncher")
        };
        Ok(Self {
            root: base.clone(),
            settings: base.join("settings.json"),
            providers: base.join("providers.json"),
            runtimes: base.join("runtimes"),
            instances: base.join("instances"),
            cache: base.join("cache"),
            logs: base.join("logs"),
            launcher_log: base.join("logs").join("launcher.log"),
        })
    }

    /// Idempotently create every directory the launcher owns.
    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [&self.root, &self.runtimes, &self.instances, &self.cache, &self.logs] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn default_instance_file(&self) -> PathBuf {
        self.instance_file("default")
    }

    /// Directory for an instance's whole tree (`instances/<id>/`).
    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances.join(id)
    }

    /// The instance's manifest file (`instances/<id>/instance.json`).
    pub fn instance_file(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("instance.json")
    }

    /// The SQLite file for launch history / index (`root/launcher.db`).
    pub fn db_file(&self) -> PathBuf {
        self.root.join("launcher.db")
    }

    /// The spawned-PID ledger (`root/spawned.pids`). Written on every launch,
    /// swept on the next one, so a hard-killed launcher can't leave an orphan
    /// harness tree behind.
    pub fn pid_ledger(&self) -> PathBuf {
        self.root.join("spawned.pids")
    }
}
