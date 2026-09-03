use std::sync::Arc;
use std::time::Duration;

use launcher_core::instance::InstanceManifest;
use launcher_core::process::{
    sweep_leftover, wait_for_port, PidLedger, ProcessState, ProcessStatus,
};
use launcher_core::{ExitSink, LogLine, LogSink, LogStream, NewUsageRecord, RuntimeAdapter};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::plugins::refresh_plugin_inventory_cache;
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

const LOG_EVENT: &str = "logs";
const DSH_URL_EVENT: &str = "dsh-url";
const PROCESS_STATE_EVENT: &str = "process-state";

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
        do_launch(&state, &app, id).await
    })
    .await
}

async fn do_launch(
    state: &AppState,
    app: &AppHandle,
    id: String,
) -> Result<ProcessState, AppError> {
    {
        let mut guard = state.child.lock().await;

        // One-at-a-time: if something else is running, stop it (and close its DSH
        // window). `Degraded` counts as live — it's a still-booting child waiting
        // on its URL in the background, not a dead one.
        if let Some(running) = guard.as_ref() {
            let status = running.handle.state().status;
            if matches!(
                status,
                ProcessStatus::Running | ProcessStatus::Starting | ProcessStatus::Degraded
            ) {
                if running.instance_id == id {
                    return Ok(running.handle.state());
                }
                emit_log(
                    &app,
                    &format!("Stopping {} to switch to {id}…", running.instance_id),
                );
                if let Some(mut r) = guard.take() {
                    drop(guard);
                    if let Some(shutdown) = r.usage_proxy_shutdown.take() {
                        let _ = shutdown.send(());
                    }
                    let _ = r.handle.stop().await;
                    close_session(&state, "stopped");
                    close_dsh_window(&app);
                }
            }
        }
    }

    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::msg("settings lock poisoned"))?
        .clone();
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let provider = state.vault.resolve(&instance.provider_ref)?;
    let mut env = state.adapter.build_env(&provider, &instance)?;
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
            emit_log(
                &app,
                &format!("{id} · usage proxy ready at {}", proxy.base_url),
            );
            (Some(proxy.base_url), Some(proxy.shutdown))
        }
        Err(e) => {
            emit_log(&app, &format!("{id} · usage proxy unavailable: {e}"));
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
    let (url_tx, mut url_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let base_sink = make_usage_sink(
        app.clone(),
        id.clone(),
        provider.profile.id.clone(),
        fallback_model,
    );
    let on_log: LogSink = {
        let tx = url_tx.clone();
        Arc::new(move |line: LogLine| {
            base_sink(line.clone());
            if let Some(url) = parse_dsh_url(&line.line) {
                let _ = tx.send(url);
            }
        })
    };
    let on_exit: ExitSink = {
        let app = app.clone();
        let id = id.clone();
        Arc::new(move |process_state: ProcessState| {
            let _ = app.emit(PROCESS_STATE_EVENT, &process_state);
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
            });
        })
    };

    // Startup zombie sweep: a PID we recorded on a previous launch but never
    // reaped (launcher hard-killed / crashed) is still out there. Kill its
    // whole tree now, before spawning again. Safe here — the one-at-a-time
    // block above already returned early for a still-running same instance.
    let ledger = PidLedger::open(state.paths.pid_ledger());
    let swept = sweep_leftover(&ledger);
    if swept > 0 {
        emit_log(
            &app,
            &format!("Reaped {swept} leftover process tree(s) from a previous session"),
        );
    }

    let handle = match state
        .adapter
        .launch(&settings, &instance, &env, on_log, Some(on_exit))
        .await
    {
        Ok(h) => h,
        Err(e) => {
            if let Some(shutdown) = usage_proxy.take() {
                let _ = shutdown.send(());
            }
            close_session(&state, "crashed");
            return Err(e.into());
        }
    };
    let pid = handle.pid;
    ledger.record(pid);
    emit_log(&app, &format!("{id} · DSH web starting (pid {pid})…"));

    // Wait for the ready URL line (or the process to die / the 20s ceiling).
    let url = match tokio::time::timeout(Duration::from_secs(20), url_rx.recv()).await {
        Ok(Some(url)) => Some(url),
        _ => None,
    };

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
        });
        let st = guard.as_ref().expect("just stored").handle.state();
        if st.status == ProcessStatus::Crashed {
            close_session(&state, "crashed");
        }
        let _ = app.emit(PROCESS_STATE_EVENT, &st);
        return Ok(st);
    }

    let ready_url = url.clone();
    let port = ready_url.as_ref().and_then(|u| url_port(u));
    match ready_url.as_deref() {
        Some(url) => {
            finalize_ready(
                &app,
                &provider,
                &settings,
                &instance,
                &handle,
                &url,
                usage_proxy_base_url.as_deref(),
            )
            .await;
        }
        None => {
            // The 20s ceiling is too short for a cold first boot (fresh `web`
            // profile materialize + pnpm install of its bundles can take over a
            // minute). Don't give up on the process — mark degraded for the UI,
            // keep it running, and hand the rest of the wait to a background
            // task that opens the DSH window the moment the URL finally lands.
            handle.set_status(ProcessStatus::Degraded);
            emit_log(
                &app,
                &format!("{id} · DSH web did not report a URL within 20s — still booting, waiting in background…"),
            );
            let app = app.clone();
            let provider = provider.clone();
            let settings = settings.clone();
            let instance = instance.clone();
            let id_task = id.clone();
            let pid = handle.pid;
            let usage_proxy_base_url = usage_proxy_base_url.clone();
            tauri::async_runtime::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(240), url_rx.recv()).await {
                    Ok(Some(url)) => {
                        let state = app.state::<AppState>();
                        let mut guard = state.child.lock().await;
                        if let Some(r) = guard.as_mut() {
                            if r.handle.pid == pid {
                                r.port = url_port(&url);
                                r.url = Some(url.clone());
                                finalize_ready(
                                    &app,
                                    &provider,
                                    &settings,
                                    &instance,
                                    &r.handle,
                                    &url,
                                    usage_proxy_base_url.as_deref(),
                                )
                                .await;
                            }
                        }
                    }
                    _ => {
                        emit_log(
                            &app,
                            &format!("{id_task} · DSH web still not ready after 4 min — check Activity logs"),
                        );
                    }
                }
            });
        }
    }
    let mut guard = state.child.lock().await;
    *guard = Some(crate::state::RunningChild {
        instance_id: id,
        handle,
        url: ready_url,
        port,
        usage_proxy_shutdown: usage_proxy,
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
                if let Some(shutdown) = running.usage_proxy_shutdown.take() {
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
        let _ = running.handle.stop().await;
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
        if line.stream == LogStream::Stderr {
            tracing::warn!(target: "dsh", "{}", line.line);
        } else {
            tracing::info!(target: "dsh", "{}", line.line);
        }
        if let Some(record) =
            parse_usage_record(&line.line, &instance_id, &api_key_alias, &fallback_model)
        {
            let state = app.state::<AppState>();
            if let Ok(Some(saved)) = state.usage.record(record) {
                let _ = app.emit("usage-recorded", &saved);
            }
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

/// The server is up: settle on the port, stamp the launcher theme + model
/// catalog into the harness, mark it running, and show it in a DSH window.
/// Shared by the fast path and the slow-boot background continuation.
async fn finalize_ready(
    app: &AppHandle,
    provider: &launcher_core::ResolvedProvider,
    settings: &launcher_core::AppSettings,
    instance: &launcher_core::InstanceManifest,
    handle: &launcher_core::process::ChildHandle,
    url: &str,
    usage_proxy_base_url: Option<&str>,
) {
    let port = url_port(url);
    if let Some(port) = port {
        let _ = wait_for_port(port, Duration::from_secs(5)).await;
    }
    handle.set_status(ProcessStatus::Running);
    emit_log(app, &format!("{} · DSH web ready at {url}", instance.id));
    let _ = app.emit(DSH_URL_EVENT, url.to_string());
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
                match dsh_adapter::llm::set_base_url(port, base_url).await {
                    Ok(()) => emit_log(
                        &app,
                        &format!("{} · usage proxy injected into DSH settings", instance.id),
                    ),
                    Err(e) => emit_log(
                        &app,
                        &format!("{} · usage proxy settings sync failed: {e}", instance.id),
                    ),
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;

            let state = app.state::<AppState>();
            let job_id = instance.id.clone();
            let result = run_instance_job(
                &state,
                &app,
                &job_id,
                HeavyJobKind::InventorySync,
                || async {
                    refresh_plugin_inventory_cache(&state, &app, &instance.id, port, "launch").await
                },
            )
            .await;
            if let Err(e) = result {
                emit_log(
                    &app,
                    &format!("{} · DSH inventory cache refresh failed: {e}", instance.id),
                );
            }

            tokio::time::sleep(Duration::from_secs(3)).await;

            if !provider.profile.models.is_empty() {
                if let Err(e) = dsh_adapter::llm::set_models(port, &provider.profile.models).await {
                    emit_log(
                        &app,
                        &format!("{} · model catalog sync failed: {e}", instance.id),
                    );
                }
            }

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
            if let Some(lang) = settings.language.as_deref() {
                match dsh_adapter::language::get_preference(port).await {
                    Ok(Some(_)) => {}
                    _ => {
                        let _ = dsh_adapter::language::set_preference(port, lang).await;
                    }
                }
            }
        });
    }
}

pub(crate) fn emit_log(app: &AppHandle, line: &str) {
    let _ = app.emit(
        LOG_EVENT,
        LogLine {
            stream: LogStream::Stdout,
            line: line.to_string(),
        },
    );
}
