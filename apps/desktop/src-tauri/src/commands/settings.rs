use std::sync::atomic::Ordering;
use std::sync::MutexGuard;

use launcher_core::AppSettings;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// The held settings guard. Every command that mutates settings takes the lock
/// through here, so the poisoned-mutex case reads the same across the app.
pub(crate) fn settings_lock(state: &AppState) -> Result<MutexGuard<'_, AppSettings>, AppError> {
    state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))
}

/// A snapshot of the current settings, for the commands that only read them.
pub(crate) fn settings_snapshot(state: &AppState) -> Result<AppSettings, AppError> {
    Ok(settings_lock(state)?.clone())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    settings_snapshot(&state)
}

#[tauri::command]
pub fn set_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<AppSettings, AppError> {
    let mut guard = settings_lock(&state)?;
    *guard = settings.clone();
    guard.save(&state.paths)?;
    // #602: consent may have flipped — reflect it live so the panic hook's
    // sidecar decision follows the checkbox without an app restart.
    state
        .telemetry_consent
        .store(guard.telemetry_enabled, Ordering::Relaxed);
    Ok(settings)
}
