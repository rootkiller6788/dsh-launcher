use launcher_core::provider::{provider_presets, DEFAULT_PROVIDER_ID};
use launcher_core::{ProviderPreset, ProviderProfile, ProviderVault};
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub profile: ProviderProfile,
    pub has_key: bool,
}

fn view(vault: &ProviderVault, profile: ProviderProfile) -> ProviderView {
    let has_key = vault.has_key(&profile.id);
    ProviderView { profile, has_key }
}

/// The default provider (kept for compatibility — instances default to it).
#[tauri::command]
pub fn get_provider(state: State<'_, AppState>) -> Result<ProviderView, AppError> {
    let profile = state.vault.get(DEFAULT_PROVIDER_ID)?;
    Ok(view(&state.vault, profile))
}

/// Every stored provider, each with its key-presence flag.
#[tauri::command]
pub fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderView>, AppError> {
    let profiles = state.vault.list()?;
    Ok(profiles
        .into_iter()
        .map(|p| view(&state.vault, p))
        .collect())
}

/// The built-in preset library (~20 OpenAI-compatible providers + local).
#[tauri::command]
pub fn list_provider_presets() -> Vec<ProviderPreset> {
    provider_presets()
}

/// Persist provider metadata under `profile.id` (any id, upsert), and the API
/// key when one is supplied. An empty / absent key leaves the stored credential
/// untouched.
#[tauri::command]
pub fn save_provider(
    state: State<'_, AppState>,
    profile: ProviderProfile,
    api_key: Option<String>,
) -> Result<ProviderProfile, AppError> {
    let key = api_key.as_deref().filter(|k| !k.trim().is_empty());
    state.vault.set(&profile, key)?;
    Ok(profile)
}

/// Remove a provider and its stored key.
#[tauri::command]
pub fn delete_provider(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.vault.delete(&id)?;
    Ok(())
}

/// Remove the stored API key from the OS credential store (metadata kept).
#[tauri::command]
pub fn remove_provider_key(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    state.vault.delete_key(&id)?;
    Ok(())
}
