use launcher_core::diagnostics::{check_tool, EnvItem};
use launcher_core::runtime::RuntimeInfo;
use launcher_core::RuntimeAdapter;
use sysinfo::Disks;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub node: EnvItem,
    pub git: EnvItem,
    pub dsh: Option<RuntimeInfo>,
    pub dsh_error: Option<String>,
}

/// One-shot environment snapshot for the Settings "detect" panel.
#[tauri::command]
pub fn system_info(state: State<'_, AppState>) -> Result<SystemInfo, AppError> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();
    let node = check_tool("node", "--version");
    let git = check_tool("git", "--version");
    let (dsh, dsh_error) = match state.adapter.detect(&settings) {
        Ok(info) => (Some(info), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(SystemInfo {
        node,
        git,
        dsh,
        dsh_error,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    /// Global CPU usage, percent (since the previous poll).
    pub cpu: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
}

/// Real CPU / memory / disk sampling for the Overview "Runtime Resources" card.
/// Uses the persistent `AppState::monitor` `System` so CPU usage is the average
/// since the last call — the frontend polls this on a timer and draws the
/// samples as sparklines. Never fabricated.
#[tauri::command]
pub async fn system_stats(state: State<'_, AppState>) -> Result<SystemStats, AppError> {
    let mut sys = state
        .monitor
        .lock()
        .map_err(|_| AppError::msg("monitor lock poisoned"))?;
    sys.refresh_cpu_usage();
    let cpu = sys.global_cpu_usage();
    sys.refresh_memory();
    let memory_total = sys.total_memory();
    let memory_used = sys.used_memory();

    // Report the disk that holds the launcher's home (the drive the user's
    // instances actually live on); fall back to the first disk if it can't be
    // matched (e.g. a drive-relative root on macOS).
    let root = state.paths.root.to_string_lossy().to_uppercase();
    let disks = Disks::new_with_refreshed_list();
    let disk = disks
        .iter()
        .find(|d| root.starts_with(&d.mount_point().to_string_lossy().to_uppercase()))
        .or_else(|| disks.iter().next());
    let (disk_used, disk_total) = match disk {
        Some(d) => {
            let total = d.total_space();
            (total.saturating_sub(d.available_space()), total)
        }
        None => (0, 0),
    };

    Ok(SystemStats {
        cpu,
        memory_used,
        memory_total,
        disk_used,
        disk_total,
    })
}
