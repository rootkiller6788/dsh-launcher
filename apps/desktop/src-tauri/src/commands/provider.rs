use launcher_core::provider::DEFAULT_PROVIDER_ID;
use launcher_core::ProviderProfile;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub profile: ProviderProfile,
    pub has_key: bool,
}

#[tauri::command]
pub fn get_provider(state: State<'_, AppState>) -> Result<ProviderView, AppError> {
    let profile = state.vault.get(DEFAULT_PROVIDER_ID)?;
    let has_key = state.vault.has_key(&profile.id);
    Ok(ProviderView { profile, has_key })
}

/// Persist provider metadata, and the API key when one is supplied. An empty /
/// absent key leaves the stored credential untouched.
#[tauri::command]
pub fn set_provider(
    state: State<'_, AppState>,
    profile: ProviderProfile,
    api_key: Option<String>,
) -> Result<ProviderProfile, AppError> {
    let mut profile = profile;
    profile.id = DEFAULT_PROVIDER_ID.to_string();
    let key = api_key.as_deref().filter(|k| !k.trim().is_empty());
    state.vault.set(&profile, key)?;
    Ok(profile)
}

/// Remove the stored API key from Windows Credential Manager (metadata kept).
#[tauri::command]
pub fn remove_provider_key(state: State<'_, AppState>) -> Result<(), AppError> {
    state.vault.delete_key(DEFAULT_PROVIDER_ID)?;
    Ok(())
}
