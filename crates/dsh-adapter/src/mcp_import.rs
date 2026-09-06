//! MCP Import — pull an *external* tool's MCP config into the launcher as
//! manifest records (roadmap §7 Phase 3 / §10.2).
//!
//! Claude Desktop (`claude_desktop_config.json`), Cursor (`mcp.json`) and
//! VSCode (`settings.json`) all describe MCP servers with the same per-server
//! shape — `command`/`args`/`env` for stdio, `url`/`headers` for HTTP, plus an
//! optional `type`. They only differ in the container key:
//!
//! - Claude/Cursor: a top-level `mcpServers: { name: {…} }` map.
//! - VSCode: `mcp.servers` nested inside the big settings object (older
//!   `servers` tolerated).
//!
//! So this module is one lenient schema-finder over `serde_json::Value` (these
//! files are external and drift), producing `McpServerRecord`s that install via
//! the same path as a Market MCP (`InstanceManifest::add_mcp` → one
//! `sync_mcp_patch`), with `id = "import:<serverName>"` to keep imports from
//! colliding with catalog `owner/name` ids. A server with neither `command` nor
//! `url` is skipped with a warning rather than failing the whole file.

use std::collections::HashMap;

use launcher_core::McpServerRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{sanitize_server_name, value_needs_token};

/// One server discovered in an external config, shown to the user for review
/// before import. `warning` is a stable code the frontend localizes (e.g.
/// `mcp.import.warnNeedsToken`) — not free-form text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedMcp {
    /// Sanitized `serverName` (the mcp-client `[A-Za-z0-9_-]{1,32}` pattern).
    pub server_name: String,
    /// The ready-to-add connection record (`enabled`, `id = import:<name>`).
    pub record: McpServerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Parse a Claude Desktop or Cursor config: a top-level `mcpServers` object
/// keyed by server name.
pub fn parse_claude_or_cursor(text: &str) -> Result<Vec<ImportedMcp>, String> {
    let root = serde_json::from_str::<Value>(text)
        .map_err(|e| format!("not valid JSON: {e}"))?;
    let servers = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| "no top-level \"mcpServers\" object found".to_string())?;
    Ok(parse_servers_map(servers))
}

/// Parse a VSCode `settings.json`: MCP servers live at `mcp.servers` (older
/// configs use a top-level `servers`).
pub fn parse_vscode_settings(text: &str) -> Result<Vec<ImportedMcp>, String> {
    let root = serde_json::from_str::<Value>(text)
        .map_err(|e| format!("not valid JSON: {e}"))?;
    let servers = root
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(Value::as_object)
        .or_else(|| root.get("servers").and_then(Value::as_object))
        .ok_or_else(|| "no \"mcp.servers\" object found".to_string())?;
    Ok(parse_servers_map(servers))
}

/// Detect the config shape and parse it. Returns `None` when the text holds
/// neither recognized container (caller surfaces a readable error).
pub fn parse_any(text: &str) -> Result<Option<Vec<ImportedMcp>>, String> {
    let root = serde_json::from_str::<Value>(text)
        .map_err(|e| format!("not valid JSON: {e}"))?;
    if let Some(map) = root.get("mcpServers").and_then(Value::as_object) {
        return Ok(Some(parse_servers_map(map)));
    }
    if let Some(map) = root
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(Value::as_object)
        .or_else(|| root.get("servers").and_then(Value::as_object))
    {
        return Ok(Some(parse_servers_map(map)));
    }
    Ok(None)
}

/// Convert each `name → server` pair into an [`ImportedMcp`]. The per-server
/// value is an object of `command`/`args`/`env` and/or `url`/`headers`, plus an
/// optional `type` hint.
fn parse_servers_map(servers: &serde_json::Map<String, Value>) -> Vec<ImportedMcp> {
    let mut out = Vec::new();
    for (name, value) in servers {
        let Some(obj) = value.as_object() else {
            continue;
        };
        out.push(parse_server(name, obj));
    }
    out
}

fn parse_server(name: &str, obj: &serde_json::Map<String, Value>) -> ImportedMcp {
    let server_name = sanitize_server_name(name);
    let record = McpServerRecord {
        id: format!("import:{server_name}"),
        server_name: server_name.clone(),
        ..McpServerRecord::default()
    };
    let mut imported = ImportedMcp {
        server_name,
        record,
        warning: None,
    };

    // Explicit transport hint, when present ("stdio" | "http" | "sse").
    let type_hint = obj.get("type").and_then(Value::as_str).map(|s| s.to_ascii_lowercase());

    // URL endpoint → streamable-http; SSE is approximated (see D3) with a note.
    let url = obj.get("url").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if !url.is_empty() {
        imported.record.transport = "streamable-http".to_string();
        imported.record.url = url.to_string();
        copy_string_map(obj.get("headers").and_then(Value::as_object), &mut imported.record.headers);
        if type_hint.as_deref() == Some("sse") {
            imported.warning = Some("mcp.import.warnSseFolded".to_string());
        }
        warn_token_if_needed(&imported.record, &mut imported.warning);
        return imported;
    }

    // Explicit http/sse without a url can't be reached.
    if matches!(type_hint.as_deref(), Some("http") | Some("sse")) {
        imported.warning = Some("mcp.import.warnNoUrl".to_string());
        return imported;
    }

    // stdio launch.
    let command = obj.get("command").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if command.is_empty() {
        imported.warning = Some("mcp.import.warnNoLaunch".to_string());
        return imported;
    }
    imported.record.command = command.to_string();
    if let Some(args) = obj.get("args").and_then(Value::as_array) {
        imported.record.args = args
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.to_string())
            .collect();
    }
    copy_string_map(obj.get("env").and_then(Value::as_object), &mut imported.record.env);
    warn_token_if_needed(&imported.record, &mut imported.warning);
    imported
}

/// Copy a JSON string→string map into the record's `env`/`headers`, dropping
/// non-string values (externally-authored configs occasionally carry numbers).
fn copy_string_map(src: Option<&serde_json::Map<String, Value>>, dst: &mut HashMap<String, String>) {
    if let Some(map) = src {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                dst.insert(k.clone(), s.to_string());
            }
        }
    }
}

/// Flag a server whose config carries a token-looking env/header value. The
/// value itself is preserved verbatim (runtime/shell expands `${VAR}`), only a
/// review warning is raised — matching the curated catalog's `mcp_needs_token`.
fn warn_token_if_needed(record: &McpServerRecord, warning: &mut Option<String>) {
    let tokenish = record
        .env
        .values()
        .chain(record.headers.values())
        .any(|v| value_needs_token(v));
    if tokenish && warning.is_none() {
        *warning = Some("mcp.import.warnNeedsToken".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_stdio_server_maps_command_and_env() {
        let text = r#"{"mcpServers":{
          "filesystem":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem"],"env":{}},
          "github":{"command":"npx","args":["-y","@modelcontextprotocol/server-github"]}
        }}"#;
        let parsed = parse_claude_or_cursor(text).unwrap();
        assert_eq!(parsed.len(), 2);
        let fs = parsed.iter().find(|m| m.server_name == "filesystem").unwrap();
        assert_eq!(fs.record.id, "import:filesystem");
        assert_eq!(fs.record.transport, "stdio");
        assert_eq!(fs.record.command, "npx");
        assert_eq!(fs.record.args, vec!["-y", "@modelcontextprotocol/server-filesystem"]);
        assert!(fs.warning.is_none());
    }

    #[test]
    fn url_server_becomes_streamable_http() {
        let text = r#"{"mcpServers":{"remote":{"url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${TOKEN}"}}}}"#;
        let parsed = parse_claude_or_cursor(text).unwrap();
        let remote = &parsed[0];
        assert_eq!(remote.record.transport, "streamable-http");
        assert_eq!(remote.record.url, "https://example.com/mcp");
        assert_eq!(remote.record.headers.get("Authorization").unwrap(), "Bearer ${TOKEN}");
        // ${TOKEN} in a header → review warning.
        assert_eq!(remote.warning.as_deref(), Some("mcp.import.warnNeedsToken"));
    }

    #[test]
    fn sse_type_folds_to_streamable_http_with_note() {
        let text = r#"{"servers":{"legacy":{"type":"sse","url":"https://example.com/sse"}}}"#;
        // VSCode settings shape tolerates top-level `servers`.
        let parsed = parse_vscode_settings(text).unwrap();
        assert_eq!(parsed[0].record.transport, "streamable-http");
        assert_eq!(parsed[0].warning.as_deref(), Some("mcp.import.warnSseFolded"));
    }

    #[test]
    fn vscode_settings_nested_mcp_servers() {
        let text = r#"{"window.zoomLevel":1,"mcp":{"servers":{"git":{"command":"npx","args":["mcp-server-git"]}}}}"#;
        let parsed = parse_vscode_settings(text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].server_name, "git");
        assert_eq!(parsed[0].record.command, "npx");
    }

    #[test]
    fn no_launch_server_is_skipped_with_warning() {
        let text = r#"{"mcpServers":{"broken":{"some":"thing"}}}"#;
        let parsed = parse_claude_or_cursor(text).unwrap();
        assert_eq!(parsed[0].warning.as_deref(), Some("mcp.import.warnNoLaunch"));
        assert!(parsed[0].record.command.is_empty());
        assert!(parsed[0].record.url.is_empty());
    }

    #[test]
    fn http_type_without_url_warns() {
        let text = r#"{"mcpServers":{"remote":{"type":"http"}}}"#;
        let parsed = parse_claude_or_cursor(text).unwrap();
        assert_eq!(parsed[0].warning.as_deref(), Some("mcp.import.warnNoUrl"));
    }

    #[test]
    fn parse_any_detects_both_shapes() {
        assert!(parse_any(r#"{"mcpServers":{"a":{"command":"npx"}}}"#).unwrap().is_some());
        assert!(parse_any(r#"{"mcp":{"servers":{"b":{"command":"npx"}}}}"#).unwrap().is_some());
        assert!(parse_any(r#"{"unrelated":1}"#).unwrap().is_none());
    }

    #[test]
    fn non_string_env_values_are_dropped() {
        // The token predicate is value-based (matches `mcp_needs_token`): a
        // `${VAR}` reference in a value trips the review warning, a bare key
        // name does not.
        let text = r#"{"mcpServers":{"n":{"command":"npx","env":{"PORT":8080,"API_KEY":"${API_KEY}"}}}}"#;
        let parsed = parse_claude_or_cursor(text).unwrap();
        assert!(!parsed[0].record.env.contains_key("PORT"), "numbers are dropped");
        assert_eq!(parsed[0].record.env.get("API_KEY").unwrap(), "${API_KEY}");
        assert_eq!(parsed[0].warning.as_deref(), Some("mcp.import.warnNeedsToken"));
    }

    #[test]
    fn bad_json_returns_readable_error() {
        let err = parse_claude_or_cursor("not json").unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
    }

    #[test]
    fn missing_container_errors_clearly() {
        let err = parse_claude_or_cursor(r#"{"foo":1}"#).unwrap_err();
        assert!(err.contains("mcpServers"), "{err}");
    }
}
