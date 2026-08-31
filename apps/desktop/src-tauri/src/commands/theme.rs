use dsh_adapter::theme;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Set the launcher's theme preference. Persisted locally and, when a harness
/// is running, pushed to DSH's `ui-theme.preference` (hot-applies — the DSH
/// window switches without a reload). DSH unreachable never fails the command;
/// the preference is still persisted and applied on the next launch.
#[tauri::command]
pub async fn set_theme(state: State<'_, AppState>, theme: String) -> Result<String, AppError> {
    if !matches!(theme.as_str(), "light" | "dark" | "system") {
        return Err(AppError::msg(format!("invalid theme `{theme}`")));
    }
    {
        let mut settings = state
            .settings
            .lock()
            .map_err(|_| AppError::msg("settings lock poisoned"))?;
        settings.theme = Some(theme.clone());
        settings.save(&state.paths)?;
    }
    let port = state.child.lock().await.as_ref().and_then(|r| r.port);
    if let Some(port) = port {
        if let Err(e) = theme::set_preference(port, &theme).await {
            tracing::warn!(target: "dsh", "theme push to DSH failed: {e:#}");
        }
    }
    Ok(theme)
}

/// Read the running DSH's current theme preference (`ui-theme.preference`).
/// `Ok(None)` when no harness is up, the document doesn't exist yet, or the
/// read failed — the poll treats all of those as "nothing to adopt".
#[tauri::command]
pub async fn dsh_theme(state: State<'_, AppState>) -> Result<Option<String>, AppError> {
    let port = state.child.lock().await.as_ref().and_then(|r| r.port);
    let Some(port) = port else {
        return Ok(None);
    };
    match theme::get_preference(port).await {
        Ok(pref) => Ok(pref),
        Err(e) => {
            // Poll-safe: a transiently unreachable harness is not an error.
            tracing::warn!(target: "dsh", "theme read from DSH failed: {e:#}");
            Ok(None)
        }
    }
}
