use launcher_core::LaunchSession;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Most recent launch sessions, newest first (for the Activity history list).
#[tauri::command]
pub fn recent_sessions(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<LaunchSession>, AppError> {
    Ok(state.history.recent(limit.unwrap_or(50))?)
}
