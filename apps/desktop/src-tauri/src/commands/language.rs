use dsh_adapter::language;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Set the launcher's interface language. Persisted locally and, when a
/// harness is running, pushed to DSH's `locale.preference` (hot-applies — the
/// DSH window switches without a reload). DSH unreachable never fails the
/// command; the preference is still persisted and applied on the next launch.
#[tauri::command]
pub async fn set_language(
    state: State<'_, AppState>,
    language: String,
) -> Result<String, AppError> {
    if !matches!(language.as_str(), "en" | "zh") {
        return Err(AppError::msg(format!("invalid language `{language}`")));
    }
    {
        let mut settings = state
            .settings
            .lock()
            .map_err(|_| AppError::msg("settings lock poisoned"))?;
        settings.language = Some(language.clone());
        settings.save(&state.paths)?;
    }
    let port = state.child.lock().await.as_ref().and_then(|r| r.port);
    if let Some(port) = port {
        if let Err(e) = language::set_preference(port, &language).await {
            tracing::warn!(target: "dsh", "language push to DSH failed: {e:#}");
        }
    }
    Ok(language)
}

/// Read the running DSH's current language preference (`locale.preference`).
/// `Ok(None)` when no harness is up, the document doesn't exist yet, or the
/// read failed — the poll treats all of those as "nothing to adopt".
#[tauri::command]
pub async fn dsh_language(state: State<'_, AppState>) -> Result<Option<String>, AppError> {
    let port = state.child.lock().await.as_ref().and_then(|r| r.port);
    let Some(port) = port else {
        return Ok(None);
    };
    match language::get_preference(port).await {
        Ok(pref) => Ok(pref),
        Err(e) => {
            // Poll-safe: a transiently unreachable harness is not an error.
            tracing::warn!(target: "dsh", "language read from DSH failed: {e:#}");
            Ok(None)
        }
    }
}
