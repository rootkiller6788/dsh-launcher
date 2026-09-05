use dsh_adapter::content as content_adapter;
use launcher_core::market::ContentKind;
use launcher_core::{
    AppSettings, BundleManifest, InstanceManifest, Job, JobPlan, McpServerRecord, RegistryPlugin,
    SkillRecord,
};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

use crate::commands::environment::{find_by_key, merged_registry, registry_index};
use crate::commands::plugins::{
    ensure_not_running, reconcile_library_inventory_after_market_change,
    record_install_metadata_with_source, record_market_install_metadata,
    resolve_plugin_install_target, LibraryItemSource,
};
use crate::commands::process::{emit_log, make_sink};
use crate::error::AppError;
use crate::jobs::{enqueue_install, run_instance_job, HeavyJobKind, JobCtx};
use crate::state::AppState;

/// Installed skills for an instance — each a full provenance record (`{source,
/// hash, installed}`) straight from the manifest. Skills are plain files under
/// `$DSH_HOME/skills/` with no npm package or enable state, so the manifest is
/// the single index.
#[tauri::command]
pub fn skill_list(state: State<'_, AppState>, id: String) -> Result<Vec<SkillRecord>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(content_adapter::installed_skills(&instance))
}

/// Install a skill by enqueueing a backend install job (Stage 8): the command
/// shell writes a `waiting` row and returns immediately; the executor runs the
/// real body and streams `job-updated` events.
#[tauri::command]
pub async fn skill_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<Job, AppError> {
    let key = entry.key();
    enqueue_install(
        &state,
        &app,
        &id,
        &key,
        &format!("skill {key}"),
        JobPlan::Skill { entry },
    )
    .await
}

/// The durable body `skill_install` enqueues. Downloads the SKILL.md, records it
/// in the manifest, then re-calibrates the Library snapshot.
pub(crate) async fn skill_install_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    entry: &RegistryPlugin,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let skill = content_adapter::skill_id(entry);
    emit_log(app, &format!("{id} · installing skill {skill}…"));
    ctx.progress("download", 30);
    let record = content_adapter::install_skill(&instance, entry).await?;
    ctx.progress("recording", 65);
    let record = SkillRecord {
        installed: now_millis(),
        ..record
    };
    InstanceManifest::add_skill(&state.paths, id, &record)?;
    record_market_install_metadata(state, id, entry)?;
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "skill install").await?;
    emit_log(app, &format!("{id} · installed skill {skill}"));
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
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Uninstall, || async {
        ensure_not_running(&state, &id).await?;
        let instance = InstanceManifest::get(&state.paths, &id)?;
        emit_log(&app, &format!("{id} · removing skill {skill}…"));
        content_adapter::uninstall_skill(&instance, &skill)?;
        InstanceManifest::remove_skill(&state.paths, &id, &skill)?;
        emit_log(&app, &format!("{id} · removed skill {skill}"));
        reconcile_library_inventory_after_market_change(&state, &app, &id, "skill uninstall")
            .await?;
        Ok(())
    })
    .await
}

/// Per-skill update status: the `SKILL.md` content hash upstream vs what is
/// installed. Skills have no version number, so the content SHA-256 *is* the
/// signal — an update appears only when the author changed the markdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdate {
    pub id: String,
    /// Hash currently installed (`record.hash`, or the disk file's when the
    /// record predates hash tracking). Empty when neither exists.
    pub installed: String,
    /// Hash currently served by the source.
    pub latest: String,
    pub updatable: bool,
}

/// Epoch milliseconds — stamps `SkillRecord.installed` (matches the
/// `MarketInstallMetadata.installed_at` unit convention elsewhere).
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Per-skill update check: re-fetch each installed skill's upstream hash and
/// compare against its installed baseline. Runs as an `UpdateCheck` job so it
/// shares the instance job gate (one background pass at a time, progress events
/// streamed), mirroring `plugin_updates`.
#[tauri::command]
pub async fn skill_updates(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<Vec<SkillUpdate>, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::UpdateCheck, || async {
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let records = content_adapter::installed_skills(&instance);
        let registry = merged_registry(&state).await;
        let entries = registry_index(&registry);
        let mut out = Vec::new();
        for record in &records {
            // Upstream probe URL: the record's captured source wins (it is
            // exactly where the file was fetched from); legacy records without a
            // source fall back to the registry entry for the same id.
            let source = if record.source.trim().is_empty() {
                find_by_key(&entries, ContentKind::Skill, &record.id)
                    .and_then(|entry| content_adapter::skill_source(&entry))
            } else {
                Some(record.source.clone())
            };
            let Some(source) = source else {
                continue;
            };
            // Installed baseline: prefer the recorded hash; a legacy record
            // with an empty hash falls back to hashing the file on disk so it
            // isn't flagged on first launch.
            let current = if record.hash.is_empty() {
                content_adapter::skill_disk_hash(&instance, &record.id)?.unwrap_or_default()
            } else {
                record.hash.clone()
            };
            let Ok(latest) = content_adapter::fetch_skill_hash(&source).await else {
                continue;
            };
            let updatable = !current.is_empty() && latest != current;
            out.push(SkillUpdate {
                id: record.id.clone(),
                installed: current,
                latest,
                updatable,
            });
        }
        Ok(out)
    })
    .await
}

/// Update a skill to the version its source currently serves: re-fetch and
/// re-land the `SKILL.md` (atomically) only when the content actually differs —
/// an unchanged hash short-circuits so the button never churns a file.
#[tauri::command]
pub async fn skill_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    skill: String,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Install, || async {
        ensure_not_running(&state, &id).await?;
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let Some(record) = instance.skills.iter().find(|r| r.id == skill) else {
            return Err(AppError::msg(format!("skill '{skill}' is not installed")));
        };
        let registry = merged_registry(&state).await;
        let entries = registry_index(&registry);
        let entry = find_by_key(&entries, ContentKind::Skill, &skill).ok_or_else(|| {
            AppError::msg(format!(
                "skill '{skill}' has no registry entry — cannot resolve an upstream source"
            ))
        })?;
        emit_log(&app, &format!("{id} · updating skill {skill}…"));
        let current_hash = if record.hash.is_empty() {
            content_adapter::skill_disk_hash(&instance, &skill)?.unwrap_or_default()
        } else {
            record.hash.clone()
        };
        if let Some(updated) =
            content_adapter::update_skill(&instance, &entry, &current_hash).await?
        {
            let updated = SkillRecord {
                installed: now_millis(),
                ..updated
            };
            InstanceManifest::add_skill(&state.paths, &id, &updated)?;
            emit_log(&app, &format!("{id} · updated skill {skill}"));
        } else {
            emit_log(&app, &format!("{id} · skill {skill} already up to date"));
        }
        reconcile_library_inventory_after_market_change(&state, &app, &id, "skill update").await?;
        Ok(())
    })
    .await
}

/// Installed MCP servers for an instance — the full connection records from the
/// manifest (the single source of truth; `cordis.patch.yml` is compiled from
/// them, so the record list *is* what DSH loads).
#[tauri::command]
pub fn mcp_list(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<McpServerRecord>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    Ok(instance.mcp)
}

/// Install an MCP server by enqueueing a backend install job.
#[tauri::command]
pub async fn mcp_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<Job, AppError> {
    let key = entry.key();
    enqueue_install(
        &state,
        &app,
        &id,
        &key,
        &format!("MCP {key}"),
        JobPlan::Mcp { entry },
    )
    .await
}

/// The durable body `mcp_install` enqueues: write the full connection record
/// into the manifest, compile `cordis.patch.yml` from it (the record is the
/// source of truth), record it, refresh the Library snapshot.
pub(crate) async fn mcp_install_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    entry: &RegistryPlugin,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let mcp = content_adapter::mcp_id(entry);
    emit_log(app, &format!("{id} · installing MCP {mcp}…"));
    let record = content_adapter::mcp_record(entry);
    let updated = InstanceManifest::add_mcp(&state.paths, id, &record)?;
    content_adapter::sync_mcp_patch(&updated, &updated.mcp)?;
    ctx.progress("recording", 65);
    record_market_install_metadata(state, id, entry)?;
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "MCP install").await?;
    emit_log(app, &format!("{id} · installed MCP {mcp}"));
    Ok(())
}

/// Uninstall an MCP server: drop its record from the manifest and recompile the
/// patch — uninstall = remove the record, the enabled records stay the source
/// of truth.
#[tauri::command]
pub async fn mcp_uninstall(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    mcp: String,
) -> Result<(), AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Uninstall, || async {
        ensure_not_running(&state, &id).await?;
        emit_log(&app, &format!("{id} · removing MCP {mcp}…"));
        let updated = InstanceManifest::remove_mcp(&state.paths, &id, &mcp)?;
        content_adapter::sync_mcp_patch(&updated, &updated.mcp)?;
        emit_log(&app, &format!("{id} · removed MCP {mcp}"));
        reconcile_library_inventory_after_market_change(&state, &app, &id, "MCP uninstall").await?;
        Ok(())
    })
    .await
}

/// Toggle an installed MCP server on/off. `enabled=false` recompiles the row out
/// of `cordis.patch.yml` (DSH stops loading it); `true` compiles it back. No
/// MCP `disabled:` toggle row exists — absent from the patch *is* disabled.
/// Returns the instance's updated record list.
#[tauri::command]
pub async fn mcp_set_enabled(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    mcp: String,
    enabled: bool,
) -> Result<Vec<McpServerRecord>, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::ProfileMutation, || async {
        ensure_not_running(&state, &id).await?;
        let mut instance = InstanceManifest::get(&state.paths, &id)?;
        let Some(record) = instance.mcp.iter_mut().find(|r| r.id == mcp) else {
            return Err(AppError::msg(format!("MCP '{mcp}' is not installed")));
        };
        if record.enabled == enabled {
            return Ok(instance.mcp);
        }
        record.enabled = enabled;
        instance.save(&state.paths.instance_file(&id))?;
        emit_log(
            &app,
            &format!(
                "{id} · {} MCP {mcp}",
                if enabled { "enabled" } else { "disabled" }
            ),
        );
        content_adapter::sync_mcp_patch(&instance, &instance.mcp)?;
        reconcile_library_inventory_after_market_change(
            &state,
            &app,
            &id,
            if enabled { "MCP enable" } else { "MCP disable" },
        )
        .await?;
        Ok(instance.mcp)
    })
    .await
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
///
/// `ctx` is `Some` when the call is driven by a backend install job (so stage
/// boundaries, stderr capture and exit codes land in the job row) and `None`
/// for synchronous paths like environment import that stay outside the store.
#[allow(clippy::too_many_arguments)] // cohesive leaf: state/app/id/instance/settings/item/source/ctx are each genuinely distinct
pub(crate) async fn install_bundle_item(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    instance: &InstanceManifest,
    settings: &AppSettings,
    item: &RegistryPlugin,
    source: LibraryItemSource,
    ctx: Option<&JobCtx>,
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
            let install_target =
                resolve_plugin_install_target(state, app, id, &spec, Some(item)).await;
            if let Some(ctx) = ctx {
                ctx.progress("dsh-install", 40);
            }
            let sink = ctx
                .map(|c| c.sink())
                .unwrap_or_else(|| make_sink(app.clone()));
            let code = state
                .adapter
                .run_plugin_command(
                    settings,
                    instance,
                    &["add".to_string(), install_target],
                    sink,
                )
                .await?;
            if code != 0 {
                if let Some(ctx) = ctx {
                    ctx.set_exit_code(i64::from(code));
                }
                return Err(AppError::msg(format!(
                    "dsh plugin add exited with code {code} — check the install spec ({spec}) resolves on npm and your network can reach the registry (detail in Activity logs)"
                )));
            }
            if let Some(ctx) = ctx {
                ctx.progress("recording", 65);
            }
            if item.kind == ContentKind::Theme {
                InstanceManifest::add_skin(&state.paths, id, &key)?;
            }
            record_install_metadata_with_source(state, id, item, source)?;
        }
        ContentKind::Skill => {
            emit_log(app, &format!("{id} · installing skill {key}…"));
            if let Some(ctx) = ctx {
                ctx.progress("download", 30);
            }
            let record = content_adapter::install_skill(instance, item).await?;
            if let Some(ctx) = ctx {
                ctx.progress("recording", 65);
            }
            let record = SkillRecord {
                installed: now_millis(),
                ..record
            };
            InstanceManifest::add_skill(&state.paths, id, &record)?;
            record_install_metadata_with_source(state, id, item, source)?;
        }
        ContentKind::Mcp => {
            emit_log(app, &format!("{id} · installing MCP {key}…"));
            let record = content_adapter::mcp_record(item);
            let updated = InstanceManifest::add_mcp(&state.paths, id, &record)?;
            content_adapter::sync_mcp_patch(&updated, &updated.mcp)?;
            record_install_metadata_with_source(state, id, item, source)?;
        }
        ContentKind::Bundle => {
            return Err(AppError::msg("nested bundles are not supported"));
        }
    }
    Ok(())
}

/// Import a bundle manifest by enqueueing a backend job. The full manifest is
/// persisted in the job's `plan` column so a Retry re-runs it from the backend.
#[tauri::command]
pub async fn bundle_import(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    manifest: BundleManifest,
) -> Result<Job, AppError> {
    let name = manifest.name.clone();
    let label = format!("bundle \"{name}\"");
    enqueue_install(
        &state,
        &app,
        &id,
        &name,
        &label,
        JobPlan::Bundle { manifest },
    )
    .await
}

/// The durable body `bundle_import` enqueues: dispatch each item to its kind's
/// installer and stream per-item progress into the job. Any leaf failure fails
/// the bundle with a count; item-level messages stay in Activity + the job tail.
pub(crate) async fn bundle_import_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    manifest: &BundleManifest,
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
            "{id} · importing bundle \"{}\" ({} items)…",
            manifest.name,
            manifest.items.len()
        ),
    );

    let item_count = manifest.items.len();
    ctx.progress("importing", 5);
    let mut failed = 0usize;
    for (idx, item) in manifest.items.iter().enumerate() {
        let key = item.key();
        let label = kind_label(item.kind).to_string();
        match install_bundle_item(
            state,
            app,
            id,
            &instance,
            &settings,
            item,
            LibraryItemSource::MarketInstalled,
            Some(ctx),
        )
        .await
        {
            Ok(()) => {
                emit_log(app, &format!("{id} · installed {label} {key}"));
            }
            Err(e) => {
                failed += 1;
                emit_log(app, &format!("{id} · FAILED {label} {key}: {e}"));
            }
        }
        if item_count > 0 {
            let pct = 5 + ((idx as i64 + 1) * 75 / item_count as i64);
            ctx.progress("importing", pct);
        }
    }

    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "bundle import").await?;
    emit_log(
        app,
        &format!(
            "{id} · bundle \"{}\" done ({} failed)",
            manifest.name, failed
        ),
    );
    if failed > 0 {
        return Err(AppError::msg(format!(
            "bundle \"{}\" finished with {failed} failed item(s)",
            manifest.name
        )));
    }
    Ok(())
}

/// Unified Market install entrypoint. The Market calls this for every leaf item
/// so install ordering is consistent: write through DSH or a DSH-recognized
/// workspace location first, then record Launcher metadata and refresh the
/// Library snapshot. Now enqueues a backend job instead of awaiting inline.
#[tauri::command]
pub async fn market_install(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    entry: RegistryPlugin,
) -> Result<Job, AppError> {
    let key = entry.key();
    let label = kind_label(entry.kind);
    enqueue_install(
        &state,
        &app,
        &id,
        &key,
        &format!("{label} {key}"),
        JobPlan::Market { entry },
    )
    .await
}

/// The durable body `market_install` enqueues. Most kinds share the single
/// `install_bundle_item` path; leaf detail (stage + stderr) streams into the job.
pub(crate) async fn market_install_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    entry: &RegistryPlugin,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();

    let label = kind_label(entry.kind);
    let key = entry.key();
    emit_log(app, &format!("{id} · Market installing {label} {key}…"));
    install_bundle_item(
        state,
        app,
        id,
        &instance,
        &settings,
        entry,
        LibraryItemSource::MarketInstalled,
        Some(ctx),
    )
    .await?;
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "market install").await?;
    emit_log(app, &format!("{id} · Market installed {label} {key}"));
    Ok(())
}
