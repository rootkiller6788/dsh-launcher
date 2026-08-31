use std::path::PathBuf;
use std::sync::Mutex;

use dsh_adapter::DshAdapter;
use launcher_core::process::ChildHandle;
use launcher_core::{AppPaths, AppSettings, LaunchHistory, ProviderVault, Registry};
use tokio::sync::Mutex as AsyncMutex;

/// A live harness child, tied to the instance that owns it.
pub struct RunningChild {
    pub instance_id: String,
    pub handle: ChildHandle,
    /// The DSH web port, once its ready URL is seen. Lets the theme commands
    /// reach the running harness's settings RPC.
    pub port: Option<u16>,
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
    /// Row id of the running session, if any (for closing it out on stop/crash).
    pub session_id: Mutex<Option<i64>>,
    /// Cached market registry (fetched lazily, avoids re-hitting the network).
    pub registry: Mutex<Option<Registry>>,
}

impl AppState {
    pub fn new(
        paths: AppPaths,
        settings: AppSettings,
        vault: ProviderVault,
        resource_dir: Option<PathBuf>,
    ) -> Self {
        let history = LaunchHistory::open(&paths.db_file())
            .expect("open launcher.db for history");
        let runtimes_dir = paths.runtimes.clone();
        Self {
            paths,
            settings: Mutex::new(settings),
            vault,
            adapter: DshAdapter::configured(runtimes_dir, resource_dir),
            child: AsyncMutex::new(None),
            history,
            session_id: Mutex::new(None),
            registry: Mutex::new(None),
        }
    }
}
