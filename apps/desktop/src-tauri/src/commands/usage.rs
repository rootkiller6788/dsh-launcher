use launcher_core::{NewUsageRecord, UsageRecord, UsageSummary};
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExportResult {
    pub path: String,
    pub format: String,
    pub records: usize,
}

#[tauri::command]
pub fn usage_recent(
    state: State<'_, AppState>,
    instance_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<UsageRecord>, AppError> {
    Ok(state
        .usage
        .recent(instance_id.as_deref(), limit.unwrap_or(100))?)
}

#[tauri::command]
pub fn usage_summary(
    state: State<'_, AppState>,
    instance_id: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    from: u64,
    to: u64,
) -> Result<UsageSummary, AppError> {
    Ok(state.usage.summary(
        instance_id.as_deref(),
        model.as_deref(),
        provider.as_deref(),
        from,
        to,
    )?)
}

#[tauri::command]
pub fn usage_record(
    state: State<'_, AppState>,
    record: NewUsageRecord,
) -> Result<Option<UsageRecord>, AppError> {
    Ok(state.usage.record(record)?)
}

#[tauri::command]
pub fn usage_export(
    state: State<'_, AppState>,
    instance_id: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    from: u64,
    to: u64,
    format: String,
) -> Result<UsageExportResult, AppError> {
    let summary = state.usage.summary(
        instance_id.as_deref(),
        model.as_deref(),
        provider.as_deref(),
        from,
        to,
    )?;
    let fmt = format.to_ascii_lowercase();
    let ext = if fmt == "csv" { "csv" } else { "json" };
    let path = downloads_dir().join(format!(
        "dsh-usage-{}-{}.{}",
        from,
        launcher_core::now_secs(),
        ext
    ));
    let body = if ext == "csv" {
        usage_csv(&summary.records)
    } else {
        serde_json::to_string_pretty(&summary)
            .map_err(|e| AppError::msg(format!("serialize usage export: {e}")))?
    };
    std::fs::write(&path, body)
        .map_err(|e| AppError::msg(format!("write usage export {}: {e}", path.display())))?;
    Ok(UsageExportResult {
        path: path.display().to_string(),
        format: ext.into(),
        records: summary.records.len(),
    })
}

fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|p| p.join("Downloads"))
        .filter(|p| p.exists())
        .unwrap_or_else(std::env::temp_dir)
}

fn usage_csv(records: &[UsageRecord]) -> String {
    let mut out = String::from(
        "id,instance_id,timestamp,model,input_tokens,output_tokens,total_tokens,cost,cost_known,api_key_alias,request_id\n",
    );
    for record in records {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.8},{},{},{}\n",
            record.id,
            csv_cell(&record.instance_id),
            record.timestamp,
            csv_cell(&record.model),
            record.input_tokens,
            record.output_tokens,
            record.total_tokens,
            record.cost,
            if record.cost_known { 1 } else { 0 },
            csv_cell(&record.api_key_alias),
            csv_cell(record.request_id.as_deref().unwrap_or(""))
        ));
    }
    out
}

fn csv_cell(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    format!("\"{escaped}\"")
}
