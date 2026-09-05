use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;

use dsh_adapter::DshAdapter;
use launcher_core::process::ChildHandle;
use launcher_core::{
    AppPaths, AppSettings, JobStore, LaunchHistory, ProviderVault, Registry, UsageLedger,
};
use std::collections::{HashMap, HashSet};
use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;

use crate::jobs::InstanceJobGate;

/// A live harness child, tied to the instance that owns it.
pub struct RunningChild {
    pub instance_id: String,
    pub handle: ChildHandle,
    /// DSH web UI URL, once its ready line is seen. The main Studio window uses
    /// it for the Workspace mode.
    pub url: Option<String>,
    /// The DSH web port, once its ready URL is seen. Lets the theme commands
    /// reach the running harness's settings RPC.
    pub port: Option<u16>,
    /// Local OpenAI-compatible proxy used to capture response usage payloads.
    pub usage_proxy_shutdown: Option<oneshot::Sender<()>>,
    /// Cancels the DSH settings SSE subscription (appearance/language watch).
    pub settings_watch_shutdown: Option<oneshot::Sender<()>>,
}

/// Managed application state shared across commands.
pub struct AppState {
    pub paths: AppPaths,
    /// Sync lock: reads are brief and never await.
    pub settings: Mutex<AppSettings>,
    pub vault: ProviderVault,
    pub adapter: DshAdapter,
    /// The live harness child, if any. Async lock because launch/stop await.
    pub child: AsyncMutex<Option<RunningChild>>,
    /// Launch-history store (SQLite).
    pub history: LaunchHistory,
    /// Request-level usage ledger (SQLite).
    pub usage: UsageLedger,
    /// Persistent install-job ledger (SQLite): the queue + history Install
    /// Center renders. Survives window reloads and app restarts.
    pub jobs: JobStore,
    /// Instance ids that currently have a drainer task draining their waiting
    /// install jobs — guards against spawning two drainers for one instance.
    pub drainers: Mutex<HashSet<String>>,
    /// Row id of the running session, if any (for closing it out on stop/crash).
    pub session_id: Mutex<Option<i64>>,
    /// Cached market registry (fetched lazily, avoids re-hitting the network).
    pub registry: Mutex<Option<Registry>>,
    /// Cached non-plugin content (themes/skills/MCP), fetched lazily the same way.
    pub content: Mutex<Option<Registry>>,
    /// Persistent system sampler for the Overview "Runtime Resources" sparklines.
    /// Kept alive so `global_cpu_usage()` returns the usage since the previous
    /// poll instead of a one-shot instant.
    pub monitor: Mutex<sysinfo::System>,
    /// Per-instance queue gates for heavy DSH/profile work.
    pub heavy_jobs: Mutex<HashMap<String, Arc<InstanceJobGate>>>,
    /// Live crash-telemetry consent (#602). Read by the panic hook's sidecar
    /// decision at crash time; the Preferences toggle writes it here so a crash
    /// honours the choice in effect when it happens, not startup's.
    pub telemetry_consent: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(
        paths: AppPaths,
        settings: AppSettings,
        vault: ProviderVault,
        resource_dir: Option<PathBuf>,
        telemetry_consent: Arc<AtomicBool>,
    ) -> Self {
        let history = LaunchHistory::open(&paths.db_file()).expect("open launcher.db for history");
        let usage = UsageLedger::open(&paths.db_file()).expect("open launcher.db for usage");
        let jobs = JobStore::open(&paths.db_file()).expect("open launcher.db for install jobs");
        let runtimes_dir = paths.runtimes.clone();
        Self {
            paths,
            settings: Mutex::new(settings),
            vault,
            adapter: DshAdapter::configured(runtimes_dir, resource_dir),
            child: AsyncMutex::new(None),
            history,
            usage,
            jobs,
            drainers: Mutex::new(HashSet::new()),
            session_id: Mutex::new(None),
            registry: Mutex::new(None),
            content: Mutex::new(None),
            monitor: Mutex::new(sysinfo::System::new()),
            heavy_jobs: Mutex::new(HashMap::new()),
            telemetry_consent,
        }
    }
}
