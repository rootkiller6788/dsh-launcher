use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use dsh_adapter::DshAdapter;
use launcher_core::market::{self, ContentKind};
use launcher_core::{
    AppSettings, BundleItemResult, BundleSummary, InstanceManifest, Registry, RegistryPlugin,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, State};
use zip::write::SimpleFileOptions;

use crate::commands::content::install_bundle_item;
use crate::commands::plugins::{
    ensure_not_running, reconcile_library_inventory_after_market_change, LibraryItemSource,
};
use crate::commands::process::emit_log;
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

const FORMAT: &str = "dsh.environment";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentSource {
    instance_id: String,
    instance_name: String,
    runtime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentManifest {
    format: String,
    format_version: u32,
    exported_at: u64,
    name: String,
    description: String,
    source: EnvironmentSource,
    items: Vec<RegistryPlugin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentPackage {
    manifest: EnvironmentManifest,
    checksum: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentExportResult {
    pub path: String,
    pub checksum: String,
    pub item_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPreviewResult {
    pub name: String,
    pub description: String,
    pub checksum: String,
    pub item_count: usize,
    pub plugins: usize,
    pub skins: usize,
    pub skills: usize,
    pub mcps: usize,
    pub exported_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentImportResult {
    pub instance: InstanceManifest,
    pub checksum: String,
    pub summary: BundleSummary,
}

fn preview_for(pkg: &EnvironmentPackage) -> EnvironmentPreviewResult {
    let mut plugins = 0;
    let mut skins = 0;
    let mut skills = 0;
    let mut mcps = 0;
    for item in &pkg.manifest.items {
        match item.kind {
            ContentKind::Plugin => plugins += 1,
            ContentKind::Theme => skins += 1,
            ContentKind::Skill => skills += 1,
            ContentKind::Mcp => mcps += 1,
            ContentKind::Bundle => {}
        }
    }
    EnvironmentPreviewResult {
        name: pkg.manifest.name.clone(),
        description: pkg.manifest.description.clone(),
        checksum: pkg.checksum.clone(),
        item_count: pkg.manifest.items.len(),
        plugins,
        skins,
        skills,
        mcps,
        exported_at: pkg.manifest.exported_at,
    }
}

fn manifest_checksum(manifest: &EnvironmentManifest) -> Result<String, AppError> {
    let bytes =
        serde_json::to_vec(manifest).context("serialize environment manifest for checksum")?;
    let hash = Sha256::digest(bytes);
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

fn validate_package(pkg: &EnvironmentPackage) -> Result<(), AppError> {
    if pkg.manifest.format != FORMAT {
        return Err(AppError::msg("not a DSH environment package"));
    }
    if pkg.manifest.format_version != FORMAT_VERSION {
        return Err(AppError::msg(format!(
            "unsupported environment package version {}",
            pkg.manifest.format_version
        )));
    }
    if pkg.manifest.items.is_empty() {
        return Err(AppError::msg(
            "environment package has no installable items",
        ));
    }
    let expected = manifest_checksum(&pkg.manifest)?;
    if expected != pkg.checksum {
        return Err(AppError::msg("environment package checksum mismatch"));
    }
    Ok(())
}

fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|p| p.join("Downloads"))
        .filter(|p| p.exists())
        .unwrap_or_else(std::env::temp_dir)
}

fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "environment".into()
    } else {
        s
    }
}

fn registry_index(registry: &Registry) -> Vec<&RegistryPlugin> {
    registry.plugins.iter().collect()
}

fn find_by_plugin_name<'a>(items: &'a [&'a RegistryPlugin], name: &str) -> Option<RegistryPlugin> {
    items
        .iter()
        .copied()
        .find(|entry| {
            matches!(entry.kind, ContentKind::Plugin | ContentKind::Theme)
                && (entry.name == name
                    || entry.key() == name
                    || entry.npm.as_deref() == Some(name)
                    || entry.spec == name
                    || entry.install_spec() == name)
        })
        .cloned()
}

fn find_by_key<'a>(
    items: &'a [&'a RegistryPlugin],
    kind: ContentKind,
    key: &str,
) -> Option<RegistryPlugin> {
    items
        .iter()
        .copied()
        .find(|entry| entry.kind == kind && entry.key() == key)
        .cloned()
}

fn fallback_plugin(name: &str) -> RegistryPlugin {
    RegistryPlugin {
        kind: ContentKind::Plugin,
        name: name.to_string(),
        npm: Some(name.to_string()),
        spec: name.to_string(),
        ..Default::default()
    }
}

async fn merged_registry(state: &AppState) -> Registry {
    let plugins = if let Some(reg) = state.registry.lock().ok().and_then(|g| g.as_ref().cloned()) {
        reg
    } else {
        market::fetch_registry(&state.paths)
            .await
            .unwrap_or_else(|_| Registry::default())
    };
    let content = if let Some(reg) = state.content.lock().ok().and_then(|g| g.as_ref().cloned()) {
        reg
    } else {
        market::fetch_content()
            .await
            .unwrap_or_else(|_| market::bundled_content())
    };
    market::extend_with_content(plugins, content)
}

fn package_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.replace('-', " "))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Imported Environment".into())
}

fn package_json(pkg: &EnvironmentPackage) -> Result<String, AppError> {
    serde_json::to_string_pretty(pkg)
        .context("serialize environment package")
        .map_err(AppError::from)
}

fn package_from_json(text: &str) -> Result<EnvironmentPackage, AppError> {
    serde_json::from_str(text)
        .context("parse environment package")
        .map_err(AppError::from)
}

fn zip_package(pkg: &EnvironmentPackage) -> Result<Vec<u8>, AppError> {
    let mut out = Cursor::new(Vec::new());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut zip = zip::ZipWriter::new(&mut out);
    zip.start_file("environment.json", options)
        .context("start environment.json")?;
    zip.write_all(package_json(pkg)?.as_bytes())
        .context("write environment.json")?;
    zip.start_file("README.md", options)
        .context("start README.md")?;
    zip.write_all(
        b"# DSH Environment Package\n\nThis package contains an install manifest only. It does not include API keys, logs, node_modules, or private workspace state.\n",
    )
    .context("write README.md")?;
    zip.finish().context("finish environment package zip")?;
    Ok(out.into_inner())
}

fn read_package_bytes(bytes: &[u8]) -> Result<EnvironmentPackage, AppError> {
    if bytes.starts_with(b"PK") {
        let reader = Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(reader).context("open .dshenv zip")?;
        let mut file = zip
            .by_name("environment.json")
            .context("read environment.json from .dshenv")?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .context("decode environment.json")?;
        return package_from_json(&text);
    }
    let text = std::str::from_utf8(bytes).context("decode environment package")?;
    package_from_json(text)
}

fn read_package_path(path: &Path) -> Result<EnvironmentPackage, AppError> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    read_package_bytes(&bytes)
}

#[tauri::command]
pub async fn environment_export(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<EnvironmentExportResult, AppError> {
    let job_id = id.clone();
    run_instance_job(
        &state,
        &app,
        &job_id,
        HeavyJobKind::EnvironmentExport,
        || async {
            let instance = InstanceManifest::get(&state.paths, &id)?;
            let registry = merged_registry(&state).await;
            let entries = registry_index(&registry);
            let installed = DshAdapter::installed_plugins(&instance);
            let mut items = Vec::new();

            for plugin in installed {
                let item = find_by_plugin_name(&entries, &plugin.name)
                    .unwrap_or_else(|| fallback_plugin(&plugin.name));
                items.push(item);
            }
            for skill in &instance.skills {
                if let Some(item) = find_by_key(&entries, ContentKind::Skill, skill) {
                    items.push(item);
                }
            }
            for mcp in &instance.mcp {
                if let Some(item) = find_by_key(&entries, ContentKind::Mcp, mcp) {
                    items.push(item);
                }
            }

            if items.is_empty() {
                return Err(AppError::msg(
                    "current instance has no plugins, skins, skills or MCP servers to export",
                ));
            }

            let manifest = EnvironmentManifest {
                format: FORMAT.into(),
                format_version: FORMAT_VERSION,
                exported_at: launcher_core::now_secs(),
                name: instance.name.clone(),
                description: format!("Environment exported from {}", instance.name),
                source: EnvironmentSource {
                    instance_id: instance.id.clone(),
                    instance_name: instance.name.clone(),
                    runtime: instance.runtime.version.clone(),
                },
                items,
            };
            let checksum = manifest_checksum(&manifest)?;
            let pkg = EnvironmentPackage {
                manifest,
                checksum: checksum.clone(),
            };
            let path = downloads_dir().join(format!(
                "dsh-{}-{}.dshenv",
                slug(&pkg.manifest.name),
                pkg.manifest.exported_at
            ));
            let bytes = zip_package(&pkg)?;
            std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
            Ok(EnvironmentExportResult {
                path: path.display().to_string(),
                checksum,
                item_count: pkg.manifest.items.len(),
            })
        },
    )
    .await
}

#[tauri::command]
pub fn environment_preview(bytes: Vec<u8>) -> Result<EnvironmentPreviewResult, AppError> {
    let pkg = read_package_bytes(&bytes)?;
    validate_package(&pkg)?;
    Ok(preview_for(&pkg))
}

#[tauri::command]
pub async fn environment_import(
    state: State<'_, AppState>,
    app: AppHandle,
    path: String,
    name: Option<String>,
) -> Result<EnvironmentImportResult, AppError> {
    run_instance_job(
        &state,
        &app,
        "__environment_import__",
        HeavyJobKind::EnvironmentImport,
        || async {
            let path = PathBuf::from(path.trim());
            let pkg = read_package_path(&path)?;
            validate_package(&pkg)?;
            let name = name.or_else(|| Some(package_name_from_path(&path)));
            import_validated_package(&state, &app, pkg, name).await
        },
    )
    .await
}

async fn import_validated_package(
    state: &AppState,
    app: &AppHandle,
    pkg: EnvironmentPackage,
    name: Option<String>,
) -> Result<EnvironmentImportResult, AppError> {
    let instance_name = name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| pkg.manifest.name.clone());
    let instance = InstanceManifest::create(&state.paths, &instance_name)?;
    ensure_not_running(state, &instance.id).await?;
    let settings: AppSettings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    emit_log(
        &app,
        &format!(
            "{} · importing environment \"{}\" ({} items)…",
            instance.id,
            pkg.manifest.name,
            pkg.manifest.items.len()
        ),
    );

    let mut summary = BundleSummary::default();
    for item in &pkg.manifest.items {
        let key = item.key();
        let kind = item.kind.as_str().to_string();
        match install_bundle_item(
            state,
            app,
            &instance.id,
            &instance,
            &settings,
            item,
            LibraryItemSource::ImportedEnvironment,
        )
        .await
        {
            Ok(()) => {
                summary.installed += 1;
                summary.results.push(BundleItemResult {
                    name: key,
                    kind,
                    ok: true,
                    error: None,
                });
            }
            Err(e) => {
                summary.failed += 1;
                summary.results.push(BundleItemResult {
                    name: key,
                    kind,
                    ok: false,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let instance = InstanceManifest::get(&state.paths, &instance.id)?;
    emit_log(
        &app,
        &format!(
            "{} · environment import done: {} installed, {} failed",
            instance.id, summary.installed, summary.failed
        ),
    );
    reconcile_library_inventory_after_market_change(state, app, &instance.id, "environment import")
        .await?;
    Ok(EnvironmentImportResult {
        instance,
        checksum: pkg.checksum,
        summary,
    })
}

#[tauri::command]
pub async fn environment_import_package(
    state: State<'_, AppState>,
    app: AppHandle,
    bytes: Vec<u8>,
    name: Option<String>,
) -> Result<EnvironmentImportResult, AppError> {
    run_instance_job(
        &state,
        &app,
        "__environment_import__",
        HeavyJobKind::EnvironmentImport,
        || async {
            let pkg = read_package_bytes(&bytes)?;
            validate_package(&pkg)?;
            import_validated_package(&state, &app, pkg, name).await
        },
    )
    .await
}
