use std::sync::Arc;
use std::time::Duration;

use launcher_core::instance::InstanceManifest;
use launcher_core::process::{
    sweep_leftover, wait_for_port, PidLedger, ProcessState, ProcessStatus,
};
use launcher_core::{LogLine, LogSink, LogStream, RuntimeAdapter};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::AppError;
use crate::state::AppState;

const LOG_EVENT: &str = "logs";

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
    let mut guard = state.child.lock().await;

    // One-at-a-time: if something else is running, stop it (and close its DSH window).
    if let Some(running) = guard.as_ref() {
        let status = running.handle.state().status;
        if matches!(status, ProcessStatus::Running | ProcessStatus::Starting) {
            if running.instance_id == id {
                return Ok(running.handle.state());
            }
            emit_log(&app, &format!("Stopping {} to switch to {id}…", running.instance_id));
            if let Some(mut r) = guard.take() {
                let _ = r.handle.stop().await;
                close_session(&state, "stopped");
                close_dsh_window(&app);
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
    let env = state.adapter.build_env(&provider, &instance)?;

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
    let base_sink = make_sink(app.clone());
    let on_log: LogSink = {
        let tx = url_tx.clone();
        Arc::new(move |line: LogLine| {
            base_sink(line.clone());
            if let Some(url) = parse_dsh_url(&line.line) {
                let _ = tx.send(url);
            }
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

    let handle = match state.adapter.launch(&settings, &instance, &env, on_log).await {
        Ok(h) => h,
        Err(e) => {
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
        *guard = Some(crate::state::RunningChild {
            instance_id: id,
            handle,
            port: None,
        });
        let st = guard.as_ref().expect("just stored").handle.state();
        if st.status == ProcessStatus::Crashed {
            close_session(&state, "crashed");
        }
        return Ok(st);
    }

    let port = url.as_ref().and_then(|u| url_port(u));
    match url {
        Some(url) => {
            // The server is up; settle briefly on the actual port, then show it.
            if let Some(port) = url_port(&url) {
                let _ = wait_for_port(port, Duration::from_secs(5)).await;
                // One lamp: stamp the launcher's persisted preference into a
                // harness that has no explicit one yet (a fresh home defaults
                // to `system`). An already-explicit DSH value wins.
                if let Some(launcher_theme) = settings.theme.as_deref() {
                    if launcher_theme != "system" {
                        match dsh_adapter::theme::get_preference(port).await {
                            Ok(Some(pref)) if pref != "system" => {}
                            _ => {
                                let _ = dsh_adapter::theme::set_preference(port, launcher_theme)
                                    .await;
                            }
                        }
                    }
                }
                // Inject the provider's model catalog so DSH's selector shows
                // the chosen provider's models. baseURL + key already reach DSH
                // through env; only the catalog needs the settings RPC.
                if !provider.profile.models.is_empty() {
                    if let Err(e) =
                        dsh_adapter::llm::set_models(port, &provider.profile.models).await
                    {
                        emit_log(&app, &format!("{id} · model catalog sync failed: {e}"));
                    }
                }
            }
            handle.set_status(ProcessStatus::Running);
            emit_log(&app, &format!("{id} · DSH web ready at {url}"));
            if let Err(e) = open_dsh_window(&app, &url, &instance.name) {
                emit_log(&app, &format!("{id} · DSH window failed to open: {e}"));
            }
        }
        None => {
            handle.set_status(ProcessStatus::Degraded);
            emit_log(&app, &format!("{id} · DSH web did not report a URL within 20s — check Activity logs"));
        }
    }
    *guard = Some(crate::state::RunningChild {
        instance_id: id,
        handle,
        port,
    });
    Ok(guard.as_ref().expect("just stored").handle.state())
}

/// Stop the managed harness process and close its history row as `stopped`.
#[tauri::command]
pub async fn stop(state: State<'_, AppState>, app: AppHandle) -> Result<ProcessState, AppError> {
    do_stop(&state, &app).await
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
    if matches!(result.status, ProcessStatus::Crashed | ProcessStatus::Stopped) {
        if close_session(&state, "crashed") {
            // Only drop the handle when we actually owned the session.
            *guard = None;
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
    let mut guard = state.child.lock().await;
    if let Some(mut running) = guard.take() {
        emit_log(app, &format!("Stopping {}…", running.instance_id));
        let _ = running.handle.stop().await;
        close_session(state, "stopped");
        close_dsh_window(app);
    }
    Ok(ProcessState::stopped())
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
    let path = after[digits.len()..].split_whitespace().next().unwrap_or("");
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
        .inner_size(1280.0, 800.0)
        .min_inner_size(800.0, 600.0)
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

pub(crate) fn emit_log(app: &AppHandle, line: &str) {
    let _ = app.emit(
        LOG_EVENT,
        LogLine {
            stream: LogStream::Stdout,
            line: line.to_string(),
        },
    );
}
