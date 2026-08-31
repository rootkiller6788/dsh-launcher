use launcher_core::{market, RecommendResult, Registry};
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

/// Load (or return the cached) registry, fetching from the network on first use
/// and falling back to the on-disk cache when unreachable.
async fn ensure_registry(state: &AppState) -> Result<Registry, AppError> {
    if let Some(r) = state
        .registry
        .lock()
        .ok()
        .and_then(|g| g.as_ref().cloned())
    {
        return Ok(r);
    }
    let reg = market::fetch_registry(&state.paths).await?;
    if let Ok(mut g) = state.registry.lock() {
        *g = Some(reg.clone());
    }
    Ok(reg)
}

/// The curated plugin catalog (fetched + disk-cached).
#[tauri::command]
pub async fn market_registry(state: State<'_, AppState>) -> Result<Registry, AppError> {
    ensure_registry(&state).await
}

/// Smart search: natural-language need → 3 validated plugin-combination plans.
/// The LLM call uses the active instance's provider (key from the vault).
#[tauri::command]
pub async fn market_recommend(
    state: State<'_, AppState>,
    need: String,
) -> Result<RecommendResult, AppError> {
    let instance = crate::commands::instance::active_instance(&state, None)?;
    let provider = state.vault.resolve(&instance.provider_ref)?;
    let registry = ensure_registry(&state).await?;
    Ok(market::recommend(&registry, &provider, &need).await?)
}
