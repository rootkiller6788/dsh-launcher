use dsh_adapter::{DshAdapter, InstalledPlugin, InstalledPluginSource, PluginUpdate};
use launcher_core::{market, InstanceManifest, RegistryPlugin};
use market::ContentKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use tokio::process::Command;

use crate::commands::process::{emit_log, make_sink};
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

#[derive(Debug, Clone)]
struct GithubPluginSpec {
    owner: String,
    repo: String,
    reference: Option<String>,
}

const LIBRARY_INVENTORY_EVENT: &str = "library-inventory-updated";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LibraryInventoryCache {
    schema_version: u32,
    #[serde(default)]
    instance_id: String,
    updated_at: u64,
    #[serde(default, alias = "plugins")]
    dsh_inventory: Vec<InstalledPlugin>,
    #[serde(default)]
    launcher_metadata: HashMap<String, MarketInstallMetadata>,
    #[serde(default)]
    install_sources: HashMap<String, InstallSourceMetadata>,
    skills: Vec<String>,
    mcp: Vec<String>,
    skins: Vec<String>,
    #[serde(default)]
    #[serde(skip_serializing)]
    market: Vec<MarketInstallMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarketInstallMetadata {
    pub key: String,
    pub kind: ContentKind,
    pub name: String,
    pub owner: String,
    pub install_spec: String,
    pub installed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallSourceMetadata {
    pub source: LibraryItemSource,
    pub installed_at: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInventorySummary {
    pub instance_id: String,
    pub plugins: usize,
    pub skills: usize,
    pub mcp: usize,
    pub skins: usize,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInventoryDetail {
    pub instance_id: String,
    pub updated_at: u64,
    pub items: Vec<LibraryInventoryItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInventoryItem {
    pub id: String,
    pub kind: ContentKind,
    pub title: String,
    pub package_name: Option<String>,
    pub enabled: Option<bool>,
    pub toggleable: bool,
    pub source: LibraryItemSource,
    pub state_source: LibraryStateSource,
    pub detail: Option<String>,
    pub market: Option<MarketInstallMetadata>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub enum LibraryItemSource {
    DshNative,
    MarketInstalled,
    LocalFile,
    ImportedEnvironment,
    UnknownDetected,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LibraryStateSource {
    DshInventory,
    DshWorkspaceFiles,
    LauncherSnapshot,
}

fn library_inventory_cache_file(state: &AppState, id: &str) -> PathBuf {
    state.paths.instance_dir(id).join("library-inventory.json")
}

fn legacy_plugin_inventory_cache_file(state: &AppState, id: &str) -> PathBuf {
    state.paths.instance_dir(id).join("plugin-inventory.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn read_library_inventory_cache(state: &AppState, id: &str) -> LibraryInventoryCache {
    let path = library_inventory_cache_file(state, id);
    let path = if path.exists() {
        path
    } else {
        legacy_plugin_inventory_cache_file(state, id)
    };
    let Ok(bytes) = fs::read(path) else {
        return LibraryInventoryCache::default();
    };
    if let Ok(mut cache) = serde_json::from_slice::<LibraryInventoryCache>(&bytes) {
        normalize_library_inventory_cache(id, &mut cache);
        return cache;
    }
    if let Ok(plugins) = serde_json::from_slice::<Vec<InstalledPlugin>>(&bytes) {
        let mut cache = LibraryInventoryCache {
            schema_version: 1,
            instance_id: id.to_string(),
            updated_at: 0,
            dsh_inventory: plugins,
            ..LibraryInventoryCache::default()
        };
        normalize_library_inventory_cache(id, &mut cache);
        return cache;
    }
    LibraryInventoryCache::default()
}

fn normalize_library_inventory_cache(id: &str, cache: &mut LibraryInventoryCache) {
    if cache.instance_id.is_empty() {
        cache.instance_id = id.to_string();
    }
    if cache.schema_version < 3 {
        cache.schema_version = 3;
    }
    for item in cache.market.drain(..) {
        cache
            .install_sources
            .entry(item.key.clone())
            .or_insert(InstallSourceMetadata {
                source: LibraryItemSource::MarketInstalled,
                installed_at: item.installed_at,
                detail: Some("legacy market metadata".to_string()),
            });
        cache
            .launcher_metadata
            .entry(item.key.clone())
            .or_insert(item);
    }
    for item in cache.launcher_metadata.values() {
        cache
            .install_sources
            .entry(item.key.clone())
            .or_insert(InstallSourceMetadata {
                source: LibraryItemSource::MarketInstalled,
                installed_at: item.installed_at,
                detail: None,
            });
    }
}

fn merge_plugin_sources(
    mut inventory: Vec<InstalledPlugin>,
    profile: Vec<InstalledPlugin>,
) -> Vec<InstalledPlugin> {
    for item in profile {
        let exists = inventory.iter().any(|inv| {
            inv.name == item.name
                || inv.entry_id.as_deref() == Some(item.name.as_str())
                || item.entry_id.as_deref() == Some(inv.name.as_str())
        });
        if !exists {
            inventory.push(item);
        }
    }
    inventory.sort_by(|a, b| {
        let source_a = matches!(a.source, InstalledPluginSource::Profile);
        let source_b = matches!(b.source, InstalledPluginSource::Profile);
        source_b.cmp(&source_a).then_with(|| a.name.cmp(&b.name))
    });
    inventory
}

fn write_library_inventory_cache(
    state: &AppState,
    id: &str,
    cache: &LibraryInventoryCache,
) -> Result<(), AppError> {
    let path = library_inventory_cache_file(state, id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::msg(format!("create inventory cache directory failed: {e}")))?;
    }
    let body = serde_json::to_vec_pretty(cache)
        .map_err(|e| AppError::msg(format!("serialize inventory cache failed: {e}")))?;
    fs::write(&path, body)
        .map_err(|e| AppError::msg(format!("write inventory cache failed: {e}")))?;
    Ok(())
}

pub(crate) fn rebuild_library_inventory_cache_from_disk(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    reason: &str,
) -> Result<(), AppError> {
    let instance = InstanceManifest::get(&state.paths, id)?;
    let cached = read_library_inventory_cache(state, id);
    let profile = DshAdapter::installed_plugins(&instance);
    let cache = LibraryInventoryCache {
        schema_version: 3,
        instance_id: id.to_string(),
        updated_at: now_secs(),
        dsh_inventory: merge_plugin_sources(cached.dsh_inventory, profile),
        launcher_metadata: cached.launcher_metadata,
        install_sources: cached.install_sources,
        skills: instance.skills,
        mcp: instance.mcp,
        skins: instance.skins,
        ..LibraryInventoryCache::default()
    };
    write_library_inventory_cache(state, id, &cache)?;
    emit_log(
        app,
        &format!("{id} · Library inventory updated after {reason}"),
    );
    let _ = app.emit(LIBRARY_INVENTORY_EVENT, id.to_string());
    Ok(())
}

fn library_inventory_summary_for(
    state: &AppState,
    instance: &InstanceManifest,
) -> LibraryInventorySummary {
    let cache = read_library_inventory_cache(state, &instance.id);
    let plugins = if cache.dsh_inventory.is_empty() {
        instance.plugins.len()
    } else {
        cache.dsh_inventory.len()
    };
    LibraryInventorySummary {
        instance_id: instance.id.clone(),
        plugins,
        skills: if cache.skills.is_empty() {
            instance.skills.len()
        } else {
            cache.skills.len()
        },
        mcp: if cache.mcp.is_empty() {
            instance.mcp.len()
        } else {
            cache.mcp.len()
        },
        skins: if cache.skins.is_empty() {
            instance.skins.len()
        } else {
            cache.skins.len()
        },
        updated_at: cache.updated_at,
    }
}

fn market_metadata_for_plugin_values<'a>(
    metadata: &'a HashMap<String, MarketInstallMetadata>,
    plugin: &InstalledPlugin,
) -> Option<&'a MarketInstallMetadata> {
    let name = plugin.name.to_lowercase();
    let entry_id = plugin.entry_id.as_deref().unwrap_or("").to_lowercase();
    metadata.values().find(|item| {
        let key = item.key.to_lowercase();
        let install = item.install_spec.to_lowercase();
        let short_name = item.name.to_lowercase();
        name == install
            || name == key
            || name.contains(&short_name)
            || entry_id == key
            || entry_id == short_name
    })
}

fn market_metadata_for_key_values<'a>(
    metadata: &'a HashMap<String, MarketInstallMetadata>,
    kind: ContentKind,
    key: &str,
) -> Option<&'a MarketInstallMetadata> {
    metadata
        .get(key)
        .filter(|item| item.kind == kind)
        .or_else(|| {
            metadata
                .values()
                .find(|item| item.kind == kind && item.key == key)
        })
}

fn skin_key_matches_plugin(skin: &str, plugin: &InstalledPlugin) -> bool {
    let tail = skin.rsplit('/').next().unwrap_or(skin).to_lowercase();
    let normalized = skin.replace(['/', '-'], "__").to_lowercase();
    let name = plugin.name.to_lowercase();
    name.contains(&tail) || name == normalized || plugin.entry_id.as_deref() == Some(skin)
}

fn plugin_library_kind(cache: &LibraryInventoryCache, plugin: &InstalledPlugin) -> ContentKind {
    if plugin.kind == dsh_adapter::InstalledPluginKind::Theme
        || cache
            .skins
            .iter()
            .any(|skin| skin_key_matches_plugin(skin, plugin))
        || market_metadata_for_plugin_values(&cache.launcher_metadata, plugin)
            .is_some_and(|item| item.kind == ContentKind::Theme)
    {
        ContentKind::Theme
    } else {
        ContentKind::Plugin
    }
}

fn library_item_source(
    plugin: &InstalledPlugin,
    metadata: Option<&MarketInstallMetadata>,
    install_source: Option<&InstallSourceMetadata>,
) -> LibraryItemSource {
    if let Some(source) = install_source {
        source.source
    } else if metadata.is_some() {
        LibraryItemSource::MarketInstalled
    } else if matches!(plugin.source, InstalledPluginSource::Inventory) {
        LibraryItemSource::DshNative
    } else {
        LibraryItemSource::UnknownDetected
    }
}

fn library_inventory_detail_for(
    state: &AppState,
    instance: &InstanceManifest,
) -> LibraryInventoryDetail {
    let cache = read_library_inventory_cache(state, &instance.id);
    let mut items = Vec::new();

    for plugin in &cache.dsh_inventory {
        let kind = plugin_library_kind(&cache, plugin);
        let metadata = market_metadata_for_plugin_values(&cache.launcher_metadata, plugin).cloned();
        let install_source = metadata
            .as_ref()
            .and_then(|item| cache.install_sources.get(&item.key));
        items.push(LibraryInventoryItem {
            id: plugin
                .entry_id
                .clone()
                .unwrap_or_else(|| plugin.name.clone()),
            kind,
            title: metadata
                .as_ref()
                .map(|item| item.key.clone())
                .unwrap_or_else(|| plugin.name.clone()),
            package_name: Some(plugin.name.clone()),
            enabled: Some(plugin.enabled),
            toggleable: plugin.toggleable,
            source: library_item_source(plugin, metadata.as_ref(), install_source),
            state_source: if matches!(plugin.source, InstalledPluginSource::Inventory) {
                LibraryStateSource::DshInventory
            } else {
                LibraryStateSource::LauncherSnapshot
            },
            detail: plugin.fiber_phase.clone(),
            market: metadata,
        });
    }

    for skill in &cache.skills {
        let metadata =
            market_metadata_for_key_values(&cache.launcher_metadata, ContentKind::Skill, skill)
                .cloned();
        let install_source = cache.install_sources.get(skill);
        items.push(LibraryInventoryItem {
            id: skill.clone(),
            kind: ContentKind::Skill,
            title: skill.clone(),
            package_name: None,
            enabled: None,
            toggleable: false,
            source: install_source
                .map(|item| item.source)
                .or_else(|| {
                    metadata
                        .as_ref()
                        .map(|_| LibraryItemSource::MarketInstalled)
                })
                .unwrap_or(LibraryItemSource::LocalFile),
            state_source: LibraryStateSource::DshWorkspaceFiles,
            detail: Some("skills/".to_string()),
            market: metadata,
        });
    }

    for mcp in &cache.mcp {
        let metadata =
            market_metadata_for_key_values(&cache.launcher_metadata, ContentKind::Mcp, mcp)
                .cloned();
        let install_source = cache.install_sources.get(mcp);
        items.push(LibraryInventoryItem {
            id: mcp.clone(),
            kind: ContentKind::Mcp,
            title: mcp.clone(),
            package_name: None,
            enabled: None,
            toggleable: false,
            source: install_source
                .map(|item| item.source)
                .or_else(|| {
                    metadata
                        .as_ref()
                        .map(|_| LibraryItemSource::MarketInstalled)
                })
                .unwrap_or(LibraryItemSource::LocalFile),
            state_source: LibraryStateSource::DshWorkspaceFiles,
            detail: Some("mcp-client patch".to_string()),
            market: metadata,
        });
    }

    for skin in &cache.skins {
        let already_present = items.iter().any(|item| {
            item.kind == ContentKind::Theme && (item.id == *skin || item.title == *skin)
        });
        if already_present {
            continue;
        }
        let metadata =
            market_metadata_for_key_values(&cache.launcher_metadata, ContentKind::Theme, skin)
                .cloned();
        let install_source = cache.install_sources.get(skin);
        items.push(LibraryInventoryItem {
            id: skin.clone(),
            kind: ContentKind::Theme,
            title: skin.clone(),
            package_name: None,
            enabled: None,
            toggleable: false,
            source: install_source
                .map(|item| item.source)
                .or_else(|| {
                    metadata
                        .as_ref()
                        .map(|_| LibraryItemSource::MarketInstalled)
                })
                .unwrap_or(LibraryItemSource::LocalFile),
            state_source: LibraryStateSource::LauncherSnapshot,
            detail: Some("skin classification".to_string()),
            market: metadata,
        });
    }

    LibraryInventoryDetail {
        instance_id: instance.id.clone(),
        updated_at: cache.updated_at,
        items,
    }
}

pub(crate) async fn refresh_plugin_inventory_cache(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    port: u16,
    reason: &str,
) -> Result<usize, AppError> {
    let inventory = DshAdapter::plugin_inventory(port)
        .await
        .map_err(AppError::from)?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let cached = read_library_inventory_cache(state, id);
    let cache = LibraryInventoryCache {
        schema_version: 3,
        instance_id: id.to_string(),
        updated_at: now_secs(),
        dsh_inventory: merge_plugin_sources(inventory, cached.dsh_inventory),
        launcher_metadata: cached.launcher_metadata,
        install_sources: cached.install_sources,
        skills: instance.skills,
        mcp: instance.mcp,
        skins: instance.skins,
        ..LibraryInventoryCache::default()
    };
    let count = cache.dsh_inventory.len();
    write_library_inventory_cache(state, id, &cache)?;
    emit_log(
        app,
        &format!(
            "{id} · DSH inventory cache refreshed after {reason} ({} plugins)",
            count
        ),
    );
    let _ = app.emit(LIBRARY_INVENTORY_EVENT, id.to_string());
    Ok(count)
}

pub(crate) fn record_market_install_metadata(
    state: &AppState,
    id: &str,
    entry: &RegistryPlugin,
) -> Result<(), AppError> {
    record_install_metadata_with_source(state, id, entry, LibraryItemSource::MarketInstalled)
}

pub(crate) fn record_install_metadata_with_source(
    state: &AppState,
    id: &str,
    entry: &RegistryPlugin,
    source: LibraryItemSource,
) -> Result<(), AppError> {
    let mut cache = read_library_inventory_cache(state, id);
    let key = entry.key();
    let installed_at = now_secs();
    cache.launcher_metadata.insert(
        key.clone(),
        MarketInstallMetadata {
            key: key.clone(),
            kind: entry.kind,
            name: entry.name.clone(),
            owner: entry.owner.clone(),
            install_spec: entry.install_spec(),
            installed_at,
        },
    );
    cache.install_sources.insert(
        key,
        InstallSourceMetadata {
            source,
            installed_at,
            detail: Some(
                match source {
                    LibraryItemSource::DshNative => "DSH native",
                    LibraryItemSource::MarketInstalled => "market install",
                    LibraryItemSource::LocalFile => "local file",
                    LibraryItemSource::ImportedEnvironment => "environment import",
                    LibraryItemSource::UnknownDetected => "detected",
                }
                .to_string(),
            ),
        },
    );
    cache.schema_version = 3;
    cache.updated_at = now_secs();
    write_library_inventory_cache(state, id, &cache)
}

pub(crate) async fn reconcile_library_inventory_after_market_change(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    reason: &str,
) -> Result<(), AppError> {
    let running_port = {
        let guard = state.child.lock().await;
        guard
            .as_ref()
            .filter(|running| running.instance_id == id)
            .and_then(|running| running.port)
    };
    if let Some(port) = running_port {
        refresh_plugin_inventory_cache(state, app, id, port, reason).await?;
    } else {
        rebuild_library_inventory_cache_from_disk(state, app, id, reason)?;
    }
    Ok(())
}

fn is_safe_github_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 128
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn parse_github_plugin_spec(target: &str) -> Option<GithubPluginSpec> {
    let rest = target.strip_prefix("github:")?;
    let (path, reference) = rest
        .split_once('#')
        .map(|(path, reference)| (path, Some(reference.to_string())))
        .unwrap_or((rest, None));
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    if parts.next().is_some()
        || !is_safe_github_part(&owner)
        || !is_safe_github_part(&repo)
        || reference.as_ref().is_some_and(|r| !is_safe_github_part(r))
    {
        return None;
    }
    Some(GithubPluginSpec {
        owner,
        repo,
        reference,
    })
}

fn github_plugin_cache_dir(base: &Path, spec: &GithubPluginSpec) -> PathBuf {
    let suffix = spec
        .reference
        .as_ref()
        .map(|r| format!("__{r}"))
        .unwrap_or_default();
    base.join("github-plugins")
        .join(format!("{}__{}{}", spec.owner, spec.repo, suffix))
}

fn github_root_package_name(entry: &RegistryPlugin) -> Option<String> {
    let rest = entry
        .url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| entry.url.trim().strip_prefix("http://github.com/"))?;
    let mut path = rest;
    for sep in ["/tree/", "/blob/", "#"] {
        if let Some(idx) = path.find(sep) {
            path = &path[..idx];
        }
    }
    let path = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase();
    if path.is_empty() {
        None
    } else {
        Some(path.replace(['/', '-'], "__"))
    }
}

async fn run_git(args: &[String], cwd: Option<&Path>) -> Result<(), AppError> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd.output().await.map_err(|e| {
        AppError::msg(format!(
            "git is required for GitHub plugin cache but could not start: {e}"
        ))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        stdout.trim()
    } else {
        detail
    };
    Err(AppError::msg(format!("git command failed: {detail}")))
}

pub(crate) async fn resolve_plugin_install_target(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    target: &str,
    entry: Option<&RegistryPlugin>,
) -> String {
    let Some(spec) = parse_github_plugin_spec(target) else {
        return target.to_string();
    };

    let cache_dir = github_plugin_cache_dir(&state.paths.cache, &spec);
    let url = format!("https://github.com/{}/{}.git", spec.owner, spec.repo);
    let display = spec
        .reference
        .as_ref()
        .map(|r| format!("{}/{}#{r}", spec.owner, spec.repo))
        .unwrap_or_else(|| format!("{}/{}", spec.owner, spec.repo));

    let result = if cache_dir.join(".git").exists() {
        emit_log(
            app,
            &format!("{id} · updating cached GitHub plugin {display}…"),
        );
        let fetch_ref = spec.reference.clone().unwrap_or_else(|| "HEAD".to_string());
        let fetch = run_git(
            &[
                "-C".to_string(),
                cache_dir.to_string_lossy().to_string(),
                "fetch".to_string(),
                "--depth".to_string(),
                "1".to_string(),
                "origin".to_string(),
                fetch_ref,
            ],
            None,
        )
        .await;
        match fetch {
            Ok(()) => {
                run_git(
                    &[
                        "-C".to_string(),
                        cache_dir.to_string_lossy().to_string(),
                        "checkout".to_string(),
                        "--force".to_string(),
                        "FETCH_HEAD".to_string(),
                    ],
                    None,
                )
                .await
            }
            Err(e) => Err(e),
        }
    } else {
        emit_log(
            app,
            &format!("{id} · shallow cloning GitHub plugin {display}…"),
        );
        let _ = std::fs::create_dir_all(
            cache_dir
                .parent()
                .unwrap_or_else(|| state.paths.cache.as_path()),
        );
        if cache_dir.exists() {
            let _ = std::fs::remove_dir_all(&cache_dir);
        }
        let mut args = vec!["clone".to_string(), "--depth".to_string(), "1".to_string()];
        if let Some(reference) = &spec.reference {
            args.push("--branch".to_string());
            args.push(reference.clone());
        }
        args.push(url);
        args.push(cache_dir.to_string_lossy().to_string());
        run_git(&args, None).await
    };

    match result {
        Ok(()) => {
            let install_dir = entry
                .and_then(|e| e.path.as_deref())
                .filter(|path| !path.trim().is_empty())
                .map(|path| cache_dir.join(path))
                .unwrap_or_else(|| cache_dir.clone());
            if !install_dir.join("package.json").exists() {
                emit_log(
                    app,
                    &format!(
                        "{id} · local GitHub target is not an installable package: {}",
                        install_dir.display()
                    ),
                );
                return target.to_string();
            }
            emit_log(
                app,
                &format!(
                    "{id} · using local shallow clone for {target}: {}",
                    install_dir.display()
                ),
            );
            install_dir.to_string_lossy().to_string()
        }
        Err(e) => {
            emit_log(
                app,
                &format!("{id} · GitHub plugin cache unavailable for {target}: {e}; falling back to pnpm"),
            );
            target.to_string()
        }
    }
}

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
pub async fn plugins_list(
    state: State<'_, AppState>,
    id: String,
    _dsh_port: Option<u16>,
) -> Result<Vec<InstalledPlugin>, AppError> {
    Ok(read_library_inventory_cache(&state, &id).dsh_inventory)
}

/// Fast per-instance inventory counts for the Instances page. Reads only local
/// JSON/manifest files; never contacts a running DSH process.
#[tauri::command]
pub fn library_inventory_summaries(
    state: State<'_, AppState>,
) -> Result<Vec<LibraryInventorySummary>, AppError> {
    let instances = InstanceManifest::list(&state.paths)?;
    Ok(instances
        .iter()
        .map(|instance| library_inventory_summary_for(&state, instance))
        .collect())
}

/// Full mixed Library view for one instance. Reads the Launcher snapshot only:
/// DSH refresh happens through launch/install/manual refresh paths so opening
/// Library stays fast.
#[tauri::command]
pub fn library_inventory_detail(
    state: State<'_, AppState>,
    id: String,
) -> Result<LibraryInventoryDetail, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(library_inventory_detail_for(&state, &instance))
}

/// Explicit cache reconciliation. This is intentionally separate from normal
/// page reads: opening Library should be instant, while the Refresh button may
/// contact a running DSH or deep-scan a stopped instance's profile.
#[tauri::command]
pub async fn library_inventory_refresh(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<LibraryInventoryDetail, AppError> {
    let job_id = id.clone();
    run_instance_job(
        &state,
        &app,
        &job_id,
        HeavyJobKind::InventorySync,
        || async {
            reconcile_library_inventory_after_market_change(&state, &app, &id, "manual refresh")
                .await?;
            let instance = InstanceManifest::get(&state.paths, &id)?;
            Ok(library_inventory_detail_for(&state, &instance))
        },
    )
    .await
}

/// Install a plugin (`dsh plugin add <target>`) into an instance, streaming
/// pnpm output to the Activity log.
#[tauri::command]
pub async fn plugin_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    target: String,
    entry: Option<RegistryPlugin>,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Install, || async {
        ensure_not_running(&state, &id).await?;
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let settings = state
            .settings
            .lock()
            .map_err(|_| AppError::msg("settings lock poisoned"))?
            .clone();

        emit_log(&app, &format!("{id} · installing plugin {target}…"));
        if let Some(entry) = entry.as_ref() {
            if entry.kind == ContentKind::Theme && entry.path.is_some() {
                if let Some(root_package) = github_root_package_name(entry) {
                    emit_log(
                        &app,
                        &format!("{id} · checking previous root skin install {root_package}…"),
                    );
                    let _ = state
                        .adapter
                        .run_plugin_command(
                            &settings,
                            &instance,
                            &["remove".to_string(), root_package],
                            make_sink(app.clone()),
                        )
                        .await;
                }
            }
        }
        let install_target =
            resolve_plugin_install_target(&state, &app, &id, &target, entry.as_ref()).await;
        let sink = make_sink(app.clone());
        let code = state
            .adapter
            .run_plugin_command(
                &settings,
                &instance,
                &["add".to_string(), install_target],
                sink,
            )
            .await?;
        if code != 0 {
            return Err(AppError::msg(format!(
                "dsh plugin add exited with code {code} — see Activity logs"
            )));
        }
        if entry
            .as_ref()
            .is_some_and(|entry| entry.kind == ContentKind::Theme)
        {
            if let Some(entry) = entry.as_ref() {
                let key = if entry.owner.trim().is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", entry.owner, entry.name)
                };
                InstanceManifest::add_skin(&state.paths, &id, &key)?;
            }
        }
        if let Some(entry) = entry.as_ref() {
            record_market_install_metadata(&state, &id, entry)?;
        }
        emit_log(&app, &format!("{id} · installed {target}"));
        reconcile_library_inventory_after_market_change(&state, &app, &id, "plugin install")
            .await?;
        Ok(())
    })
    .await
}

/// Uninstall a plugin (`dsh plugin remove <name>`).
#[tauri::command]
pub async fn plugin_uninstall(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Uninstall, || async {
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
            .run_plugin_command(
                &settings,
                &instance,
                &["remove".to_string(), name.clone()],
                sink,
            )
            .await?;
        if code != 0 {
            return Err(AppError::msg(format!(
                "dsh plugin remove exited with code {code} — see Activity logs"
            )));
        }
        if let Ok(manifest) = InstanceManifest::get(&state.paths, &id) {
            for skin in manifest.skins {
                let tail = skin.rsplit('/').next().unwrap_or(&skin).to_lowercase();
                let normalized = skin.replace('/', "__").replace('-', "__").to_lowercase();
                let package = name.to_lowercase();
                if package.contains(&tail) || package == normalized {
                    let _ = InstanceManifest::remove_skin(&state.paths, &id, &skin);
                }
            }
        }
        DshAdapter::remove_patch_rows(&instance, &patch_ids)?;
        emit_log(&app, &format!("{id} · removed {name}"));
        reconcile_library_inventory_after_market_change(&state, &app, &id, "plugin uninstall")
            .await?;
        Ok(())
    })
    .await
}

/// Enable/disable a plugin (writes `disabled` into `cordis.patch.yml`; DSH
/// hot-applies it, and it survives the `dsh plugin` bundle reconcile). The
/// plugin stays installed either way.
#[tauri::command]
pub async fn plugin_toggle(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
    enabled: bool,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(
        &state,
        &app,
        &job_id,
        HeavyJobKind::ProfileMutation,
        || async {
            ensure_not_running(&state, &id).await?;
            let instance = InstanceManifest::get(&state.paths, &id)?;
            DshAdapter::set_plugin_enabled(&instance, &name, enabled)?;
            reconcile_library_inventory_after_market_change(&state, &app, &id, "plugin toggle")
                .await?;
            Ok(())
        },
    )
    .await
}

/// Per-plugin update status: npm `latest` vs the installed version.
#[tauri::command]
pub async fn plugin_updates(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<PluginUpdate>, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::UpdateCheck, || async {
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
    })
    .await
}

/// Update a plugin to its latest (`dsh plugin update <name>`).
#[tauri::command]
pub async fn plugin_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Install, || async {
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
            .run_plugin_command(
                &settings,
                &instance,
                &["update".to_string(), name.clone()],
                sink,
            )
            .await?;
        if code != 0 {
            return Err(AppError::msg(format!(
                "dsh plugin update exited with code {code} — see Activity logs"
            )));
        }
        emit_log(&app, &format!("{id} · updated {name}"));
        reconcile_library_inventory_after_market_change(&state, &app, &id, "plugin update").await?;
        Ok(())
    })
    .await
}
