use launcher_core::diagnostics::{check_tool, EnvItem};
use launcher_core::runtime::RuntimeInfo;
use launcher_core::RuntimeAdapter;
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
