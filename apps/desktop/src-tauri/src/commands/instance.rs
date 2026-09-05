use launcher_core::InstanceManifest;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameRequest {
    pub name: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdNameRequest {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub fn list_instances(state: State<'_, AppState>) -> Result<Vec<InstanceManifest>, AppError> {
    Ok(InstanceManifest::list(&state.paths)?)
}

/// The active instance: `id` when given, else `settings.lastInstance` (with
/// fallback to `default`, then the first instance).
#[tauri::command]
pub fn get_instance(
    state: State<'_, AppState>,
    id: Option<String>,
) -> Result<InstanceManifest, AppError> {
    active_instance(&state, id.as_deref())
}

#[tauri::command]
pub fn create_instance(
    state: State<'_, AppState>,
    request: NameRequest,
) -> Result<InstanceManifest, AppError> {
    let manifest = InstanceManifest::create(&state.paths, &request.name)?;
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?;
    if guard.last_instance.is_none() {
        guard.last_instance = Some(manifest.id.clone());
        guard.save(&state.paths)?;
    }
    Ok(manifest)
}

#[tauri::command]
pub fn rename_instance(
    state: State<'_, AppState>,
    request: IdNameRequest,
) -> Result<InstanceManifest, AppError> {
    Ok(InstanceManifest::rename(
        &state.paths,
        &request.id,
        &request.name,
    )?)
}

#[tauri::command]
pub fn clone_instance(
    state: State<'_, AppState>,
    request: IdNameRequest,
) -> Result<InstanceManifest, AppError> {
    Ok(InstanceManifest::clone(
        &state.paths,
        &request.id,
        &request.name,
    )?)
}

#[tauri::command]
pub async fn delete_instance(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    // Refuse to delete a running instance.
    let guard = state.child.lock().await;
    if let Some(running) = guard.as_ref() {
        if running.instance_id == id {
            return Err(AppError::msg(
                "stop the instance before deleting it".to_string(),
            ));
        }
    }
    drop(guard);
    InstanceManifest::delete(&state.paths, &id)?;
    // If the deleted instance was active, move the active marker elsewhere.
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?;
    if settings.last_instance.as_deref() == Some(id.as_str()) {
        let fallback = InstanceManifest::list(&state.paths)?
            .into_iter()
            .map(|m| m.id)
            .next();
        settings.last_instance = fallback;
        settings.save(&state.paths)?;
    }
    Ok(())
}

#[tauri::command]
pub fn switch_instance(
    state: State<'_, AppState>,
    id: String,
) -> Result<InstanceManifest, AppError> {
    // Verify it exists first.
    let manifest = InstanceManifest::get(&state.paths, &id)?;
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?;
    guard.last_instance = Some(id);
    guard.save(&state.paths)?;
    Ok(manifest)
}

pub(crate) fn active_instance(
    state: &AppState,
    explicit: Option<&str>,
) -> Result<InstanceManifest, AppError> {
    if let Some(id) = explicit {
        return Ok(InstanceManifest::get(&state.paths, id)?);
    }
    let last = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .last_instance
        .clone();
    if let Some(id) = last {
        if let Ok(manifest) = InstanceManifest::get(&state.paths, &id) {
            return Ok(manifest);
        }
    }
    // Fallback: `default` if it exists, else the first instance.
    let all = InstanceManifest::list(&state.paths)?;
    if let Some(default) = all.iter().find(|m| m.id == "default") {
        return Ok(default.clone());
    }
    all.into_iter()
        .next()
        .ok_or_else(|| AppError::msg("no instances exist — create one"))
}
