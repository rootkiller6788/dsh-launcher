use std::sync::atomic::Ordering;

use launcher_core::AppSettings;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    Ok(state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone())
}

#[tauri::command]
pub fn set_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSettings, AppError> {
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?;
    *guard = settings.clone();
    guard.save(&state.paths)?;
    // #602: consent may have flipped — reflect it live so the panic hook's
    // sidecar decision follows the checkbox without an app restart.
    state
        .telemetry_consent
        .store(guard.telemetry_enabled, Ordering::Relaxed);
    Ok(settings)
}
