//! Rescue-point commands and the two automatic hooks that feed them.
//!
//! The launcher mutates an instance's profile files on every install / toggle /
//! update (see `crates/dsh-adapter/src/rescue.rs` for the file set). A rescue
//! point is the pre-change copy of those files, so a bricked boot is one click
//! from recovery instead of a hand-edit or a reinstall.
//!
//! Two hooks, and they are deliberately asymmetric:
//!
//! - [`refresh_rescue_point`] runs after a successful boot — the files that just
//!   booted are *known good*, so they become the point to restore to.
//! - [`reserve_rescue_point`] runs before a destructive job and writes only when
//!   no point exists yet, so a second change made without a reboot cannot
//!   overwrite the last known-good copy with already-damaged files.

use dsh_adapter::rescue::{self, RescueStatus};
use launcher_core::InstanceManifest;
use tauri::{AppHandle, State};

use crate::commands::plugins::ensure_not_running;
use crate::commands::process::{emit_log, emit_warn};
use crate::error::AppError;
use crate::state::AppState;

/// Take a rescue point before a destructive change, unless one already exists.
///
/// Best-effort by design: a profile with no snapshot-able files (a fresh
/// instance that has never booted) or an unwritable data dir logs a warning and
/// lets the job proceed. Blocking an install because it could not be backed up
/// would be a worse trade than an unrescuable install, and the next successful
/// boot refreshes the point anyway.
pub fn reserve_rescue_point(state: &AppState, app: &AppHandle, id: &str) {
    let Ok(instance) = InstanceManifest::get(&state.paths, id) else {
        return;
    };
    let files = rescue::rescue_files(&instance);
    let dir = state.paths.rescue_dir(id);
    match rescue::snapshot_if_absent(&files, &dir) {
        Ok(Some(meta)) => emit_log(
            app,
            &format!(
                "{id} · rescue point taken ({} file(s), before installing)",
                meta.files.len()
            ),
        ),
        Ok(None) => {}
        Err(e) => emit_warn(app, &format!("{id} · could not take a rescue point: {e:#}")),
    }
}

/// Refresh the rescue point after a successful boot. Best-effort, like
/// [`reserve_rescue_point`].
pub fn refresh_rescue_point(state: &AppState, app: &AppHandle, id: &str) {
    let Ok(instance) = InstanceManifest::get(&state.paths, id) else {
        return;
    };
    let files = rescue::rescue_files(&instance);
    let dir = state.paths.rescue_dir(id);
    match rescue::create_snapshot(&files, &dir) {
        Ok(meta) => crate::commands::process::emit_debug(
            app,
            &format!(
                "{id} · rescue point refreshed after a good boot ({} file(s))",
                meta.files.len()
            ),
        ),
        Err(e) => emit_warn(
            app,
            &format!("{id} · could not refresh the rescue point: {e:#}"),
        ),
    }
}

/// Whether the instance has a rescue point, and what it holds.
#[tauri::command]
pub fn rescue_status(state: State<'_, AppState>, id: String) -> Result<RescueStatus, AppError> {
    Ok(rescue::snapshot_status(&state.paths.rescue_dir(&id)))
}

/// Take a rescue point now, overwriting any previous one.
///
/// The UI exposes this as "mark the current state as good", which is the manual
/// counterpart to the automatic refresh after a boot. Allowed while the instance
/// is running: the profile files it copies are not rewritten by a live harness,
/// only by the launcher's own jobs.
#[tauri::command]
pub async fn rescue_create(
    state: State<'_, AppState>,
    id: String,
) -> Result<RescueStatus, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let dir = state.paths.rescue_dir(&id);
    let files = rescue::rescue_files(&instance);
    rescue::create_snapshot(&files, &dir)?;
    Ok(rescue::snapshot_status(&dir))
}

/// Restore the instance's profile files from its rescue point.
///
/// Requires the instance to be stopped: restoring underneath a live harness
/// would be silently undone the next time it writes its config.
#[tauri::command]
pub async fn rescue_restore(
    state: State<'_, AppState>,
    id: String,
) -> Result<RescueStatus, AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let dir = state.paths.rescue_dir(&id);
    let files = rescue::rescue_files(&instance);
    rescue::restore_snapshot(&files, &dir)?;
    Ok(rescue::snapshot_status(&dir))
}
