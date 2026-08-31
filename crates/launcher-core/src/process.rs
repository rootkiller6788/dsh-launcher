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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub stream: LogStream,
    pub line: String,
}

/// Callback invoked once per streamed line. Clone-heavy on purpose: each read
/// task holds its own copy.
pub type LogSink = Arc<dyn Fn(LogLine) + Send + Sync>;

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
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION)
                    .cast::<core::ffi::c_void>(),
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
pub fn kill_tree(_pid: u32) {}

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
pub fn pid_alive(_pid: u32) -> bool {
    false
}

/// Persistent record of every PID the launcher has spawned. Written on launch,
/// swept on the next launch, so a hard-killed launcher (or a crash) can never
/// leave an orphaned harness tree behind — the "startup zombie sweep".
#[derive(Debug, Clone)]
pub struct PidLedger {
    path: PathBuf,
}

impl PidLedger {
    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append a PID if not already recorded. Best-effort; one PID per line.
    pub fn record(&self, pid: u32) {
        let mut pids = self.read();
        if !pids.contains(&pid) {
            pids.push(pid);
        }
        self.write(&pids);
    }

    pub fn read(&self) -> Vec<u32> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect()
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    fn write(&self, pids: &[u32]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let content = pids
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&self.path, content);
    }
}

/// Kill every previously-recorded PID that is still alive and reset the
/// ledger. Returns how many trees were reaped. Runs before every launch.
#[cfg(windows)]
pub fn sweep_leftover(ledger: &PidLedger) -> usize {
    let mut swept = 0;
    for pid in ledger.read() {
        if pid_alive(pid) {
            tracing::warn!(pid, "reaping leftover harness tree from a previous session");
            kill_tree(pid);
            swept += 1;
        }
    }
    ledger.clear();
    swept
}

#[cfg(not(windows))]
pub fn sweep_leftover(_ledger: &PidLedger) -> usize {
    0
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
pub async fn spawn_child(mut cmd: tokio::process::Command, on_log: LogSink) -> Result<ChildHandle> {
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
    if job.is_some() {
        use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

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
                            sink(LogLine { stream: LogStream::Stdout, line });
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
                            sink(LogLine { stream: LogStream::Stderr, line });
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);
    let wstate = state.clone();
    #[cfg(windows)]
    let wjob = job.clone();
    let watcher = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
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
        tokio::time::sleep(Duration::from_millis(250)).await;
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

    #[test]
    fn pid_ledger_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-rt-{}", std::process::id()));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record(1001);
        ledger.record(1002);
        ledger.record(1001); // dedup
        assert_eq!(ledger.read(), vec![1001, 1002]);
        ledger.clear();
        assert!(ledger.read().is_empty());
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
        ledger.record(pid);
        assert_eq!(ledger.read(), vec![pid]);

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
}
