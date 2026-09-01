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

/// Load (or return the cached) non-plugin content (themes/skills/MCP), fetching
/// from the hosted endpoint on first use and falling back to the bundled
/// snapshots when a kind is unreachable.
async fn ensure_content(state: &AppState) -> Result<Registry, AppError> {
    if let Some(c) = state
        .content
        .lock()
        .ok()
        .and_then(|g| g.as_ref().cloned())
    {
        return Ok(c);
    }
    let content = market::fetch_content().await?;
    if let Ok(mut g) = state.content.lock() {
        *g = Some(content.clone());
    }
    Ok(content)
}

/// The curated catalog: fetched plugins + live-fetched content (themes/skins,
/// skills, MCP servers, each with a bundled offline fallback), each entry tagged
/// with its [`ContentKind`]. Smart search stays plugin-only, so content is only
/// appended here at the response boundary.
#[tauri::command]
pub async fn market_registry(state: State<'_, AppState>) -> Result<Registry, AppError> {
    let plugins = ensure_registry(&state).await?;
    let content = ensure_content(&state).await?;
    Ok(market::extend_with_content(plugins, content))
}

/// Smart search: natural-language need → 3 validated bundle plans drawn from
/// the full merged catalog (plugins + themes + skills + MCP). The LLM call uses
/// the active instance's provider (key from the vault).
#[tauri::command]
pub async fn market_recommend(
    state: State<'_, AppState>,
    need: String,
) -> Result<RecommendResult, AppError> {
    let instance = crate::commands::instance::active_instance(&state, None)?;
    let provider = state.vault.resolve(&instance.provider_ref)?;
    let plugins = ensure_registry(&state).await?;
    let content = ensure_content(&state).await?;
    let registry = market::extend_with_content(plugins, content);
    Ok(market::recommend(&registry, &provider, &need).await?)
}
