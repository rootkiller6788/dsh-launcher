//! Process supervisor — the boundary between "a GUI wrapper" and "a launcher".
//!
//! `spawn_child` owns the full lifecycle of a managed harness process: streamed
//! stdout/stderr through a `LogSink`, a watcher that reaps on exit, and a kill
//! channel so `ChildHandle::stop()` can ask the watcher to terminate it.

use std::path::{Path, PathBuf};
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

/// Windows `FILETIME` → unix seconds. `FILETIME` counts 100 ns ticks from
/// 1601-01-01; the constant below is that epoch difference.
#[cfg(windows)]
fn filetime_to_unix_secs(ft: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    ticks.saturating_sub(EPOCH_DIFF_100NS) / 10_000_000
}

/// OS-reported creation time of `pid`, in unix seconds — the process's identity,
/// as opposed to `pid_alive`'s "some process holds this number". Two different
/// processes never share a creation time to the second, so an entry whose
/// recorded value no longer matches has had its PID recycled onto a stranger.
///
/// `None` when the process is gone or cannot be queried (permission, or a
/// platform without the probe) — callers treat that as "unverifiable".
#[cfg(windows)]
pub fn pid_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let (mut creation, mut exit, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
            std::mem::zeroed();
        let ok = GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(h);
        (ok != 0).then(|| filetime_to_unix_secs(creation))
    }
}

/// Linux reads the creation time out of `/proc`: field 22 of `stat` is the
/// start time in clock ticks since boot, which `btime` in `/proc/stat` and
/// `_SC_CLK_TCK` convert to unix seconds.
#[cfg(target_os = "linux")]
pub fn pid_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` (field 2) is the process's own name and may contain spaces or
    // parens, so anchor on the last ')' and count from there — field 3 onward.
    let rest = &stat[stat.rfind(')')? + 1..];
    let ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let btime: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (hz > 0).then(|| btime + ticks / hz as u64)
}

/// No portable creation-time probe on this platform (macOS and the BSDs have
/// no `/proc`). Identity checks fall back to a bare liveness probe.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn pid_start_time(_pid: u32) -> Option<u64> {
    None
}

/// True when `pid` is alive *and* is the same process the ledger recorded.
///
/// `recorded` is the creation time captured at spawn. `None` means the entry
/// predates the fingerprint (or it could not be read), and there is nothing to
/// verify against — the caller's old liveness-only behaviour stands.
///
/// A `None` from the *probe* while `recorded` is `Some`, by contrast, is a
/// mismatch: we cannot prove this is our process, and killing a stranger's tree
/// is far worse than leaving one node process behind.
pub fn pid_is_recorded_process(pid: u32, recorded: Option<u64>) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match recorded {
        None => true,
        Some(recorded) => match pid_start_time(pid) {
            Some(actual) if actual == recorded => true,
            other => {
                tracing::warn!(
                    pid,
                    recorded,
                    actual = ?other,
                    "recorded PID now belongs to a different process — leaving it alone"
                );
                false
            }
        },
    }
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
    /// The spawned process's OS-reported creation time, captured at spawn. PIDs
    /// are recycled by the system, so "PID 4321 is alive" is not the same
    /// question as "our process 4321 is alive" — this is what tells them apart
    /// ([`pid_is_recorded_process`]). `None` for entries written before the
    /// fingerprint existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}

/// Cross-process lock over the ledger, held for the duration of one
/// read-modify-write.
///
/// Every mutation of the ledger reads the whole file, edits, and writes it
/// back, so two launchers doing that at once would each clobber the other's
/// rows — losing a record that the zombie sweep exists to keep. The lock lives
/// in a *sidecar* file rather than on the ledger itself because an emptied
/// ledger is deleted, which would take a lock held on it with it.
///
/// Released when dropped, and by the OS if the process dies while holding it.
struct LedgerLock {
    file: std::fs::File,
}

impl LedgerLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Read+write, not append: Windows refuses to lock a handle opened for
        // append only (LockFileEx wants GENERIC_READ/WRITE, and FILE_APPEND_DATA
        // is neither). Never truncated — the file is a rendezvous point, and its
        // contents are meaningless.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock()?;
        Ok(Self { file })
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
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
            // Read now, while the process is unmistakably ours; this is the
            // fingerprint the sweep later checks for.
            created_at: pid_start_time(pid),
        });
    }

    /// Append an entry if its `pid` is not already recorded.
    pub fn record_entry(&self, entry: LedgerEntry) {
        self.mutate(|mut entries| {
            if !entries.iter().any(|e| e.pid == entry.pid) {
                entries.push(entry);
            }
            entries
        });
    }

    /// The ledger's read-modify-write cycle, under the cross-process lock.
    fn mutate(&self, f: impl FnOnce(Vec<LedgerEntry>) -> Vec<LedgerEntry>) {
        let _guard = match self.lock() {
            Ok(guard) => Some(guard),
            Err(e) => {
                // Degrade to an unlocked write rather than dropping the row: an
                // unrecorded tree is one nothing will ever reap.
                tracing::warn!(error = %e, "PID ledger lock unavailable, writing unlocked");
                None
            }
        };
        let entries = f(self.read());
        self.write(&entries);
    }

    /// Take the ledger's cross-process lock. Callers hold it across a whole
    /// read-modify-write; `read` and `write` themselves take nothing.
    fn lock(&self) -> std::io::Result<LedgerLock> {
        LedgerLock::acquire(&self.lock_path())
    }

    /// Sidecar to [`Self::lock`]. `spawned.pids` → `spawned.pids.lock`, so the
    /// two launchers sharing a data root also share the lock.
    fn lock_path(&self) -> PathBuf {
        let mut path = self.path.clone();
        path.set_extension("pids.lock");
        path
    }

    pub fn read(&self) -> Vec<LedgerEntry> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content.lines().filter_map(parse_ledger_line).collect()
    }

    /// Drop this launcher's row for a tree that has stopped. Call it on every
    /// clean stop and every observed exit: the row exists only so a *later*
    /// session can reap what this one failed to clean up, so once the tree is
    /// gone keeping it just means the ledger grows a dead row per launch — and
    /// one this launcher's own sweeps will never take, since its owner (us) is
    /// still alive.
    ///
    /// Only our own row is removed. The ledger is shared with any other
    /// launcher on this data root, and their bookkeeping is not ours to drop.
    pub fn forget(&self, pid: u32) {
        let owner = std::process::id();
        self.mutate(|entries| {
            entries
                .into_iter()
                .filter(|e| !(e.pid == pid && e.owner == Some(owner)))
                .collect()
        });
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
    line.parse::<u32>().ok().map(|pid| LedgerEntry {
        pid,
        owner: None,
        created_at: None,
    })
}

/// Reap the harness trees this launcher's *predecessors* left behind. Returns
/// how many trees were killed. Runs before every launch.
///
/// An entry is reaped only when all three hold:
/// 1. its owner launcher is gone — otherwise a second launcher would kill a
///    healthy tree the first one is still managing (`LedgerEntry::owner`);
/// 2. the PID is still alive; and
/// 3. it is still the *same* process (`LedgerEntry::created_at`) — a PID the
///    system recycled onto an unrelated process must not be killed.
///
/// Entries that survive the sweep (owner still running) are written back — the
/// ledger is shared, so clearing it would drop another launcher's bookkeeping.
///
/// Cross-platform: the per-PID probe (`pid_alive`) and tree-kill (`kill_tree`)
/// are cfg'd per OS, the sweep loop is not.
pub fn sweep_leftover(ledger: &PidLedger) -> usize {
    // The sweep is a read-modify-write too, and it is the writer most likely to
    // race a *second* launcher's `record` — both run at launch. The kills happen
    // under the lock so nobody's row can slip in between the read and the
    // write-back and be silently dropped.
    let _guard = ledger.lock().ok();

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
        if pid_is_recorded_process(entry.pid, entry.created_at) {
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
        let entries = ledger.read();
        assert_eq!(
            entries.iter().map(|e| (e.pid, e.owner)).collect::<Vec<_>>(),
            vec![(1001, Some(owner)), (1002, Some(owner))]
        );
        ledger.clear();
        assert!(ledger.read().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    /// The fingerprint is captured at spawn, from the OS — not from our own
    /// clock — so a later sweep compares like with like. Our own PID is the one
    /// process we can always query.
    #[test]
    fn ledger_records_process_creation_fingerprint() {
        let me = std::process::id();
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-fp-{}", me));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record(me);

        let entries = ledger.read();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].created_at, pid_start_time(me));
        assert!(
            entries[0].created_at.is_some(),
            "own creation time must be queryable"
        );

        // A live process whose fingerprint still matches is ours to kill.
        assert!(pid_is_recorded_process(me, entries[0].created_at));
        // One whose fingerprint does not match belongs to someone else.
        assert!(!pid_is_recorded_process(
            me,
            entries[0].created_at.map(|t| t - 3600)
        ));
        // An entry too old to carry a fingerprint falls back to liveness.
        assert!(pid_is_recorded_process(me, None));

        let _ = std::fs::remove_file(&tmp);
    }

    /// Stopping a tree drops its row, so a launcher that starts and stops all
    /// day does not accumulate dead rows its own sweeps would never take (they
    /// skip entries whose owner — itself — is still alive).
    #[test]
    fn ledger_forgets_only_its_own_rows() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-forget-{}", std::process::id()));
        let ledger = PidLedger::open(tmp.clone());
        let me = std::process::id();
        let theirs = LedgerEntry {
            pid: 4243,
            owner: Some(me + 1), // a second launcher on the same data root
            created_at: None,
        };
        ledger.record(4242); // ours
        ledger.record_entry(theirs.clone());

        // A pid we never recorded is a no-op, not a clobber.
        ledger.forget(9999);
        assert_eq!(ledger.read().len(), 2);

        // Another launcher's row is not ours to drop, even when asked for its
        // exact pid — only our own rows leave the shared ledger.
        ledger.forget(theirs.pid);
        assert_eq!(
            ledger.read(),
            vec![LedgerEntry { pid: 4242, owner: Some(me), created_at: None }, theirs.clone()],
            "another launcher's row must survive our forget"
        );

        ledger.forget(4242);
        assert_eq!(ledger.read(), vec![theirs]);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Two launchers mutate the shared ledger; the lock is what stops one's
    /// read-modify-write from clobbering the other's. File locks are enforced
    /// between open handles, so a second handle here contends exactly as a
    /// second process would.
    #[test]
    fn ledger_lock_is_exclusive_across_writers() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-lock-{}", std::process::id()));
        let ledger = PidLedger::open(tmp.clone());
        let lock_path = ledger.lock_path();
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open lock file")
        };

        let held = LedgerLock::acquire(&lock_path).expect("first acquire");
        assert!(
            open().try_lock().is_err(),
            "a second writer got in while the lock was held"
        );

        drop(held);
        let after = open();
        assert!(
            after.try_lock().is_ok(),
            "the lock was not released when the holder dropped it"
        );

        let _ = std::fs::remove_file(&lock_path);
        let _ = std::fs::remove_file(&tmp);
    }

    /// An emptied ledger is removed rather than left as a blank file — the
    /// sweep's `read` treats both the same, but a launcher that has stopped
    /// everything should leave no trace on disk.
    #[test]
    fn ledger_forget_removes_the_file_when_it_empties() {
        let tmp = std::env::temp_dir().join(format!("ahl-ledger-forget-all-{}", std::process::id()));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record(4242);
        assert!(tmp.exists());

        ledger.forget(4242);
        assert!(ledger.read().is_empty());
        assert!(!tmp.exists(), "an emptied ledger should be removed, not blanked");
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
                LedgerEntry { pid: 1234, owner: None, created_at: None },
                LedgerEntry { pid: 5678, owner: None, created_at: None },
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
        let fingerprint = pid_start_time(pid);
        assert!(fingerprint.is_some(), "cannot fingerprint the survivor");
        // The owner is gone too: that is what makes this a leftover rather than
        // a tree someone is still managing.
        let owner = dead_pid().await;
        ledger.record_entry(LedgerEntry {
            pid,
            owner: Some(owner),
            created_at: fingerprint,
        });
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
        let entry = LedgerEntry {
            pid,
            owner: Some(owner),
            created_at: pid_start_time(pid),
        };
        ledger.record_entry(entry.clone());

        let swept = sweep_leftover(&ledger);
        assert_eq!(swept, 0, "sweep must not reap a live owner's tree");
        assert!(
            pid_alive(pid),
            "sweep killed {pid}, which a live launcher still owns"
        );
        assert_eq!(
            ledger.read(),
            vec![entry],
            "the owning launcher's ledger entry must survive another launcher's sweep"
        );

        // Cleanup: this one IS ours to kill.
        kill_tree(pid);
        let _ = wait_dead(pid, Duration::from_secs(5)).await;
        let _ = std::fs::remove_file(&tmp);
    }

    /// The PID-recycling hazard: our tree died, the system handed its PID to
    /// something unrelated (here, a stand-in node process), and the launcher
    /// that owned it is gone too. A liveness check alone would kill a stranger —
    /// the creation-time fingerprint is what stops it.
    #[tokio::test(flavor = "multi_thread")]
    async fn sweep_spares_recycled_pid() {
        let child = std::process::Command::new("node")
            .args(["-e", "setInterval(()=>{},1000)"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn stand-in process");
        let pid = child.id();
        let actual = pid_start_time(pid).expect("fingerprint the stand-in");
        drop(child);
        // Deliberately left running; killed by hand at the end.

        let tmp = std::env::temp_dir().join(format!("ahl-ledger-recycled-{pid}"));
        let ledger = PidLedger::open(tmp.clone());
        ledger.record_entry(LedgerEntry {
            pid,
            owner: Some(dead_pid().await),
            // A stale fingerprint: this PID is alive, but it is not the process
            // we recorded — which is exactly what a recycled PID looks like.
            created_at: Some(actual - 3600),
        });

        let swept = sweep_leftover(&ledger);
        assert_eq!(swept, 0, "sweep must not kill a process it cannot identify");
        assert!(
            pid_alive(pid),
            "sweep killed {pid}, which the ledger never recorded"
        );
        assert!(
            ledger.read().is_empty(),
            "the unidentifiable entry should be dropped, not retried forever"
        );

        kill_tree(pid);
        let _ = wait_dead(pid, Duration::from_secs(5)).await;
        let _ = std::fs::remove_file(&tmp);
    }
}
