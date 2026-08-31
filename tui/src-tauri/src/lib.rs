//! dsh-tauri — a lean Tauri shell around the DSH Host.
//!
//! Phase 2 lifecycle: a supervisor thread boots the real Host sidecar, waits for
//! its `DSH_READY <port> <token>` line, navigates the main window to
//! `http://127.0.0.1:<port>/?token=<token>`, and watches the sidecar for an
//! unexpected exit. If the Host crashes after becoming ready, the supervisor
//! restarts it up to [`MAX_HOST_RESTARTS`] times with a short backoff and only
//! then drops the window back on the launcher recovery page (via `?crashed=`).
//! Closing the window asks for confirmation, then stops the Host tree.

mod logging;
mod sidecar;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tracing::{error, info, warn};

/// Restarts permitted after the initial launch before we give up and show the
/// recovery page. Total launches = `MAX_HOST_RESTARTS + 1`.
const MAX_HOST_RESTARTS: u32 = 3;
/// Backoff before the *next* attempt: `2^attempt` × this base (1s, 2s, 4s).
const RESTART_BACKOFF_BASE_MS: u64 = 1_000;

/// Lightweight handle to a running sidecar, kept in shell state so a shutdown
/// can flag it and kill its tree even though the supervisor owns the real
/// `HostManager` (the child process itself lives in a watcher thread).
struct HostHandle {
    pid: u32,
    shutting_down: Arc<AtomicBool>,
}

/// Runtime state owned by the shell.
struct ShellState {
    /// Port the running Host sidecar bound (set once ready).
    port: Option<u16>,
    /// Sidecar to stop when the app exits.
    host: Option<HostHandle>,
    /// Launcher page URL (dev server in dev, app origin in production), used to
    /// navigate back for a restart / recovery view.
    launcher_url: Option<tauri::Url>,
    /// User confirmed quitting (set from the close-confirmation dialog).
    closing: bool,
    /// A supervisor thread is currently owning the Host lifecycle.
    supervising: bool,
    /// Writable DSH home dir (`<app_data>/dsh-home`), passed to the sidecar as
    /// `--home` so the Host never writes into the (read-only) bundled resources.
    home: Option<PathBuf>,
}

impl Default for ShellState {
    fn default() -> Self {
        ShellState {
            port: None,
            host: None,
            launcher_url: None,
            closing: false,
            supervising: false,
            home: None,
        }
    }
}

type State = Mutex<ShellState>;

/// Stop the sidecar (taken out of state so the kill is never run twice). Also
/// marks the shell as closing so a supervisor mid-restart backs off instead of
/// relaunching the Host after the window is gone.
fn stop_host(state: &State) {
    let mut locked = state.lock();
    if let Ok(s) = &mut locked {
        s.closing = true;
        if let Some(host) = s.host.take() {
            host.shutting_down.store(true, Ordering::SeqCst);
            let _ = sidecar::kill_process_tree(host.pid);
        }
    }
}

fn app_is_closing(app: &tauri::AppHandle) -> bool {
    app.state::<State>().lock().map(|s| s.closing).unwrap_or(true)
}

/// Navigate the main window to `url`. Webview navigation is thread-safe; this
/// is scheduled on the main thread for determinism.
fn navigate_window(app: &tauri::AppHandle, url: &str) {
    let url = url.to_string();
    let closure_app = app.clone();
    let _ = app.run_on_main_thread(move || match url.parse::<tauri::Url>() {
        Ok(url) => {
            if let Some(win) = closure_app.get_webview_window("main") {
                let url_str = url.to_string();
                if let Err(e) = win.navigate(url) {
                    error!("navigate to {url_str} failed: {e}");
                }
            }
        }
        Err(e) => error!("invalid navigation URL {url}: {e}"),
    });
}

fn navigate_launcher(app: &tauri::AppHandle) {
    info!("navigating back to launcher");
    let launcher = app.state::<State>().lock().ok().and_then(|s| s.launcher_url.clone());
    if let Some(url) = launcher {
        navigate_window(app, url.as_str());
    }
}

/// Navigate to the launcher with `?crashed=<msg>` so the recovery page renders
/// without relying on an event that could race the launcher's listener setup.
fn navigate_crashed(app: &tauri::AppHandle, message: &str) {
    warn!("navigating to recovery page: {message}");
    let launcher = app.state::<State>().lock().ok().and_then(|s| s.launcher_url.clone());
    if let Some(mut url) = launcher {
        let encoded: String = url::form_urlencoded::byte_serialize(message.as_bytes()).collect();
        url.set_query(Some(&format!("crashed={encoded}")));
        navigate_window(app, url.as_str());
    }
}

/// Backoff before retrying a failed start: 1s, 2s, 4s, capped at ~64s.
fn sleep_backoff(attempt: u32) {
    let ms = RESTART_BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(6));
    std::thread::sleep(Duration::from_millis(ms));
}

/// Own the Host lifecycle in a background thread: boot, navigate, watch for
/// crashes, restart with backoff, and finally drop back to the recovery page.
fn supervise_host(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        {
            let state = app.state::<State>();
            let mut locked = state.lock();
            if let Ok(s) = &mut locked {
                if s.supervising {
                    warn!("supervisor already running; ignoring start request");
                    return;
                }
                s.supervising = true;
            }
        }

        let total = MAX_HOST_RESTARTS + 1;
        for attempt in 0..total {
            if app_is_closing(&app) {
                info!("supervisor exiting: window closing");
                break;
            }

            let home = app.state::<State>().lock().ok().and_then(|s| s.home.clone());
            let launch = match home {
                Some(home) => {
                    let node = sidecar::resolve_node_binary(&app);
                    let script = sidecar::resolve_host_script(&app);
                    sidecar::HostManager::start(&node, &script, &home)
                }
                None => Err("DSH home dir not resolved".into()),
            };
            let launch = match launch {
                Ok(launch) => launch,
                Err(e) => {
                    error!("host start failed (attempt {} of {}): {e}", attempt + 1, total);
                    if attempt + 1 >= total {
                        navigate_crashed(&app, &format!("Host 启动失败：{e}"));
                        break;
                    }
                    sleep_backoff(attempt);
                    continue;
                }
            };

            let manager = launch.manager;
            // The user may have confirmed quitting while this start was in
            // flight; don't navigate to a Host we're about to kill.
            if app_is_closing(&app) {
                manager.stop();
                info!("host started but window closing; stopping");
                break;
            }
            let host_url = manager.url();
            let pid = manager.pid();
            let shutting_down = manager.shutting_down_flag();

            {
                let state = app.state::<State>();
                let mut locked = state.lock();
                if let Ok(s) = &mut locked {
                    s.port = Some(manager.port);
                    s.host = Some(HostHandle { pid, shutting_down: shutting_down.clone() });
                }
            }
            navigate_window(&app, &host_url);
            info!("host ready at {host_url} (pid {pid})");

            match launch.exit_rx.recv() {
                Ok(_) if shutting_down.load(Ordering::SeqCst) => {
                    info!("host stopped on request; supervisor exiting");
                    break;
                }
                Ok(exit) => {
                    warn!(
                        "host crashed: code={:?} signal={:?} (attempt {} of {})",
                        exit.code, exit.signal, attempt + 1, total
                    );
                    {
                        let state = app.state::<State>();
                        let mut locked = state.lock();
                        if let Ok(s) = &mut locked {
                            s.host = None;
                            s.port = None;
                        }
                    }
                    if attempt + 1 >= total {
                        navigate_crashed(&app, &format!("Host 进程已退出（code={:?}）", exit.code));
                        break;
                    }
                    navigate_launcher(&app);
                    sleep_backoff(attempt);
                }
                Err(e) => {
                    warn!("host exit channel closed: {e}");
                    break;
                }
            }
        }

        {
            let state = app.state::<State>();
            let mut locked = state.lock();
            if let Ok(s) = &mut locked {
                s.supervising = false;
            }
        }
    });
}

/// Manual "retry" action from the launcher recovery page.
#[tauri::command]
fn restart_host(app: tauri::AppHandle) -> Result<(), String> {
    info!("restart_host invoked from recovery page");
    {
        let state = app.state::<State>();
        let mut locked = state.lock();
        if let Ok(s) = &mut locked {
            s.host = None;
            s.port = None;
        }
    }
    navigate_launcher(&app);
    supervise_host(app);
    Ok(())
}

/// Deterministic launcher target: the dev server URL in dev builds, otherwise
/// the app's own protocol origin.
fn launcher_url(app: &tauri::AppHandle) -> tauri::Url {
    #[cfg(debug_assertions)]
    if let Some(dev) = &app.config().build.dev_url {
        return dev.clone();
    }
    let _ = app; // used only in debug builds (dev server URL)
    tauri::Url::parse("tauri://localhost").expect("static app origin")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![restart_host])
        .manage(State::default())
        .setup(|app| {
            let app_data = app.path().app_data_dir().ok();
            if let Some(dir) = &app_data {
                logging::init(dir);
            }
            // Writable DSH home, independent of the (read-only) bundled resources.
            let home = app_data.map(|d| d.join("dsh-home"));
            if let Some(h) = &home {
                if let Err(e) = std::fs::create_dir_all(h) {
                    error!("create DSH home dir {} failed: {e}", h.display());
                }
                info!("DSH home at {}", h.display());
            }
            {
                let state = app.state::<State>();
                let mut locked = state.lock();
                if let Ok(s) = &mut locked {
                    s.launcher_url = Some(launcher_url(app.handle()));
                    s.home = home;
                }
            }
            supervise_host(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                let app = window.app_handle().clone();
                if app_is_closing(&app) {
                    return;
                }
                api.prevent_close();
                let window = window.clone();
                let app = app.clone();
                app.dialog()
                    .message("关闭窗口将停止本地 DSH 服务。确定退出 DSH 吗？")
                    .title("退出 DSH")
                    .kind(tauri_plugin_dialog::MessageDialogKind::Warning)
                    .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
                        "退出".to_string(),
                        "取消".to_string(),
                    ))
                    .show(move |confirmed| {
                        if confirmed {
                            if let Ok(mut s) = app.state::<State>().lock() {
                                s.closing = true;
                            }
                            let _ = window.destroy();
                        }
                    });
            }
            WindowEvent::Destroyed => {
                info!("window destroyed -> stop host");
                let app = window.app_handle();
                let state = app.state::<State>();
                stop_host(&state);
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Safety net: stop the sidecar on exit even if the window event missed it.
            if let RunEvent::Exit = event {
                info!("run event: Exit -> stop host");
                let state = app_handle.state::<State>();
                stop_host(&state);
            }
        });
}
