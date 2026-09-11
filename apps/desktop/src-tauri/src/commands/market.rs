use launcher_core::{market, RecommendResult, Registry};
use tauri::{AppHandle, State};

use crate::error::AppError;
use crate::state::AppState;

/// Load (or return the cached) registry, fetching from the network on first use
/// and falling back to the on-disk cache when unreachable.
async fn ensure_registry(state: &AppState) -> Result<Registry, AppError> {
    if let Some(r) = state.registry.lock().ok().and_then(|g| g.as_ref().cloned()) {
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
/// snapshots when a kind is unreachable. Cached in the state so the four files
/// are fetched once per session, not once per command.
///
/// The fallback is otherwise invisible — the catalog looks the same whether the
/// hosted files answered or the bundled snapshots did — so the first load that
/// had to fall back says so in Activity, where the user will actually see it.
pub(crate) async fn ensure_content(state: &AppState, app: &AppHandle) -> Registry {
    if let Some(c) = state.content.lock().ok().and_then(|g| g.as_ref().cloned()) {
        return c;
    }
    let (content, failures) = market::fetch_content_report().await;
    if !failures.is_empty() {
        crate::commands::process::emit_warn(app, &market::content_failure_summary(&failures));
    }
    if let Ok(mut g) = state.content.lock() {
        *g = Some(content.clone());
    }
    content
}

/// Fetched plugins + live-fetched content merged into the single catalog both
/// commands below serve. The merge lives here so the two cannot drift.
async fn merged_catalog(state: &AppState, app: &AppHandle) -> Result<Registry, AppError> {
    let plugins = ensure_registry(state).await?;
    let content = ensure_content(state, app).await;
    Ok(market::extend_with_content(plugins, content))
}

/// The curated catalog: fetched plugins + live-fetched content (themes/skins,
/// skills, MCP servers, each with a bundled offline fallback), each entry tagged
/// with its [`ContentKind`]. Smart search stays plugin-only, so content is only
/// appended here at the response boundary.
#[tauri::command]
pub async fn market_registry(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Registry, AppError> {
    merged_catalog(&state, &app).await
}

/// Smart search: natural-language need → 3 validated bundle plans drawn from
/// the full merged catalog (plugins + themes + skills + MCP). The LLM call uses
/// the active instance's provider (key from the vault).
#[tauri::command]
pub async fn market_recommend(
    state: State<'_, AppState>,
    app: AppHandle,
    need: String,
) -> Result<RecommendResult, AppError> {
    let instance = crate::commands::instance::active_instance(&state, None)?;
    let provider = state.vault.resolve(&instance.provider_ref)?;
    let registry = merged_catalog(&state, &app).await?;
    Ok(market::recommend(&registry, &provider, &need).await?)
}
