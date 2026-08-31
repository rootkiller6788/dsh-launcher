use dsh_adapter::diagnostics;
use dsh_adapter::DiagnosticsReport;
use launcher_core::InstanceManifest;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Read-only introspection of an instance's DSH profile: bundle stack, duplicate
/// entry ids, orphan patch targets, and load-order constraints.
#[tauri::command]
pub fn profile_diagnostics(
    state: State<'_, AppState>,
    id: String,
) -> Result<DiagnosticsReport, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(diagnostics::diagnose_profile(&instance))
}
