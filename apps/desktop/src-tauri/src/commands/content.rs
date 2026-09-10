use dsh_adapter::content as content_adapter;
use dsh_adapter::mcp_import::{
    parse_any, parse_claude_or_cursor, parse_vscode_settings, ImportedMcp,
};
use dsh_adapter::mcp_local;
use dsh_adapter::mcp_prefetch::{classify_install, prefetch_mcp, InstallClass};
use dsh_adapter::mcp_probe::probe_mcp;
use dsh_adapter::mcp_resolver::probe_mcp_install;
use launcher_core::market::ContentKind;
use launcher_core::process::LogSink;
use launcher_core::{
    load_runtime, save_runtime, AppSettings, BundleManifest, InstanceManifest, Job, JobPlan,
    LogLevel, LogLine, LogStream, McpConfigStore, McpConfigVar, McpInstallManifest,
    McpRuntimeState, McpServerRecord, RegistryPlugin, SkillRecord, MCP_STATE_ERROR,
    MCP_STATE_UNTESTED,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};

use crate::commands::environment::{find_by_key, merged_registry, registry_index};
use crate::commands::plugins::{
    ensure_not_running, reconcile_library_inventory_after_market_change,
    record_install_metadata_with_source, record_market_install_metadata,
    remove_market_install_metadata, resolve_plugin_install_target, LibraryItemSource,
};
use crate::commands::process::{emit_log, make_sink};
use crate::commands::settings::settings_snapshot;
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
        // Phase 1 — resolve each record's probe URL + installed baseline
        // synchronously. The slow part is the network: fetching upstream hashes
        // serially (each raw.githubusercontent fetch can hang ~30s before the
        // gh-proxy fallback) would stall the whole Library update state, so the
        // probes run concurrently in phase 2.
        struct Probe {
            id: String,
            current: String,
        }
        let mut probes: Vec<(Probe, String)> = Vec::new();
        for record in &records {
            tracing::info!(
                target: "update-check",
                "skill {}: records={} source_len={} hash={}",
                record.id,
                records.len(),
                record.source.len(),
                &record.hash[..record.hash.len().min(8)],
            );
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
                tracing::info!(target: "update-check", "skill {}: NO SOURCE", record.id);
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
            probes.push((
                Probe {
                    id: record.id.clone(),
                    current,
                },
                source,
            ));
        }
        // Phase 2 — concurrent upstream probes.
        let mut workers = tokio::task::JoinSet::new();
        for (probe, source) in probes {
            workers.spawn(async move {
                let fetched = content_adapter::fetch_skill_hash(&source).await;
                (probe.id, probe.current, fetched)
            });
        }
        let mut out = Vec::new();
        while let Some(res) = workers.join_next().await {
            let Ok((skill_id, current, fetched)) = res else {
                continue;
            };
            match fetched {
                Ok(latest) => {
                    let updatable = !current.is_empty() && latest != current;
                    tracing::info!(
                        target: "update-check",
                        "skill {}: current={} latest={} updatable={}",
                        skill_id,
                        &current[..current.len().min(8)],
                        &latest[..latest.len().min(8)],
                        updatable,
                    );
                    out.push(SkillUpdate {
                        id: skill_id,
                        installed: current,
                        latest,
                        updatable,
                    });
                }
                Err(e) => {
                    // Don't swallow probe failures silently — surface them in
                    // the Activity feed so a skill that can't reach its source
                    // shows up instead of quietly never offering an Update.
                    emit_log(
                        &app,
                        &format!("{id} · skill update check: {skill_id} probe failed: {e}"),
                    );
                    tracing::warn!(
                        target: "update-check",
                        "skill {skill_id}: probe failed: {e}"
                    );
                }
            }
        }
        tracing::info!(target: "update-check", "skill_updates -> {} entries", out.len());
        Ok(out)
    })
    .await
}

/// Update a skill to the version its source currently serves. Like installs,
/// the click enqueues a durable job (Install Center gets a row with real stage
/// progress) and returns immediately; the executor re-fetches and re-lands the
/// `SKILL.md` atomically, short-circuiting when content already matches.
#[tauri::command]
pub async fn skill_update(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    skill: String,
) -> Result<Job, AppError> {
    let registry = merged_registry(&state).await;
    let entries = registry_index(&registry);
    let entry = find_by_key(&entries, ContentKind::Skill, &skill).ok_or_else(|| {
        AppError::msg(format!(
            "skill '{skill}' has no registry entry — cannot resolve an upstream source"
        ))
    })?;
    enqueue_install(
        &state,
        &app,
        &id,
        &skill,
        &format!("update skill {skill}"),
        JobPlan::SkillUpdate { entry },
    )
    .await
}

/// The durable body `skill_update` enqueues: fetch the current upstream
/// `SKILL.md`, land it only when the hash changed, and re-record the manifest.
pub(crate) async fn skill_update_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    entry: &RegistryPlugin,
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let skill = content_adapter::skill_id(entry);
    let Some(record) = instance.skills.iter().find(|r| r.id == skill) else {
        return Err(AppError::msg(format!("skill '{skill}' is not installed")));
    };
    emit_log(app, &format!("{id} · updating skill {skill}…"));
    ctx.progress("fetching", 40);
    let current_hash = if record.hash.is_empty() {
        content_adapter::skill_disk_hash(&instance, &skill)?.unwrap_or_default()
    } else {
        record.hash.clone()
    };
    if let Some(updated) = content_adapter::update_skill(&instance, entry, &current_hash).await? {
        ctx.progress("recording", 70);
        let updated = SkillRecord {
            installed: now_millis(),
            ..updated
        };
        InstanceManifest::add_skill(&state.paths, id, &updated)?;
        emit_log(app, &format!("{id} · updated skill {skill}"));
    } else {
        emit_log(app, &format!("{id} · skill {skill} already up to date"));
    }
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "skill update").await?;
    Ok(())
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
/// The MCP record to persist: the resolver's canonical launch — `mcpInstall.launch`
/// when the catalog precomputed one (it replaces any best-effort pseudo command) —
/// otherwise the entry's own command as today.
fn mcp_record_effective(entry: &RegistryPlugin, plan: Option<&McpInstallManifest>) -> McpServerRecord {
    let mut record = content_adapter::mcp_record(entry);
    if let Some(plan) = plan {
        if !plan.launch.command.is_empty() {
            record.command = plan.launch.command.clone();
            record.args = plan.launch.args.clone();
            if !record.transport.eq_ignore_ascii_case("streamable-http") {
                record.transport = "stdio".to_string();
            }
        }
    }
    record
}

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

    // Effective install plan: catalog precompute → (only when there is no launch
    // command at all) a quick runtime github probe for a freshly-published repo.
    let mut plan = entry.mcp_install.clone();
    if plan.is_none() && entry.command.is_none() {
        if let Some(probed) = probe_mcp_install(entry, entry.command.as_deref()).await {
            emit_log(
                app,
                &format!("{id} · {mcp}: resolver found published package {}", probed.package),
            );
            plan = Some(probed);
        }
    }
    let mut record = mcp_record_effective(entry, plan.as_ref());

    // Classify what "download" means for this server *before* the record write:
    //  - registry package (npm/uv, cacheable spec) → a required install-time
    //    download stage: the package is fetched into the shared cache now so the
    //    post-install verification (and DSH's first npx/uvx launch) is a cache
    //    hit. Failure fails the install.
    //  - remote streamable-http / url-only server → there is no local package
    //    (the server is hosted elsewhere) — the record *is* the install.
    //  - anything else (github:/git+ source-run, no published package, compiled
    //    languages) → Phase 4 local install: shallow-clone the repo and build
    //    the deterministic entry so DSH's first launch never live-fetches.
    // Whatever the class, every install ends in a transient real-launch/connect
    // verification (probe → Library badge). See §8.4 / roadmap §11.
    // `classify_install` is the single tested source of truth for the three-way
    // route (dsh-adapter unit tests lock the invariants below).
    let cls = classify_install(&record, plan.as_ref());

    if cls == InstallClass::RegistryPackage {
        let plan = plan.as_ref().expect("registry_pkg implies a plan");
        ctx.progress("download", 20);
        let settings = settings_snapshot(state)?;
        let node = state.adapter.resolve_node(&settings);
        emit_log(
            app,
            &format!("{id} · {mcp}: downloading package {}…", plan.package),
        );
        match prefetch_mcp(plan, node.as_deref(), ctx.sink()).await {
            Ok(detail) => emit_log(app, &format!("{id} · {mcp}: {detail}")),
            Err(e) => {
                emit_log(app, &format!("{id} · {mcp}: download failed: {e}"));
                return Err(AppError::msg(format!(
                    "{mcp} package download failed — nothing was installed: {e}"
                )));
            }
        }
    } else if cls == InstallClass::Remote {
        emit_log(
            app,
            &format!(
                "{id} · {mcp}: remote {} server — no package to download; writing record",
                record.transport
            ),
        );
    } else {
        // Phase 4 (roadmap §11): a source-run / unpublished / compiled-language
        // server is *built from its repo at install time*. Shallow-clone into the
        // instance's per-server `mcp/<server>/repo`, fingerprint + build the
        // deterministic entry (go → cargo → node → python), and fall back to the
        // configured provider's LLM only when the repo is ambiguous. On success
        // the record is rewritten to the local absolute entry — DSH's first
        // launch (and the probe below) spawns the built artifact, never a live
        // github fetch.
        match mcp_local::github_source(&entry.url) {
            Some((clone_url, _, _)) => {
                let repo_dir = state.paths.mcp_dir(id, &record.id).join("repo");
                let settings = settings_snapshot(state)?;
                let node = state.adapter.resolve_node(&settings);
                // The AI fallback is optional: resolve the provider key lazily so
                // a deterministic build works even with none configured.
                let instance = InstanceManifest::get(&state.paths, id)?;
                let provider = state.vault.resolve(&instance.provider_ref).ok();
                ctx.progress("clone", 25);
                emit_log(
                    app,
                    &format!("{id} · {mcp}: source install — cloning {clone_url}"),
                );
                match mcp_local::install_local(
                    &clone_url,
                    &repo_dir,
                    node.as_deref(),
                    provider.as_ref(),
                    ctx.sink(),
                )
                .await
                {
                    Ok((launch, _, how)) => {
                        if how.starts_with("ai-resolve") {
                            ctx.progress("ai-resolve", 60);
                        } else {
                            ctx.progress("build", 55);
                        }
                        emit_log(
                            app,
                            &format!("{id} · {mcp}: {how} → local entry {}", launch.command),
                        );
                        record.command = launch.command;
                        record.args = launch.args;
                        record.env = launch.env;
                        record.transport = "stdio".to_string();
                    }
                    Err(e) => {
                        emit_log(app, &format!("{id} · {mcp}: source install failed: {e}"));
                        return Err(AppError::msg(format!(
                            "{mcp} source install failed — nothing was installed: {e}"
                        )));
                    }
                }
            }
            None => {
                // No resolvable GitHub repo: today's behavior — write the entry's
                // own command and let the probe exercise it.
                emit_log(
                    app,
                    &format!(
                        "{id} · {mcp}: no registry package and no resolvable GitHub repo — writing the entry's command as-is"
                    ),
                );
            }
        }
    }

    // Directory-gated server (filesystem family): a stdio launch with no
    // allowed-directory arg idles waiting for MCP roots and rejects every path
    // (observed real-machine — `list_allowed_directories` empty). Inject the
    // instance workspace as the default allowed directory so a fresh install
    // actually operates on its own workspace; the user can re-point it later.
    // Recognition bound lives in `content_adapter::needs_allowed_directory`.
    if content_adapter::needs_allowed_directory(&record)
        && !record.args.iter().any(|a| std::path::Path::new(a).is_absolute())
    {
        let inst = InstanceManifest::get(&state.paths, id)?;
        emit_log(
            app,
            &format!(
                "{id} · {mcp}: filesystem-family server — adding workspace {} as allowed-directory",
                inst.workspace
            ),
        );
        record.args.push(inst.workspace);
    }

    ctx.progress("install", 45);
    // Rollback guard: only a *fresh* install may be rolled back by a failed
    // probe. If the record already existed before this job (a re-install /
    // update flow), removing it on probe failure would destroy the previously
    // working server — so genuine-failure rollback is gated to `!pre_existing`.
    let pre_existing = InstanceManifest::get(&state.paths, id)?
        .mcp
        .iter()
        .any(|r| r.id == record.id);
    let updated = InstanceManifest::add_mcp(&state.paths, id, &record)?;
    content_adapter::sync_mcp_patch(&updated, &updated.mcp)?;
    ctx.progress("recording", 65);
    record_market_install_metadata(state, id, entry)?;
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "MCP install").await?;

    // 装完即验证 (post-install verification) — every installed MCP is transiently
    // launched (stdio / local-built / source-run) or connected (remote http) once
    // so the Library badge carries a real state immediately instead of `untested`.
    // Locally-built servers hand the probe their just-recorded absolute entry, so
    // this is a true health check.
    //
    // 真装败才回滚 (genuine-failure rollback only): a stdio install whose probe
    // cannot even get an `initialize` answer (spawn failed / exited non-zero /
    // timed out / no launch at all) is a *failed install* — the just-written
    // record is rolled back (manifest + patch + per-server dir + market metadata)
    // so a broken download/build never lingers in the library as "installed". It
    // is deliberately NOT rolled back when any of these holds:
    //   - probe verdict is ok / degraded — a degraded server came up but refused
    //     the handshake (config-grade: missing token/env, wrong server); it is
    //     legitimately installed and must stay so the user can configure it;
    //   - the record itself declares auth it needs supplied (`mcp_record_needs_token`)
    //     — same config-grade reasoning: fill it in Preferences, then 检查 again;
    //   - the server is remote streamable-http — the record *is* the install; an
    //     unreachable endpoint at install time is transient, not a failed install,
    //     so the pointer stays with an honest error badge;
    //   - the record already existed before this job (`pre_existing`) — rolling back
    //     would destroy a previously-working server that a re-install/update replaced.
    ctx.progress("probe", 92);
    match probe_and_persist(state, app, id, &record.id).await {
        Ok(snapshot) => {
            emit_log(app, &format!("{id} · {mcp}: probe → {}", snapshot.state));
            let genuine_failure = snapshot.state == MCP_STATE_ERROR
                && cls != InstallClass::Remote
                && !pre_existing
                && !content_adapter::mcp_record_needs_token(&record);
            if genuine_failure {
                let reason = snapshot.error.clone().unwrap_or_default();
                emit_log(
                    app,
                    &format!("{id} · {mcp}: genuine install failure — rolling back ({reason})"),
                );
                let updated = InstanceManifest::remove_mcp(&state.paths, id, &record.id)?;
                content_adapter::sync_mcp_patch(&updated, &updated.mcp)?;
                let mcp_dir = state.paths.mcp_dir(id, &record.id);
                if mcp_dir.exists() {
                    match std::fs::remove_dir_all(&mcp_dir) {
                        Ok(()) => emit_log(
                            app,
                            &format!("{id} · {mcp}: rolled back local files"),
                        ),
                        Err(e) => emit_log(
                            app,
                            &format!(
                                "{id} · {mcp}: rollback removed record, local-file cleanup failed: {e}"
                            ),
                        ),
                    }
                }
                remove_market_install_metadata(state, id, &record.id)?;
                reconcile_library_inventory_after_market_change(
                    state,
                    app,
                    id,
                    "MCP install rollback",
                )
                .await?;
                return Err(AppError::msg(format!(
                    "{mcp} failed its post-install check ({reason}) — the install was rolled back \
                     and nothing was left installed. If the server needs an API key or URL to start, \
                     set it first, then install again."
                )));
            }
        }
        Err(e) => emit_log(app, &format!("{id} · {mcp}: verification skipped ({e})")),
    }

    emit_log(app, &format!("{id} · installed MCP {mcp}"));
    Ok(())
}

/// Uninstall an MCP server: drop its record from the manifest, recompile the
/// patch, and remove the server's per-instance `mcp/<server>/` dir (local build
/// + probe state) — the enabled records stay the source of truth.
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
        // Phase 4 cleanup: uninstall drops the record *and* the server's whole
        // per-instance `mcp/<server>/` dir — the local build (repo clone / bin /
        // .venv), the probe's runtime.json, and its logs. remove_dir_all through
        // std::fs (never cmd /c rd) so a local-built server leaves nothing behind.
        let mcp_dir = state.paths.mcp_dir(&id, &mcp);
        if mcp_dir.exists() {
            match std::fs::remove_dir_all(&mcp_dir) {
                Ok(()) => emit_log(&app, &format!("{id} · removed MCP {mcp} local files")),
                Err(e) => {
                    emit_log(
                        &app,
                        &format!(
                            "{id} · removed MCP {mcp} record, but local-file cleanup failed: {e}"
                        ),
                    );
                }
            }
        }
        // Drop the market-install provenance too (launcher_metadata +
        // install_sources) so a removed MCP leaves no "market installed" residue
        // in library-inventory.json.
        remove_market_install_metadata(&state, &id, &mcp)?;
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

/// One MCP's persisted runtime snapshot, returned by [`mcp_runtime`] for the
/// Library badge (roadmap §9.4). The record list is the source of truth; the
/// snapshot is whatever the last health check (or none) left in runtime.json.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRuntimeEntry {
    pub id: String,
    pub server_name: String,
    pub state: McpRuntimeState,
}

/// Health-check a single installed MCP (roadmap §9.4): transiently spawn the
/// server, run an `initialize` handshake, persist the verdict into
/// `mcp/<server>/runtime.json` plus a `logs/last.log` transcript, and return the
/// folded state. Runs inside the per-instance heavy gate so a probe never races
/// a launch/install. Never touches DSH's live process — the launcher self-proves.
#[tauri::command]
pub async fn mcp_health(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    server: String,
) -> Result<McpRuntimeState, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::McpHealth, || async {
        probe_and_persist(&state, &app, &id, &server).await
    })
    .await
}

/// One transient probe + persist, shared by the 「检查」button ([`mcp_health`],
/// serialized behind the instance gate) and by remote-streamable-http installs
/// ([`mcp_install_job`], already inside that gate — so this body never re-acquires
/// it). Looks the record up by id or serverName, transiently spawns/HTTP-POSTs the
/// server for an `initialize` handshake, persists the verdict into
/// `mcp/<server>/runtime.json` + `logs/last.log`, and returns the folded state.
async fn probe_and_persist(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    server: &str,
) -> Result<McpRuntimeState, AppError> {
    let instance = InstanceManifest::get(&state.paths, id)?;
    let Some(record) = instance
        .mcp
        .iter()
        .find(|r| r.id == server || r.server_name == server)
        .cloned()
    else {
        return Err(AppError::msg(format!("MCP '{server}' is not installed")));
    };

    let settings = settings_snapshot(state)?;
    let node = state.adapter.resolve_node(&settings);

    let runtime_file = state.paths.mcp_runtime_file(id, &record.id);
    let log_file = state.paths.mcp_log_file(id, &record.id);
    let mut snapshot = load_runtime(&runtime_file);

    // Capturing sink: forwards to Activity (same as install jobs) and keeps a
    // transcript for `logs/last.log`.
    let buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let sink = health_sink(app.clone(), buf.clone());
    line(&sink, &format!("{id} · health-check MCP {} (transport {})…", record.id, record.transport));

    let incoming = probe_mcp(&record, node.as_deref(), sink.clone()).await;
    snapshot.record(&incoming);
    save_runtime(&runtime_file, &snapshot)?;
    if let Some(dir) = log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = buf.lock() {
        let _ = std::fs::write(&log_file, text.as_str());
    }
    line(&sink, &format!("{id} · MCP {} → {}", record.id, snapshot.state));
    emit_log(app, &format!("{id} · MCP {} health: {}", record.id, snapshot.state));
    Ok(snapshot)
}

/// Read-only snapshot of every installed MCP's persisted runtime state
/// (roadmap §9.4). `mcp_list` carries the records; this carries the badges.
#[tauri::command]
pub fn mcp_runtime(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<McpRuntimeEntry>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let mut entries = instance
        .mcp
        .iter()
        .map(|r| {
            let mut snapshot = load_runtime(&state.paths.mcp_runtime_file(&id, &r.id));
            if snapshot.state == MCP_STATE_UNTESTED {
                // Brand-new server (install doesn't auto-probe): show the transport
                // so the badge tooltip isn't empty, keep the untested grade.
                snapshot.transport = r.transport.clone();
            }
            McpRuntimeEntry {
                id: r.id.clone(),
                server_name: r.server_name.clone(),
                state: snapshot,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

// --- MCP config (roadmap §12 "fill + inject") --------------------------------
//
// Values live in the OS credential store (keyring); only key names + secret
// flags are on disk. These commands read/write that split — no value ever
// crosses the IPC boundary back to the frontend.

/// One key/value pair from the config form. A `null`/blank `value` keeps any
/// stored secret untouched (the row was left empty); when present, the value
/// goes to the OS credential store and is never written to disk.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigEntryIn {
    key: String,
    #[serde(default)]
    secret: bool,
    value: Option<String>,
}

/// The keys a server's config currently holds (names + secret flags only —
/// values never return to the UI, which can only see whether a key is set).
#[tauri::command]
pub fn mcp_config_get(
    state: State<'_, AppState>,
    id: String,
    server: String,
) -> Result<Vec<McpConfigVar>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let record = mcp_record_for(&instance, &server)?;
    Ok(McpConfigStore::new(state.paths.clone()).list(&id, &record.id)?)
}

/// Upsert one or more configured keys. Filled values go to the OS credential
/// store; blank rows only touch the on-disk key name/secret flag.
#[tauri::command]
pub fn mcp_config_save(
    state: State<'_, AppState>,
    id: String,
    server: String,
    entries: Vec<McpConfigEntryIn>,
) -> Result<Vec<McpConfigVar>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let record = mcp_record_for(&instance, &server)?;
    let store = McpConfigStore::new(state.paths.clone());
    for entry in entries {
        let key = entry.key.trim();
        if key.is_empty() {
            continue;
        }
        store.set(&id, &record.id, key, entry.secret, entry.value.as_deref())?;
    }
    Ok(store.list(&id, &record.id)?)
}

/// Drop a configured key: its name from disk and its value from the OS
/// credential store.
#[tauri::command]
pub fn mcp_config_remove(
    state: State<'_, AppState>,
    id: String,
    server: String,
    key: String,
) -> Result<Vec<McpConfigVar>, AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let record = mcp_record_for(&instance, &server)?;
    let store = McpConfigStore::new(state.paths.clone());
    store.remove(&id, &record.id, &key)?;
    Ok(store.list(&id, &record.id)?)
}

/// Resolve an installed record by either its catalog key (`owner/name`) or its
/// DSH-facing server name — callers pass whichever the row knows.
fn mcp_record_for<'a>(
    instance: &'a InstanceManifest,
    server: &str,
) -> Result<&'a McpServerRecord, AppError> {
    instance
        .mcp
        .iter()
        .find(|r| r.id == server || r.server_name == server)
        .ok_or_else(|| AppError::msg(format!("MCP '{server}' is not installed")))
}

// --- MCP Import (roadmap §10 / Phase 3) ------------------------------------

/// One detectable MCP-import source, surfaced by [`mcp_import_detect`] for the
/// frontend's Import modal. `servers` carries the parser warnings (e.g. a
/// `${TOKEN}` env value) so the UI can explain each row before import.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpImportSource {
    /// Stable source label the import command routes on: `"claude"` |
    /// `"cursor"` | `"vscode"`.
    pub kind: String,
    /// Absolute path scanned (empty when the source's root env var is missing).
    pub path: String,
    /// Whether the config file exists on disk right now.
    pub found: bool,
    /// Parsed servers (empty when the file is absent / unparseable).
    pub servers: Vec<ImportedMcp>,
    /// Read/parse failure text when the file exists but can't be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Body of [`mcp_import`]. `source` selects which known config to parse
/// (`claude`/`cursor`/`vscode`), or `"raw"` to parse pasted `raw` JSON.
/// `serverNames` optionally narrows the import to a subset of the parsed
/// servers; empty imports every importable server.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpImportRequest {
    pub source: String,
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub server_names: Vec<String>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The known config locations the launcher scans for importable MCP servers.
/// Claude + VSCode live under `%APPDATA%`; Cursor's global config sits in the
/// user profile (`~/.cursor/mcp.json`).
fn candidate_sources() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(appdata) = env_path("APPDATA") {
        out.push((
            "claude".to_string(),
            appdata.join("Claude").join("claude_desktop_config.json"),
        ));
        out.push((
            "vscode".to_string(),
            appdata.join("Code").join("User").join("settings.json"),
        ));
    }
    if let Some(home) = env_path("USERPROFILE").or_else(|| env_path("HOME")) {
        out.push(("cursor".to_string(), home.join(".cursor").join("mcp.json")));
    }
    out
}

/// Route a source label to its config parser. Cursor + Claude share the
/// top-level `mcpServers` shape; VSCode nests under `mcp.servers`.
fn parse_for_source(kind: &str, text: &str) -> Result<Vec<ImportedMcp>, String> {
    if kind == "vscode" {
        parse_vscode_settings(text)
    } else {
        parse_claude_or_cursor(text)
    }
}

/// Scan the known config locations and parse whatever exists — the read-only
/// half of the Import flow (roadmap §10.3). Always returns one entry per known
/// source; a missing/unparseable file yields `found:false` + `error`, never a
/// hard failure, so the modal can show all three rows.
#[tauri::command]
pub fn mcp_import_detect(state: State<'_, AppState>) -> Result<Vec<McpImportSource>, AppError> {
    let _ = state; // path scanning is environment-only
    let mut out = Vec::new();
    for (kind, path) in candidate_sources() {
        let (found, servers, error) = if !path.exists() {
            (false, Vec::new(), None)
        } else {
            match std::fs::read_to_string(&path) {
                Ok(text) => match parse_for_source(&kind, &text) {
                    Ok(list) => (true, list, None),
                    Err(e) => (true, Vec::new(), Some(e)),
                },
                Err(e) => (false, Vec::new(), Some(e.to_string())),
            }
        };
        out.push(McpImportSource {
            kind,
            path: path.display().to_string(),
            found,
            servers,
            error,
        });
    }
    Ok(out)
}

/// Import MCP servers parsed from an external tool config (Claude/Cursor/VSCode
/// or pasted JSON) into the current instance, enqueued as a backend job
/// (`JobPlan::McpImport`). The job dedupes via `add_mcp`'s id upsert and
/// recompiles `cordis.patch.yml` once.
#[tauri::command]
pub async fn mcp_import(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    request: McpImportRequest,
) -> Result<Job, AppError> {
    let (label, parsed) = if request.source == "raw" {
        let text = request
            .raw
            .clone()
            .ok_or_else(|| AppError::msg("raw import needs JSON text"))?;
        let list = parse_any(&text)
            .map_err(AppError::msg)?
            .ok_or_else(|| AppError::msg("no mcpServers / mcp.servers object found"))?;
        ("pasted JSON".to_string(), list)
    } else {
        let path = candidate_sources()
            .into_iter()
            .find(|(k, _)| k == &request.source)
            .map(|(_, p)| p)
            .ok_or_else(|| AppError::msg(format!("unknown MCP import source '{}'", request.source)))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| AppError::msg(format!("read {}: {e}", path.display())))?;
        let list = parse_for_source(&request.source, &text).map_err(AppError::msg)?;
        (request.source.clone(), list)
    };

    // Only records that actually carry a launch command/url are importable; a
    // parsed placeholder (no launch / no url) is dropped from the job — its
    // warning already told the user why.
    let subset: &[String] = &request.server_names;
    let importable: Vec<McpServerRecord> = parsed
        .into_iter()
        .filter(|m| !m.record.command.trim().is_empty() || !m.record.url.trim().is_empty())
        .filter(|m| subset.is_empty() || subset.iter().any(|n| n == &m.server_name))
        .map(|m| m.record)
        .collect();
    if importable.is_empty() {
        return Err(AppError::msg("no importable MCP servers in this config"));
    }

    enqueue_install(
        &state,
        &app,
        &id,
        &format!("import:{label}"),
        &format!("MCP import from {label}"),
        JobPlan::McpImport {
            source: label,
            servers: importable,
        },
    )
    .await
}

/// Durable body of [`mcp_import`]: add each parsed record (dedupe by id via
/// `InstanceManifest::add_mcp`'s upsert — `import:<serverName>` overwrites on
/// re-import, fresh and enabled), then recompile `cordis.patch.yml` **once**
/// from the whole manifest and refresh the Library snapshot.
pub(crate) async fn mcp_import_job(
    state: &AppState,
    app: &AppHandle,
    id: &str,
    source: &str,
    servers: &[McpServerRecord],
    ctx: &JobCtx,
) -> Result<(), AppError> {
    ensure_not_running(state, id).await?;
    emit_log(
        app,
        &format!(
            "{id} · importing MCP config {source} ({} server(s))…",
            servers.len()
        ),
    );

    let total = servers.len();
    for (idx, record) in servers.iter().enumerate() {
        InstanceManifest::add_mcp(&state.paths, id, record)?;
        emit_log(
            app,
            &format!(
                "{id} · imported MCP {} (transport {})",
                record.server_name, record.transport
            ),
        );
        if total > 0 {
            ctx.progress("recording", 60 + ((idx as i64 + 1) * 20 / total as i64));
        }
    }

    ctx.progress("recording", 82);
    let instance = InstanceManifest::get(&state.paths, id)?;
    content_adapter::sync_mcp_patch(&instance, &instance.mcp)?;
    ctx.progress("inventory-sync", 88);
    reconcile_library_inventory_after_market_change(state, app, id, "MCP import").await?;
    emit_log(
        app,
        &format!("{id} · MCP import from {source} done ({total} server(s))"),
    );
    Ok(())
}

/// A sink that forwards each line to Activity (like `make_sink`) and appends it
/// to a transcript buffer for the server's `logs/last.log`.
fn health_sink(app: AppHandle, buf: Arc<Mutex<String>>) -> LogSink {
    let forward = make_sink(app);
    Arc::new(move |log_line: LogLine| {
        if let Ok(mut text) = buf.lock() {
            text.push_str(&log_line.line);
            text.push('\n');
        }
        forward(log_line);
    })
}

/// Emit a health line through the capturing sink (so it lands in Activity and
/// the transcript, not just the log file).
fn line(sink: &LogSink, msg: &str) {
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: msg.to_string(),
    });
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

/// Land a just-installed plugin/skin **disabled** (install ≠ enable): a new
/// install must not auto-mount at the next boot. Enabling is the user's later
/// explicit act (launcher toggle → `plugin_toggle`).
///
/// - a **skin** is recorded `enabled: false`. A *bundle* skin — auto-registered
///   into `dsh.profile.bundles` by `dsh plugin add`, which would load it by
///   default — additionally gets `disabled:` rows so it stays off; a *non-bundle*
///   skin simply gets no insert row (`sync_skin_patch` also drops any stale row
///   from an earlier enable on reinstall).
/// - a **bundle plugin** likewise gets `disabled:` rows; a non-bundle plugin was
///   installed as a plain dependency (inert until an insert row exists) and
///   needs nothing.
pub(crate) fn land_install_disabled(
    state: &AppState,
    id: &str,
    key: &str,
    package: &str,
    kind: ContentKind,
) -> Result<(), AppError> {
    match kind {
        ContentKind::Theme => {
            let updated = InstanceManifest::add_skin_package(&state.paths, id, key, package)?;
            if content_adapter::skin_has_bundle(&updated, package) {
                disable_bundle_rows(&updated, package)?;
            } else {
                content_adapter::sync_skin_patch(&updated, &updated.skin_packages)?;
            }
        }
        _ => {
            // Plugins carry no instance record — their state lives purely in the
            // patch layer (`cordis.patch.yml`).
            let manifest = InstanceManifest::get(&state.paths, id)?;
            if content_adapter::skin_has_bundle(&manifest, package) {
                disable_bundle_rows(&manifest, package)?;
            }
        }
    }
    Ok(())
}

/// Write `disabled:` rows for a bundle package's inserted entry ids so its
/// auto-registered profile layer does not load at the next boot. A bundle whose
/// patch only reconfigures (inserts no rows) has nothing to disable — leave it.
fn disable_bundle_rows(instance: &InstanceManifest, package: &str) -> Result<(), AppError> {
    if dsh_adapter::DshAdapter::plugin_row_ids(instance, package).is_empty() {
        return Ok(());
    }
    dsh_adapter::DshAdapter::set_plugin_enabled(instance, package, false)?;
    Ok(())
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
            let mut install_target =
                resolve_plugin_install_target(state, app, id, &spec, Some(item)).await?;
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
                    &["add".to_string(), install_target.clone()],
                    sink,
                )
                .await?;
            if code != 0 {
                // A catalog `npm` name can be the author's *declared* package
                // name rather than one actually published to the registry
                // (e.g. `people-ai` on `95384/DSH-themes-people-ai` fails with
                // a pnpm 404 on every install). When the record also carries a
                // GitHub URL, retry once against that source before giving up
                // (plugins/skins only; skills/MCP route elsewhere). Reusing
                // `resolve_plugin_install_target` keeps the same cache-then-link
                // semantics as a native github-spec install.
                let fallback = item.github_spec().filter(|gh| gh != &spec);
                let second = if let Some(gh) = &fallback {
                    emit_log(
                        app,
                        &format!(
                            "{id} · {label} install via {spec} failed (code {code}) — retrying from GitHub {gh}"
                        ),
                    );
                    let gh_target =
                        resolve_plugin_install_target(state, app, id, gh, Some(item)).await?;
                    let retry_sink = ctx
                        .map(|c| c.sink())
                        .unwrap_or_else(|| make_sink(app.clone()));
                    let code2 = state
                        .adapter
                        .run_plugin_command(
                            settings,
                            instance,
                            &["add".to_string(), gh_target.clone()],
                            retry_sink,
                        )
                        .await?;
                    if code2 == 0 {
                        install_target = gh_target;
                    }
                    code2
                } else {
                    code
                };
                if second != 0 {
                    if let Some(ctx) = ctx {
                        ctx.set_exit_code(i64::from(second));
                    }
                    let tried = fallback
                        .map(|gh| format!(" (tried {spec} and {gh})"))
                        .unwrap_or_default();
                    return Err(AppError::msg(format!(
                        "dsh plugin add exited with code {second}{tried} — check the install spec resolves on npm or GitHub and your network can reach the source (detail in Activity logs)"
                    )));
                }
            }
            // Post-install loadability gate (ported from dsh-market's
            // validateAddedPlugins): `dsh plugin add` exits 0 even for a
            // source-only GitHub checkout — it only links the source directory
            // and never checks the package has a built entry. Writing the insert
            // row for such a package makes the NEXT boot die with
            // ERR_MODULE_NOT_FOUND (the tp7 skin family). If the just-added
            // package is not a bundle and ships no loadable entry artifact,
            // remove it now and fail the install — never leave it for boot.
            if let Some(pkg) = content_adapter::skin_package_name(std::path::Path::new(&install_target)) {
                if !content_adapter::installed_skin_loadable(instance, &pkg) {
                    emit_log(
                        app,
                        &format!(
                            "{id} · {label} {pkg} installed but has no loadable entry — removing to protect the next boot"
                        ),
                    );
                    let cleanup_sink = ctx
                        .map(|c| c.sink())
                        .unwrap_or_else(|| make_sink(app.clone()));
                    let _ = state
                        .adapter
                        .run_plugin_command(
                            settings,
                            instance,
                            &["remove".to_string(), pkg.clone()],
                            cleanup_sink,
                        )
                        .await;
                    // `dsh plugin remove` drops the manifest entry but on Windows
                    // pnpm leaves the top-level node_modules dir behind (observed
                    // with tp7). Prune it ourselves — `std::fs::remove_dir_all`
                    // never follows reparse points, so a junction to the source
                    // cache is removed as a link, target untouched.
                    let installed_dir = dsh_adapter::DshAdapter::profile_dir(instance)
                        .join("node_modules")
                        .join(&pkg);
                    let _ = std::fs::remove_dir_all(&installed_dir);
                    if let Some(ctx) = ctx {
                        ctx.set_exit_code(1);
                    }
                    return Err(AppError::msg(format!(
                        "installed but not loadable: {pkg} ships no built entry (a source-only checkout) — it was removed; install a published build instead"
                    )));
                }
            }
            if let Some(ctx) = ctx {
                ctx.progress("recording", 65);
            }
            // New plugin/skin installs land DISABLED — record (skins) + patch
            // state, but never auto-mount. Enabling is an explicit later toggle.
            if item.kind == ContentKind::Theme || item.kind == ContentKind::Plugin {
                let package = content_adapter::skin_package_name(std::path::Path::new(&install_target))
                    .unwrap_or_else(|| install_target.clone());
                land_install_disabled(state, id, &key, &package, item.kind)?;
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
    let settings = settings_snapshot(state)?;

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
    // MCPs are not "write the record" items: an install must carry real
    // download → probe semantics so the Library badge is a true state the moment
    // it lands (roadmap §8.4 / §11). `install_bundle_item`'s MCP branch only
    // records, which left every Market-installed MCP permanently `untested`
    // (no prefetch, no runtime.json). Route market MCP installs through the
    // dedicated job — registry download warm / Phase-4 git local build + the
    // post-install verification. Bundles that embed MCPs still use the generic
    // record-only path (`install_bundle_item`, dispatched from bundle import).
    if entry.kind == ContentKind::Mcp {
        return mcp_install_job(state, app, id, entry, ctx).await;
    }
    ensure_not_running(state, id).await?;
    let instance = InstanceManifest::get(&state.paths, id)?;
    let settings = settings_snapshot(state)?;

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
