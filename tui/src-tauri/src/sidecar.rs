//! Host sidecar process management.
//!
//! Spawns the sidecar script (`host/index.js`), which launches the real upstream
//! DSH Host (`dsh web`), waits for its `DSH_READY <port> <token>` readiness
//! line, and can stop the whole process tree. The webview loads
//! `http://127.0.0.1:<port>/?token=<token>`; the Host exchanges that launch token
//! for an authority-bound session cookie and then serves the DSH Web UI.
//!
//! After readiness the sidecar is *supervised*: a background watcher reaps the
//! child and delivers its `ExitStatus`, so the shell can distinguish a Host crash
//! from a requested shutdown and restart when appropriate.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use tauri::Manager; // `.path()` on AppHandle

/// Shared Desktop Web port policy (mirrors `dsh-desktop/src/desktop-port.ts`).
/// Phase 1 launches with `--port 0` and lets the OS pick a free port; the real
/// port comes back in the readiness line. A stable port is deferred to the LAN
/// phase (Phase 3), where this constant becomes the default bind port.
#[allow(dead_code)] // referenced by the Phase 3 LAN policy; not passed today.
pub const DESKTOP_DEFAULT_WEB_PORT: u16 = 43_120;
/// Maximum number of sequential ports tried after a real bind collision
/// (kept for the `--mock` sidecar path and the future stable-port policy).
pub const DESKTOP_WEB_PORT_RETRY_LIMIT: u32 = 32;

/// How long we wait for the sidecar to report readiness.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// A live Host sidecar: the handle the shell needs to stop it.
pub struct HostManager {
    /// Loopback port the Host's web carrier bound (OS-assigned in Phase 1).
    pub port: u16,
    /// Browser-trust launch token; the webview URL must carry it as `?token=`.
    pub token: String,
    pid: u32,
    /// Shared with the supervisor; `stop()` flips it so a crash report after a
    /// requested shutdown is recognised as clean and not restarted.
    shutting_down: Arc<AtomicBool>,
}

/// A freshly started sidecar plus its exit channel. The `manager` is handed to
/// the shell for cleanup; the `exit_rx` is handed to a supervisor that watches
/// for the Host crashing after it was reported ready.
pub struct HostLaunch {
    pub manager: HostManager,
    pub exit_rx: mpsc::Receiver<ExitStatus>,
}

/// Terminal state of the sidecar process (the wrapper around the real Host).
#[derive(Debug, Clone, Copy)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl HostManager {
    /// Spawn the sidecar and wait for its readiness line.
    ///
    /// Port allocation is delegated to the sidecar: `--port 0` lets the OS pick a
    /// free port, and the Host's URL line reports the actual port + token back.
    /// This avoids the TOCTOU race of probing the port ourselves before spawning.
    ///
    /// `node`/`script`/`home` are resolved by the caller (see
    /// [`resolve_node_binary`]/[`resolve_host_script`]) so packaged builds can
    /// point at the bundled Node + resources while dev builds keep using the
    /// system `node` and the working-tree runtime.
    pub fn start(node: &str, script: &str, home: &std::path::Path) -> Result<HostLaunch, String> {
        let mut child = Command::new(node)
            .arg(script)
            .arg("--port")
            .arg("0")
            .arg("--retry-limit")
            .arg(DESKTOP_WEB_PORT_RETRY_LIMIT.to_string())
            .arg("--home")
            .arg(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn host sidecar (`{node} {script}`) failed: {e}"))?;

        let pid = child.id();
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = kill_process_tree(pid);
                return Err("host sidecar stdout unavailable".into());
            }
        };
        let stderr = match child.stderr.take() {
            Some(s) => s,
            None => {
                let _ = kill_process_tree(pid);
                return Err("host sidecar stderr unavailable".into());
            }
        };

        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u16, String), String>>();
        let (exit_tx, exit_rx) = mpsc::channel::<ExitStatus>();
        let shutting_down = Arc::new(AtomicBool::new(false));

        // Watch stdout: consume every line (so the pipe never fills and stalls the
        // Host), report `DSH_READY <port> <token>`, and forward the rest as logs.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut announced = false;
            loop {
                line.clear();
                let read = reader.read_line(&mut line);
                let trimmed = line.trim();
                match read {
                    Ok(0) => break, // EOF: wrapper exited.
                    Ok(_) if trimmed.is_empty() => continue,
                    Ok(_) => {
                        if !announced {
                            if let Some(rest) = trimmed.strip_prefix("DSH_READY ") {
                                let mut parts = rest.split_whitespace();
                                let port = parts.next().and_then(|p| p.parse::<u16>().ok());
                                let token = parts.next().map(str::to_string);
                                announced = true;
                                match (port, token) {
                                    (Some(port), Some(token)) => {
                                        let _ = ready_tx.send(Ok((port, token)));
                                    }
                                    _ => {
                                        let _ = ready_tx
                                            .send(Err(format!("malformed DSH_READY line: {trimmed}")));
                                    }
                                }
                                continue;
                            }
                        }
                        tracing::info!(target: "dsh::host", "{trimmed}");
                    }
                    Err(e) => {
                        if !announced {
                            let _ = ready_tx.send(Err(format!("host stdout read error: {e}")));
                        }
                        break;
                    }
                }
            }
            if !announced {
                let _ = ready_tx.send(Err("host sidecar exited before reporting ready".into()));
            }
        });

        // Watch stderr (carries the Host's own errors) and forward as logs.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            tracing::warn!(target: "dsh::host", "{trimmed}");
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Reap the child and report its exit so the supervisor can react.
        std::thread::spawn(move || {
            // Windows: `Child::wait()` closes the piped stdin write-end, which the
            // sidecar reads as "parent dropped me" and would use to kill the Host.
            // Take the write-end out and hold it in this thread for the wrapper's
            // whole lifetime so its stdin stays open; it is dropped right after the
            // wrapper exits (wait() returns), which is exactly when EOF is fine.
            let _stdin = child.stdin.take();
            let status = child.wait();
            let exit = match status {
                Ok(status) => ExitStatus {
                    code: status.code(),
                    signal: exit_signal(&status),
                },
                Err(e) => {
                    tracing::warn!("wait for host sidecar failed: {e}");
                    ExitStatus { code: None, signal: None }
                }
            };
            let _ = exit_tx.send(exit);
        });

        match ready_rx.recv_timeout(READY_TIMEOUT) {
            Ok(Ok((port, token))) => Ok(HostLaunch {
                manager: HostManager { port, token, pid, shutting_down },
                exit_rx,
            }),
            Ok(Err(e)) => {
                let _ = kill_process_tree(pid);
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = kill_process_tree(pid);
                Err("host sidecar readiness timeout".into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = kill_process_tree(pid);
                Err("host sidecar readiness channel closed".into())
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Full URL the webview must load: the launch token gates the index and the
    /// Host exchanges it for the session cookie on the first navigation.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.port, self.token)
    }

    /// Clone the shared shutdown flag, so a supervisor can observe a shutdown
    /// request that was made on the copy of this manager stored in shell state.
    pub fn shutting_down_flag(&self) -> Arc<AtomicBool> {
        self.shutting_down.clone()
    }

    /// Ask the Host (and its whole tree) to stop. Idempotent and non-blocking;
    /// the watcher thread reaps the child when it actually exits.
    pub fn stop(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let _ = kill_process_tree(self.pid);
    }
}

fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// Node binary to launch the sidecar with in development: the system `node`
/// (the dev runtime was installed against it). Packaged builds use
/// [`resolve_node_binary`] to point at the bundled `node.exe` instead.
fn node_binary() -> String {
    "node".to_string()
}

/// Sidecar script in development: `host/index.js` next to `src-tauri`.
/// Packaged builds use [`resolve_host_script`] to point at the bundled copy.
fn host_script_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../host/index.js").to_string()
}

/// Locate a bundled resource under the app's resource dir. Tauri v2 places
/// `bundle.resources` entries relative to the resource dir on some platforms and
/// under a `resources/` subdirectory on others, so probe both layouts. Returns
/// `None` in dev (no resources) so callers fall back to working-tree paths.
pub fn locate_resource(app: &tauri::AppHandle, rel: &str) -> Option<std::path::PathBuf> {
    let res_dir = app.path().resource_dir().ok()?;
    let candidates = [
        res_dir.join(rel),
        res_dir.join("resources").join(rel),
        res_dir.join("bundle-assets").join(rel),
    ];
    for c in &candidates {
        if c.exists() {
            let clean = normalize_windows_path(c);
            tracing::info!("resolved bundled resource {rel} -> {}", clean.display());
            return Some(clean);
        }
    }
    None
}

/// Strip the `\\?\` extended-length prefix that Windows APIs (e.g. Tauri's
/// `resource_dir()`, and MSYS-style process launching) can attach to absolute
/// paths. Node's own module resolution breaks on the prefixed form
/// (`realpathSync` ends up `lstat`-ing the bare drive root like `D:` and fails
/// with `EISDIR`), so the bundled `node.exe` must always receive ordinary
/// `C:\`-style paths. A no-op for paths without the prefix.
fn normalize_windows_path(p: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        let cleaned = s.strip_prefix(r"\\?\").unwrap_or(&s);
        std::path::PathBuf::from(cleaned)
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

/// Node binary for the sidecar: the bundled `node.exe` when packaged, otherwise
/// the system `node`.
pub fn resolve_node_binary(app: &tauri::AppHandle) -> String {
    if let Some(bin) = locate_resource(app, "node/node.exe") {
        return bin.to_string_lossy().into_owned();
    }
    node_binary()
}

/// Sidecar script for the host: the bundled `host/index.js` when packaged,
/// otherwise the working-tree `host/index.js`.
pub fn resolve_host_script(app: &tauri::AppHandle) -> String {
    if let Some(script) = locate_resource(app, "host/index.js") {
        return script.to_string_lossy().into_owned();
    }
    host_script_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolate the Rust spawn path: if the wrapper's piped stdin is being closed
    /// by the parent, `DSH_READY` never arrives and this test fails fast with
    /// "host sidecar exited before reporting ready".
    #[test]
    fn wrapper_reports_ready() {
        // Wire up the shell logger so the wrapper's forwarded dsh stderr becomes
        // visible (writes to <CARGO_MANIFEST_DIR>/logs/dsh-*.log).
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let _ = crate::logging::init(dir);
        let home = dir.join("../host/runtime/.dsh-home");
        let launch = HostManager::start(&node_binary(), &host_script_path(), &home)
            .expect("host sidecar should report ready");
        eprintln!("[test] host ready on port {}", launch.manager.port);
        launch.manager.stop();
    }

    /// Spawn `dsh web` DIRECTLY from Rust (no wrapper) and dump its output, to
    /// determine whether dsh can start under a Rust parent at all.
    #[test]
    fn dsh_direct_spawn() {
        use std::io::Read;
        let dsh_bin = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../host/runtime/node_modules/@deepseek-ai/dsh/lib/bin.js"
        );
        let home = concat!(env!("CARGO_MANIFEST_DIR"), "/../host/runtime/.dsh-home");
        let mut child = Command::new("node")
            .arg(dsh_bin)
            .args(["web", "--host", "127.0.0.1", "--no-open", "--port", "0"])
            .env("DSH_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dsh directly");
        let mut stderr = child.stderr.take().expect("stderr");
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf);
            eprintln!("[dsh stderr]\n{buf}");
        });
        let mut stdout = child.stdout.take().expect("stdout");
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = stdout.read_to_string(&mut buf);
            eprintln!("[dsh stdout]\n{buf}");
        });
        std::thread::sleep(Duration::from_secs(3));
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("[test] direct dsh done");
    }
}

/// Kill a process tree. Windows uses `taskkill /T /F`; elsewhere send SIGTERM
/// then escalate to SIGKILL after a short grace period.
pub fn kill_process_tree(pid: u32) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
        std::thread::sleep(Duration::from_millis(300));
        let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).status();
        Ok(())
    }
}
