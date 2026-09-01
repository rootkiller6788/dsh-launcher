use dsh_adapter::content as content_adapter;
use launcher_core::market::ContentKind;
use launcher_core::{
    AppSettings, BundleItemResult, BundleManifest, BundleSummary, InstanceManifest, RegistryPlugin,
};
use tauri::{AppHandle, State};

use crate::commands::plugins::ensure_not_running;
use crate::commands::process::{emit_log, make_sink};
use crate::error::AppError;
use crate::state::AppState;

/// Installed skills for an instance (from its manifest — skills are plain files
/// under `$DSH_HOME/skills/`, with no npm package or enable state).
#[tauri::command]
pub fn skill_list(state: State<'_, AppState>, id: String) -> Result<Vec<String>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(content_adapter::installed_skills(&instance))
}

/// Install a skill: download its SKILL.md and record it in the manifest.
#[tauri::command]
pub async fn skill_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let skill = content_adapter::skill_id(&entry);
    emit_log(&app, &format!("{id} · installing skill {skill}…"));
    content_adapter::install_skill(&instance, &entry).await?;
    InstanceManifest::add_skill(&state.paths, &id, &skill)?;
    emit_log(&app, &format!("{id} · installed skill {skill}"));
    Ok(())
}

/// Uninstall a skill: remove its directory and drop it from the manifest.
#[tauri::command]
pub async fn skill_uninstall(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    skill: String,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    emit_log(&app, &format!("{id} · removing skill {skill}…"));
    content_adapter::uninstall_skill(&instance, &skill)?;
    InstanceManifest::remove_skill(&state.paths, &id, &skill)?;
    emit_log(&app, &format!("{id} · removed skill {skill}"));
    Ok(())
}

/// Installed MCP servers for an instance (from its manifest — MCP is a cordis
/// patch row, not an npm package).
#[tauri::command]
pub fn mcp_list(state: State<'_, AppState>, id: String) -> Result<Vec<String>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(content_adapter::installed_mcp(&instance))
}

/// Install an MCP server: append its `mcp-client` insert row to the instance's
/// profile `cordis.patch.yml` and record it in the manifest.
#[tauri::command]
pub async fn mcp_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let mcp = content_adapter::mcp_id(&entry);
    emit_log(&app, &format!("{id} · installing MCP {mcp}…"));
    content_adapter::install_mcp(&instance, &entry)?;
    InstanceManifest::add_mcp(&state.paths, &id, &mcp)?;
    emit_log(&app, &format!("{id} · installed MCP {mcp}"));
    Ok(())
}

/// Uninstall an MCP server: remove its insert row and drop it from the manifest.
#[tauri::command]
pub async fn mcp_uninstall(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<(), AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let mcp = content_adapter::mcp_id(&entry);
    emit_log(&app, &format!("{id} · removing MCP {mcp}…"));
    content_adapter::uninstall_mcp(&instance, &entry)?;
    InstanceManifest::remove_mcp(&state.paths, &id, &mcp)?;
    emit_log(&app, &format!("{id} · removed MCP {mcp}"));
    Ok(())
}

fn kind_label(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Plugin => "plugin",
        ContentKind::Theme => "skin",
        ContentKind::Skill => "skill",
        ContentKind::Mcp => "mcp",
        ContentKind::Bundle => "bundle",
    }
}

/// Install one bundle item via its kind's installer. Plugin/skin items go
/// through `dsh plugin add`; skill and MCP items reuse the content installers
/// and then update the manifest index.
async fn install_bundle_item(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    instance: &InstanceManifest,
    settings: &AppSettings,
    item: &RegistryPlugin,
) -> Result<(), AppError> {
    let key = item.key();
    let label = kind_label(item.kind);
    match item.kind {
        ContentKind::Plugin | ContentKind::Theme => {
            let spec = item.install_spec();
            if spec.trim().is_empty() {
                return Err(AppError::msg("no install spec (npm/tarball/url)"));
            }
            emit_log(app, &format!("{id} · installing {label} {spec}…"));
            let code = state
                .adapter
                .run_plugin_command(settings, instance, &["add".to_string(), spec], make_sink(app.clone()))
                .await?;
            if code != 0 {
                return Err(AppError::msg(format!("dsh plugin add exited with code {code}")));
            }
        }
        ContentKind::Skill => {
            emit_log(app, &format!("{id} · installing skill {key}…"));
            content_adapter::install_skill(instance, item).await?;
            InstanceManifest::add_skill(&state.paths, id, &key)?;
        }
        ContentKind::Mcp => {
            emit_log(app, &format!("{id} · installing MCP {key}…"));
            content_adapter::install_mcp(instance, item)?;
            InstanceManifest::add_mcp(&state.paths, id, &key)?;
        }
        ContentKind::Bundle => {
            return Err(AppError::msg("nested bundles are not supported"));
        }
    }
    Ok(())
}

/// Import a bundle manifest: dispatch each item to its kind's installer,
/// streaming per-item progress to Activity and collecting a per-item summary.
#[tauri::command]
pub async fn bundle_import(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    manifest: BundleManifest,
) -> Result<BundleSummary, AppError> {
    ensure_not_running(&state, &id).await?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    emit_log(
        &app,
        &format!(
            "{id} · importing bundle \"{}\" ({} items)…",
            manifest.name,
            manifest.items.len()
        ),
    );

    let mut summary = BundleSummary::default();
    for item in &manifest.items {
        let key = item.key();
        let label = kind_label(item.kind).to_string();
        match install_bundle_item(&state, &app, &id, &instance, &settings, item).await {
            Ok(()) => {
                summary.installed += 1;
                emit_log(&app, &format!("{id} · installed {label} {key}"));
                summary.results.push(BundleItemResult {
                    name: key,
                    kind: label,
                    ok: true,
                    error: None,
                });
            }
            Err(e) => {
                summary.failed += 1;
                emit_log(&app, &format!("{id} · FAILED {label} {key}: {e}"));
                summary.results.push(BundleItemResult {
                    name: key,
                    kind: label,
                    ok: false,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    emit_log(
        &app,
        &format!(
            "{id} · bundle \"{}\" done: {} installed, {} failed",
            manifest.name, summary.installed, summary.failed
        ),
    );
    Ok(summary)
}
