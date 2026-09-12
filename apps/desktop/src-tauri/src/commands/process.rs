use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsh_adapter::safe_boot::{remove_safe_profile, MINIMAL_BUNDLES, SAFE_PROFILE_NAME};
use dsh_adapter::web_check::{check_root, WebVerdict};
use dsh_adapter::{SafeProfileVerdict, SafeTier};
use launcher_core::instance::InstanceManifest;
use launcher_core::process::{
    sweep_leftover, wait_for_port, PidLedger, ProcessState, ProcessStatus,
};
use launcher_core::{
    ErrorCode, ExitSink, LogLevel, LogLine, LogSink, LogStream, McpConfigStore, NewUsageRecord,
    RuntimeAdapter,
};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

use crate::commands::plugins::reconcile_library_inventory_after_market_change;
use crate::commands::settings::settings_snapshot;
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

const LOG_EVENT: &str = "logs";
const DSH_URL_EVENT: &str = "dsh-url";
const PROCESS_STATE_EVENT: &str = "process-state";
/// Payload is the settings namespace that changed (`ui-theme` | `locale`).
const SETTINGS_CHANGED_EVENT: &str = "dsh-settings-changed";
/// Emitted whenever a proxied response yields a usage record. Shared with
/// `usage_proxy`, and matched by the frontend listener in `appStore.ts`.
pub(crate) const USAGE_EVENT: &str = "usage-recorded";
/// A failed boot, diagnosed: payload is a [`LaunchDiagnosis`]. Emitted from the
/// crash sink and the degraded-boot branch, the two places a boot is known to
/// have gone wrong.
pub(crate) const LAUNCH_DIAGNOSIS_EVENT: &str = "launch-diagnosis";

/// Seconds between "still booting after Ns" progress hints while a launch sits
/// on the slow path. The hint is visible in Activity so a long cold boot reads
/// as "working" rather than "hung".
const BOOT_HINT_SECS: u64 = 15;
/// Seconds of *total silence* — no DSH output at all — before a slow boot is
/// declared degraded. A child that keeps printing (pnpm install progress, plugin
/// logs) is still making progress and is never timed out on wall-clock alone:
/// the clock is the last emitted line, not the launch click. Tradeoff inherited
/// from DSH-Launcher: a plugin stuck in an infinite print loop would evade this,
/// but that is rarer than the cold boot this saves.
const BOOT_SILENCE_SECS: u64 = 120;

/// How long the workspace-URL check may take before its verdict is "unreadable"
/// (see [`dsh_adapter::web_check`]). Generous for a loopback request that is
/// already accepting connections, short enough that a server which never
/// answers the root cannot stall a launch — the verdict fails open, so a longer
/// wait buys nothing.
const WEB_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Launch an instance's harness as a managed child, wait for DSH to report its
/// web URL, then show the UI in a launcher-owned DSH window. One instance runs
/// at a time: launching a different instance while one is up stops the old one
/// first. Same-instance relaunch is idempotent (returns current state).
#[tauri::command]
pub async fn launch(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<ProcessState, AppError> {
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Launch, || async {
        do_launch(&state, &app, id, None).await
    })
    .await
}

/// Start the instance's harness in safe mode at `tier` (see
/// [`dsh_adapter::safe_boot`]): generate the scratch profile, let dsh judge it,
/// then boot `--profile .ahl-safe`. A refusal from dsh is returned to the
/// caller rather than a boot that would fail the same way.
#[tauri::command]
pub async fn safe_launch(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    tier: SafeTier,
) -> Result<ProcessState, AppError> {
    // A user-initiated safe launch starts a fresh ladder: clear the once-only
    // escalation guard so this boot can climb again if it fails.
    state.safe_escalating.store(false, Ordering::SeqCst);
    let job_id = id.clone();
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Launch, || async {
        do_safe_launch(&state, &app, id, tier).await
    })
    .await
}

/// The pre-flight half of [`safe_launch`]: build the safe profile and let dsh
/// compose it before anything is stopped or booted. On a refusal, the user's
/// profile is untouched and dsh's own sentence is what the caller sees.
async fn do_safe_launch(
    state: &AppState,
    app: &AppHandle,
    id: String,
    tier: SafeTier,
) -> Result<ProcessState, AppError> {
    let settings = settings_snapshot(state)?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let mut current = tier;
    loop {
        let (plan, verdict) = state
            .adapter
            .prepare_safe_profile(&settings, &instance, current)
            .await
            .map_err(|e| AppError::coded(ErrorCode::LaunchFailed, e.to_string()))?;
        match verdict {
            SafeProfileVerdict::Composed { .. } => {
                emit_log(
                    app,
                    &format!(
                        "{id} · dsh composed the safe profile ({}){}",
                        tier_label(current),
                        if plan.dropped.is_empty() {
                            String::new()
                        } else {
                            format!("; not loaded this boot: {}", plan.dropped.join(", "))
                        }
                    ),
                );
                return do_launch(state, app, id, Some(current)).await;
            }
            SafeProfileVerdict::Refused { message, from_dsh } => {
                // Never boot a profile dsh just refused: the dump composes the
                // same layers the boot does, so this would fail again — but with
                // the advantage that dsh already said why, and we can quote it.
                emit_error(app, &AppError::msg(message.clone()));
                match current.next() {
                    Some(next) => {
                        // dsh could not resolve a Tier-1 row (usually a
                        // first-party plugin installed from npm, which the safe
                        // profile has no node_modules for). The minimal pair
                        // resolves from the installation anchor, so climb before
                        // giving up.
                        emit_log(
                            app,
                            &format!(
                                "{id} · dsh refused the safe profile at {} — climbing the ladder to {}…",
                                tier_label(current),
                                tier_label(next)
                            ),
                        );
                        current = next;
                    }
                    None => {
                        // The bottom of the ladder: no narrower profile exists.
                        return Err(AppError::msg(if from_dsh {
                            message
                        } else {
                            format!("the safe profile did not compose: {message}")
                        }));
                    }
                }
            }
        }
    }
}

/// The human name of a ladder rung, for log lines the user reads in Activity.
fn tier_label(tier: SafeTier) -> &'static str {
    match tier {
        SafeTier::Plugins => "L1 (first-party plugins)",
        SafeTier::Minimal => "L2 (minimal)",
    }
}

/// Climb the recovery ladder after a safe boot failed at `from`.
///
/// Only a Tier 1 failure climbs: [`SafeTier::next`] is the ladder's only
/// direction, and `None` is the bottom — the minimal pair did not boot either,
/// so nothing AHL generated was the cause, and the honest move is to say so
/// rather than keep trying. A normal (non-safe) boot never reaches here: the
/// recovery panel owns that path.
///
/// The `safe_escalating` guard makes the climb once-only, so a boot that both
/// degrades and then crashes cannot race two next-tier launches off the same
/// failure. `do_launch` clears it the moment a safe child is actually up, so the
/// *next* failure is a fresh opportunity.
fn escalate_safe_mode(app: &AppHandle, id: &str, from: Option<SafeTier>, stage: &'static str) {
    let Some(from) = from else {
        return;
    };
    let state = app.state::<AppState>();
    if state.safe_escalating.swap(true, Ordering::SeqCst) {
        return; // already climbing off this failure
    }
    match from.next() {
        Some(next) => {
            emit_log(
                app,
                &format!(
                    "{id} · safe mode {stage} at {} — climbing the ladder to {}…",
                    tier_label(from),
                    tier_label(next)
                ),
            );
            let app = app.clone();
            let id = id.to_string();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                // `do_safe_launch` emits its own failure; nothing more to say here.
                let _ = do_safe_launch(&state, &app, id, next).await;
            });
        }
        None => {
            emit_error(
                app,
                &AppError::msg(format!(
                    "{id} · safe mode reached the end of the ladder: even the minimal pair ({}) did not boot",
                    MINIMAL_BUNDLES.join(", ")
                )),
            );
        }
    }
}

async fn do_launch(
    state: &AppState,
    app: &AppHandle,
    id: String,
    safe_tier: Option<SafeTier>,
) -> Result<ProcessState, AppError> {
    let launch_start = Instant::now();
    {
        let mut guard = state.child.lock().await;

        // One-at-a-time: if something else is running, stop it (and close its DSH
        // window). `Starting` and `Degraded` both count as live — either way the
        // child process is up, so a second launch must not spawn a second tree.
        if let Some(running) = guard.as_ref() {
            let status = running.handle.state().status;
            if matches!(
                status,
                ProcessStatus::Running | ProcessStatus::Starting | ProcessStatus::Degraded
            ) {
                // A normal relaunch of the instance already running is a no-op.
                // Safe mode is not: its whole point is to stop the broken boot
                // and boot the scratch profile instead, so it must fall through
                // to the stop-and-switch below rather than return the live state.
                if running.instance_id == id && safe_tier.is_none() {
                    return Ok(running.handle.state());
                }
                let stop_note = if safe_tier.is_some() {
                    format!("Stopping {id} to start it in safe mode…")
                } else {
                    format!("Stopping {} to switch to {id}…", running.instance_id)
                };
                emit_log(app, &stop_note);
                if let Some(mut r) = guard.take() {
                    drop(guard);
                    let prev_pid = r.handle.pid;
                    if let Some(shutdown) = r.usage_proxy_shutdown.take() {
                        let _ = shutdown.send(());
                    }
                    if let Some(shutdown) = r.settings_watch_shutdown.take() {
                        let _ = shutdown.send(());
                    }
                    let _ = r.handle.stop().await;
                    // Switching instances stops the old tree the same way
                    // `do_stop` does, so its ledger row goes the same way —
                    // otherwise every switch leaves one behind.
                    PidLedger::open(state.paths.pid_ledger()).forget(prev_pid);
                    close_session(state, "stopped");
                    close_dsh_window(app);
                }
            }
        }
    }

    let settings = settings_snapshot(state)?;
    let instance = InstanceManifest::get(&state.paths, &id)?;
    // A normal boot returns the instance to its own profile, so it also clears
    // the scratch profile a previous safe boot left behind — leaving safe mode
    // needs no other undo. Safe mode itself skips this: it is about to rewrite
    // the scratch profile at its own tier, and removing it here would only race
    // that write.
    if safe_tier.is_none() {
        if let Err(e) = remove_safe_profile(&instance) {
            emit_warn(app, &format!("{id} · could not remove the safe profile: {e}"));
        }
    }
    let provider = state.vault.resolve(&instance.provider_ref)?;
    let mut env = state.adapter.build_env(&provider, &instance)?;
    // Fold each installed MCP server's configured env (key names on disk,
    // values in the OS credential store) into DSH's own environment, so the MCP
    // server children DSH spawns inherit them. The patch `config.env` stays
    // clean — configured secrets never land on disk in the patch.
    {
        let store = McpConfigStore::new(state.paths.clone());
        for record in instance.mcp.iter().filter(|r| r.enabled) {
            for (key, value) in store.resolve_env(&id, &record.id) {
                env.entry(key).or_insert(value); // DSH's own env wins on clashes
            }
        }
    }
    let fallback_model = provider
        .profile
        .model
        .clone()
        .or_else(|| provider.profile.models.first().cloned())
        .unwrap_or_else(|| "unknown".into());
    let upstream_base = provider
        .profile
        .base_url
        .as_deref()
        .filter(|base| !base.trim().is_empty())
        .unwrap_or("https://api.deepseek.com");
    let proxy_start = Instant::now();
    let (usage_proxy_base_url, mut usage_proxy) = match crate::usage_proxy::start(
        app.clone(),
        upstream_base.to_string(),
        provider.api_key.clone(),
        id.clone(),
        provider.profile.id.clone(),
        fallback_model.clone(),
    )
    .await
    {
        Ok(proxy) => {
            env.insert("DEEPSEEK_BASE_URL".into(), proxy.base_url.clone());
            emit_debug(
                app,
                &format!(
                    "{id} · usage proxy ready at {} (+{}ms)",
                    proxy.base_url,
                    proxy_start.elapsed().as_millis()
                ),
            );
            (Some(proxy.base_url), Some(proxy.shutdown))
        }
        Err(e) => {
            emit_warn(app, &format!("{id} · usage proxy unavailable: {e}"));
            (None, None)
        }
    };

    let session_id = state.history.record_start(&id)?;
    *state
        .session_id
        .lock()
        .map_err(|_| AppError::msg("session lock poisoned"))? = Some(session_id);

    // Tap the log sink for the `dsh web: http://127.0.0.1:<port>…` line DSH
    // prints once its server is up (some dsh builds append `/?token=…`).
    // With `--port 0` the port is dynamic, so the old fixed-3080 probe no
    // longer applies. The Activity stream is untouched.
    //
    // `last_output` is the adaptive-boot clock: bumped on every streamed line so
    // the slow path can tell "still printing, therefore still booting" apart
    // from "gone silent, therefore hung". `std::sync::Mutex` (not an async
    // mutex) because the writer is a plain `Fn` sink and the reader holds the
    // guard for a single `Instant::elapsed()` with no `.await` in between.
    let last_output = Arc::new(std::sync::Mutex::new(Instant::now()));
    // Kept for crash diagnosis if this boot fails (see `diagnose_and_emit`).
    let tail = LogTail::default();
    let (url_tx, mut url_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let base_sink = make_usage_sink(
        app.clone(),
        id.clone(),
        provider.profile.id.clone(),
        fallback_model,
    );
    let on_log: LogSink = {
        let tx = url_tx.clone();
        let last_output = last_output.clone();
        let tail = tail.clone();
        Arc::new(move |line: LogLine| {
            if let Ok(mut clock) = last_output.lock() {
                *clock = Instant::now();
            }
            tail.push(&line.line);
            base_sink(line.clone());
            if let Some(url) = parse_dsh_url(&line.line) {
                let _ = tx.send(url);
            }
        })
    };
    let on_exit: ExitSink = {
        let app = app.clone();
        let id = id.clone();
        let tail = tail.clone();
        Arc::new(move |process_state: ProcessState| {
            let _ = app.emit(PROCESS_STATE_EVENT, &process_state);
            // A clean stop is a user action, not a failure — nothing to diagnose.
            // Only a crash wants a reason, and this sink is the one place a crash
            // is recognised, so the rule table runs here and nowhere else.
            if process_state.status == ProcessStatus::Crashed {
                diagnose_and_emit(&app, &id, "crashed", &tail);
            }
            if !matches!(
                process_state.status,
                ProcessStatus::Crashed | ProcessStatus::Stopped
            ) {
                return;
            }
            let app = app.clone();
            let id = id.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let mut guard = state.child.lock().await;
                let pid = process_state.pid;
                let owns_child = guard.as_ref().is_some_and(|running| {
                    running.instance_id == id && Some(running.handle.pid) == pid
                });
                if !owns_child {
                    return;
                }
                let safe_tier = guard.as_ref().and_then(|running| running.safe_tier);
                if let Some(mut running) = guard.take() {
                    if let Some(shutdown) = running.usage_proxy_shutdown.take() {
                        let _ = shutdown.send(());
                    }
                }
                drop(guard);
                let status = if process_state.status == ProcessStatus::Crashed {
                    "crashed"
                } else {
                    "stopped"
                };
                close_session(&state, status);
                close_dsh_window(&app);
                // A safe boot that crashed climbs the ladder: Tier 1 tries the
                // minimal pair, Tier 2 is the bottom. A normal boot never
                // escalates — the recovery panel owns that path.
                if process_state.status == ProcessStatus::Crashed {
                    escalate_safe_mode(&app, &id, safe_tier, "crashed");
                }
            });
        })
    };

    // Startup zombie sweep: a tree recorded by a launcher that never got to
    // reap it (hard-killed / crashed) is still out there. Kill it now, before
    // spawning again. The ledger is shared by every launcher on this data root,
    // so the sweep only touches entries whose owner launcher is gone — a
    // *second, still-running* launcher's healthy tree is left alone.
    let ledger = PidLedger::open(state.paths.pid_ledger());
    let swept = sweep_leftover(&ledger);
    if !swept.is_empty() {
        // Name the instances: "a leftover tree was reaped" is only actionable
        // if the user knows which instance it came from.
        let whose = if swept.instances.is_empty() {
            String::new()
        } else {
            format!(" (instance: {})", swept.instances.join(", "))
        };
        emit_log(
            app,
            &format!(
                "Reaped {} leftover process tree(s){whose} from a previous session",
                swept.reaped
            ),
        );
    }

    // Pre-spawn quarantine: a leftover profile bundle that mounts a web client
    // but whose built entry is missing (an interrupted `dsh plugin add` that
    // never reached the install-time gate) would brick THIS boot with
    // ERR_MODULE_NOT_FOUND — DSH dies before the post-launch reconcile (which
    // only runs once DSH is up) can act. Disable its loader rows now; the
    // launch proceeds with the package off and the user removes it in Library.
    //
    // Safe mode skips this: it edits the user's profile (`cordis.patch.yml`),
    // and the one thing a safe boot must not do is touch the profile it is
    // booting around. The safe profile does not list the user's bundles anyway,
    // so there is nothing to quarantine.
    if safe_tier.is_none() {
        for pkg in dsh_adapter::content::quarantine_unloadable_client_bundles(&instance) {
            emit_warn(
                app,
                &format!(
                    "{id} · quarantined {pkg}: no built client entry — remove it in Library to uninstall"
                ),
            );
        }
    }

    let spawn_start = Instant::now();
    let profile = safe_tier.map(|_| SAFE_PROFILE_NAME);
    let handle = match state
        .adapter
        .launch_profile(&settings, &instance, &env, profile, on_log, Some(on_exit))
        .await
    {
        Ok(h) => h,
        Err(e) => {
            if let Some(shutdown) = usage_proxy.take() {
                let _ = shutdown.send(());
            }
            close_session(state, "crashed");
            let err = AppError::coded(classify_launch_error(&e), e.to_string());
            emit_error(app, &err);
            return Err(err);
        }
    };
    let pid = handle.pid;
    // A safe child is now up, so its own failure (if it comes) is a fresh
    // escalation opportunity: clear the once-only guard the climb set. Normal
    // boots skip this — they never read the guard.
    if safe_tier.is_some() {
        state.safe_escalating.store(false, Ordering::SeqCst);
    }
    ledger.record(&id, pid);
    emit_log(app, &format!("{id} · DSH web starting (pid {pid})…"));
    emit_debug(
        app,
        &format!("{id} · spawn took {}ms", spawn_start.elapsed().as_millis()),
    );

    // Wait for the ready URL line (or the process to die / the 20s ceiling).
    let url = match tokio::time::timeout(Duration::from_secs(20), url_rx.recv()).await {
        Ok(Some(url)) => Some(url),
        _ => None,
    };
    if url.is_some() {
        emit_debug(
            app,
            &format!(
                "{id} · DSH URL ready after {}ms",
                launch_start.elapsed().as_millis()
            ),
        );
    }

    // The process may have already died while we waited — reflect that.
    let died = matches!(
        handle.state().status,
        ProcessStatus::Crashed | ProcessStatus::Stopped
    );
    if died {
        let mut guard = state.child.lock().await;
        *guard = Some(crate::state::RunningChild {
            instance_id: id,
            handle,
            url: None,
            port: None,
            usage_proxy_shutdown: usage_proxy,
            settings_watch_shutdown: None,
            safe_tier,
        });
        let st = guard.as_ref().expect("just stored").handle.state();
        if st.status == ProcessStatus::Crashed {
            close_session(state, "crashed");
        }
        let _ = app.emit(PROCESS_STATE_EVENT, &st);
        return Ok(st);
    }

    let ready_url = url.clone();
    let port = ready_url.as_ref().and_then(|u| url_port(u));
    let settings_watch = match ready_url.as_deref() {
        Some(url) => {
            finalize_ready(
                app,
                &provider,
                &settings,
                &instance,
                &handle,
                url,
                usage_proxy_base_url.as_deref(),
                launch_start,
                safe_tier,
            )
            .await
        }
        None => {
            // The 20s ceiling is too short for a cold first boot (fresh `web`
            // profile materialize + pnpm install of its bundles can take over a
            // minute). Don't give up on the process — keep it running and hand
            // the rest of the wait to a background task that opens the DSH
            // window the moment the URL finally lands.
            //
            // Status deliberately stays `Starting` rather than flipping to
            // `Degraded`: the child is mid-boot, not degraded, and the UI reads
            // `degraded` as a failure badge — which is exactly the "it said it
            // failed, then came up on its own" flicker users reported. The
            // frontend shows a disabled "starting" button for as long as this
            // holds, and `finalize_ready` moves it to `Running` when the URL
            // lands.
            emit_log(
                app,
                &format!("{id} · DSH web did not report a URL within 20s — still booting, waiting in background…"),
            );
            let app = app.clone();
            let provider = provider.clone();
            let settings = settings.clone();
            let instance = instance.clone();
            let id_task = id.clone();
            let pid = handle.pid;
            let usage_proxy_base_url = usage_proxy_base_url.clone();
            let last_output = last_output.clone();
            let tail = tail.clone();
            // The slow path settles minutes later, so its total has to count from
            // the launch the user clicked, not from the URL landing.
            let boot_start = launch_start;
            tauri::async_runtime::spawn(async move {
                // Adaptive boot wait. No fixed wall-clock ceiling: a cold first
                // boot (profile materialize + pnpm install of its bundles) runs
                // for minutes while printing steadily. The clock is DSH's last
                // emitted line — while it keeps talking it is "still starting";
                // only `BOOT_SILENCE_SECS` of total silence marks it degraded.
                // Even after that the watcher stays alive, so a URL that finally
                // lands still flips the instance back to Running (self-healing).
                let mut hint = tokio::time::interval(Duration::from_secs(BOOT_HINT_SECS));
                hint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                hint.tick().await; // swallow the first (immediate) tick

                let mut degraded = false;
                loop {
                    tokio::select! {
                        url = url_rx.recv() => {
                            match url {
                                Some(url) => {
                                    let state = app.state::<AppState>();
                                    let mut guard = state.child.lock().await;
                                    if let Some(r) = guard.as_mut() {
                                        if r.handle.pid == pid {
                                            r.port = url_port(&url);
                                            r.url = Some(url.clone());
                                            r.settings_watch_shutdown = finalize_ready(
                                                &app,
                                                &provider,
                                                &settings,
                                                &instance,
                                                &r.handle,
                                                &url,
                                                usage_proxy_base_url.as_deref(),
                                                boot_start,
                                                safe_tier,
                                            )
                                            .await;
                                        }
                                    }
                                    return;
                                }
                                None => {
                                    // Channel closed: the child is gone, and the
                                    // `on_exit` sink owns crash/stop cleanup.
                                    return;
                                }
                            }
                        }
                        _ = hint.tick() => {
                            // No `.await` while the lock is held: read the clock
                            // into a plain `Duration` first.
                            let silent_for = last_output
                                .lock()
                                .map(|clock| clock.elapsed())
                                .unwrap_or_default();
                            if degraded {
                                // Already flagged — stay quiet; only the URL
                                // branch above can still rescue this boot.
                                continue;
                            }
                            if silent_for >= Duration::from_secs(BOOT_SILENCE_SECS) {
                                degraded = true;
                                let state = app.state::<AppState>();
                                let mut guard = state.child.lock().await;
                                if let Some(r) = guard.as_mut() {
                                    if r.handle.pid == pid {
                                        r.handle.set_status(ProcessStatus::Degraded);
                                    }
                                }
                                // Released before the diagnosis: that path reads
                                // `crash-signatures.json`, and no reader of the child
                                // registry should be blocked behind a file read.
                                drop(guard);
                                emit_coded(
                                    &app,
                                    ErrorCode::BootTimedOut,
                                    format!(
                                        "{id_task} · no DSH output for {}s — degraded, still watching for a late startup",
                                        BOOT_SILENCE_SECS
                                    ),
                                );
                                // Silence this long is usually a boot that already
                                // printed why it gave up. Run the rule table now
                                // rather than waiting for the watcher to exit: the
                                // child is still alive, so the crash sink may never
                                // fire, and the user is staring at "degraded".
                                diagnose_and_emit(&app, &id_task, "degraded", &tail);
                                // A safe boot that went silent has the same answer
                                // as one that crashed: climb the ladder. A normal
                                // boot leaves the degraded child alone — it may
                                // still self-heal the moment its URL lands.
                                escalate_safe_mode(&app, &id_task, safe_tier, "degraded");
                            } else {
                                emit_log(
                                    &app,
                                    &format!(
                                        "{id_task} · still booting after {}s (last output {}s ago)…",
                                        boot_start.elapsed().as_secs(),
                                        silent_for.as_secs()
                                    ),
                                );
                            }
                        }
                    }
                }
            });
            None
        }
    };
    let mut guard = state.child.lock().await;
    *guard = Some(crate::state::RunningChild {
        instance_id: id,
        handle,
        url: ready_url,
        port,
        usage_proxy_shutdown: usage_proxy,
        settings_watch_shutdown: settings_watch,
        safe_tier,
    });
    let st = guard.as_ref().expect("just stored").handle.state();
    let _ = app.emit(PROCESS_STATE_EVENT, &st);
    Ok(st)
}

/// Stop the managed harness process and close its history row as `stopped`.
#[tauri::command]
pub async fn stop(state: State<'_, AppState>, app: AppHandle) -> Result<ProcessState, AppError> {
    do_stop(&state, &app).await
}

/// Bring the DSH window to the front (the Overview "Open DSH" action). The
/// window lives exactly as long as the harness runs — closing it stops DSH —
/// so while a process is up the window exists; just show + focus it.
#[tauri::command]
pub fn open_dsh(app: AppHandle) -> Result<(), AppError> {
    if let Some(window) = app.get_webview_window(DSH_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }
    Ok(())
}

/// The URL of the running DSH workspace, if the process has reported one.
#[tauri::command]
pub async fn current_dsh_url(state: State<'_, AppState>) -> Result<Option<String>, AppError> {
    let guard = state.child.lock().await;
    Ok(guard.as_ref().and_then(|r| r.url.clone()))
}

/// Open the running DSH workspace in a separate window as an escape hatch. The
/// primary Workspace mode lives inside the launcher window.
#[tauri::command]
pub async fn open_dsh_external(state: State<'_, AppState>, app: AppHandle) -> Result<(), AppError> {
    let (id, url) = {
        let guard = state.child.lock().await;
        let Some(running) = guard.as_ref() else {
            return Ok(());
        };
        let Some(url) = running.url.clone() else {
            return Ok(());
        };
        (running.instance_id.clone(), url)
    };
    let instance = InstanceManifest::get(&state.paths, &id)?;
    open_dsh_window(&app, &url, &instance.name)
}

/// Current process state (polled by the UI status dot). Also reconciles
/// history: if the child ended on its own (crash) but its session row was never
/// closed, close it now — the 1.5s poll bounds the gap.
#[tauri::command]
pub async fn process_state(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ProcessState, AppError> {
    let mut guard = state.child.lock().await;
    let result = match guard.as_ref() {
        Some(running) => running.handle.state(),
        None => ProcessState::stopped(),
    };
    if matches!(
        result.status,
        ProcessStatus::Crashed | ProcessStatus::Stopped
    ) {
        if close_session(&state, "crashed") {
            // Only drop the handle when we actually owned the session.
            if let Some(mut running) = guard.take() {
                // The tree ended on its own, so its ledger row has nothing left
                // to record — see `PidLedger::forget`.
                PidLedger::open(state.paths.pid_ledger()).forget(running.handle.pid);
                if let Some(shutdown) = running.usage_proxy_shutdown.take() {
                    let _ = shutdown.send(());
                }
                if let Some(shutdown) = running.settings_watch_shutdown.take() {
                    let _ = shutdown.send(());
                }
            }
        }
        close_dsh_window(&app);
    }
    Ok(result)
}

/// Which instance is currently running (if any).
#[tauri::command]
pub async fn running_instance(state: State<'_, AppState>) -> Result<Option<String>, AppError> {
    let guard = state.child.lock().await;
    Ok(guard.as_ref().map(|r| r.instance_id.clone()))
}

/// The safe-mode tier the running child was booted at, if it was a safe boot.
/// The frontend reads this to show the "safe mode" banner, and to know the
/// next normal launch must clear the scratch profile.
#[tauri::command]
pub async fn running_safe_tier(state: State<'_, AppState>) -> Result<Option<SafeTier>, AppError> {
    let guard = state.child.lock().await;
    Ok(guard.as_ref().and_then(|r| r.safe_tier))
}

/// Stop the managed harness (if any) and close its DSH window. Shared by the
/// `stop` command and the DSH window's own close button.
async fn do_stop(state: &AppState, app: &AppHandle) -> Result<ProcessState, AppError> {
    let mut running = {
        let mut guard = state.child.lock().await;
        guard.take()
    };
    if let Some(mut running) = running.take() {
        emit_log(app, &format!("Stopping {}…", running.instance_id));
        if let Some(shutdown) = running.usage_proxy_shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(shutdown) = running.settings_watch_shutdown.take() {
            let _ = shutdown.send(());
        }
        let pid = running.handle.pid;
        let _ = running.handle.stop().await;
        // `stop` awaited the watcher, so the tree is down — the ledger row has
        // done its job and would otherwise sit there until some future session
        // swept it.
        PidLedger::open(state.paths.pid_ledger()).forget(pid);
        close_session(state, "stopped");
        close_dsh_window(app);
    }
    let stopped = ProcessState::stopped();
    let _ = app.emit(PROCESS_STATE_EVENT, &stopped);
    Ok(stopped)
}

/// Pull the `http://127.0.0.1:<port>/…` URL out of a DSH stdout line — the
/// `dsh web: http://127.0.0.1:<port>…` announcement printed on boot (newer
/// dsh builds append `/?token=…`; older ones print the bare URL).
fn parse_dsh_url(line: &str) -> Option<String> {
    const PREFIX: &str = "http://127.0.0.1:";
    let start = line.find(PREFIX)? + PREFIX.len();
    let after = &line[start..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let path = after[digits.len()..]
        .split_whitespace()
        .next()
        .unwrap_or("");
    Some(format!("{PREFIX}{digits}{path}"))
}

/// The port component of a DSH URL (for the readiness probe).
fn url_port(url: &str) -> Option<u16> {
    url.parse::<tauri::Url>().ok()?.port()
}

/// Label of the DSH webview window (a second window in this app).
const DSH_WINDOW_LABEL: &str = "dsh";

/// Show DSH's UI in a launcher-owned window at `url`, replacing any stale one.
/// Closing that window stops the harness — the UI lives only there, the same
/// contract dsh-tauri gives its own window.
fn open_dsh_window(app: &AppHandle, url: &str, instance_name: &str) -> Result<(), AppError> {
    close_dsh_window(app);
    let web_url = tauri::WebviewUrl::External(
        url.parse::<tauri::Url>()
            .map_err(|e| AppError::msg(format!("invalid DSH url `{url}`: {e}")))?,
    );
    let window = tauri::WebviewWindowBuilder::new(app, DSH_WINDOW_LABEL, web_url)
        .title(format!("{instance_name} · DSH"))
        // Same default size as the launcher main window.
        .inner_size(1280.0, 810.0)
        .min_inner_size(900.0, 640.0)
        .build()
        .map_err(|e| AppError::msg(format!("failed to open DSH window: {e}")))?;

    let app = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { .. } = event {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let _ = do_stop(&state, &app).await;
            });
        }
    });
    Ok(())
}

/// Close the DSH window if it's open (stop/crash/switch paths).
fn close_dsh_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(DSH_WINDOW_LABEL) {
        let _ = window.close();
    }
}

/// Close the open history session (if any) and clear it. Returns whether one
/// was open.
fn close_session(state: &AppState, status: &str) -> bool {
    let mut guard = match state.session_id.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    match guard.take() {
        Some(sid) => {
            let _ = state.history.record_end(sid, status, None);
            true
        }
        None => false,
    }
}

pub(crate) fn make_sink(app: AppHandle) -> LogSink {
    Arc::new(move |line: LogLine| {
        // Both destinations outlive the process (Activity panel + rolling log
        // file), and DSH's ready line carries the web token — mask before
        // either. The caller still holds the raw line for URL parsing.
        let line = line.redacted();
        if line.stream == LogStream::Stderr {
            tracing::warn!(target: "dsh", "{}", line.line);
        } else {
            tracing::info!(target: "dsh", "{}", line.line);
        }
        let _ = app.emit(LOG_EVENT, &line);
    })
}

fn make_usage_sink(
    app: AppHandle,
    instance_id: String,
    api_key_alias: String,
    fallback_model: String,
) -> LogSink {
    Arc::new(move |line: LogLine| {
        // Parse the *raw* line: a usage record legitimately carries fields like
        // `total_tokens`, and masking before parsing would corrupt the number.
        if let Some(record) =
            parse_usage_record(&line.line, &instance_id, &api_key_alias, &fallback_model)
        {
            let state = app.state::<AppState>();
            if let Ok(Some(saved)) = state.usage.record(record) {
                let _ = app.emit(USAGE_EVENT, &saved);
            }
        }
        // Only what leaves the process gets masked.
        let line = line.redacted();
        if line.stream == LogStream::Stderr {
            tracing::warn!(target: "dsh", "{}", line.line);
        } else {
            tracing::info!(target: "dsh", "{}", line.line);
        }
        let _ = app.emit(LOG_EVENT, &line);
    })
}

fn parse_usage_record(
    line: &str,
    instance_id: &str,
    api_key_alias: &str,
    fallback_model: &str,
) -> Option<NewUsageRecord> {
    parse_usage_json(line, instance_id, api_key_alias, fallback_model)
        .or_else(|| parse_usage_tokens(line, instance_id, api_key_alias, fallback_model))
}

fn parse_usage_json(
    line: &str,
    instance_id: &str,
    api_key_alias: &str,
    fallback_model: &str,
) -> Option<NewUsageRecord> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    if end <= start {
        return None;
    }
    let value: Value = serde_json::from_str(&line[start..=end]).ok()?;
    let usage = value.get("usage").unwrap_or(&value);
    let input = first_u64(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "inputTokens",
            "promptTokens",
        ],
    )?;
    let output = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "outputTokens",
            "completionTokens",
        ],
    )?;
    let total = first_u64(usage, &["total_tokens", "totalTokens"]).unwrap_or(input + output);
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let request_id = value
        .get("id")
        .or_else(|| value.get("request_id"))
        .or_else(|| value.get("requestId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let cost = value
        .get("cost")
        .or_else(|| usage.get("cost"))
        .and_then(Value::as_f64);
    Some(NewUsageRecord {
        instance_id: instance_id.to_string(),
        timestamp: None,
        model,
        input_tokens: input,
        output_tokens: output,
        total_tokens: Some(total),
        cost,
        api_key_alias: api_key_alias.to_string(),
        request_id,
    })
}

fn parse_usage_tokens(
    line: &str,
    instance_id: &str,
    api_key_alias: &str,
    fallback_model: &str,
) -> Option<NewUsageRecord> {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("token") {
        return None;
    }
    let input = number_after_any(
        &lower,
        &[
            "input_tokens",
            "prompt_tokens",
            "input tokens",
            "prompt tokens",
        ],
    )?;
    let output = number_after_any(
        &lower,
        &[
            "output_tokens",
            "completion_tokens",
            "output tokens",
            "completion tokens",
        ],
    )?;
    let total =
        number_after_any(&lower, &["total_tokens", "total tokens"]).unwrap_or(input + output);
    Some(NewUsageRecord {
        instance_id: instance_id.to_string(),
        timestamp: None,
        model: fallback_model.to_string(),
        input_tokens: input,
        output_tokens: output,
        total_tokens: Some(total),
        cost: None,
        api_key_alias: api_key_alias.to_string(),
        request_id: None,
    })
}

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn number_after_any(line: &str, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| number_after(line, key))
}

fn number_after(line: &str, key: &str) -> Option<u64> {
    let idx = line.find(key)? + key.len();
    let tail = &line[idx..];
    let digits: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Stage timings for one boot, from launch to fully-configured.
///
/// Every line is debug: Activity's default view holds the user-facing milestones
/// (spawn, URL ready, web ready) and nothing else, while the segments that
/// explain *why* a start felt slow — the port settle, the usage-proxy inject,
/// the inventory sync, the catalog/theme/language stamps — are recorded here and
/// closed by one total. Without that total there was no way to tell a slow boot
/// from a slow first paint of it.
#[derive(Default)]
struct BootClock {
    started: Option<Instant>,
    stages: Vec<(&'static str, u128)>,
}

impl BootClock {
    /// Start counting from the launch that owns this boot (not from the point
    /// the URL landed), so the total covers everything the user waited for.
    fn since(started: Instant) -> Self {
        BootClock {
            started: Some(started),
            stages: Vec::new(),
        }
    }

    /// Record one finished stage. Each is also logged on its own as it lands, so
    /// a boot that never settles still shows how far it got.
    fn record(&mut self, stage: &'static str, ms: u128) -> u128 {
        self.stages.push((stage, ms));
        ms
    }

    fn summary(&self) -> String {
        let total = self
            .started
            .map(|s| s.elapsed().as_millis())
            .unwrap_or_default();
        let stages = self
            .stages
            .iter()
            .map(|(name, ms)| format!("{name} {ms}ms"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("boot settled {total}ms after launch ({stages})")
    }
}

/// How many trailing child-output lines are kept for crash diagnosis.
///
/// The signature that explains a failed boot (`cannot resolve profile bundle …`)
/// is printed as the boot aborts, so it lands near the end; keeping the tail is
/// enough and bounds the buffer at a few tens of KB per launch. A cold boot's
/// pnpm progress can easily exceed this, which is fine — those lines are noise
/// to the rule table.
const LOG_TAIL_CAP: usize = 400;

/// The trailing child output of a launch, for [`diagnose_crash`].
///
/// Lines are **redacted as they are captured**, not when they are reported, so
/// everything derived from the tail — plugin names, message excerpts — is
/// already safe to put on an event payload. `emit_log_at` redacts for the same
/// reason; an event carrying a diagnosis would otherwise be the one path out
/// that skips it. `std::sync::Mutex` like `last_output`: held for a push or a
/// clone, never across an `.await`.
#[derive(Clone, Default)]
struct LogTail(Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl LogTail {
    fn push(&self, line: &str) {
        let Ok(mut tail) = self.0.lock() else {
            return; // poisoned by another task's panic; diagnosis is best-effort
        };
        if tail.len() == LOG_TAIL_CAP {
            tail.pop_front();
        }
        tail.push_back(launcher_core::redact_secrets(line).into_owned());
    }

    /// The tail in original order, for the rule table.
    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Diagnose a boot that went wrong and emit it for the UI. Best-effort: a boot
/// whose log matches no rule emits nothing rather than a hollow "failed" state.
///
/// `stage` names where the failure was noticed, because the two callers mean
/// different things by it — `crashed` is a dead child, `degraded` is one still
/// running but silent. The rules themselves are the same either way.
///
/// The user's `crash-signatures.json` is re-read here rather than cached at
/// startup: this runs once per failed boot, and re-reading means a signature
/// added *because* of this boot applies to the next one without a launcher
/// restart. A missing or malformed file contributes nothing (see
/// [`dsh_adapter::crash::load_signatures`]) — the built-ins still diagnose.
fn diagnose_and_emit(app: &AppHandle, id: &str, stage: &'static str, tail: &LogTail) {
    let extras =
        dsh_adapter::crash::load_signatures(&app.state::<AppState>().paths.crash_signatures);
    let issues = dsh_adapter::crash::diagnose_crash_with(&tail.lines(), &extras);
    // Dumped before the empty-issues return, not after: a failure that matches no
    // rule is the one where the archive is the only evidence there is, and the
    // rule table is exactly the thing that cannot help there.
    dump_diagnostic_package(app, id, stage, &issues, tail);
    if issues.is_empty() {
        emit_debug(
            app,
            &format!("{id} · boot {stage}: log matched no known failure pattern"),
        );
        return;
    }
    for issue in &issues {
        emit_warn(app, &format!("{id} · {stage}: {}", issue.message));
    }
    let _ = app.emit(
        LAUNCH_DIAGNOSIS_EVENT,
        LaunchDiagnosis {
            instance_id: id.to_string(),
            stage: stage.to_string(),
            issues,
        },
    );
}

/// Packages kept per instance before the oldest are removed.
///
/// A boot that fails into a retry loop fails every few seconds, and each dump is
/// a few hundred KB of profile state plus a 2000-line log tail. The last few
/// describe one incident as well as the first fifty do.
const KEEP_DIAGNOSTIC_PACKAGES: usize = 5;

/// Write the package `export_diagnostics` writes, without a frontend.
///
/// Ported from `1/`'s ADR-023 auto-dump. The manual export can only describe a
/// crash to someone who was already looking at the window; this puts the same
/// evidence on disk when the boot dies, so "it crashed last night" is still
/// answerable in the morning.
///
/// Best-effort throughout: every step returns on failure rather than
/// propagating. This runs on the crash sink, and a launcher that failed to write
/// a diagnostic package *about* a failure must not turn it into two failures.
fn dump_diagnostic_package(
    app: &AppHandle,
    id: &str,
    stage: &str,
    issues: &[dsh_adapter::crash::CrashIssue],
    tail: &LogTail,
) {
    let state = app.state::<AppState>();
    let Ok(settings) = settings_snapshot(&state) else {
        return;
    };
    let Ok(instance) = InstanceManifest::get(&state.paths, id) else {
        return;
    };

    // The Activity panel is not reachable from here, so its contents are
    // reconstructed from what this path does have. The launcher's own findings
    // come first — they are the reading of the child output that follows — and
    // are levelled `warn` to match the `emit_warn` that reported them; the tail
    // is levelled `error` because on this path it is the output of a boot that
    // died, not because every line in it is one.
    let mut activity: Vec<crate::commands::diagnose::ActivityLine> = issues
        .iter()
        .map(|issue| crate::commands::diagnose::ActivityLine {
            level: "warn".into(),
            line: format!("launcher diagnosis (boot {stage}): {}", issue.message),
        })
        .collect();
    activity.extend(
        tail.lines()
            .into_iter()
            .map(|line| crate::commands::diagnose::ActivityLine {
                level: "error".into(),
                line,
            }),
    );

    let Ok(package) = crate::commands::diagnose::collect(&state, &settings, &instance, &activity)
    else {
        return;
    };
    let Ok(bytes) = crate::commands::diagnose::write_package(&package.files) else {
        return;
    };
    let dir = state.paths.diagnostics_dir(id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // One dump per second, and a second attempt inside the same second lands on
    // the same name: that is the same incident, so overwriting loses nothing the
    // first write did not already describe.
    let name = format!("boot-{stage}-{}.zip", launcher_core::now_secs());
    if std::fs::write(dir.join(&name), bytes).is_err() {
        return;
    }
    prune_diagnostic_packages(&dir, KEEP_DIAGNOSTIC_PACKAGES);
    emit_debug(
        app,
        &format!("{id} · boot {stage}: diagnostic package written to {name}"),
    );
}

/// Keep the newest `keep` packages in `dir`; remove the rest.
///
/// Ordered by the epoch the file name encodes, not by directory order or mtime:
/// the name is what the launcher wrote, and a directory listing carries no
/// promise of order. A file whose name does not parse as one of ours is left
/// alone — this prunes what it created, and deleting an unrecognised file in a
/// retention pass would be the wrong way to find out what it was.
fn prune_diagnostic_packages(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut packages: Vec<(u64, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| package_epoch(&path).map(|at| (at, path)))
        .collect();
    if packages.len() <= keep {
        return;
    }
    packages.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in packages.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// The epoch a dump's file name encodes (`boot-<stage>-<secs>.zip`), if it is
/// one of ours.
fn package_epoch(path: &Path) -> Option<u64> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".zip")?;
    stem.strip_prefix("boot-")?.rsplit('-').next()?.parse().ok()
}

/// Payload of [`LAUNCH_DIAGNOSIS_EVENT`], paired with `RescueStatus` in the UI so
/// a diagnosed crash and the restore that fixes it arrive together.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchDiagnosis {
    pub instance_id: String,
    /// `crashed` (child died) or `degraded` (silent but alive).
    pub stage: String,
    pub issues: Vec<dsh_adapter::crash::CrashIssue>,
}

/// The server is up: settle on the port, stamp the launcher theme + model
/// catalog into the harness, mark it running, and show it in a DSH window.
/// Shared by the fast path and the slow-boot background continuation.
#[allow(clippy::too_many_arguments)] // the launch it finalizes: app/provider/settings/instance/handle/url/proxy/clock, each genuinely distinct
async fn finalize_ready(
    app: &AppHandle,
    provider: &launcher_core::ResolvedProvider,
    settings: &launcher_core::AppSettings,
    instance: &launcher_core::InstanceManifest,
    handle: &launcher_core::process::ChildHandle,
    url: &str,
    usage_proxy_base_url: Option<&str>,
    boot_start: Instant,
    safe_tier: Option<SafeTier>,
) -> Option<oneshot::Sender<()>> {
    let mut clock = BootClock::since(boot_start);
    let port = url_port(url);
    if let Some(port) = port {
        // The URL is announced before the socket accepts, so this wait is part
        // of the boot and is worth naming when it is what took the time.
        let settle_start = Instant::now();
        let _ = wait_for_port(port, Duration::from_secs(5)).await;
        let settle_ms = clock.record("port settle", settle_start.elapsed().as_millis());
        if settle_ms > 250 {
            emit_debug(
                app,
                &format!("{} · web port accepted after {settle_ms}ms", instance.id),
            );
        }
    }
    // An open port is not an answerable URL: dsh can refuse the very URL it
    // printed (that URL's token is not the one the server is authenticating
    // with) while the port accepts perfectly, which is how a boot used to be
    // reported ready with the window showing an authentication notice. Ask the
    // URL itself and let its answer decide what this boot may claim.
    //
    // Short timeout on purpose: the verdict fails open anyway, so waiting longer
    // would only delay a boot for a signal that cannot change the outcome.
    let verdict = port.map(|_| check_root(url, WEB_CHECK_TIMEOUT));
    let verdict = match verdict {
        Some(pending) => Some(pending.await),
        None => None, // no port in the ready line: nothing to ask
    };
    let refused = matches!(&verdict, Some(v) if v.refused());
    handle.set_status(ProcessStatus::Running);
    match &verdict {
        Some(WebVerdict::Refused { status, detail }) => {
            emit_warn(
                app,
                &format!(
                    "{} · dsh refused the URL it printed — HTTP {status}: {detail}",
                    instance.id
                ),
            );
            // dsh is up and the profile booted; what failed is the workspace
            // URL, so this is a diagnosis rather than a failed launch. The
            // process is left running: restarting it would not change which
            // token its server authenticates with, and stopping it would take
            // the user's harness down over a page they may still be able to
            // open (a saved session cookie covers a refused token).
            let _ = app.emit(
                LAUNCH_DIAGNOSIS_EVENT,
                LaunchDiagnosis {
                    instance_id: instance.id.clone(),
                    stage: "refused".to_string(),
                    issues: vec![dsh_adapter::crash::web_auth_refused(detail)],
                },
            );
        }
        other => {
            if let Some(WebVerdict::Unreadable { detail }) = other {
                emit_debug(
                    app,
                    &format!("{} · workspace URL unclassified: {detail}", instance.id),
                );
            }
            emit_log(app, &format!("{} · DSH web ready at {url}", instance.id));
        }
    }
    let _ = app.emit(DSH_URL_EVENT, url.to_string());
    // The profile files that got here boot, so they are the ones worth restoring
    // to — refresh the instance's rescue point. A failed boot never reaches this
    // point, which is what makes the pairing with `reserve_rescue_point` work.
    //
    // This sits behind the URL check above, which is what absorb-plan 1.4 asked
    // for, and the reason is now sharper than "the page might not have
    // rendered": a refusal means the workspace was never usable at that URL, and
    // a rescue point is supposed to be a state worth going back to.
    //
    // What the check still cannot see, and why this refresh is fail-open on
    // `Unreadable`: reachability and token acceptance are all that can be
    // measured from here. A client bundle that serves the root but renders an
    // error panel is invisible — the launcher has no DOM access to a dsh page
    // (`App.tsx` renders it in a cross-origin iframe), so that class of failure
    // stays where it was: diagnosed from the log when it also breaks the boot
    // (`crash.rs` rule 7), silent when it does not.
    if refused {
        emit_debug(
            app,
            &format!(
                "{} · rescue point left as it was: this boot never served its workspace URL",
                instance.id
            ),
        );
    } else if safe_tier.is_some() {
        // A safe boot reached a serving URL, but that proves the scratch
        // profile boots — not the user's. Refresh would capture the (still
        // broken) user profile as last-known-good, so the point is left alone:
        // the next normal boot is the one that may refresh it.
        emit_debug(
            app,
            &format!(
                "{} · rescue point left as it was: safe mode does not validate the user's profile",
                instance.id
            ),
        );
    } else {
        crate::commands::rescue::refresh_rescue_point(&app.state::<AppState>(), app, &instance.id);
    }
    // Subscribe to DSH's host SSE stream so an appearance/language change made
    // inside the DSH window reaches the launcher immediately (no poll lag). The
    // sender is handed to the caller to cancel on stop.
    let mut settings_watch = None;
    if let Some(port) = port {
        let (tx, rx) = oneshot::channel::<()>();
        settings_watch = Some(tx);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let emitter = app.clone();
            let res = dsh_adapter::events::watch_settings_changes(port, rx, move |ns| {
                let _ = emitter.emit(SETTINGS_CHANGED_EVENT, ns.to_string());
            })
            .await;
            if let Err(e) = res {
                emit_debug(&app, &format!("settings watch ended: {e:#}"));
            }
        });
    }
    if let Some(port) = port {
        let app = app.clone();
        let provider = provider.clone();
        let settings = settings.clone();
        let instance = instance.clone();
        let usage_proxy_base_url = usage_proxy_base_url.map(str::to_string);
        tauri::async_runtime::spawn(async move {
            // Keep first paint quiet: usage proxy is required for token capture,
            // while cosmetic/catalog maintenance can wait until DSH has hydrated.
            if let Some(base_url) = usage_proxy_base_url.as_deref() {
                let inject_start = Instant::now();
                match dsh_adapter::llm::set_base_url(port, base_url).await {
                    Ok(()) => {
                        let inject_ms =
                            clock.record("proxy inject", inject_start.elapsed().as_millis());
                        emit_debug(
                            &app,
                            &format!(
                                "{} · usage proxy injected into DSH settings in {inject_ms}ms",
                                instance.id
                            ),
                        );
                    }
                    Err(e) => emit_warn(
                        &app,
                        &format!("{} · usage proxy settings sync failed: {e}", instance.id),
                    ),
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;

            let state = app.state::<AppState>();
            let job_id = instance.id.clone();
            let sync_start = Instant::now();
            let result = run_instance_job(
                &state,
                &app,
                &job_id,
                HeavyJobKind::InventorySync,
                || async {
                    // Full sync on every launch: live refresh (DSH is up) plus a
                    // disk rebuild, so stale snapshot rows (removed skins, or a
                    // live row shadowing a launcher-managed one) never survive
                    // past the next normal start — no manual refresh needed.
                    reconcile_library_inventory_after_market_change(
                        &state,
                        &app,
                        &instance.id,
                        "launch",
                    )
                    .await
                },
            )
            .await;
            if let Err(e) = result {
                emit_warn(
                    &app,
                    &format!("{} · DSH inventory cache refresh failed: {e}", instance.id),
                );
            } else {
                let sync_ms = clock.record("inventory sync", sync_start.elapsed().as_millis());
                emit_debug(
                    &app,
                    &format!("{} · inventory sync took {sync_ms}ms", instance.id),
                );
            }

            tokio::time::sleep(Duration::from_secs(3)).await;

            let models_start = Instant::now();
            if !provider.profile.models.is_empty() {
                if let Err(e) = dsh_adapter::llm::set_models(port, &provider.profile.models).await {
                    emit_warn(
                        &app,
                        &format!("{} · model catalog sync failed: {e}", instance.id),
                    );
                }
            }
            clock.record("model catalog", models_start.elapsed().as_millis());

            let theme_start = Instant::now();
            if let Some(launcher_theme) = settings.theme.as_deref() {
                if launcher_theme != "system" {
                    match dsh_adapter::theme::get_preference(port).await {
                        Ok(Some(pref)) if pref != "system" => {}
                        _ => {
                            let _ = dsh_adapter::theme::set_preference(port, launcher_theme).await;
                        }
                    }
                }
            }
            clock.record("theme", theme_start.elapsed().as_millis());

            let lang_start = Instant::now();
            if let Some(lang) = settings.language.as_deref() {
                match dsh_adapter::language::get_preference(port).await {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = dsh_adapter::language::set_preference(port, lang).await;
                    }
                }
            }
            clock.record("language", lang_start.elapsed().as_millis());

            // The one line that answers "how long did startup really take, and
            // which stage was it". The two sleeps above are deliberate pacing
            // (let DSH hydrate before we touch its settings), so they are part of
            // the total and the segments are what stays actionable.
            emit_debug(&app, &format!("{} · {}", instance.id, clock.summary()));
        });
    }
    settings_watch
}

pub(crate) fn emit_log(app: &AppHandle, line: &str) {
    emit_log_at(app, line, LogLevel::Info);
}

/// Low-signal bookkeeping (proxy inject, inventory sync, queue progress) —
/// hidden from Activity's default view, kept for debugging.
pub(crate) fn emit_debug(app: &AppHandle, line: &str) {
    emit_log_at(app, line, LogLevel::Debug);
}

pub(crate) fn emit_warn(app: &AppHandle, line: &str) {
    emit_log_at(app, line, LogLevel::Warn);
}

pub(crate) fn emit_log_at(app: &AppHandle, line: &str, level: LogLevel) {
    let _ = app.emit(
        LOG_EVENT,
        LogLine {
            stream: LogStream::Stdout,
            level,
            // Launcher-authored messages are not exempt: a ready-URL or an MCP
            // env value interpolated into one would leak the same way.
            line: launcher_core::redact_secrets(line).into_owned(),
        },
    );
}

/// Emit a coded failure to the Activity log as `[E2001] message`, so the same
/// code the banner shows also lands in the log the user is pointed at.
pub(crate) fn emit_error(app: &AppHandle, error: &crate::error::AppError) {
    let e = error.coded_error();
    emit_log_at(app, &e.log_line(), LogLevel::Error);
}

/// Emit a coded failure without building an `AppError` (e.g. the degraded-boot
/// branch, which sets status instead of returning an error).
pub(crate) fn emit_coded(app: &AppHandle, code: ErrorCode, message: impl Into<String>) {
    emit_log_at(
        app,
        &launcher_core::CodedError::new(code, message).log_line(),
        LogLevel::Error,
    );
}

/// Classify a launch-time `anyhow` error from `dsh-adapter` into a stable code.
/// The adapter surfaces these as plain `anyhow!` strings (it has no typed error
/// enum), so this is a *boundary* classification: the two recognised strings are
/// stable and only matched here, once, not parsed on the hot path.
fn classify_launch_error(e: &anyhow::Error) -> ErrorCode {
    let s = e.to_string();
    if s.contains("Node not found") {
        ErrorCode::NodeNotFound
    } else if s.contains("DSH not found") {
        ErrorCode::DshBinUnresolvable
    } else {
        ErrorCode::LaunchFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-process-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dump(dir: &Path, epoch: u64) {
        std::fs::write(dir.join(format!("boot-crashed-{epoch}.zip")), b"PK").unwrap();
    }

    fn epochs(dir: &Path) -> Vec<u64> {
        let mut found: Vec<u64> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| package_epoch(&e.path()))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn package_epoch_reads_only_names_the_launcher_wrote() {
        assert_eq!(package_epoch(Path::new("boot-degraded-42.zip")), Some(42));
        assert_eq!(package_epoch(Path::new("boot-crashed-nope.zip")), None);
        assert_eq!(package_epoch(Path::new("boot-crashed-42")), None);
        assert_eq!(
            package_epoch(Path::new("ahl-diagnose-default-42.zip")),
            None
        );
    }

    #[test]
    fn prune_keeps_the_newest_and_removes_the_oldest() {
        let dir = tmp_dir("prune-newest");
        for epoch in [100, 200, 300, 400, 500, 600, 700] {
            dump(&dir, epoch);
        }
        prune_diagnostic_packages(&dir, 5);
        assert_eq!(epochs(&dir), vec![300, 400, 500, 600, 700]);
    }

    #[test]
    fn prune_removes_nothing_when_at_or_under_the_limit() {
        let dir = tmp_dir("prune-under");
        for epoch in [100, 200, 300] {
            dump(&dir, epoch);
        }
        prune_diagnostic_packages(&dir, 5);
        assert_eq!(epochs(&dir), vec![100, 200, 300]);
    }

    #[test]
    fn prune_leaves_files_it_did_not_name() {
        let dir = tmp_dir("prune-foreign");
        for epoch in [100, 200, 300, 400, 500, 600] {
            dump(&dir, epoch);
        }
        // Neither of these is a name this launcher writes. A retention pass that
        // deletes an unrecognised file is how you find out what it was the hard way.
        std::fs::write(dir.join("notes.txt"), b"keep me").unwrap();
        std::fs::write(dir.join("backup.zip"), b"PK").unwrap();

        prune_diagnostic_packages(&dir, 5);

        assert_eq!(epochs(&dir), vec![200, 300, 400, 500, 600]);
        assert!(dir.join("notes.txt").is_file());
        assert!(dir.join("backup.zip").is_file());
    }
}
