//! Install job ledger — durable queue + history for Install Center installs.
//!
//! Previously install "jobs" were pure frontend Zustand state with fabricated
//! progress stages. Here each install (market leaf / raw plugin target / skill /
//! MCP / bundle import) is persisted as a row in the same `launcher.db` as the
//! launch history and usage ledger. The backend executor (in the Tauri crate)
//! serializes them per instance, writes real stage transitions, and streams
//! `job-updated` events so a window reload or app restart can recover both the
//! live queue and the terminal history.
//!
//! Retry is possible from a cold start because each row keeps the full install
//! [`JobPlan`] (the `RegistryPlugin` / `BundleManifest` it was created from),
//! not a reference the page still happens to hold.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::bundle::BundleManifest;
use crate::environment::EnvironmentManifest;
use crate::market::{ContentKind, RegistryPlugin};
use crate::now_secs;

/// Terminal statuses are excluded from the live "waiting" queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Waiting,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Waiting | Self::Running)
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What the Install Center badge shows. Mirrors [`ContentKind`]: skin/skill/MCP
/// are their own kinds even though a "market" install drove them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Plugin,
    Theme,
    Skill,
    Mcp,
    Bundle,
    Environment,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Theme => "theme",
            Self::Skill => "skill",
            Self::Mcp => "mcp",
            Self::Bundle => "bundle",
            Self::Environment => "environment",
        }
    }
}

/// The full, self-contained install plan persisted for retry. Kept distinct
/// from the market kinds so the backend dispatcher knows which installer to
/// call: `Market`/`Plugin` are entry-driven leaf installs, `Bundle` imports a
/// whole manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum JobPlan {
    Market {
        entry: RegistryPlugin,
    },
    Plugin {
        target: String,
        entry: Option<RegistryPlugin>,
    },
    Skill {
        entry: RegistryPlugin,
    },
    Mcp {
        entry: RegistryPlugin,
    },
    Bundle {
        manifest: BundleManifest,
    },
    Environment {
        manifest: EnvironmentManifest,
    },
}

impl JobPlan {
    /// Display kind for the job row + Install Center badge.
    pub fn kind(&self) -> JobKind {
        match self {
            Self::Bundle { .. } => JobKind::Bundle,
            Self::Environment { .. } => JobKind::Environment,
            Self::Skill { entry } => job_kind_from_entry(entry),
            Self::Mcp { entry } => job_kind_from_entry(entry),
            Self::Market { entry } => job_kind_from_entry(entry),
            Self::Plugin { entry, .. } => entry
                .as_ref()
                .map(job_kind_from_entry)
                .unwrap_or(JobKind::Plugin),
        }
    }
}

fn job_kind_from_entry(entry: &RegistryPlugin) -> JobKind {
    match entry.kind {
        ContentKind::Theme => JobKind::Theme,
        ContentKind::Skill => JobKind::Skill,
        ContentKind::Mcp => JobKind::Mcp,
        _ => JobKind::Plugin,
    }
}

/// A persisted install job row (wire form, camelCase for the frontend).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub instance_id: String,
    /// Content key (e.g. `owner/name`) the frontend matches a Market card to.
    pub key: String,
    pub kind: JobKind,
    pub label: String,
    pub status: JobStatus,
    /// Current backend stage (download / clone / dsh-install / inventory-sync…).
    pub stage: Option<String>,
    pub progress: i64,
    pub error: Option<String>,
    /// Tail of the most recent sub-process stderr (pnpm / git / dsh).
    pub stderr_tail: Option<String>,
    pub exit_code: Option<i64>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
}

pub struct JobStore {
    conn: Mutex<Connection>,
}

impl JobStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(db_path).map_err(|e| anyhow!("open jobs db: {e}"))?;
        // Three stores (history / usage / jobs) share one SQLite file; a short
        // busy timeout keeps a concurrent usage write from failing an install
        // bookkeeping update.
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| anyhow!("set jobs busy timeout: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS install_jobs (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id  TEXT    NOT NULL,
                key          TEXT    NOT NULL,
                kind         TEXT    NOT NULL,
                label        TEXT    NOT NULL,
                status       TEXT    NOT NULL,
                stage        TEXT,
                progress     INTEGER NOT NULL DEFAULT 0,
                plan         TEXT    NOT NULL,
                error        TEXT,
                stderr_tail  TEXT,
                exit_code    INTEGER,
                created_at   INTEGER NOT NULL,
                started_at   INTEGER,
                finished_at  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_install_jobs_instance
                ON install_jobs (instance_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_install_jobs_status
                ON install_jobs (status, created_at);",
        )
        .map_err(|e| anyhow!("init install_jobs schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a new `waiting` job and return the persisted row.
    pub fn create(&self, instance_id: &str, key: &str, label: &str, plan: &JobPlan) -> Result<Job> {
        let kind = plan.kind();
        let plan_json = serde_json::to_string(plan)?;
        let created_at = now_secs();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "INSERT INTO install_jobs
                (instance_id, key, kind, label, status, progress, plan, created_at)
             VALUES (?1, ?2, ?3, ?4, 'waiting', 0, ?5, ?6)",
            rusqlite::params![
                instance_id,
                key,
                kind.as_str(),
                label,
                plan_json,
                created_at
            ],
        )
        .map_err(|e| anyhow!("create install job: {e}"))?;
        self.get_locked(&conn, conn.last_insert_rowid())
    }

    pub fn get(&self, id: i64) -> Result<Option<Job>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        Self::get_conn(&conn, id)
    }

    /// Atomically take the oldest `waiting` job for an instance and flip it to
    /// `running`. The drainer calls this so two waiters can never double-run a
    /// row; it also stamps `started_at` as the real execution begin.
    pub fn claim_next(&self, instance_id: &str) -> Result<Option<Job>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM install_jobs
                 WHERE instance_id = ?1 AND status = 'waiting'
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                rusqlite::params![instance_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| anyhow!("find waiting job: {e}"))?;
        let Some(id) = id else {
            return Ok(None);
        };
        conn.execute(
            "UPDATE install_jobs
             SET status = 'running', started_at = ?2
             WHERE id = ?1",
            rusqlite::params![id, now_secs()],
        )
        .map_err(|e| anyhow!("claim job {id}: {e}"))?;
        Self::get_conn(&conn, id)
    }

    /// Deserialize the persisted retry plan for a job.
    pub fn plan(&self, id: i64) -> Result<JobPlan> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let json: Option<String> = conn
            .query_row(
                "SELECT plan FROM install_jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| anyhow!("read job plan {id}: {e}"))?;
        let json = json.ok_or_else(|| anyhow!("install job {id} missing"))?;
        serde_json::from_str(&json).map_err(|e| anyhow!("decode job plan {id}: {e}"))
    }

    /// Newest first, bounded (Install Center history list).
    pub fn list(&self, limit: usize) -> Result<Vec<Job>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, instance_id, key, kind, label, status, stage, progress,
                        error, stderr_tail, exit_code, created_at, started_at, finished_at
                 FROM install_jobs ORDER BY created_at DESC, id DESC LIMIT ?1",
            )
            .map_err(|e| anyhow!("prepare jobs list: {e}"))?;
        let mut rows = stmt
            .query(rusqlite::params![limit as i64])
            .map_err(|e| anyhow!("query jobs list: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| anyhow!("read jobs row: {e}"))? {
            out.push(row_to_job(row)?);
        }
        Ok(out)
    }

    /// All non-terminal jobs for one instance, oldest first — the drainer's
    /// FIFO view of what still needs to run.
    pub fn list_waiting(&self, instance_id: &str) -> Result<Vec<Job>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, instance_id, key, kind, label, status, stage, progress,
                        error, stderr_tail, exit_code, created_at, started_at, finished_at
                 FROM install_jobs
                 WHERE instance_id = ?1 AND status = 'waiting'
                 ORDER BY created_at ASC, id ASC",
            )
            .map_err(|e| anyhow!("prepare waiting jobs: {e}"))?;
        let mut rows = stmt
            .query(rusqlite::params![instance_id])
            .map_err(|e| anyhow!("query waiting jobs: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| anyhow!("read waiting row: {e}"))? {
            out.push(row_to_job(row)?);
        }
        Ok(out)
    }

    /// How many rows are still `waiting` for an instance. The executor uses this
    /// under the drainer-marker lock to avoid the lost-wakeup: it re-checks the
    /// count after `claim_next` returns `None` before retiring a drainer task.
    pub fn waiting_count(&self, instance_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_jobs
                 WHERE instance_id = ?1 AND status = 'waiting'",
                rusqlite::params![instance_id],
                |r| r.get(0),
            )
            .map_err(|e| anyhow!("count waiting jobs: {e}"))?;
        Ok(n as usize)
    }

    /// Instance ids with at least one leftover `waiting` job — boot-time resume
    /// uses this to spawn a drainer per instance that was mid-queue on exit.
    pub fn waiting_instance_ids(&self) -> Result<Vec<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT instance_id FROM install_jobs WHERE status = 'waiting'")
            .map_err(|e| anyhow!("prepare waiting instances: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| anyhow!("query waiting instances: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| anyhow!("read waiting instance: {e}"))?
        {
            out.push(row.get(0)?);
        }
        Ok(out)
    }

    pub fn mark_running(&self, id: i64, stage: &str) -> Result<Job> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "UPDATE install_jobs
             SET status = 'running', stage = ?2, started_at = ?3
             WHERE id = ?1",
            rusqlite::params![id, stage, now_secs()],
        )
        .map_err(|e| anyhow!("mark job running: {e}"))?;
        Self::get_conn(&conn, id).map(|j| j.expect("job row just updated"))
    }

    /// Advance stage/progress. Progress is coarse-grained and stage-driven (see
    /// executor); it never ticks on its own.
    pub fn update_progress(&self, id: i64, stage: &str, progress: i64) -> Result<Job> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "UPDATE install_jobs SET stage = ?2, progress = ?3 WHERE id = ?1",
            rusqlite::params![id, stage, progress],
        )
        .map_err(|e| anyhow!("update job progress: {e}"))?;
        Self::get_conn(&conn, id).map(|j| j.expect("job row just updated"))
    }

    /// Append a sub-process stderr line to the job's tail (capped).
    pub fn append_stderr(&self, id: i64, line: &str) -> Result<Job> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT stderr_tail FROM install_jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .map_err(|e| anyhow!("read stderr tail: {e}"))?;
        let mut lines: Vec<String> = existing
            .map(|s| s.split('\n').map(str::to_string).collect())
            .unwrap_or_default();
        lines.push(line.to_string());
        if lines.len() > 8 {
            lines = lines.split_off(lines.len() - 8);
        }
        conn.execute(
            "UPDATE install_jobs SET stderr_tail = ?2 WHERE id = ?1",
            rusqlite::params![id, lines.join("\n")],
        )
        .map_err(|e| anyhow!("update stderr tail: {e}"))?;
        Self::get_conn(&conn, id).map(|j| j.expect("job row just updated"))
    }

    pub fn mark_done(&self, id: i64) -> Result<Job> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "UPDATE install_jobs
             SET status = 'done', progress = 100, finished_at = ?2
             WHERE id = ?1",
            rusqlite::params![id, now_secs()],
        )
        .map_err(|e| anyhow!("mark job done: {e}"))?;
        Self::get_conn(&conn, id).map(|j| j.expect("job row just updated"))
    }

    pub fn mark_failed(&self, id: i64, error: &str, exit_code: Option<i64>) -> Result<Job> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "UPDATE install_jobs
             SET status = 'failed', error = ?2, exit_code = ?3, finished_at = ?4
             WHERE id = ?1",
            rusqlite::params![id, error, exit_code, now_secs()],
        )
        .map_err(|e| anyhow!("mark job failed: {e}"))?;
        Self::get_conn(&conn, id).map(|j| j.expect("job row just updated"))
    }

    /// Cancel a job that has not started yet. Running jobs are not force-killed
    /// (external git/pnpm processes), so this only ever flips `waiting`.
    pub fn cancel_if_waiting(&self, id: i64) -> Result<Option<Job>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "UPDATE install_jobs
             SET status = 'cancelled', finished_at = ?2
             WHERE id = ?1 AND status = 'waiting'",
            rusqlite::params![id, now_secs()],
        )
        .map_err(|e| anyhow!("cancel job: {e}"))?;
        Self::get_conn(&conn, id)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        conn.execute(
            "DELETE FROM install_jobs WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| anyhow!("delete job: {e}"))?;
        Ok(())
    }

    pub fn clear_finished(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("jobs lock poisoned"))?;
        let removed = conn
            .execute(
                "DELETE FROM install_jobs WHERE status IN ('done', 'failed', 'cancelled')",
                [],
            )
            .map_err(|e| anyhow!("clear finished jobs: {e}"))?;
        Ok(removed)
    }

    fn get_conn(conn: &Connection, id: i64) -> Result<Option<Job>> {
        conn.query_row(
            "SELECT id, instance_id, key, kind, label, status, stage, progress,
                    error, stderr_tail, exit_code, created_at, started_at, finished_at
             FROM install_jobs WHERE id = ?1",
            rusqlite::params![id],
            row_to_job,
        )
        .map(Some)
        .map_err(|e| anyhow!("read install job {id}: {e}"))
    }

    fn get_locked(&self, conn: &Connection, id: i64) -> Result<Job> {
        Self::get_conn(conn, id)
            .and_then(|j| j.ok_or_else(|| anyhow!("install job {id} missing after insert")))
    }
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let kind_str: String = row.get(3)?;
    let status_str: String = row.get(5)?;
    Ok(Job {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        key: row.get(2)?,
        kind: parse_kind(&kind_str),
        label: row.get(4)?,
        status: parse_status(&status_str),
        stage: row.get(6)?,
        progress: row.get(7)?,
        error: row.get(8)?,
        stderr_tail: row.get(9)?,
        exit_code: row.get(10)?,
        created_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
    })
}

fn parse_status(s: &str) -> JobStatus {
    match s {
        "running" => JobStatus::Running,
        "done" => JobStatus::Done,
        "failed" => JobStatus::Failed,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Waiting,
    }
}

fn parse_kind(s: &str) -> JobKind {
    match s {
        "theme" => JobKind::Theme,
        "skill" => JobKind::Skill,
        "mcp" => JobKind::Mcp,
        "bundle" => JobKind::Bundle,
        "environment" => JobKind::Environment,
        _ => JobKind::Plugin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch dir cleaned up on drop (no `tempfile` dev-dependency here).
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let unique = format!(
                "jobs-test-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0)
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&dir).expect("create scratch");
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_db() -> (Scratch, JobStore) {
        let dir = Scratch::new();
        let store = JobStore::open(&dir.0.join("test.db")).expect("open store");
        (dir, store)
    }

    #[test]
    fn lifecycle_create_running_done() {
        let (_dir, store) = tmp_db();
        let plan = JobPlan::Skill {
            entry: RegistryPlugin {
                name: "prompt-eng".into(),
                ..Default::default()
            },
        };
        let job = store
            .create("inst-1", "owner/prompt-eng", "owner/prompt-eng", &plan)
            .expect("create");
        assert_eq!(job.status, JobStatus::Waiting);
        assert_eq!(job.progress, 0);

        let running = store.mark_running(job.id, "download").expect("running");
        assert_eq!(running.status, JobStatus::Running);
        assert_eq!(running.stage.as_deref(), Some("download"));

        let mid = store
            .update_progress(job.id, "install", 55)
            .expect("progress");
        assert_eq!(mid.progress, 55);

        let done = store.mark_done(job.id).expect("done");
        assert_eq!(done.status, JobStatus::Done);
        assert_eq!(done.progress, 100);
        assert!(done.finished_at.is_some());
    }

    #[test]
    fn plan_roundtrip() {
        let plan = JobPlan::Market {
            entry: RegistryPlugin {
                owner: "acme".into(),
                name: "toolbox".into(),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&plan).expect("serialize");
        let back: JobPlan = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, JobPlan::Market { .. }));
        assert_eq!(plan.kind(), JobKind::Plugin);
    }

    #[test]
    fn waiting_fifo_and_cancel() {
        let (_dir, store) = tmp_db();
        let plan = JobPlan::Skill {
            entry: RegistryPlugin {
                name: "a".into(),
                ..Default::default()
            },
        };
        let first = store.create("i", "x/a", "x/a", &plan).expect("create a");
        let second = store.create("i", "x/b", "x/b", &plan).expect("create b");
        let waiting = store.list_waiting("i").expect("list waiting");
        assert_eq!(waiting.len(), 2);
        assert_eq!(waiting[0].id, first.id, "FIFO by created order");

        let cancelled = store.cancel_if_waiting(second.id).expect("cancel");
        assert_eq!(cancelled.unwrap().status, JobStatus::Cancelled);
        assert_eq!(store.list_waiting("i").expect("list").len(), 1);

        // A running job must not be cancel-able through this path.
        store.mark_running(first.id, "install").expect("run");
        let none = store.cancel_if_waiting(first.id).expect("try cancel");
        assert!(none.is_none() || none.unwrap().status == JobStatus::Running);
    }
}
