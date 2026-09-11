use dsh_adapter::content as content_adapter;
use dsh_adapter::{DshAdapter, InstalledPlugin, InstalledPluginSource, PluginUpdate};
use launcher_core::{
    market, InstanceManifest, Job, JobPlan, McpConfigStore, McpEnvRequirement, RegistryPlugin,
    SkinPackage,
};
use market::ContentKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

use crate::commands::content::land_install_disabled;
use crate::commands::process::{emit_log, make_sink};
use crate::commands::settings::settings_snapshot;
use crate::error::AppError;
use crate::jobs::{enqueue_install, run_instance_job, HeavyJobKind, JobCtx};
use crate::state::AppState;

#[derive(Debug, Clone)]
struct GithubPluginSpec {
    owner: String,
    repo: String,
    reference: Option<String>,
}

const LIBRARY_INVENTORY_EVENT: &str = "library-inventory-updated";

/// Bumped when `LibraryInventoryCache` drops a field. v4 removed the `skills`
/// and `mcp` id mirrors — those types' state lives solely in `InstanceManifest`
/// records (the enriched source of truth from the MCP/skill work). v5 removed
/// `mcp_issues`: MCP row issues are now computed live from the manifest record
/// at read time (mirroring skills rows), never cached from the catalog entry —
/// a Phase-4 git-source server's `null` catalog `command` is by design and would
/// otherwise report a false "missing command" forever. So the snapshot cache
/// only keeps DSH-owned data (`dsh_inventory`, `skins`) plus launcher
/// bookkeeping.
const LIBRARY_INVENTORY_CACHE_SCHEMA: u32 = 5;

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
    /// Installed "version" label, per kind: npm version for plugins/skins,
    /// `#<short content hash>` for skills, `None` for MCP. Read from disk so the
    /// Library snapshot shows it without needing a live DSH.
    #[serde(default)]
    pub version: Option<String>,
    pub enabled: Option<bool>,
    pub toggleable: bool,
    pub source: LibraryItemSource,
    pub state_source: LibraryStateSource,
    pub detail: Option<String>,
    pub market: Option<MarketInstallMetadata>,
    #[serde(default)]
    pub issues: Vec<String>,
    /// Catalog-declared required env vars still unset on the record — the row's
    /// "needs configuring" signal (see `mcp_record_missing_config`). Carries the
    /// key/label/secret; never a value.
    #[serde(default)]
    pub missing_config: Vec<McpEnvRequirement>,
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

/// `#` + first 8 chars of a content hash — the "version" label a skill row
/// shows (skills have no npm version, the hash is the only revision signal).
/// `None` when the record predates hash tracking.
fn short_hash(hash: &str) -> Option<String> {
    let h = hash.trim();
    if h.is_empty() {
        None
    } else {
        Some(format!("#{}", &h[..h.len().min(8)]))
    }
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
    if cache.schema_version < LIBRARY_INVENTORY_CACHE_SCHEMA {
        cache.schema_version = LIBRARY_INVENTORY_CACHE_SCHEMA;
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
        let existing = inventory.iter().position(|inv| {
            inv.name == item.name
                || inv.entry_id.as_deref() == Some(item.name.as_str())
                || item.entry_id.as_deref() == Some(inv.name.as_str())
        });
        match existing {
            // The launcher's disk scan is authoritative for the packages it
            // manages: live DSH rows hardcode `toggleable: false`, so a name
            // collision must not let the live copy shadow the profile copy's
            // enabled/toggleable/kind (see `plugin_inventory`). Replace it.
            Some(pos)
                if matches!(inventory[pos].source, InstalledPluginSource::Inventory)
                    && matches!(item.source, InstalledPluginSource::Profile) =>
            {
                inventory[pos] = item;
            }
            // Same-source collision (e.g. two live rows): keep the first arg —
            // a just-fetched DSH inventory must win over a cached copy.
            Some(_) => {}
            None => inventory.push(item),
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
    // Quarantine a leftover client bundle with no built entry BEFORE the disk
    // scan derives enabled state — an interrupted install's source-only bundle
    // (`dsh plugin add` linked it, then the job died before the install gate /
    // `land_install_disabled`) would otherwise read back as enabled and shadow
    // the real state. Disabling its loader rows keeps the next boot safe; the
    // user removes the package from Library to uninstall it.
    for pkg in content_adapter::quarantine_unloadable_client_bundles(&instance) {
        emit_log(
            app,
            &format!(
                "{id} · quarantined {pkg}: no built client entry — remove it in Library to uninstall"
            ),
        );
    }
    let cached = read_library_inventory_cache(state, id);
    let profile = DshAdapter::installed_plugins(&instance);
    // Drop stale profile-sourced entries first: `merge_plugin_sources` only
    // appends what is missing, so without this a toggle's `enabled` flip would
    // never reach a row that already exists in the cache. The disk scan below
    // is the authoritative state for profile packages (live DSH entries kept).
    let live = cached
        .dsh_inventory
        .into_iter()
        .filter(|p| !matches!(p.source, InstalledPluginSource::Profile))
        .collect();
    let cache = LibraryInventoryCache {
        schema_version: LIBRARY_INVENTORY_CACHE_SCHEMA,
        instance_id: id.to_string(),
        updated_at: launcher_core::now_secs(),
        dsh_inventory: merge_plugin_sources(live, profile),
        launcher_metadata: cached.launcher_metadata,
        install_sources: cached.install_sources,
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
        // skills/mcp are enriched manifest records — the manifest is the single
        // source of truth, so counts come straight from it (never a stale mirror).
        skills: instance.skills.len(),
        mcp: instance.mcp.len(),
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

/// The installed-plugin record backing a cataloged skin key.
///
/// The catalog key (`owner/name`, e.g. `zhijun-dai/Catppuccin-dsh-theme`) is
/// not a reliable textual match against the npm package name it installs
/// (`dsh-catppuccin`) — the repo's `package.json.name` need not contain the
/// catalog's short name. `skin_packages` is the authoritative key→package
/// link; the text heuristic is only a fallback for entries recorded before
/// that map existed.
fn skin_inventory_plugin<'a>(
    skin: &str,
    skin_packages: &[SkinPackage],
    inventory: &'a [InstalledPlugin],
) -> Option<&'a InstalledPlugin> {
    let package = skin_packages
        .iter()
        .find(|sp| sp.key == skin)
        .map(|sp| sp.package.as_str());
    package
        .and_then(|package| inventory.iter().find(|p| p.name == package))
        .or_else(|| inventory.iter().find(|p| skin_key_matches_plugin(skin, p)))
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

    // A plugin that is the installed package of a cataloged skin (per
    // `skin_packages`) is rendered by the skin loop below under its catalog
    // key — skip it here so one skin never appears as two rows (the bare
    // package row and the catalog-keyed row).
    let cataloged_skin_packages: Vec<&str> = instance
        .skin_packages
        .iter()
        .filter(|s| cache.skins.iter().any(|k| k == &s.key))
        .map(|s| s.package.as_str())
        .collect();

    for plugin in &cache.dsh_inventory {
        if cataloged_skin_packages.contains(&plugin.name.as_str()) {
            continue;
        }
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
            version: DshAdapter::installed_version(instance, &plugin.name),
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
            issues: Vec::new(),
            missing_config: Vec::new(),
        });
    }

    let has_live_inventory = cache
        .dsh_inventory
        .iter()
        .any(|plugin| matches!(plugin.source, InstalledPluginSource::Inventory));
    let skill_loader_active =
        has_live_inventory && content_adapter::skill_loader_active(&cache.dsh_inventory);

    // Skill rows come straight from the manifest records — the record carries
    // the provenance (source/hash) and the disk state is checked per row.
    for skill in &instance.skills {
        let id = &skill.id;
        let metadata =
            market_metadata_for_key_values(&cache.launcher_metadata, ContentKind::Skill, id)
                .cloned();
        let install_source = cache.install_sources.get(id);
        let disk = content_adapter::skill_disk_state(instance, id);
        let mut issues = Vec::new();
        let detail = if disk.valid {
            disk.dir.clone()
        } else {
            issues.push(if disk.present {
                "skill.invalidFrontmatter".to_string()
            } else {
                "skill.missingFile".to_string()
            });
            disk.dir.clone()
        };
        if has_live_inventory && !skill_loader_active {
            issues.push("skill.loaderInactive".to_string());
        }
        items.push(LibraryInventoryItem {
            id: id.clone(),
            kind: ContentKind::Skill,
            title: id.clone(),
            version: short_hash(&skill.hash),
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
            detail: Some(detail),
            market: metadata,
            issues,
            missing_config: Vec::new(),
        });
    }

    // MCP rows are the manifest records themselves (the single source of
    // truth): the record's `enabled` drives the Library toggle and `transport`
    // shows in the detail. `toggleable` stays false — the UI toggles via
    // `mcp_set_enabled`, not the patch's plugin `disabled:` mechanism.
    // Legacy-record fallback for `missing_config`: a server installed before
    // `required_env` was persisted carries an empty declaration set, so the
    // record can't surface its own "needs configuring" hint. Its declaration
    // lives in the catalog (`content-mcp-env.json`) — look it up once here so
    // pre-existing installs show the same hint without a reinstall. New
    // installs carry `required_env` on the record itself and are authoritative.
    let declared_env = market::required_env_map();
    let config_store = McpConfigStore::new(state.paths.clone());
    for record in &instance.mcp {
        // Keys the user already configured for THIS server (name on disk + value
        // in the OS credential store) — the ones a declared requirement is
        // satisfied by. Per-record: another server's key never clears this one's
        // "needs configuring" hint.
        let resolved = config_store.resolved_keys(&instance.id, &record.id);
        let id = &record.id;
        let metadata =
            market_metadata_for_key_values(&cache.launcher_metadata, ContentKind::Mcp, id)
                .cloned();
        let install_source = cache.install_sources.get(id);
        items.push(LibraryInventoryItem {
            id: id.clone(),
            kind: ContentKind::Mcp,
            title: id.clone(),
            version: None,
            package_name: None,
            enabled: Some(record.enabled),
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
            detail: Some(format!("mcp-client · {}", record.transport)),
            market: metadata,
            // Live from the manifest record, like skills rows: a git-source
            // server's catalog `command` is null by design pre-build, so the
            // row must reflect what the record actually launches now, not an
            // install-time snapshot of the catalog entry.
            issues: content_adapter::mcp_record_config_issues(record),
            // A declared key is only "missing" when it also has no value in the
            // config store (OS credential vault) — once the user configures it,
            // the row stops asking even though `record.env` stays clean.
            missing_config: content_adapter::mcp_missing_against(
                record,
                if record.required_env.is_empty() {
                    declared_env
                        .get(id)
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                } else {
                    record.required_env.as_slice()
                },
            )
            .into_iter()
            .filter(|req| !resolved.contains(&req.key))
            .collect(),
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
        // A skin is a DSH plugin; when its plugin row is still in the last
        // inventory, surface its real enabled/toggleable state instead of
        // leaving the row as classification-only.
        let plugin = skin_inventory_plugin(skin, &instance.skin_packages, &cache.dsh_inventory);
        let (enabled, toggleable, package_name, state_source) = match plugin {
            Some(plugin) => (
                Some(plugin.enabled),
                plugin.toggleable,
                Some(plugin.name.clone()),
                if matches!(plugin.source, InstalledPluginSource::Inventory) {
                    LibraryStateSource::DshInventory
                } else {
                    LibraryStateSource::LauncherSnapshot
                },
            ),
            None => (None, false, None, LibraryStateSource::LauncherSnapshot),
        };
        items.push(LibraryInventoryItem {
            id: skin.clone(),
            kind: ContentKind::Theme,
            title: skin.clone(),
            version: package_name
                .as_ref()
                .and_then(|name| DshAdapter::installed_version(instance, name)),
            package_name,
            enabled,
            toggleable,
            source: install_source
                .map(|item| item.source)
                .or_else(|| {
                    metadata
                        .as_ref()
                        .map(|_| LibraryItemSource::MarketInstalled)
                })
                .unwrap_or(LibraryItemSource::LocalFile),
            state_source,
            detail: Some("skin classification".to_string()),
            market: metadata,
            issues: Vec::new(),
            missing_config: Vec::new(),
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
        schema_version: LIBRARY_INVENTORY_CACHE_SCHEMA,
        instance_id: id.to_string(),
        updated_at: launcher_core::now_secs(),
        dsh_inventory: merge_plugin_sources(inventory, cached.dsh_inventory),
        launcher_metadata: cached.launcher_metadata,
        install_sources: cached.install_sources,
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
    let installed_at = launcher_core::now_secs();
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
        key.clone(),
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
    cache.schema_version = LIBRARY_INVENTORY_CACHE_SCHEMA;
    cache.updated_at = launcher_core::now_secs();
    write_library_inventory_cache(state, id, &cache)
}

/// Drop an item's market-install provenance (`launcher_metadata` +
/// `install_sources`) from the library-inventory cache. Used when an install is
/// rolled back after a failed post-install probe, and by uninstall, so a removed
/// MCP leaves no residual "market installed" row in `library-inventory.json`.
pub(crate) fn remove_market_install_metadata(
    state: &AppState,
    id: &str,
    key: &str,
) -> Result<(), AppError> {
    let mut cache = read_library_inventory_cache(state, id);
    let had_metadata = cache.launcher_metadata.remove(key).is_some();
    let had_source = cache.install_sources.remove(key).is_some();
    if had_metadata || had_source {
        cache.schema_version = LIBRARY_INVENTORY_CACHE_SCHEMA;
        cache.updated_at = launcher_core::now_secs();
        write_library_inventory_cache(state, id, &cache)?;
    }
    Ok(())
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
    }
    // Always re-scan the profile from disk as well: `installed_plugins` derives
    // profile-sourced rows (including skin enable/disable state), which the live
    // DSH inventory does not carry. Runs after the live refresh so its entries
    // are preserved and the disk scan remains authoritative.
    rebuild_library_inventory_cache_from_disk(state, app, id, reason)?;
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

/// GitHub plugin cache clones get their own budget, not `GIT_TIMEOUT`'s 180 s:
/// a multi-MB skin repo (e.g. 26 MB) at github's ~40 KB/s throttle to a China
/// link outruns 180 s every time and would be killed mid-transfer (the
/// dsh-rhodes-angelina stall). A genuinely stalled transfer still fails cleanly
/// at this bound with Retry available — plugins/skins github fetch only; skills
/// and MCP keep their own shorter timeouts.
const GITHUB_CACHE_TIMEOUT: Duration = Duration::from_secs(900);

async fn run_git(args: &[String], cwd: Option<&Path>) -> Result<(), AppError> {
    // Collect streamed lines so a non-zero exit can surface the real git error
    // detail, as before. The shared timed runner enforces GIT_TIMEOUT and kills
    // the whole process tree on expiry (a bare `output().await` had no timeout
    // and wedged the install job at `running` forever on a stalled transfer).
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let collector = {
        let lines = Arc::clone(&lines);
        Arc::new(move |line: launcher_core::LogLine| {
            if let Ok(mut v) = lines.lock() {
                v.push(line.line);
            }
        })
    };
    let code = dsh_adapter::run_timed(
        "git",
        args,
        cwd.unwrap_or(Path::new(".")),
        &[],
        collector,
        GITHUB_CACHE_TIMEOUT,
    )
    .await
    .map_err(|e| {
        if e.starts_with("spawn ") {
            AppError::msg(format!(
                "git is required for the GitHub plugin cache but could not start ({e}). \
                 Install git and make sure it is on PATH, then Retry."
            ))
        } else {
            AppError::msg(e)
        }
    })?;
    if code == 0 {
        return Ok(());
    }
    let detail = {
        let v = lines
            .lock()
            .map(|v| v.join("\n"))
            .unwrap_or_default();
        let trimmed = v.trim();
        if trimmed.is_empty() {
            format!("exit code {code}")
        } else {
            trimmed.to_string()
        }
    };
    // The relay toggle is the fix for the case that brings most people here — a
    // clone that cannot finish against github.com from a throttled network — so
    // name it rather than leaving "git failed" as the last word.
    Err(AppError::msg(format!(
        "git failed for this GitHub cache ({detail}). If the repo exists and is public, the \
         usual cause is the connection: turn on the GitHub mirror in the Install Center and \
         Retry, then check your network."
    )))
}

/// When a github skin repo lacks a root `package.json` (a monorepo shell), the
/// real skin package lives one level down. Scan immediate subdirectories for a
/// `package.json` declaring `dsh.client`; return the first match (the catalog
/// `path` field normally names it, so this is a fallback for the 399 skins
/// without one).
fn find_skin_subdir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut hits: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path.join("package.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if value.pointer("/dsh/client").is_some() {
            hits.push(path);
        }
    }
    hits.sort();
    hits.into_iter().next()
}

/// True when `cache_dir/.git` exists but the clone never finished: a stale
/// `shallow.lock` or an in-flight `objects/pack/tmp_pack_*` means a shallow
/// fetch/clone was interrupted, and an empty working tree (no files beside
/// `.git`) means the checkout never completed. Fetching into such a dir only
/// re-fails on the same half-written pack — the caller must wipe it and start a
/// fresh shallow clone.
fn clone_cache_broken(cache_dir: &Path) -> bool {
    let git = cache_dir.join(".git");
    if !git.is_dir() {
        return false;
    }
    if git.join("shallow.lock").exists() {
        return true;
    }
    let pack = git.join("objects").join("pack");
    if let Ok(entries) = std::fs::read_dir(&pack) {
        if entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("tmp_pack_"))
        {
            return true;
        }
    }
    // Any entry at the repo root other than `.git` means a tree was checked out.
    let mut checked_out = false;
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for e in entries.flatten() {
            if e.file_name() != ".git" {
                checked_out = true;
                break;
            }
        }
    }
    !checked_out
}

/// The git URL a `github:` plugin spec clones/fetches from. Mirror ON rewrites
/// the upstream URL through the gh-proxy relay (`launcher_core::github`); OFF
/// is the plain upstream. Transport only — every downstream step (shallow
/// clone, checkout, broken-cache wipe) is shared.
/// The clone URL for a GitHub plugin/skin, relayed through gh-proxy when the
/// Install Center toggle is on. Switching is purely a URL rewrite — depth,
/// checkout, and the broken-cache handling below are identical either way, and
/// the relay prefix itself lives in launcher-core so this path and the skill
/// path rewrite through the same host. Default off: repo bytes only pass
/// through the third party when the user flips the toggle, which is the fix for
/// multi-MB skin repos that cannot finish a direct clone on a throttled
/// China→github link even at `GITHUB_CACHE_TIMEOUT`.
fn github_remote_url(owner: &str, repo: &str, mirror: bool) -> String {
    let direct = format!("https://github.com/{owner}/{repo}.git");
    if mirror {
        launcher_core::github::mirror_url(&direct)
    } else {
        direct
    }
}

pub(crate) async fn resolve_plugin_install_target(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    target: &str,
    entry: Option<&RegistryPlugin>,
) -> Result<String, AppError> {
    let Some(spec) = parse_github_plugin_spec(target) else {
        // Not a `github:`-prefixed spec — an npm package name or a local path.
        return Ok(target.to_string());
    };

    let cache_dir = github_plugin_cache_dir(&state.paths.cache, &spec);
    // `github_mirror` (Install Center toggle, default off) routes the fetch
    // through gh-proxy instead of hitting github.com directly — the escape
    // hatch for multi-MB skin repos that time out even at `GITHUB_CACHE_TIMEOUT`.
    // Read live on each resolve so a mid-run toggle takes effect on the next
    // op; a poisoned settings lock degrades to direct (the safe default).
    let mirror = state
        .settings
        .lock()
        .map(|s| s.github_mirror)
        .unwrap_or(false);
    let transport = if mirror { " (via gh-proxy)" } else { "" };
    let url = github_remote_url(&spec.owner, &spec.repo, mirror);
    let display = spec
        .reference
        .as_ref()
        .map(|r| format!("{}/{}#{r}", spec.owner, spec.repo))
        .unwrap_or_else(|| format!("{}/{}", spec.owner, spec.repo));

    let broken = clone_cache_broken(&cache_dir);
    let result = if cache_dir.join(".git").exists() && !broken {
        emit_log(
            app,
            &format!(
                "{id} · updating cached GitHub plugin {display}…{transport}"
            ),
        );
        // Repoint `origin` at the transport the current mirror toggle picks so
        // flipping it applies to updates of an existing clone, not just fresh
        // clones. A no-op when the setting is unchanged.
        let set_url = run_git(
            &[
                "-C".to_string(),
                cache_dir.to_string_lossy().to_string(),
                "remote".to_string(),
                "set-url".to_string(),
                "origin".to_string(),
                url.clone(),
            ],
            None,
        )
        .await;
        match set_url {
            Ok(()) => {
                let fetch_ref =
                    spec.reference.clone().unwrap_or_else(|| "HEAD".to_string());
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
            }
            Err(e) => Err(e),
        }
    } else {
        if broken {
            emit_log(
                app,
                &format!(
                    "{id} · cached clone for GitHub plugin {display} is incomplete (interrupted fetch) — re-cloning from scratch"
                ),
            );
        }
        emit_log(
            app,
            &format!(
                "{id} · shallow cloning GitHub plugin {display}…{transport}"
            ),
        );
        let _ = std::fs::create_dir_all(
            cache_dir
                .parent()
                .unwrap_or(state.paths.cache.as_path()),
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
            let install_dir = if install_dir.join("package.json").exists() {
                install_dir
            } else if let Some(subdir) = find_skin_subdir(&install_dir) {
                emit_log(
                    app,
                    &format!(
                        "{id} · repo root has no package.json — using skin subdir {}",
                        subdir.display()
                    ),
                );
                subdir
            } else {
                return Err(AppError::msg(format!(
                    "local GitHub clone {display} is not an installable package (no root package.json or dsh client subdir) — check the catalog source and Retry"
                )));
            };
            emit_log(
                app,
                &format!(
                    "{id} · using local shallow clone for {target}: {}",
                    install_dir.display()
                ),
            );
            Ok(install_dir.to_string_lossy().to_string())
        }
        Err(e) => {
            // A `github:` spec only resolves over git; routing the same spec back
            // to `dsh plugin add` would make pnpm re-fetch the very source that
            // just failed (the old "fall back to pnpm" dead end). Surface the
            // cause instead and let the row's Retry run a clean clone.
            emit_log(
                app,
                &format!("{id} · GitHub plugin cache failed for {target}: {e}"),
            );
            Err(AppError::msg(format!(
                "could not fetch GitHub source {display}: {e} (check the repo and your network, then Retry)"
            )))
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

/// Install a plugin (`dsh plugin add <target>`) by enqueueing a backend install
/// job. The plan keeps both the raw target and the optional Market entry, so a
/// Retry restores the exact original install.
#[tauri::command]
pub async fn plugin_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    target: String,
    entry: Option<RegistryPlugin>,
) -> Result<Job, AppError> {
    let key = if entry
        .as_ref()
        .map(|e| e.owner.trim().is_empty())
        .unwrap_or(true)
    {
        target.clone()
    } else {
        entry
            .as_ref()
            .map(|e| format!("{}/{}", e.owner, e.name))
            .unwrap_or_else(|| target.clone())
    };
    enqueue_install(
        &state,
        &app,
        &id,
        &key,
        &format!("plugin {key}"),
        JobPlan::Plugin {
            target,
            entry,
        },
    )
    .await
}

/// The durable body `plugin_install` enqueues: optional previous-root-skin
/// removal, then `dsh plugin add <target>`, recording metadata + Library refresh.
pub(crate) async fn plugin_install_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    target: &str,
    entry: Option<&RegistryPlugin>,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let settings = settings_snapshot(state)?;

    emit_log(app, &format!("{id} · installing plugin {target}…"));
    if let Some(entry) = entry {
        if entry.kind == ContentKind::Theme && entry.path.is_some() {
            if let Some(root_package) = github_root_package_name(entry) {
                emit_log(
                    app,
                    &format!("{id} · checking previous root skin install {root_package}…"),
                );
                let _ = state
                    .adapter
                    .run_plugin_command(
                        &settings,
                        &instance,
                        &["remove".to_string(), root_package],
                        ctx.sink(),
                    )
                    .await;
            }
        }
    }
    let install_target = resolve_plugin_install_target(state, app, id, target, entry).await?;
    ctx.progress("dsh-install", 40);
    let code = state
        .adapter
        .run_plugin_command(
            &settings,
            &instance,
            &["add".to_string(), install_target.clone()],
            ctx.sink(),
        )
        .await?;
    if code != 0 {
        ctx.set_exit_code(i64::from(code));
        return Err(AppError::msg(format!(
            "dsh plugin add exited with code {code} — the package name/version may be wrong or the npm registry unreachable. Check the spec and your network, then Retry (pnpm detail in Activity logs)"
        )));
    }
    // Post-install loadability gate — mirror of the one in
    // `install_bundle_item` (ported from dsh-market's validateAddedPlugins):
    // `dsh plugin add` exits 0 even for a source-only GitHub checkout, so a
    // package with no built entry would be recorded and kill the NEXT boot with
    // ERR_MODULE_NOT_FOUND (the tp7 skin family). This raw `plugin_install`
    // path must not be the hole the Market flow's gate already plugs: if the
    // just-added package is not a bundle and ships no loadable entry artifact,
    // remove it now and fail the install — never leave it for boot.
    if let Some(pkg) = content_adapter::skin_package_name(Path::new(&install_target)) {
        if !content_adapter::installed_skin_loadable(&instance, &pkg) {
            emit_log(
                app,
                &format!(
                    "{id} · plugin {pkg} installed but has no loadable entry — removing to protect the next boot"
                ),
            );
            let _ = state
                .adapter
                .run_plugin_command(
                    &settings,
                    &instance,
                    &["remove".to_string(), pkg.clone()],
                    ctx.sink(),
                )
                .await;
            // `dsh plugin remove` drops the manifest entry but on Windows pnpm
            // leaves the top-level node_modules dir behind (observed with tp7).
            // Prune it ourselves — `std::fs::remove_dir_all` never follows
            // reparse points, so a junction to the source cache is removed as a
            // link, target untouched.
            let installed_dir = DshAdapter::profile_dir(&instance)
                .join("node_modules")
                .join(&pkg);
            let _ = std::fs::remove_dir_all(&installed_dir);
            ctx.set_exit_code(1);
            return Err(AppError::msg(format!(
                "installed but not loadable: {pkg} ships no built entry (a source-only checkout) — it was removed; install a published build instead"
            )));
        }
    }
    ctx.progress("recording", 65);
    if let Some(entry) = entry {
        let key = if entry.owner.trim().is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", entry.owner, entry.name)
        };
        // The real npm package name comes from the installed target's own
        // `package.json.name` (a github subdir), or the bare npm spec when the
        // target was a registry package.
        let package = content_adapter::skin_package_name(Path::new(&install_target))
            .unwrap_or_else(|| install_target.clone());
        // New plugin/skin installs land DISABLED — record (skins) + patch state,
        // but never auto-mount; enabling is an explicit later toggle.
        if entry.kind == ContentKind::Theme || entry.kind == ContentKind::Plugin {
            land_install_disabled(state, id, &key, &package, entry.kind)?;
        }
        record_market_install_metadata(state, id, entry)?;
    } else {
        // A raw target install (no registry entry) has no instance record; a
        // bundle still auto-registers into the profile bundles and would load,
        // so land it disabled too.
        if let Some(package) = content_adapter::skin_package_name(Path::new(&install_target)) {
            land_install_disabled(state, id, &package, &package, ContentKind::Plugin)?;
        }
    }
    emit_log(app, &format!("{id} · installed {target}"));
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "plugin install").await?;
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
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Uninstall, || async {
        ensure_not_running(&state, &id).await?;
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let settings = settings_snapshot(&state)?;

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
                "dsh plugin remove exited with code {code} — the plugin may not be installed, or DSH is busy. Check Activity logs for the detail"
            )));
        }
        if let Ok(manifest) = InstanceManifest::get(&state.paths, &id) {
            for skin in manifest.skins {
                let tail = skin.rsplit('/').next().unwrap_or(&skin).to_lowercase();
                let normalized = skin.replace(['/', '-'], "__").to_lowercase();
                let package = name.to_lowercase();
                if package.contains(&tail) || package == normalized {
                    let _ = InstanceManifest::remove_skin_package(&state.paths, &id, &skin);
                }
            }
        }
        DshAdapter::remove_patch_rows(&instance, &patch_ids)?;
        // Recompile the skin insert block: a removed skin's row must not survive
        // as an orphan (its package is already gone from the profile).
        if let Ok(updated) = InstanceManifest::get(&state.paths, &id) {
            content_adapter::sync_skin_patch(&updated, &updated.skin_packages)?;
        }
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
            // Enabling must not re-arm a quarantined brick: a client bundle
            // (`dsh.bundle` + `dsh.client`) whose built entry is missing will
            // crash the next boot the moment its loader row mounts. Refuse with
            // the same guidance the install gate gives — remove, don't re-enable.
            if enabled {
                let nm = DshAdapter::profile_dir(&instance)
                    .join("node_modules")
                    .join(&name);
                if content_adapter::skin_has_bundle(&instance, &name)
                    && content_adapter::package_mounts_client(&nm)
                    && !content_adapter::entry_artifact_exists(&nm)
                {
                    return Err(AppError::msg(format!(
                        "can't enable {name}: it ships no built client entry (a source-only checkout). Remove it from Library instead"
                    )));
                }
            }
            // Skins toggle through the patch layer, but which rows depends on how
            // the skin mounts. A `dsh.bundle` skin is a profile bundle and turns
            // off via `disabled:` rows on its own entries, exactly like any other
            // bundle plugin. A client-plugin skin (no bundle) mounts solely through
            // its insert row, so toggling writes/removes that row.
            if let Some(skin) = instance
                .skin_packages
                .iter()
                .find(|p| p.package == name)
                .cloned()
            {
                if content_adapter::skin_has_bundle(&instance, &skin.package) {
                    DshAdapter::set_plugin_enabled(&instance, &name, enabled)?;
                    InstanceManifest::set_skin_enabled(&state.paths, &id, &skin.key, enabled)?;
                } else {
                    InstanceManifest::set_skin_enabled(&state.paths, &id, &skin.key, enabled)?;
                    let updated = InstanceManifest::get(&state.paths, &id)?;
                    content_adapter::sync_skin_patch(&updated, &updated.skin_packages)?;
                }
            } else {
                DshAdapter::set_plugin_enabled(&instance, &name, enabled)?;
            }
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
        // Resolve the installed baseline + probe candidates synchronously; each
        // npm `latest` fetch is a separate HTTP round-trip, so the probes run
        // concurrently below instead of serialising the whole update state on
        // the slowest registry call.
        struct Probe {
            name: String,
            installed: String,
        }
        let mut probes = Vec::new();
        for p in installed {
            // In-box DSH packages are not market-managed.
            if p.name.starts_with("@deepseek-ai/") {
                continue;
            }
            let Some(installed_ver) = DshAdapter::installed_version(&instance, &p.name) else {
                continue;
            };
            probes.push(Probe {
                name: p.name,
                installed: installed_ver,
            });
        }
        let mut workers = tokio::task::JoinSet::new();
        for probe in probes {
            let registry = registry.clone();
            workers.spawn(async move {
                let latest = market::npm_latest(&registry, &probe.name).await;
                (probe, latest)
            });
        }
        let mut out = Vec::new();
        while let Some(res) = workers.join_next().await {
            let Ok((probe, latest)) = res else {
                continue;
            };
            let Ok(latest) = latest else {
                continue;
            };
            let updatable = market::version_newer(&latest, &probe.installed);
            out.push(PluginUpdate {
                name: probe.name,
                installed: probe.installed,
                latest,
                updatable,
            });
        }
        Ok(out)
    })
    .await
}

/// Update a plugin to its latest (`dsh plugin update <name>`). Enqueues a
/// durable job like an install, so Install Center shows the row with real stage
/// progress instead of the call silently blocking on the whole pnpm pass.
#[tauri::command]
pub async fn plugin_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    name: String,
) -> Result<Job, AppError> {
    let label = format!("update plugin {name}");
    enqueue_install(
        &state,
        &app,
        &id,
        &name,
        &label,
        JobPlan::PluginUpdate {
            name: name.clone(),
        },
    )
    .await
}

/// The durable body `plugin_update` enqueues: `dsh plugin update <name>`, then
/// re-calibrates the Library snapshot.
pub(crate) async fn plugin_update_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    name: &str,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let settings = settings_snapshot(state)?;

    emit_log(app, &format!("{id} · updating plugin {name}…"));
    ctx.progress("dsh-install", 40);
    let code = state
        .adapter
        .run_plugin_command(
            &settings,
            &instance,
            &["update".to_string(), name.to_string()],
            ctx.sink(),
        )
        .await?;
    if code != 0 {
        ctx.set_exit_code(i64::from(code));
        return Err(AppError::msg(format!(
            "dsh plugin update exited with code {code} — check the plugin is installed and your network can reach the npm registry (detail in Activity logs)"
        )));
    }
    ctx.progress("recording", 70);
    emit_log(app, &format!("{id} · updated {name}"));
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "plugin update").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_adapter::InstalledPluginKind;
    use launcher_core::{AppPaths, AppSettings, ProviderVault};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_paths(label: &str) -> AppPaths {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ahl-snapshot-{label}-{nanos}"));
        let _ = fs::create_dir_all(&root);
        AppPaths::rooted_at(root, false)
    }

    fn sample_state(label: &str) -> AppState {
        let paths = temp_paths(label);
        let vault = ProviderVault::new(paths.clone());
        AppState::new(paths, AppSettings::default(), vault, None, Arc::new(AtomicBool::new(false)))
    }

    fn plugin(name: &str, source: InstalledPluginSource) -> InstalledPlugin {
        InstalledPlugin {
            name: name.into(),
            enabled: true,
            toggleable: false,
            kind: InstalledPluginKind::Plugin,
            source,
            entry_id: None,
            fiber_phase: None,
        }
    }

    #[test]
    fn merge_plugin_sources_dedupes_and_prefers_profile() {
        // A live DSH row hardcodes `toggleable: false`; the profile disk scan
        // derives the real value (a bundle skin with a patch row is toggleable).
        let mut live_alpha = plugin("alpha", InstalledPluginSource::Inventory);
        live_alpha.toggleable = false;
        let mut profile_alpha = plugin("alpha", InstalledPluginSource::Profile);
        profile_alpha.toggleable = true;
        let inventory = vec![
            live_alpha,
            plugin("beta", InstalledPluginSource::Inventory),
        ];
        let profile = vec![profile_alpha, plugin("gamma", InstalledPluginSource::Profile)];
        let merged = merge_plugin_sources(inventory, profile);
        let names: Vec<&str> = merged.iter().map(|p| p.name.as_str()).collect();

        // "alpha" appears once (deduped across inventory/profile), and its
        // profile copy wins the name collision: the disk scan is authoritative,
        // so the launcher-managed row keeps its toggleable switch rather than
        // being shadowed by the live row's hardcoded `false`.
        assert_eq!(merged.len(), 3, "alpha must dedupe: {names:?}");
        assert_eq!(names, vec!["alpha", "gamma", "beta"]);
        let alpha = merged.iter().find(|p| p.name == "alpha").expect("alpha row");
        assert_eq!(alpha.source, InstalledPluginSource::Profile);
        assert!(alpha.toggleable, "profile copy's toggleable must survive");
        assert_eq!(merged[0].source, InstalledPluginSource::Profile);
    }

    #[test]
    fn record_install_metadata_roundtrips_through_snapshot_cache() {
        let state = sample_state("record");
        let entry = RegistryPlugin {
            kind: ContentKind::Plugin,
            name: "toolbox".into(),
            owner: "acme".into(),
            npm: Some("@acme/toolbox".into()),
            ..Default::default()
        };

        record_install_metadata_with_source(
            &state,
            "test-instance",
            &entry,
            LibraryItemSource::MarketInstalled,
        )
        .expect("record metadata");

        let cache = read_library_inventory_cache(&state, "test-instance");
        let meta = cache
            .launcher_metadata
            .get("acme/toolbox")
            .expect("market metadata persisted");
        assert_eq!(meta.kind, ContentKind::Plugin);
        assert_eq!(meta.install_spec, "@acme/toolbox");
        assert!(matches!(
            cache
                .install_sources
                .get("acme/toolbox")
                .expect("install source persisted")
                .source,
            LibraryItemSource::MarketInstalled
        ));
    }

    #[test]
    fn skin_inventory_plugin_links_catalog_key_to_package_via_skin_packages() {
        // Real-machine case: the catalog key (`zhijun-dai/Catppuccin-dsh-theme`)
        // shares no text with the npm package name it installed (`dsh-catppuccin`),
        // so the name heuristic alone misses the row and the skin shows up with
        // no toggle. The `skin_packages` key→package map is authoritative.
        let packages = vec![SkinPackage {
            key: "zhijun-dai/Catppuccin-dsh-theme".into(),
            package: "dsh-catppuccin".into(),
            enabled: false,
        }];
        let inventory = vec![InstalledPlugin {
            name: "dsh-catppuccin".into(),
            enabled: false,
            toggleable: true,
            kind: InstalledPluginKind::Theme,
            source: InstalledPluginSource::Profile,
            entry_id: None,
            fiber_phase: None,
        }];

        let linked =
            skin_inventory_plugin("zhijun-dai/Catppuccin-dsh-theme", &packages, &inventory)
                .expect("skin_packages maps catalog key to its package");
        assert_eq!(linked.name, "dsh-catppuccin");
        assert!(linked.toggleable, "installed skin must be toggleable");

        // Without the map the heuristic would come up empty — the old bug.
        assert!(
            skin_inventory_plugin("zhijun-dai/Catppuccin-dsh-theme", &[], &inventory).is_none(),
            "name heuristic must NOT link zhijun-dai/Catppuccin-dsh-theme to dsh-catppuccin"
        );
    }

    #[test]
    fn clone_cache_broken_detects_partial_clones() {
        // Build a cache dir shaped like a github-plugins cache entry.
        fn make(git: bool) -> std::path::PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("ahl-clone-broken-{nanos}"));
            let _ = fs::create_dir_all(&dir);
            if git {
                let _ = fs::create_dir_all(dir.join(".git").join("objects").join("pack"));
            }
            dir
        }

        // No `.git` → not "broken"; the caller simply runs a fresh clone.
        let empty = make(false);
        assert!(!clone_cache_broken(&empty));

        // Completed clone: `.git` plus a checked-out root file.
        let healthy = make(true);
        fs::write(healthy.join("package.json"), "{}").unwrap();
        assert!(!clone_cache_broken(&healthy));

        // `.git` but an empty working tree → checkout never finished.
        let no_tree = make(true);
        assert!(clone_cache_broken(&no_tree));

        // Stale shallow-fetch lock (the Angelina-dsh-plugin stall signature).
        let lock = make(true);
        fs::write(lock.join(".git").join("shallow.lock"), "lock").unwrap();
        fs::write(lock.join("package.json"), "{}").unwrap();
        assert!(clone_cache_broken(&lock));

        // In-flight pack left over from an interrupted transfer.
        let pack = make(true);
        fs::write(
            pack.join(".git").join("objects").join("pack").join("tmp_pack_xjAdEM"),
            "partial",
        )
        .unwrap();
        fs::write(pack.join("package.json"), "{}").unwrap();
        assert!(clone_cache_broken(&pack));

        // A normal pack file (no `tmp_pack_` prefix) with a checkout is healthy.
        let real = make(true);
        fs::write(
            real.join(".git").join("objects").join("pack").join("real.pack"),
            "x",
        )
        .unwrap();
        fs::write(real.join("package.json"), "{}").unwrap();
        assert!(!clone_cache_broken(&real));

        // Best-effort cleanup of the scratch dirs.
        for p in [empty, healthy, no_tree, lock, pack, real] {
            let _ = fs::remove_dir_all(&p);
        }
    }

    #[test]
    fn github_remote_url_prefixes_mirror_only_when_enabled() {
        // Default (off) is the plain upstream URL — the launcher never routes
        // through a third party unless the Install Center toggle is on.
        assert_eq!(
            github_remote_url("FlowerWater1019", "Angelina-dsh-plugin", false),
            "https://github.com/FlowerWater1019/Angelina-dsh-plugin.git"
        );
        // Mirror ON rewrites through the gh-proxy relay; transport is the only
        // difference, so `.git` and the repo coordinates survive verbatim.
        assert_eq!(
            github_remote_url("FlowerWater1019", "Angelina-dsh-plugin", true),
            "https://gh-proxy.com/https://github.com/FlowerWater1019/Angelina-dsh-plugin.git"
        );
    }
}
