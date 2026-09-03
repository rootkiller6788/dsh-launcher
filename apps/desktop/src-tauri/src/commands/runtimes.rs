//! Runtime Manager — the Settings → Runtime panel's backend.
//!
//! Commands: `runtime_list` (whole panel in one call), `runtime_install`
//! (import a working DSH tree), `runtime_set_active`, `runtime_remove`,
//! `runtime_verify`, `runtime_repair`. Install/repair copy gigabytes via
//! robocopy, so they run inside `spawn_blocking` and are `async` commands.

use std::path::PathBuf;

use dsh_adapter::runtimes::{NodeInfo, RuntimeEntry, Runtimes, VerifyReport};
use launcher_core::AppSettings;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Everything the Settings → Runtime panel renders in one shot.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeManagerView {
    pub node: NodeInfo,
    /// The `runtimes/active` version, if any.
    pub active: Option<String>,
    pub runtimes: Vec<RuntimeEntry>,
    /// Non-fatal problems collecting the view (e.g. unreadable node).
    pub error: Option<String>,
}

fn manager(state: &AppState) -> Runtimes {
    Runtimes::new(state.paths.runtimes.clone())
}

fn locked_settings(state: &AppState) -> Result<AppSettings, AppError> {
    Ok(state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone())
}

#[tauri::command]
pub fn runtime_list(state: State<'_, AppState>) -> Result<RuntimeManagerView, AppError> {
    let settings = locked_settings(&state)?;
    let (node, node_err) = match state.adapter.node_info(&settings) {
        Ok((version, path)) => (
            NodeInfo {
                present: true,
                path: Some(path.display().to_string()),
                version: Some(version),
                error: None,
            },
            None,
        ),
        Err(e) => (
            NodeInfo {
                present: false,
                path: None,
                version: None,
                error: None,
            },
            Some(e.to_string()),
        ),
    };
    let mgr = manager(&state);
    let (runtimes, list_err) = match mgr.list() {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let error = node_err.or(list_err);
    Ok(RuntimeManagerView {
        node,
        active: mgr.active_version(),
        runtimes,
        error,
    })
}

/// Import a working DSH tree (a checkout with `apps/cli/lib/bin.js`) into the
/// managed runtimes dir. The copy is a full tree, so it can take a while.
#[tauri::command]
pub async fn runtime_install(
    state: State<'_, AppState>,
    source: String,
    version: Option<String>,
) -> Result<RuntimeEntry, AppError> {
    let mgr = manager(&state);
    let src = PathBuf::from(source.trim());
    tauri::async_runtime::spawn_blocking(move || mgr.install_from_source(&src, version.as_deref()))
        .await
        .map_err(|e| AppError::msg(format!("install task failed: {e}")))?
        .map_err(Into::into)
}

#[tauri::command]
pub fn runtime_set_active(state: State<'_, AppState>, version: String) -> Result<(), AppError> {
    manager(&state).set_active(&version)?;
    Ok(())
}

#[tauri::command]
pub fn runtime_remove(state: State<'_, AppState>, version: String) -> Result<(), AppError> {
    manager(&state).remove(&version)?;
    Ok(())
}

#[tauri::command]
pub fn runtime_verify(
    state: State<'_, AppState>,
    version: String,
) -> Result<VerifyReport, AppError> {
    let settings = locked_settings(&state)?;
    let node = state.adapter.resolve_node(&settings);
    manager(&state)
        .verify(&version, node.as_deref())
        .map_err(Into::into)
}

/// Reinstall a broken runtime from `source` (or just re-verify when it's fine).
#[tauri::command]
pub async fn runtime_repair(
    state: State<'_, AppState>,
    version: String,
    source: Option<String>,
) -> Result<RuntimeEntry, AppError> {
    let settings = locked_settings(&state)?;
    let node = state.adapter.resolve_node(&settings);
    let mgr = manager(&state);
    let src = source.map(|s| PathBuf::from(s.trim()));
    tauri::async_runtime::spawn_blocking(move || {
        mgr.repair(&version, src.as_deref(), node.as_deref())
    })
    .await
    .map_err(|e| AppError::msg(format!("repair task failed: {e}")))?
    .map_err(Into::into)
}
