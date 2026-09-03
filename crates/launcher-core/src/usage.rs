//! Usage ledger — durable request-level token accounting.
//!
//! DSH owns the actual model calls, so the launcher records usage when a
//! runtime reports OpenAI-compatible `usage` payloads or token summary lines.
//! The schema is intentionally request-shaped so a later direct DSH integration
//! can write the same rows without changing the UI.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::now_secs;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: i64,
    pub instance_id: String,
    pub timestamp: u64,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub api_key_alias: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUsageRecord {
    pub instance_id: String,
    pub timestamp: Option<u64>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: Option<u64>,
    pub cost: Option<f64>,
    pub api_key_alias: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub records: Vec<UsageRecord>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u64,
    pub total_cost: f64,
    pub by_hour: Vec<UsageBucket>,
    pub by_day: Vec<UsageBucket>,
    pub by_model: Vec<UsageModelTotal>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub timestamp: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelTotal {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub cost: f64,
}

pub struct UsageLedger {
    conn: Mutex<Connection>,
}

impl UsageLedger {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(db_path).map_err(|e| anyhow!("open usage db: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_records (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id    TEXT    NOT NULL,
                timestamp      INTEGER NOT NULL,
                model          TEXT    NOT NULL,
                input_tokens   INTEGER NOT NULL,
                output_tokens  INTEGER NOT NULL,
                total_tokens   INTEGER NOT NULL,
                cost           REAL    NOT NULL,
                api_key_alias  TEXT    NOT NULL,
                request_id     TEXT,
                UNIQUE(instance_id, request_id)
                    ON CONFLICT IGNORE
            );
            CREATE INDEX IF NOT EXISTS idx_usage_records_time
                ON usage_records (timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_usage_records_instance_time
                ON usage_records (instance_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_usage_records_model_time
                ON usage_records (model, timestamp DESC);",
        )
        .map_err(|e| anyhow!("init usage schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn record(&self, record: NewUsageRecord) -> Result<Option<UsageRecord>> {
        let total = record
            .total_tokens
            .unwrap_or(record.input_tokens + record.output_tokens);
        let cost = record.cost.unwrap_or_else(|| estimate_cost(total));
        let timestamp = record.timestamp.unwrap_or_else(now_secs);
        let conn = self.conn.lock().map_err(|_| anyhow!("usage lock poisoned"))?;
        conn.execute(
            "INSERT INTO usage_records
                (instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                record.instance_id,
                timestamp,
                record.model,
                record.input_tokens,
                record.output_tokens,
                total,
                cost,
                record.api_key_alias,
                record.request_id,
            ],
        )
        .map_err(|e| anyhow!("record usage: {e}"))?;
        if conn.changes() == 0 {
            return Ok(None);
        }
        Ok(Some(self.get_locked(&conn, conn.last_insert_rowid())?))
    }

    pub fn recent(&self, instance_id: Option<&str>, limit: usize) -> Result<Vec<UsageRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("usage lock poisoned"))?;
        let sql = if instance_id.is_some() {
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE instance_id = ?1 ORDER BY timestamp DESC LIMIT ?2"
        } else {
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records ORDER BY timestamp DESC LIMIT ?1"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| anyhow!("prepare usage recent: {e}"))?;
        let mut rows = if let Some(id) = instance_id {
            stmt.query(rusqlite::params![id, limit as i64])
        } else {
            stmt.query(rusqlite::params![limit as i64])
        }
        .map_err(|e| anyhow!("query usage recent: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| anyhow!("read usage row: {e}"))? {
            out.push(read_record(row)?);
        }
        Ok(out)
    }

    pub fn summary(
        &self,
        instance_id: Option<&str>,
        model: Option<&str>,
        api_key_alias: Option<&str>,
        from: u64,
        to: u64,
    ) -> Result<UsageSummary> {
        let from = if from == 0 {
            self.earliest_timestamp(instance_id, model, api_key_alias)?.unwrap_or(to)
        } else {
            from
        };
        let records = self.range(instance_id, model, api_key_alias, from, to, 1_000)?;
        let mut by_hour = bucketize(&records, 3_600, from, to);
        let by_day = bucketize(&records, 86_400, from, to);
        if by_hour.len() > 48 {
            by_hour = by_hour.split_off(by_hour.len() - 48);
        }
        let mut models = std::collections::BTreeMap::<String, UsageModelTotal>::new();
        let mut input_tokens = 0;
        let mut output_tokens = 0;
        let mut total_tokens = 0;
        let mut total_cost = 0.0;
        for r in &records {
            input_tokens += r.input_tokens;
            output_tokens += r.output_tokens;
            total_tokens += r.total_tokens;
            total_cost += r.cost;
            let entry = models.entry(r.model.clone()).or_insert(UsageModelTotal {
                model: r.model.clone(),
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                requests: 0,
                cost: 0.0,
            });
            entry.input_tokens += r.input_tokens;
            entry.output_tokens += r.output_tokens;
            entry.total_tokens += r.total_tokens;
            entry.requests += 1;
            entry.cost += r.cost;
        }
        let mut by_model: Vec<_> = models.into_values().collect();
        by_model.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
        Ok(UsageSummary {
            requests: records.len() as u64,
            records,
            input_tokens,
            output_tokens,
            total_tokens,
            total_cost,
            by_hour,
            by_day,
            by_model,
        })
    }

    fn earliest_timestamp(
        &self,
        instance_id: Option<&str>,
        model: Option<&str>,
        api_key_alias: Option<&str>,
    ) -> Result<Option<u64>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("usage lock poisoned"))?;
        let sql = match (instance_id.is_some(), model.is_some(), api_key_alias.is_some()) {
            (true, true, true) => "SELECT MIN(timestamp) FROM usage_records WHERE instance_id = ?1 AND model = ?2 AND api_key_alias = ?3",
            (true, true, false) => "SELECT MIN(timestamp) FROM usage_records WHERE instance_id = ?1 AND model = ?2",
            (true, false, true) => "SELECT MIN(timestamp) FROM usage_records WHERE instance_id = ?1 AND api_key_alias = ?2",
            (false, true, true) => "SELECT MIN(timestamp) FROM usage_records WHERE model = ?1 AND api_key_alias = ?2",
            (true, false, false) => "SELECT MIN(timestamp) FROM usage_records WHERE instance_id = ?1",
            (false, true, false) => "SELECT MIN(timestamp) FROM usage_records WHERE model = ?1",
            (false, false, true) => "SELECT MIN(timestamp) FROM usage_records WHERE api_key_alias = ?1",
            (false, false, false) => "SELECT MIN(timestamp) FROM usage_records",
        };
        let mut stmt = conn.prepare(sql).map_err(|e| anyhow!("prepare usage min: {e}"))?;
        let value: Option<u64> = match (instance_id, model, api_key_alias) {
            (Some(i), Some(m), Some(p)) => stmt.query_row(rusqlite::params![i, m, p], |r| r.get(0)),
            (Some(i), Some(m), None) => stmt.query_row(rusqlite::params![i, m], |r| r.get(0)),
            (Some(i), None, Some(p)) => stmt.query_row(rusqlite::params![i, p], |r| r.get(0)),
            (None, Some(m), Some(p)) => stmt.query_row(rusqlite::params![m, p], |r| r.get(0)),
            (Some(i), None, None) => stmt.query_row(rusqlite::params![i], |r| r.get(0)),
            (None, Some(m), None) => stmt.query_row(rusqlite::params![m], |r| r.get(0)),
            (None, None, Some(p)) => stmt.query_row(rusqlite::params![p], |r| r.get(0)),
            (None, None, None) => stmt.query_row([], |r| r.get(0)),
        }
        .map_err(|e| anyhow!("query usage min: {e}"))?;
        Ok(value)
    }

    fn range(
        &self,
        instance_id: Option<&str>,
        model: Option<&str>,
        api_key_alias: Option<&str>,
        from: u64,
        to: u64,
        limit: usize,
    ) -> Result<Vec<UsageRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("usage lock poisoned"))?;
        let sql = match (instance_id.is_some(), model.is_some(), api_key_alias.is_some()) {
            (true, true, true) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE instance_id = ?1 AND model = ?2 AND api_key_alias = ?3 AND timestamp >= ?4 AND timestamp < ?5
             ORDER BY timestamp DESC LIMIT ?6",
            (true, true, false) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE instance_id = ?1 AND model = ?2 AND timestamp >= ?3 AND timestamp < ?4
             ORDER BY timestamp DESC LIMIT ?5",
            (true, false, true) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE instance_id = ?1 AND api_key_alias = ?2 AND timestamp >= ?3 AND timestamp < ?4
             ORDER BY timestamp DESC LIMIT ?5",
            (false, true, true) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE model = ?1 AND api_key_alias = ?2 AND timestamp >= ?3 AND timestamp < ?4
             ORDER BY timestamp DESC LIMIT ?5",
            (true, false, false) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE instance_id = ?1 AND timestamp >= ?2 AND timestamp < ?3
             ORDER BY timestamp DESC LIMIT ?4",
            (false, true, false) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE model = ?1 AND timestamp >= ?2 AND timestamp < ?3
             ORDER BY timestamp DESC LIMIT ?4",
            (false, false, true) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE api_key_alias = ?1 AND timestamp >= ?2 AND timestamp < ?3
             ORDER BY timestamp DESC LIMIT ?4",
            (false, false, false) =>
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE timestamp >= ?1 AND timestamp < ?2
             ORDER BY timestamp DESC LIMIT ?3",
        };
        let mut stmt = conn.prepare(sql).map_err(|e| anyhow!("prepare usage range: {e}"))?;
        let mut rows = match (instance_id, model, api_key_alias) {
            (Some(i), Some(m), Some(p)) => stmt.query(rusqlite::params![i, m, p, from, to, limit as i64]),
            (Some(i), Some(m), None) => stmt.query(rusqlite::params![i, m, from, to, limit as i64]),
            (Some(i), None, Some(p)) => stmt.query(rusqlite::params![i, p, from, to, limit as i64]),
            (None, Some(m), Some(p)) => stmt.query(rusqlite::params![m, p, from, to, limit as i64]),
            (Some(i), None, None) => stmt.query(rusqlite::params![i, from, to, limit as i64]),
            (None, Some(m), None) => stmt.query(rusqlite::params![m, from, to, limit as i64]),
            (None, None, Some(p)) => stmt.query(rusqlite::params![p, from, to, limit as i64]),
            (None, None, None) => stmt.query(rusqlite::params![from, to, limit as i64]),
        }
        .map_err(|e| anyhow!("query usage range: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| anyhow!("read usage range row: {e}"))? {
            out.push(read_record(row)?);
        }
        out.sort_by_key(|r| r.timestamp);
        Ok(out)
    }

    fn get_locked(&self, conn: &Connection, id: i64) -> Result<UsageRecord> {
        conn.query_row(
            "SELECT id, instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id
             FROM usage_records WHERE id = ?1",
            rusqlite::params![id],
            read_record,
        )
        .map_err(|e| anyhow!("read inserted usage: {e}"))
    }
}

pub fn estimate_cost(total_tokens: u64) -> f64 {
    total_tokens as f64 * 0.00000028
}

fn bucketize(records: &[UsageRecord], bucket_secs: u64, from: u64, to: u64) -> Vec<UsageBucket> {
    if to <= from {
        return Vec::new();
    }
    let start = from;
    let len = ((to - from + bucket_secs - 1) / bucket_secs) as usize;
    let mut buckets = (0..len)
        .map(|i| UsageBucket {
            timestamp: start + (i as u64 * bucket_secs),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            requests: 0,
            cost: 0.0,
        })
        .collect::<Vec<_>>();
    for record in records {
        if record.timestamp < start {
            continue;
        }
        let idx = ((record.timestamp - start) / bucket_secs) as usize;
        if let Some(bucket) = buckets.get_mut(idx) {
            bucket.input_tokens += record.input_tokens;
            bucket.output_tokens += record.output_tokens;
            bucket.total_tokens += record.total_tokens;
            bucket.requests += 1;
            bucket.cost += record.cost;
        }
    }
    buckets
}

fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<UsageRecord> {
    Ok(UsageRecord {
        id: row.get(0)?,
        instance_id: row.get(1)?,
        timestamp: row.get(2)?,
        model: row.get(3)?,
        input_tokens: row.get(4)?,
        output_tokens: row.get(5)?,
        total_tokens: row.get(6)?,
        cost: row.get(7)?,
        api_key_alias: row.get(8)?,
        request_id: row.get(9)?,
    })
}
