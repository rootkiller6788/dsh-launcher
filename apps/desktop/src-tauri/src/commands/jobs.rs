use launcher_core::Job;
use tauri::{AppHandle, State};

use crate::error::AppError;
use crate::jobs::{emit_job, enqueue_install};
use crate::state::AppState;

/// Install Center history + live queue: newest first, bounded to what the panel
/// renders (reloaded state comes from `job-updated` events + this list).
#[tauri::command]
pub fn jobs_list(state: State<'_, AppState>) -> Result<Vec<Job>, AppError> {
    Ok(state.jobs.list(200)?)
}

/// Cancel a job that has not started yet. Only `waiting` rows are flipped —
/// running pnpm/git sub-processes are deliberately not force-killed.
#[tauri::command]
pub fn jobs_cancel(
    state: State<'_, AppState>,
    app: AppHandle,
    id: i64,
) -> Result<Option<Job>, AppError> {
    match state.jobs.cancel_if_waiting(id)? {
        Some(job) => {
            emit_job(&app, &job);
            Ok(Some(job))
        }
        None => Ok(None),
    }
}

/// Re-run a finished/failed/cancelled job from its persisted install plan. The
/// original `RegistryPlugin` / `BundleManifest` lives in the `plan` column, so
/// retry works from a cold start with no page-held object.
#[tauri::command]
pub async fn jobs_retry(
    state: State<'_, AppState>,
    app: AppHandle,
    id: i64,
) -> Result<Job, AppError> {
    let plan = state.jobs.plan(id)?;
    let existing = state
        .jobs
        .get(id)?
        .ok_or_else(|| AppError::msg("install job not found"))?;
    let label = existing.label;
    let key = existing.key;
    let instance_id = existing.instance_id;
    enqueue_install(&state, &app, &instance_id, &key, &label, plan).await
}

/// Remove a single job row from history.
#[tauri::command]
pub fn jobs_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    state.jobs.delete(id)?;
    Ok(())
}

/// Clear all terminal history (done / failed / cancelled) at once.
#[tauri::command]
pub fn jobs_clear_finished(state: State<'_, AppState>) -> Result<usize, AppError> {
    Ok(state.jobs.clear_finished()?)
}
