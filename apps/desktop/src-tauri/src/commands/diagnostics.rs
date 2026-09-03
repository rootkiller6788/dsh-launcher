use dsh_adapter::diagnostics;
use dsh_adapter::DiagnosticsReport;
use launcher_core::InstanceManifest;
use tauri::{AppHandle, State};

use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

/// Introspect an instance's DSH profile after cleaning stale launcher-managed
/// toggle rows that target no mounted bundle entry.
#[tauri::command]
pub async fn profile_diagnostics(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<DiagnosticsReport, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Diagnostics, || async {
        let instance = InstanceManifest::get(&state.paths, &id)?;
        diagnostics::repair_orphan_toggle_rows(&instance)?;
        Ok(diagnostics::diagnose_profile(&instance))
    })
    .await
}
