use dsh_adapter::{DshAdapter, InstalledPlugin, PluginUpdate};
use launcher_core::{market, InstanceManifest};
use tauri::{AppHandle, State};

use crate::commands::process::{emit_log, make_sink};
use crate::error::AppError;
use crate::state::AppState;

/// Refuse plugin mutations while the target instance is running (its profile
/// files are being read by the live DSH process).
pub(crate) async fn ensure_not_running(state: &AppState, id: &str) -> Result<(), AppError> {
    let guard = state.child.lock().await;
    if let Some(running) = guard.as_ref() {
        if running.instance_id == id {
            return Err(AppError::msg(
                "stop the instance before changing its plugins".to_string(),
            ));
        }
    }
    Ok(())
}

/// Installed plugins for an instance (from its DSH profile's package.json).
#[tauri::command]
pub fn plugins_list(state: State<'_, AppState>, id: String) -> Result<Vec<InstalledPlugin>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(DshAdapter::installed_plugins(&instance))
}

/// Install a plugin (`dsh plugin add <target>`) into an instance, streaming
/// pnpm output to the Activity log.
#[tauri::command]
pub async fn plugin_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    target: String,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    emit_log(&app, &format!("{id} · installing plugin {target}…"));
    let sink = make_sink(app.clone());
    let code = state
        .adapter
        .run_plugin_command(&settings, &instance, &["add".to_string(), target.clone()], sink)
        .await?;
    if code != 0 {
        return Err(AppError::msg(format!(
            "dsh plugin add exited with code {code} — see Activity logs"
        )));
    }
    emit_log(&app, &format!("{id} · installed {target}"));
    Ok(())
}

/// Uninstall a plugin (`dsh plugin remove <name>`).
#[tauri::command]
pub async fn plugin_uninstall(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    // Capture the plugin's patch rows BEFORE `dsh plugin remove` deletes its
    // node_modules, then clear them so a removed plugin leaves no orphan
    // `disabled:` rows in cordis.patch.yml.
    let patch_ids = DshAdapter::plugin_row_ids(&instance, &name);

    emit_log(&app, &format!("{id} · removing plugin {name}…"));
    let sink = make_sink(app.clone());
    let code = state
        .adapter
        .run_plugin_command(&settings, &instance, &["remove".to_string(), name.clone()], sink)
        .await?;
    if code != 0 {
        return Err(AppError::msg(format!(
            "dsh plugin remove exited with code {code} — see Activity logs"
        )));
    }
    DshAdapter::remove_patch_rows(&instance, &patch_ids)?;
    emit_log(&app, &format!("{id} · removed {name}"));
    Ok(())
}

/// Enable/disable a plugin (writes `disabled` into `cordis.patch.yml`; DSH
/// hot-applies it, and it survives the `dsh plugin` bundle reconcile). The
/// plugin stays installed either way.
#[tauri::command]
pub async fn plugin_toggle(
    state: State<'_, AppState>,
    id: String,
    name: String,
    enabled: bool,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    DshAdapter::set_plugin_enabled(&instance, &name, enabled)?;
    Ok(())
}

/// Per-plugin update status: npm `latest` vs the installed version.
#[tauri::command]
pub async fn plugin_updates(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<PluginUpdate>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let installed = DshAdapter::installed_plugins(&instance);
    let registry = market::npm_registry();
    let mut out = Vec::new();
    for p in installed {
        // In-box DSH packages are not market-managed.
        if p.name.starts_with("@deepseek-ai/") {
            continue;
        }
        let Some(installed_ver) = DshAdapter::installed_version(&instance, &p.name) else {
            continue;
        };
        let Ok(latest) = market::npm_latest(&registry, &p.name).await else {
            continue;
        };
        let updatable = market::version_newer(&latest, &installed_ver);
        out.push(PluginUpdate {
            name: p.name,
            installed: installed_ver,
            latest,
            updatable,
        });
    }
    Ok(out)
}

/// Update a plugin to its latest (`dsh plugin update <name>`).
#[tauri::command]
pub async fn plugin_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    emit_log(&app, &format!("{id} · updating plugin {name}…"));
    let sink = make_sink(app.clone());
    let code = state
        .adapter
        .run_plugin_command(&settings, &instance, &["update".to_string(), name.clone()], sink)
        .await?;
    if code != 0 {
        return Err(AppError::msg(format!(
            "dsh plugin update exited with code {code} — see Activity logs"
        )));
    }
    emit_log(&app, &format!("{id} · updated {name}"));
    Ok(())
}
