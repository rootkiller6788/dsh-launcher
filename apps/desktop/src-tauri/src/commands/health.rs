use dsh_adapter::health::{health_report, HealthReport};
use launcher_core::InstanceManifest;
use tauri::{AppHandle, State};

use crate::commands::settings::settings_snapshot;
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

/// Measure the instance's health right now.
///
/// Read-only, and cheap — no subprocess is spawned (`resolve_bin` stats files,
/// it does not run `--version`), so this is safe to re-run after every fix and
/// every boot. It does take the instance's heavy-job gate anyway, like
/// `profile_diagnostics` does: a check that reads the profile files while an
/// install is halfway through writing them would report a fault that is really
/// just a write in progress.
#[tauri::command]
pub async fn instance_health(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<HealthReport, AppError> {
    let job_id = id.clone();
    let settings = settings_snapshot(&state)?;
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Diagnostics, || async {
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let rescue_dir = state.paths.rescue_dir(&id);
        Ok(health_report(
            &state.adapter,
            &settings,
            &instance,
            &rescue_dir,
        ))
    })
    .await
}
