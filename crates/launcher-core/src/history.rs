//! Launch history — the one thing SQLite owns in v0.2.
//!
//! Instance main facts stay in `instance.json` (portable, copyable). The DB is
//! the Index + Cache + History layer plan.md reserves for it; for now that's a
//! single table of launch sessions so the Activity page survives restarts.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use crate::now_secs;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSession {
    pub id: i64,
    pub instance_id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub exit_code: Option<i32>,
    /// `running` | `stopped` | `crashed`
    pub status: String,
}

/// Thread-safe handle over the launcher DB. One connection, serialized via a
/// std mutex — fine for a personal launcher's single-user write rate.
pub struct LaunchHistory {
    conn: Mutex<Connection>,
}

impl LaunchHistory {
    /// Open (or create) the DB at `db_path` and ensure the schema exists.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(db_path).map_err(|e| anyhow!("open history db: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS launch_sessions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id TEXT    NOT NULL,
                started_at  INTEGER NOT NULL,
                ended_at    INTEGER,
                exit_code   INTEGER,
                status      TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_launch_sessions_started
                ON launch_sessions (started_at DESC);",
        )
        .map_err(|e| anyhow!("init history schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a running session and return its row id.
    pub fn record_start(&self, instance_id: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("history lock poisoned"))?;
        conn.execute(
            "INSERT INTO launch_sessions (instance_id, started_at, status) VALUES (?1, ?2, 'running')",
            rusqlite::params![instance_id, now_secs()],
        )
        .map_err(|e| anyhow!("record start: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    /// Close out a running session.
    pub fn record_end(&self, session_id: i64, status: &str, exit_code: Option<i32>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("history lock poisoned"))?;
        conn.execute(
            "UPDATE launch_sessions SET ended_at = ?1, status = ?2, exit_code = ?3 WHERE id = ?4",
            rusqlite::params![now_secs(), status, exit_code, session_id],
        )
        .map_err(|e| anyhow!("record end: {e}"))?;
        Ok(())
    }

    /// Most recent sessions, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<LaunchSession>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("history lock poisoned"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, instance_id, started_at, ended_at, exit_code, status
                      FROM launch_sessions ORDER BY started_at DESC LIMIT ?1",
            )
            .map_err(|e| anyhow!("prepare recent: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(LaunchSession {
                    id: row.get(0)?,
                    instance_id: row.get(1)?,
                    started_at: row.get(2)?,
                    ended_at: row.get(3)?,
                    exit_code: row.get(4)?,
                    status: row.get(5)?,
                })
            })
            .map_err(|e| anyhow!("query recent: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| anyhow!("read recent row: {e}"))?);
        }
        Ok(out)
    }
}
