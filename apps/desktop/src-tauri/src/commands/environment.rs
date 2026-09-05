use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use dsh_adapter::DshAdapter;
use launcher_core::environment::{ENVIRONMENT_FORMAT, ENVIRONMENT_FORMAT_VERSION};
use launcher_core::market::{self, ContentKind};
use launcher_core::{
    EnvironmentManifest, EnvironmentSource, ExportedItem, InstanceManifest, Job, JobPlan, Registry,
    RegistryPlugin,
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
use crate::jobs::{enqueue_install, run_instance_job, HeavyJobKind, JobCtx};
use crate::state::AppState;

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
pub struct EnvironmentPreviewItem {
    pub kind: ContentKind,
    pub name: String,
    pub source: String,
    pub version: Option<String>,
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
    pub compatible_with: String,
    pub items: Vec<EnvironmentPreviewItem>,
    pub conflicts: Vec<String>,
    pub missing_tokens: Vec<String>,
}

/// Heuristic for "this MCP references a secret the importer must supply": any
/// `env`/`headers` value that is a `${VAR}` reference or names a token/key.
/// The curated catalog ships these empty today, but the schema reserves them so
/// a future MCP can declare auth the same way the runtime consumes it.
fn mcp_needs_token(item: &RegistryPlugin) -> bool {
    item.env
        .iter()
        .flat_map(|m| m.values())
        .chain(item.headers.iter().flat_map(|m| m.values()))
        .any(|v| {
            v.contains("${")
                || {
                    let upper = v.to_ascii_uppercase();
                    upper.contains("TOKEN")
                        || upper.contains("API_KEY")
                        || upper.contains("SECRET")
                        || upper.contains("BEARER")
                }
        })
}

fn preview_for(pkg: &EnvironmentPackage) -> EnvironmentPreviewResult {
    let mut plugins = 0;
    let mut skins = 0;
    let mut skills = 0;
    let mut mcps = 0;

    // Provenance + versions captured at export time, keyed by item key so the
    // preview can show where each resource re-installs from.
    let exports: HashMap<&str, &ExportedItem> = pkg
        .manifest
        .exports
        .iter()
        .map(|e| (e.key.as_str(), e))
        .collect();

    let mut items = Vec::with_capacity(pkg.manifest.items.len());
    let mut conflicts = Vec::new();
    let mut missing_tokens = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for item in &pkg.manifest.items {
        match item.kind {
            ContentKind::Plugin => plugins += 1,
            ContentKind::Theme => skins += 1,
            ContentKind::Skill => skills += 1,
            ContentKind::Mcp => mcps += 1,
            ContentKind::Bundle => {}
        }

        let key = item.key();
        let count = seen.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count == 2 {
            conflicts.push(format!("duplicate item \"{key}\""));
        }

        let mut source = item.install_spec();
        let version = exports.get(key.as_str()).and_then(|e| e.version.clone());
        if source.is_empty() {
            if let Some(e) = exports.get(key.as_str()) {
                source = e.source.clone();
            }
        }
        if source.is_empty() {
            conflicts.push(format!("no download source for \"{key}\""));
        }

        if item.kind == ContentKind::Mcp && mcp_needs_token(item) {
            missing_tokens.push(key.clone());
        }

        items.push(EnvironmentPreviewItem {
            kind: item.kind,
            name: key,
            source,
            version,
        });
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
        compatible_with: pkg.manifest.compatible_with.clone(),
        items,
        conflicts,
        missing_tokens,
    }
}

fn manifest_checksum(manifest: &EnvironmentManifest) -> Result<String, AppError> {
    let bytes =
        serde_json::to_vec(manifest).context("serialize environment manifest for checksum")?;
    let hash = Sha256::digest(bytes);
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}

fn validate_package(pkg: &EnvironmentPackage) -> Result<(), AppError> {
    if pkg.manifest.format != ENVIRONMENT_FORMAT {
        return Err(AppError::msg("not a DSH environment package"));
    }
    if pkg.manifest.format_version != ENVIRONMENT_FORMAT_VERSION {
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

pub(crate) fn registry_index(registry: &Registry) -> Vec<&RegistryPlugin> {
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

pub(crate) fn find_by_key<'a>(
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

pub(crate) async fn merged_registry(state: &AppState) -> Registry {
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
                if let Some(item) = find_by_key(&entries, ContentKind::Skill, &skill.id) {
                    items.push(item);
                }
            }
            for mcp in &instance.mcp {
                if let Some(item) = find_by_key(&entries, ContentKind::Mcp, &mcp.id) {
                    items.push(item);
                }
            }

            if items.is_empty() {
                return Err(AppError::msg(
                    "current instance has no plugins, skins, skills or MCP servers to export",
                ));
            }

            // Provenance + version per resource (best-effort): plugins/themes
            // read their installed package.json version; skills/MCP are indexed
            // by key and carry no tracked version.
            let exports: Vec<ExportedItem> = items
                .iter()
                .map(|item| {
                    let version = match item.kind {
                        ContentKind::Plugin | ContentKind::Theme => {
                            DshAdapter::installed_version(&instance, &item.name)
                        }
                        _ => None,
                    };
                    ExportedItem {
                        key: item.key(),
                        kind: item.kind,
                        name: item.name.clone(),
                        source: item.install_spec(),
                        version,
                    }
                })
                .collect();

            let manifest = EnvironmentManifest {
                format: ENVIRONMENT_FORMAT.into(),
                format_version: ENVIRONMENT_FORMAT_VERSION,
                exported_at: launcher_core::now_secs(),
                name: instance.name.clone(),
                description: format!("Environment exported from {}", instance.name),
                compatible_with: instance.runtime.version.clone(),
                source: EnvironmentSource {
                    instance_id: instance.id.clone(),
                    instance_name: instance.name.clone(),
                    runtime: instance.runtime.version.clone(),
                },
                items,
                exports,
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
) -> Result<Job, AppError> {
    let path = PathBuf::from(path.trim());
    let pkg = read_package_path(&path)?;
    validate_package(&pkg)?;
    let name = name.or_else(|| Some(package_name_from_path(&path)));
    enqueue_environment_import(&state, &app, pkg, name).await
}

#[tauri::command]
pub async fn environment_import_package(
    state: State<'_, AppState>,
    app: AppHandle,
    bytes: Vec<u8>,
    name: Option<String>,
) -> Result<Job, AppError> {
    let pkg = read_package_bytes(&bytes)?;
    validate_package(&pkg)?;
    enqueue_environment_import(&state, &app, pkg, name).await
}

/// Validate → create a fresh instance → write its empty Library snapshot →
/// enqueue an Install Center job. Returns the `Job` immediately; the instance
/// drainer runs [`environment_import_job`] with per-leaf progress + retry.
async fn enqueue_environment_import(
    state: &AppState,
    app: &AppHandle,
    pkg: EnvironmentPackage,
    name: Option<String>,
) -> Result<Job, AppError> {
    let instance_name = name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| pkg.manifest.name.clone());
    let instance = InstanceManifest::create(&state.paths, &instance_name)?;
    // Stage 12 校准 part 1: write the empty snapshot up front so the new
    // instance appears in the Library before its first leaf lands.
    reconcile_library_inventory_after_market_change(state, app, &instance.id, "environment import")
        .await?;

    let key = slug(&instance_name);
    let label = format!(
        "{} ({} items)",
        pkg.manifest.name,
        pkg.manifest.items.len()
    );
    enqueue_install(
        state,
        app,
        &instance.id,
        &key,
        &label,
        JobPlan::Environment {
            manifest: pkg.manifest,
        },
    )
    .await
}

/// Durable Install Center body: install each manifest leaf with its own
/// progress tick + log line, then reconcile the Library snapshot. Any failed
/// leaf fails the whole row so the package can be retried from Install Center.
pub(crate) async fn environment_import_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    manifest: &EnvironmentManifest,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    emit_log(
        app,
        &format!(
            "{id} · importing environment \"{}\" ({} items)…",
            manifest.name,
            manifest.items.len()
        ),
    );

    let item_count = manifest.items.len();
    ctx.progress("importing", 5);
    let mut failed = 0usize;
    for (idx, item) in manifest.items.iter().enumerate() {
        let key = item.key();
        let kind = item.kind.as_str().to_string();
        match install_bundle_item(
            state,
            app,
            id,
            &instance,
            &settings,
            item,
            LibraryItemSource::ImportedEnvironment,
            Some(ctx),
        )
        .await
        {
            Ok(()) => {
                emit_log(app, &format!("{id} · installed {kind} {key}"));
            }
            Err(e) => {
                failed += 1;
                emit_log(app, &format!("{id} · FAILED {kind} {key}: {e}"));
            }
        }
        if item_count > 0 {
            let pct = 5 + ((idx as i64 + 1) * 75 / item_count as i64);
            ctx.progress("importing", pct);
        }
    }

    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "environment import").await?;
    emit_log(
        app,
        &format!(
            "{id} · environment \"{}\" done ({} failed)",
            manifest.name, failed
        ),
    );
    if failed > 0 {
        return Err(AppError::msg(format!(
            "environment \"{}\" finished with {failed} failed item(s)",
            manifest.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> EnvironmentManifest {
        EnvironmentManifest {
            format: ENVIRONMENT_FORMAT.into(),
            format_version: ENVIRONMENT_FORMAT_VERSION,
            exported_at: 1,
            name: "demo".into(),
            description: String::new(),
            compatible_with: "0.1.0".into(),
            source: EnvironmentSource {
                instance_id: "i".into(),
                instance_name: "demo".into(),
                runtime: "0.1.0".into(),
            },
            items: vec![RegistryPlugin {
                kind: ContentKind::Plugin,
                name: "toolbox".into(),
                npm: Some("@acme/toolbox".into()),
                ..Default::default()
            }],
            exports: vec![ExportedItem {
                key: "toolbox".into(),
                kind: ContentKind::Plugin,
                name: "toolbox".into(),
                source: "npm:@acme/toolbox".into(),
                version: Some("1.0.0".into()),
            }],
        }
    }

    #[test]
    fn export_package_bundles_manifest_and_readme_only() {
        let pkg = EnvironmentPackage {
            manifest: sample_manifest(),
            checksum: "deadbeef".into(),
        };
        let bytes = zip_package(&pkg).expect("zip package");
        let reader = Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(reader).expect("open zip");
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        // No logs, no node_modules, no workspace files — install manifest only.
        assert_eq!(names.len(), 2, "package must bundle manifest + README only");
        assert!(names.iter().any(|n| n == "environment.json"));
        assert!(names.iter().any(|n| n == "README.md"));
    }

    #[test]
    fn export_manifest_carries_no_secrets() {
        // The provider vault (API keys) is never read at export time, and the
        // manifest is install metadata only — no key/env/log content may appear.
        let pkg = EnvironmentPackage {
            manifest: sample_manifest(),
            checksum: "deadbeef".into(),
        };
        let json = package_json(&pkg).expect("serialize package");
        assert!(!json.contains("sk-"), "no API key material in package");
        assert!(!json.contains("DEEPSEEK_API_KEY"), "no key env var in package");
    }

    #[test]
    fn preview_flags_conflicts_and_missing_tokens() {
        let plugin = RegistryPlugin {
            kind: ContentKind::Plugin,
            name: "toolbox".into(),
            npm: Some("@acme/toolbox".into()),
            ..Default::default()
        };
        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".into(), "${GITHUB_TOKEN}".into());
        let mcp = RegistryPlugin {
            kind: ContentKind::Mcp,
            name: "server-github".into(),
            transport: Some("stdio".into()),
            command: Some("npx".into()),
            env: Some(env),
            ..Default::default()
        };
        let ghost = RegistryPlugin {
            kind: ContentKind::Skill,
            name: "ghost".into(),
            ..Default::default()
        };

        let manifest = EnvironmentManifest {
            items: vec![plugin.clone(), plugin, mcp, ghost],
            exports: vec![],
            ..sample_manifest()
        };
        let pkg = EnvironmentPackage {
            manifest,
            checksum: "deadbeef".into(),
        };
        let preview = preview_for(&pkg);

        assert_eq!(preview.items.len(), 4);
        assert_eq!(preview.missing_tokens, vec!["server-github".to_string()]);
        assert!(
            preview
                .conflicts
                .iter()
                .any(|c| c.contains("duplicate item \"toolbox\"")),
            "duplicate flagged: {:?}",
            preview.conflicts
        );
        assert!(
            preview
                .conflicts
                .iter()
                .any(|c| c.contains("no download source for \"ghost\"")),
            "unresolved source flagged: {:?}",
            preview.conflicts
        );
    }
}
