//! Usage ledger — durable request-level token accounting.
//!
//! DSH owns the actual model calls, so the launcher records usage when a
//! runtime reports OpenAI-compatible `usage` payloads or token summary lines.
//! The schema is intentionally request-shaped so a later direct DSH integration
//! can write the same rows without changing the UI.
//!
//! Cost is either provider-reported (`usage.cost`) or looked up in the curated
//! [`crate::pricing`] table from `(api_key_alias, model)`. When neither applies
//! the row is stored as *unknown* (`cost = 0`, `cost_known = false`) rather
//! than given a fabricated flat estimate.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use rusqlite::types::ToSql;
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
    /// `true` when `cost` is provider-reported or price-table derived; `false`
    /// means the model could not be priced and `cost` is 0 (unknown).
    #[serde(default)]
    pub cost_known: bool,
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
    pub cost_known_records: u64,
    pub unknown_cost_records: u64,
    pub total_records: u64,
    pub records_truncated: bool,
    pub by_hour: Vec<UsageBucket>,
    pub by_day: Vec<UsageBucket>,
    pub by_model: Vec<UsageModelTotal>,
    pub by_provider: Vec<UsageDimension>,
    pub by_instance: Vec<UsageDimension>,
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

/// A generic dimension aggregate (provider alias / instance).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDimension {
    pub key: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub cost: f64,
}

/// SELECT column list shared by every row read (positional order must match
/// [`read_record`]).
const RECORD_COLS: &str = "id, instance_id, timestamp, model, input_tokens, output_tokens, \
     total_tokens, cost, api_key_alias, request_id, cost_known";

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
                cost_known     INTEGER NOT NULL DEFAULT 0,
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
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn record(&self, record: NewUsageRecord) -> Result<Option<UsageRecord>> {
        let total = record
            .total_tokens
            .unwrap_or(record.input_tokens + record.output_tokens);
        let (cost, cost_known) = match record.cost {
            Some(c) => (c, true),
            None => crate::pricing::cost_for(
                &record.api_key_alias,
                &record.model,
                record.input_tokens,
                record.output_tokens,
            )
            .map(|c| (c, true))
            .unwrap_or((0.0, false)),
        };
        let timestamp = record.timestamp.unwrap_or_else(now_secs);
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("usage lock poisoned"))?;
        conn.execute(
            "INSERT INTO usage_records
                (instance_id, timestamp, model, input_tokens, output_tokens, total_tokens, cost, api_key_alias, request_id, cost_known)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                cost_known as i64,
            ],
        )
        .map_err(|e| anyhow!("record usage: {e}"))?;
        if conn.changes() == 0 {
            return Ok(None);
        }
        Ok(Some(self.get_locked(&conn, conn.last_insert_rowid())?))
    }

    pub fn recent(&self, instance_id: Option<&str>, limit: usize) -> Result<Vec<UsageRecord>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("usage lock poisoned"))?;
        let sql = if instance_id.is_some() {
            format!(
                "SELECT {RECORD_COLS} FROM usage_records WHERE instance_id = ?1
                 ORDER BY timestamp DESC LIMIT ?2"
            )
        } else {
            format!("SELECT {RECORD_COLS} FROM usage_records ORDER BY timestamp DESC LIMIT ?1")
        };
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage recent: {e}"))?;
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
            self.earliest_timestamp(instance_id, model, api_key_alias)?
                .unwrap_or(to)
        } else {
            from
        };
        let (where_sql, params) = build_filter(instance_id, model, api_key_alias, from, to);
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("usage lock poisoned"))?;

        let (
            requests,
            input_tokens,
            output_tokens,
            total_tokens,
            total_cost,
            cost_known_records,
            unknown_cost_records,
        ) = self.totals_locked(&conn, &where_sql, &params)?;

        let by_model: Vec<UsageModelTotal> = self
            .grouped_locked(&conn, "model", &where_sql, &params)?
            .into_iter()
            .map(|(model, agg)| UsageModelTotal {
                model,
                input_tokens: agg.input,
                output_tokens: agg.output,
                total_tokens: agg.total,
                requests: agg.requests,
                cost: agg.cost,
            })
            .collect();
        let by_provider: Vec<UsageDimension> = self
            .grouped_locked(&conn, "api_key_alias", &where_sql, &params)?
            .into_iter()
            .map(|(key, agg)| UsageDimension {
                key,
                input_tokens: agg.input,
                output_tokens: agg.output,
                total_tokens: agg.total,
                requests: agg.requests,
                cost: agg.cost,
            })
            .collect();
        let by_instance: Vec<UsageDimension> = self
            .grouped_locked(&conn, "instance_id", &where_sql, &params)?
            .into_iter()
            .map(|(key, agg)| UsageDimension {
                key,
                input_tokens: agg.input,
                output_tokens: agg.output,
                total_tokens: agg.total,
                requests: agg.requests,
                cost: agg.cost,
            })
            .collect();

        let mut by_hour = fill_buckets(
            self.bucket_locked(&conn, &where_sql, &params, from, 3_600)?,
            3_600,
            from,
            to,
        );
        if by_hour.len() > 48 {
            by_hour = by_hour.split_off(by_hour.len() - 48);
        }
        let by_day = fill_buckets(
            self.bucket_locked(&conn, &where_sql, &params, from, 86_400)?,
            86_400,
            from,
            to,
        );

        let records = self.range_locked(&conn, instance_id, model, api_key_alias, from, to, 200)?;
        let total_records = self.count_locked(&conn, &where_sql, &params)?;

        Ok(UsageSummary {
            records: records.clone(),
            total_tokens,
            input_tokens,
            output_tokens,
            requests,
            total_cost,
            cost_known_records,
            unknown_cost_records,
            total_records,
            records_truncated: (records.len() as u64) < total_records,
            by_hour,
            by_day,
            by_model,
            by_provider,
            by_instance,
        })
    }

    fn earliest_timestamp(
        &self,
        instance_id: Option<&str>,
        model: Option<&str>,
        api_key_alias: Option<&str>,
    ) -> Result<Option<u64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("usage lock poisoned"))?;
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
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| anyhow!("prepare usage min: {e}"))?;
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

    fn totals_locked(
        &self,
        conn: &Connection,
        where_sql: &str,
        params: &[Box<dyn ToSql>],
    ) -> Result<(u64, u64, u64, u64, f64, u64, u64)> {
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
             COALESCE(SUM(total_tokens),0), COALESCE(SUM(cost),0), COALESCE(SUM(cost_known),0), \
             COALESCE(SUM(CASE WHEN cost_known = 0 THEN 1 ELSE 0 END),0) \
             FROM usage_records{where_sql}"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage totals: {e}"))?;
        stmt.query_row(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                    r.get::<_, i64>(3)? as u64,
                    r.get::<_, f64>(4)?,
                    r.get::<_, i64>(5)? as u64,
                    r.get::<_, i64>(6)? as u64,
                ))
            },
        )
        .map_err(|e| anyhow!("query usage totals: {e}"))
    }

    fn grouped_locked(
        &self,
        conn: &Connection,
        group_col: &str,
        where_sql: &str,
        params: &[Box<dyn ToSql>],
    ) -> Result<Vec<(String, GroupAgg)>> {
        let sql = format!(
            "SELECT {group_col}, SUM(input_tokens), SUM(output_tokens), SUM(total_tokens), \
             COUNT(*), SUM(cost) FROM usage_records{where_sql} \
             GROUP BY {group_col} ORDER BY SUM(total_tokens) DESC"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage group: {e}"))?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        GroupAgg {
                            input: r.get::<_, i64>(1)? as u64,
                            output: r.get::<_, i64>(2)? as u64,
                            total: r.get::<_, i64>(3)? as u64,
                            requests: r.get::<_, i64>(4)? as u64,
                            cost: r.get::<_, f64>(5)?,
                        },
                    ))
                },
            )
            .map_err(|e| anyhow!("query usage group: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| anyhow!("read usage group: {e}"))?);
        }
        Ok(out)
    }

    fn bucket_locked(
        &self,
        conn: &Connection,
        where_sql: &str,
        params: &[Box<dyn ToSql>],
        from: u64,
        bucket_secs: u64,
    ) -> Result<Vec<(u64, GroupAgg)>> {
        let sql = format!(
            "SELECT ((timestamp - {from}) / {bucket_secs}) AS b, SUM(input_tokens), \
             SUM(output_tokens), SUM(total_tokens), COUNT(*), SUM(cost) \
             FROM usage_records{where_sql} GROUP BY 1"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage bucket: {e}"))?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        GroupAgg {
                            input: r.get::<_, i64>(1)? as u64,
                            output: r.get::<_, i64>(2)? as u64,
                            total: r.get::<_, i64>(3)? as u64,
                            requests: r.get::<_, i64>(4)? as u64,
                            cost: r.get::<_, f64>(5)?,
                        },
                    ))
                },
            )
            .map_err(|e| anyhow!("query usage bucket: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| anyhow!("read usage bucket: {e}"))?);
        }
        Ok(out)
    }

    fn count_locked(
        &self,
        conn: &Connection,
        where_sql: &str,
        params: &[Box<dyn ToSql>],
    ) -> Result<u64> {
        let sql = format!("SELECT COUNT(*) FROM usage_records{where_sql}");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage count: {e}"))?;
        stmt.query_row(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v as u64)
        .map_err(|e| anyhow!("query usage count: {e}"))
    }

    #[allow(clippy::too_many_arguments)] // filter fields mirror the query's WHERE/range/LIMIT exactly
    fn range_locked(
        &self,
        conn: &Connection,
        instance_id: Option<&str>,
        model: Option<&str>,
        api_key_alias: Option<&str>,
        from: u64,
        to: u64,
        limit: usize,
    ) -> Result<Vec<UsageRecord>> {
        let (where_sql, mut params) = build_filter(instance_id, model, api_key_alias, from, to);
        params.push(Box::new(limit as i64));
        let sql = format!(
            "SELECT {RECORD_COLS} FROM usage_records{where_sql} ORDER BY timestamp DESC LIMIT ?"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| anyhow!("prepare usage range: {e}"))?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(
                params.iter().map(|p| p.as_ref()),
            ))
            .map_err(|e| anyhow!("query usage range: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| anyhow!("read usage range row: {e}"))?
        {
            out.push(read_record(row)?);
        }
        out.sort_by_key(|r| r.timestamp);
        Ok(out)
    }

    fn get_locked(&self, conn: &Connection, id: i64) -> Result<UsageRecord> {
        conn.query_row(
            &format!("SELECT {RECORD_COLS} FROM usage_records WHERE id = ?1"),
            rusqlite::params![id],
            read_record,
        )
        .map_err(|e| anyhow!("read inserted usage: {e}"))
    }
}

/// Add the `cost_known` column to a pre-existing ledger, if missing.
fn migrate(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare(
            "SELECT COUNT(*) FROM pragma_table_info('usage_records') WHERE name = 'cost_known'",
        )?
        .query_row([], |r| r.get::<_, i64>(0))
        .map_err(|e| anyhow!("inspect usage schema: {e}"))?;
    if has == 0 {
        conn.execute(
            "ALTER TABLE usage_records ADD COLUMN cost_known INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| anyhow!("add cost_known column: {e}"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct GroupAgg {
    input: u64,
    output: u64,
    total: u64,
    requests: u64,
    cost: f64,
}

/// Build a dynamic `WHERE timestamp >= ? AND timestamp < ?` filter plus its
/// bound params for the optional instance/model/alias filters.
fn build_filter<'a>(
    instance_id: Option<&'a str>,
    model: Option<&'a str>,
    api_key_alias: Option<&'a str>,
    from: u64,
    to: u64,
) -> (String, Vec<Box<dyn ToSql>>) {
    let mut clauses = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(v) = instance_id {
        clauses.push("instance_id = ?".to_string());
        params.push(Box::new(v.to_string()));
    }
    if let Some(v) = model {
        clauses.push("model = ?".to_string());
        params.push(Box::new(v.to_string()));
    }
    if let Some(v) = api_key_alias {
        clauses.push("api_key_alias = ?".to_string());
        params.push(Box::new(v.to_string()));
    }
    clauses.push("timestamp >= ?".to_string());
    params.push(Box::new(from as i64));
    clauses.push("timestamp < ?".to_string());
    params.push(Box::new(to as i64));
    let where_sql = format!(" WHERE {}", clauses.join(" AND "));
    (where_sql, params)
}

/// Rebuild the full `from..to` bucket span (including empty buckets) from the
/// non-empty buckets returned by the SQL aggregation.
fn fill_buckets(
    grouped: Vec<(u64, GroupAgg)>,
    bucket_secs: u64,
    from: u64,
    to: u64,
) -> Vec<UsageBucket> {
    if to <= from {
        return Vec::new();
    }
    let len = (to - from).div_ceil(bucket_secs) as usize;
    let mut buckets: Vec<UsageBucket> = (0..len)
        .map(|i| UsageBucket {
            timestamp: from + (i as u64 * bucket_secs),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            requests: 0,
            cost: 0.0,
        })
        .collect();
    for (idx, agg) in grouped {
        if let Some(bucket) = buckets.get_mut(idx as usize) {
            bucket.input_tokens = agg.input;
            bucket.output_tokens = agg.output;
            bucket.total_tokens = agg.total;
            bucket.requests = agg.requests;
            bucket.cost = agg.cost;
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
        cost_known: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn open_tmp() -> UsageLedger {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("ahl-usage-test-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        UsageLedger::open(&dir.join("usage.db")).unwrap()
    }

    fn rec(
        instance: &str,
        model: &str,
        alias: &str,
        input: u64,
        output: u64,
        ts: u64,
    ) -> NewUsageRecord {
        NewUsageRecord {
            instance_id: instance.into(),
            timestamp: Some(ts),
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            total_tokens: None,
            cost: None,
            api_key_alias: alias.into(),
            request_id: None,
        }
    }

    #[test]
    fn record_marks_unknown_when_unpriced() {
        let ledger = open_tmp();
        let saved = ledger
            .record(rec("i1", "no-such-model", "acme", 100, 50, 1000))
            .unwrap()
            .unwrap();
        assert!(!saved.cost_known);
        assert_eq!(saved.cost, 0.0);
    }

    #[test]
    fn record_uses_price_table_when_no_provider_cost() {
        let ledger = open_tmp();
        let saved = ledger
            .record(rec(
                "i1",
                "gpt-4o-mini",
                "openai",
                1_000_000,
                1_000_000,
                1000,
            ))
            .unwrap()
            .unwrap();
        assert!(saved.cost_known);
        assert!((saved.cost - 0.75).abs() < 1e-9, "got {}", saved.cost);
    }

    #[test]
    fn record_keeps_provider_cost() {
        let ledger = open_tmp();
        let mut r = rec("i1", "gpt-4o", "openai", 10, 20, 1000);
        r.cost = Some(1.2345);
        let saved = ledger.record(r).unwrap().unwrap();
        assert!(saved.cost_known);
        assert_eq!(saved.cost, 1.2345);
    }

    #[test]
    fn summary_aggregates_beyond_1000_records() {
        let ledger = open_tmp();
        for i in 0..1500u64 {
            let ts = 1_000_000 + i;
            // Alternate two models so by_model still groups meaningfully.
            let model = if i % 2 == 0 { "gpt-4o" } else { "gpt-4o-mini" };
            let _ = ledger.record(rec("i1", model, "openai", 10, 20, ts));
        }
        let summary = ledger
            .summary(Some("i1"), None, None, 0, 1_000_000 + 2000)
            .unwrap();
        // All 1500 rows must be counted, not just the first 1000.
        assert_eq!(summary.requests, 1500);
        assert_eq!(summary.total_records, 1500);
        // The raw list is capped at 200, but aggregates must stay exact.
        assert!(summary.records_truncated);
        assert_eq!(summary.records.len(), 200);
        let by_model_total: u64 = summary.by_model.iter().map(|m| m.total_tokens).sum();
        assert_eq!(by_model_total, 1500 * 30);
    }

    #[test]
    fn summary_records_truncated_when_more_than_200() {
        let ledger = open_tmp();
        for i in 0..250u64 {
            let _ = ledger.record(rec("i1", "gpt-4o", "openai", 1, 1, 1_000_000 + i));
        }
        let summary = ledger
            .summary(Some("i1"), None, None, 0, 1_000_000 + 1000)
            .unwrap();
        assert!(summary.records_truncated);
        assert_eq!(summary.total_records, 250);
        assert_eq!(summary.records.len(), 200);
    }

    #[test]
    fn summary_by_provider_and_instance_breakdown() {
        let ledger = open_tmp();
        let _ = ledger.record(rec("i1", "gpt-4o", "openai", 10, 10, 1000));
        let _ = ledger.record(rec("i2", "deepseek-chat", "deepseek", 10, 10, 1001));
        let summary = ledger.summary(None, None, None, 0, 2000).unwrap();
        assert_eq!(summary.by_provider.len(), 2);
        assert_eq!(summary.by_instance.len(), 2);
        let openai = summary
            .by_provider
            .iter()
            .find(|d| d.key == "openai")
            .unwrap();
        assert_eq!(openai.requests, 1);
        assert!(openai.cost > 0.0);
    }
}
