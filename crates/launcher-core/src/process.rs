//! Process supervisor — the boundary between "a GUI wrapper" and "a launcher".
//!
//! `spawn_child` owns the full lifecycle of a managed harness process: streamed
//! stdout/stderr through a `LogSink`, a watcher that reaps on exit, and a kill
//! channel so `ChildHandle::stop()` can ask the watcher to terminate it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::now_secs;

/// Poll cadence shared by the child watcher and [`wait_for_port`]: how often the
/// supervisor asks whether the harness is still alive / listening yet.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessStatus {
    Stopped,
    Starting,
    Running,
    Degraded,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessState {
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub started_at: Option<u64>,
    pub exit_code: Option<i32>,
}

impl ProcessState {
    pub fn stopped() -> Self {
        Self {
            pid: None,
            status: ProcessStatus::Stopped,
            started_at: None,
            exit_code: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// Severity of a launcher-originated log line. DSH's own streamed stdout/stderr
/// map onto `Info`/`Warn`; launcher bookkeeping (proxy inject, inventory sync,
/// install queue progress) uses `Debug` so Activity can hide the noise by
/// default and surface only actionable errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub stream: LogStream,
    #[serde(default)]
    pub level: LogLevel,
    pub line: String,
}

/// Callback invoked once per streamed line. Clone-heavy on purpose: each read
/// task holds its own copy.
pub type LogSink = Arc<dyn Fn(LogLine) + Send + Sync>;
pub type ExitSink = Arc<dyn Fn(ProcessState) + Send + Sync>;

/// A Windows Job Object that kills its whole process tree on `terminate()` —
/// and, as a belt-and-suspenders, whenever the last handle closes
/// (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). This is the primary tree-killer:
/// node, pnpm, and every grandchild die together, no matter how deep.
#[cfg(windows)]
struct WindowsJob {
    _handle: OwnedHandle,
}

#[cfg(windows)]
impl WindowsJob {
    fn new() -> Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(anyhow!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<core::ffi::c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                let _ = CloseHandle(job);
                return Err(anyhow!("SetInformationJobObject failed: {e}"));
            }
            Ok(Self {
                _handle: OwnedHandle::from_raw_handle(job),
            })
        }
    }

    fn assign(&self, process: RawHandle) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        unsafe {
            if AssignProcessToJobObject(self._handle.as_raw_handle(), process) == 0 {
                return Err(anyhow!(
                    "AssignProcessToJobObject failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(())
    }

    fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            let _ = TerminateJobObject(self._handle.as_raw_handle(), 1);
        }
    }
}

/// Recursive `taskkill /T /F` — the fallback for trees that escaped the job
/// object (e.g. assignment failed) or are reaped outside any live handle.
#[cfg(windows)]
pub fn kill_tree(pid: u32) {
    let out = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();
    match out {
        Ok(o) => tracing::debug!(
            pid,
            code = o.status.code(),
            stderr = %String::from_utf8_lossy(&o.stderr).trim(),
            "kill_tree"
        ),
        Err(e) => tracing::debug!(pid, error = %e, "kill_tree could not run taskkill"),
    }
}

#[cfg(not(windows))]
pub fn kill_tree(pid: u32) {
    // The child is spawned as its own process-group leader (`process_group(0)`
    // in spawn_child), so its pgid == pid. killpg(SIGKILL) reaps the whole
    // tree — node, pnpm, corepack, and every grandchild — no matter how deep.
    unsafe {
        let _ = libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// True if a process with this PID is currently alive (best-effort probe).
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let _ = CloseHandle(h);
        true
    }
}

#[cfg(not(windows))]
pub fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) succeeds (errno != ESRCH) iff a process with that pid
    // exists — including a zombie not yet reaped. Best-effort, like the
    // Windows OpenProcess probe.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// One harness tree recorded in the ledger: the spawned process, plus the
/// launcher process that spawned it.
///
/// `owner` is what makes the pre-launch sweep safe with more than one launcher
/// running. The ledger is a single file shared by every launcher on the data
/// root, so "PID is alive" alone cannot distinguish a tree orphaned by a
/// crashed launcher from one a *different, still-running* launcher is actively
/// managing. Entries whose owner is alive are left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub pid: u32,
    /// PID of the launcher that spawned `pid`, or `None` for a line written by
    /// a launcher older than this schema (which stored a bare PID). A `None`
    /// owner cannot be claimed by anyone, so such leftovers are always reaped —
    /// the sweep keeps working across the upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<u32>,
}

/// Persistent record of every harness tree the launcher has spawned. Written on
/// launch, swept on the next launch, so a hard-killed launcher (or a crash) can
/// never leave an orphaned harness tree behind — the "startup zombie sweep".
///
/// One JSON entry per line. Older builds wrote a bare PID per line; those still
/// parse (with an unknown owner) so an upgraded launcher reaps them too.
#[derive(Debug, Clone)]
pub struct PidLedger {
    path: PathBuf,
}

impl PidLedger {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    /// Record a harness tree this launcher just spawned. The owner is this
    /// process — by definition alive — so a sweep in another launcher will skip
    /// it.
    pub fn record(&self, pid: u32) {
        self.record_entry(LedgerEntry {
            pid,
            owner: Some(std::process::id()),
        });
    }

    /// Append an entry if its `pid` is not already recorded.
    pub fn record_entry(&self, entry: LedgerEntry) {
        let mut entries = self.read();
        if !entries.iter().any(|e| e.pid == entry.pid) {
            entries.push(entry);
        }
        self.write(&entries);
    }

    pub fn read(&self) -> Vec<LedgerEntry> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content.lines().filter_map(parse_ledger_line).collect()
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    fn write(&self, entries: &[LedgerEntry]) {
        if entries.is_empty() {
            self.clear();
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let content = entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&self.path, content);
    }
}

/// Parse one ledger line. Current format is a JSON `LedgerEntry`; a bare PID is
/// accepted as pre-ownership format so a leftover tree recorded by an older
/// launcher is still reaped rather than silently forgotten.
fn parse_ledger_line(line: &str) -> Option<LedgerEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(line) {
        return Some(entry);
    }
    line.parse::<u32>().ok().map(|pid| LedgerEntry { pid, owner: None })
}

/// Reap the harness trees this launcher's *predecessors* left behind. Returns
/// how many trees were killed. Runs before every launch.
///
/// An entry is reaped only when both hold:
/// 1. its owner launcher is gone — otherwise a second launcher would kill a
///    healthy tree the first one is still managing (`LedgerEntry::owner`); and
/// 2. the spawned PID is still alive.
///
/// Entries that survive the sweep (owner still running) are written back — the
/// ledger is shared, so clearing it would drop another launcher's bookkeeping.
///
/// Cross-platform: the per-PID probe (`pid_alive`) and tree-kill (`kill_tree`)
/// are cfg'd per OS, the sweep loop is not.
pub fn sweep_leftover(ledger: &PidLedger) -> usize {
    let entries = ledger.read();
    let total = entries.len();
    let mut swept = 0;
    let mut keep = Vec::new();
    for entry in entries {
        // A live owner is still managing this tree, whoever we are — leave it
        // alone. `None` (pre-ownership format) can never be claimed.
        if entry.owner.is_some_and(pid_alive) {
            keep.push(entry);
            continue;
        }
        if pid_alive(entry.pid) {
            tracing::warn!(
                pid = entry.pid,
                owner = ?entry.owner,
                "reaping leftover harness tree from a previous session"
            );
            kill_tree(entry.pid);
            swept += 1;
        }
    }
    // Rewrite only when something was dropped; otherwise the file is already
    // what `keep` would write.
    if keep.len() != total {
        ledger.write(&keep);
    }
    swept
}

/// A spawned harness process. Holding the handle keeps the process alive;
/// dropping it (or calling `stop`) asks the watcher to terminate the child.
pub struct ChildHandle {
    pub pid: u32,
    state: Arc<Mutex<ProcessState>>,
    kill_tx: mpsc::Sender<()>,
    /// Keeps the Job Object handle alive for the life of the handle so
    /// `KILL_ON_JOB_CLOSE` stays armed; the watcher takes it to terminate.
    #[cfg(windows)]
    _job: Option<Arc<Mutex<Option<WindowsJob>>>>,
    _watcher: JoinHandle<()>,
}

impl ChildHandle {
    pub fn state(&self) -> ProcessState {
        match self.state.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => ProcessState::stopped(),
        }
    }

    pub fn set_status(&self, status: ProcessStatus) {
        if let Ok(mut guard) = self.state.lock() {
            guard.status = status;
        }
    }

    /// Ask the watcher to kill the child and wait for it to reap.
    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.kill_tx.send(()).await;
        let _ = (&mut self._watcher).await;
        Ok(())
    }
}

/// Spawn `cmd`, wire up log streaming + exit watching, and hand back a handle.
pub async fn spawn_child(cmd: tokio::process::Command, on_log: LogSink) -> Result<ChildHandle> {
    spawn_child_with_exit(cmd, on_log, None).await
}

/// Spawn with an optional backend-owned lifecycle callback. This lets GUI
/// shells subscribe to process exits without front-end polling.
pub async fn spawn_child_with_exit(
    mut cmd: tokio::process::Command,
    on_log: LogSink,
    on_exit: Option<ExitSink>,
) -> Result<ChildHandle> {
    #[cfg(windows)]
    let job = match WindowsJob::new() {
        Ok(j) => {
            tracing::debug!("job object armed");
            Some(Arc::new(Mutex::new(Some(j))))
        }
        Err(e) => {
            tracing::warn!(error = %e, "job object unavailable — falling back to taskkill /T");
            None
        }
    };

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        };
        // This GUI process has no console, so a console-subsystem child (node
        // running DSH, taskkill fallback, …) would otherwise get its own brand
        // new terminal window on every launch. Suppress it — DSH renders in the
        // launcher's own window and its stdout/stderr are piped to the log.
        let mut flags = CREATE_NO_WINDOW;
        if job.is_some() {
            flags |= CREATE_NEW_PROCESS_GROUP;
        }
        cmd.creation_flags(flags);
    }

    // Own process group so kill_tree can killpg(-pgid) the whole tree on Unix
    // (Windows uses the Job Object above instead).
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child: Child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to spawn process: {e}"))?;

    let pid = child.id().unwrap_or(0);

    #[cfg(windows)]
    if let Some(job) = &job {
        if let Ok(mut guard) = job.lock() {
            if let Some(j) = guard.as_ref() {
                if let Some(raw) = child.raw_handle() {
                    if let Err(e) = j.assign(raw) {
                        tracing::warn!(
                            error = %e, pid,
                            "could not assign to job — falling back to taskkill /T"
                        );
                        *guard = None;
                    }
                }
            }
        }
    }

    let state = Arc::new(Mutex::new(ProcessState {
        pid: Some(pid),
        status: ProcessStatus::Starting,
        started_at: Some(now_secs()),
        exit_code: None,
    }));

    if let Some(out) = child.stdout.take() {
        let sink = on_log.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(out);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let line = buf.trim_end_matches(['\r', '\n']).to_string();
                        if !line.is_empty() {
                            sink(LogLine {
                                stream: LogStream::Stdout,
                                level: LogLevel::Info,
                                line,
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let sink = on_log.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(err);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let line = buf.trim_end_matches(['\r', '\n']).to_string();
                        if !line.is_empty() {
                            sink(LogLine {
                                stream: LogStream::Stderr,
                                level: LogLevel::Warn,
                                line,
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);
    let wstate = state.clone();
    let exit_sink = on_exit.clone();
    #[cfg(windows)]
    let wjob = job.clone();
    let watcher = tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = kill_rx.recv() => {
                    // kill_tx dropped (handle dropped) or explicit stop — terminate.
                    #[cfg(windows)]
                    if let Some(job) = wjob.as_ref() {
                        if let Some(j) = job.lock().ok().and_then(|mut g| g.take()) {
                            j.terminate();
                        }
                    }
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    #[cfg(windows)]
                    kill_tree(pid);
                    if let Ok(mut s) = wstate.lock() {
                        s.exit_code = None;
                        s.status = ProcessStatus::Stopped;
                    }
                    if let Some(sink) = exit_sink.as_ref() {
                        sink(wstate.lock().map(|s| s.clone()).unwrap_or_else(|_| ProcessState::stopped()));
                    }
                    break;
                }
                _ = tick.tick() => {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if let Ok(mut s) = wstate.lock() {
                                s.exit_code = status.code();
                                if matches!(s.status, ProcessStatus::Starting | ProcessStatus::Running) {
                                    s.status = ProcessStatus::Crashed;
                                } else {
                                    s.status = ProcessStatus::Stopped;
                                }
                            }
                            if let Some(sink) = exit_sink.as_ref() {
                                sink(wstate.lock().map(|s| s.clone()).unwrap_or_else(|_| ProcessState::stopped()));
                            }
                            break;
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            }
        }
    });

    Ok(ChildHandle {
        pid,
        state,
        kill_tx,
        #[cfg(windows)]
        _job: job,
        _watcher: watcher,
    })
}

/// Poll a TCP port until it accepts a connection or the timeout elapses.
/// Used to detect "the web server is actually up" after spawning.
pub async fn wait_for_port(port: u16, timeout: Duration) -> bool {
    use tokio::net::TcpStream;

    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    false
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    async fn wait_dead(pid: u32, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if !pid_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Spawn a process, let it exit, and return its now-dead PID — an owner
    /// that is guaranteed not to be alive.
    async fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("node")
            .args(["-e", ""])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn short-lived process");
        let pid = child.id();
        child.wait().expect("wait");
        // `wait` reaps the child but the `Child` still holds the process
        // handle, which pins the process object and makes `OpenProcess` keep
        // succeeding — `pid_alive` would report a dead process as alive.
        drop(child);
        assert!(
            wait_dead(pid, Duration::from_secs(5)).await,
            "{pid} should be dead once its handle is closed"
        );
        pid
    }

    #[test]
    fn pid_ledger_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-rt-{}", std::process::id()));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record(1001);
        ledger.record(1002);
        ledger.record(1001); // dedup
        let owner = std::process::id();
        assert_eq!(
            ledger.read(),
            vec![
                LedgerEntry { pid: 1001, owner: Some(owner) },
                LedgerEntry { pid: 1002, owner: Some(owner) },
            ]
        );
        ledger.clear();
        assert!(ledger.read().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    /// A ledger written by a launcher predating ownership records still parses,
    /// with an unknown owner — so its leftovers survive the upgrade long enough
    /// to be reaped instead of being silently dropped.
    #[test]
    fn pid_ledger_reads_legacy_bare_pid_lines() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-legacy-{}", std::process::id()));
        std::fs::write(&tmp, "1234\n5678\n").expect("seed legacy ledger");
        let ledger = PidLedger::open(tmp.clone());
        assert_eq!(
            ledger.read(),
            vec![
                LedgerEntry { pid: 1234, owner: None },
                LedgerEntry { pid: 5678, owner: None },
            ]
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// The P1 acceptance contract: 10 consecutive spawn→stop cycles and not a
    /// single grandchild survives — the Job Object (or the taskkill fallback)
    /// tears the whole tree down every time.
    #[tokio::test(flavor = "multi_thread")]
    async fn job_kills_whole_tree_10_rounds() {
        const SCRIPT: &str = r#"const {spawn}=require('child_process');
const g=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{detached:true,stdio:'ignore'});
console.log('PIDS '+process.pid+' '+g.pid);
setInterval(()=>{},1000);"#;

        for round in 1..=10 {
            let lines = Arc::new(Mutex::new(Vec::<String>::new()));
            let sink_lines = lines.clone();
            let on_log: LogSink = Arc::new(move |l| {
                if let Ok(mut v) = sink_lines.lock() {
                    v.push(l.line);
                }
            });
            let mut cmd = tokio::process::Command::new("node");
            cmd.arg("-e")
                .arg(SCRIPT)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut handle = spawn_child(cmd, on_log).await.expect("spawn_child");

            // Node prints `PIDS <parent> <grandchild>`; grab both.
            let (parent, grandchild) = {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let parsed = {
                        let v = lines.lock().expect("lock");
                        v.iter().find_map(|l| {
                            let rest = l.strip_prefix("PIDS ")?;
                            let mut it = rest.split_whitespace();
                            let p: u32 = it.next()?.parse().ok()?;
                            let g: u32 = it.next()?.parse().ok()?;
                            Some((p, g))
                        })
                    };
                    if let Some(pair) = parsed {
                        break pair;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "round {round}: no `PIDS` line within 10s"
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            assert_eq!(parent, handle.pid, "round {round}: parent pid mismatch");

            handle.stop().await.expect("stop");

            assert!(
                wait_dead(parent, Duration::from_secs(5)).await,
                "round {round}: parent {parent} survived stop"
            );
            assert!(
                wait_dead(grandchild, Duration::from_secs(5)).await,
                "round {round}: grandchild {grandchild} survived the job kill — tree not torn down"
            );
        }
    }

    /// A process that escaped the job (spawned outside it, e.g. by a previous
    /// launcher session) must be reaped by the pre-launch ledger sweep.
    #[tokio::test(flavor = "multi_thread")]
    async fn sweep_kills_stale_ledger_pid() {
        let child = std::process::Command::new("node")
            .args(["-e", "setInterval(()=>{},1000)"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn survivor");
        let pid = child.id();
        // Drop our handle immediately: std::process::Child doesn't kill on drop,
        // but an open handle would pin the process object as a zombie after the
        // sweep kills it, so pid_alive would keep returning true.
        drop(child);
        assert!(pid_alive(pid), "survivor {pid} should be alive");

        let tmp = std::env::temp_dir().join(format!("ahl-ledger-sweep-{pid}"));
        let ledger = PidLedger::open(tmp.clone());
        // The owner is gone too: that is what makes this a leftover rather than
        // a tree someone is still managing.
        let owner = dead_pid().await;
        ledger.record_entry(LedgerEntry { pid, owner: Some(owner) });
        assert_eq!(ledger.read().len(), 1);

        let swept = sweep_leftover(&ledger);
        assert_eq!(swept, 1, "sweep should reap exactly the stale pid");
        assert!(
            wait_dead(pid, Duration::from_secs(5)).await,
            "sweep did not kill {pid}"
        );
        assert!(
            ledger.read().is_empty(),
            "ledger should be cleared after the sweep"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// The two-launcher contract: launcher A is running a healthy DSH tree;
    /// launcher B starts, sweeps the shared ledger, and must leave A's tree
    /// alone — `pid_alive` alone cannot tell the two apart, the owner can.
    #[tokio::test(flavor = "multi_thread")]
    async fn sweep_spares_tree_of_live_owner() {
        let child = std::process::Command::new("node")
            .args(["-e", "setInterval(()=>{},1000)"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn healthy tree");
        let pid = child.id();
        // Deliberately left running: the whole point is that the sweep must
        // spare it. Killed by hand at the end instead.
        drop(child);

        // This test process stands in for the launcher that owns it: alive for
        // the whole sweep, which is the point.
        let owner = std::process::id();

        let tmp = std::env::temp_dir().join(format!("ahl-ledger-sweep-live-{pid}"));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record_entry(LedgerEntry { pid, owner: Some(owner) });

        let swept = sweep_leftover(&ledger);
        assert_eq!(swept, 0, "sweep must not reap a live owner's tree");
        assert!(
            pid_alive(pid),
            "sweep killed {pid}, which a live launcher still owns"
        );
        assert_eq!(
            ledger.read(),
            vec![LedgerEntry { pid, owner: Some(owner) }],
            "the owning launcher's ledger entry must survive another launcher's sweep"
        );

        // Cleanup: this one IS ours to kill.
        kill_tree(pid);
        let _ = wait_dead(pid, Duration::from_secs(5)).await;
        let _ = std::fs::remove_file(&tmp);
    }
}
