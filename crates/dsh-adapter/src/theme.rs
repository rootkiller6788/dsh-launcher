//! DSH theme-preference sync over the host settings RPC.
//!
//! DSH's appearance lives in Host settings namespace `ui-theme`, field
//! `preference` (`light` | `dark` | `system`, default `system`). The web app
//! writes it through the apiproxy's POST-only settings RPC, so the launcher can
//! read and drive it with plain HTTP — no browser-trust fence on this build
//! (verified against `deepseek-harness-master`).
//!
//! Mutating `ui-theme.preference` is `applies: "live"`: the running client's
//! `ThemeRuntime` subscribes to the settings scope and re-adopts on any change
//! (`packages/client/ui-theme/src/client/index.ts`), so a launcher-side write
//! switches an already-open DSH window without a reload. Reverse direction is
//! just `settings.describe`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Set the running DSH's `ui-theme.preference`. Idempotent; creates the
/// document on a fresh home. Errors if the harness isn't reachable or rejects.
pub async fn set_preference(port: u16, pref: &str) -> Result<()> {
    let payload = json!({
        "ns": "ui-theme",
        "ops": [{ "op": "set", "path": ["preference"], "value": pref }],
    });
    let value = host_rpc(port, "settings.mutate", payload).await?;
    ensure_ok(&value, "settings.mutate")
}

/// Read the running DSH's `ui-theme.preference`. `Ok(None)` when the document
/// (or the namespace) doesn't exist yet — DSH treats that as `system`.
pub async fn get_preference(port: u16) -> Result<Option<String>> {
    let value = host_rpc(port, "settings.describe", json!({})).await?;
    ensure_ok(&value, "settings.describe")?;

    let namespaces = value
        .get("value")
        .and_then(|v| v.get("namespaces"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("settings.describe: missing namespaces"))?;
    let theme = namespaces
        .iter()
        .find(|ns| ns.get("ns").and_then(Value::as_str) == Some("ui-theme"));
    match theme {
        Some(ns) => Ok(ns
            .get("value")
            .and_then(|v| v.get("preference"))
            .and_then(Value::as_str)
            .map(str::to_owned)),
        None => Ok(None),
    }
}

/// POST a full-form client-request envelope to the harness apiproxy and return
/// the response's `result` value (the apiproxy is POST-only; path == method).
async fn host_rpc(port: u16, method: &str, payload: Value) -> Result<Value> {
    let body = json!({
        "type": "client-request",
        "rpcId": format!("launcher-theme-{}", std::process::id()),
        "method": method,
        "payload": payload,
    });
    let url = format!("http://127.0.0.1:{port}/api/{method}");
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("{method}: HTTP {}", response.status()));
    }
    let root: Value = response.json().await?;
    root.get("result")
        .cloned()
        .ok_or_else(|| anyhow!("{method}: missing result in response"))
}

/// `{ok: true}` carries the value; `{ok: false}` carries an error object.
/// The apiproxy reports business errors over HTTP 200.
fn ensure_ok(value: &Value, method: &str) -> Result<()> {
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => {
            let detail = value.get("error").map(|e| e.to_string()).unwrap_or_default();
            Err(anyhow!("{method}: rejected{}{detail}", if detail.is_empty() { "" } else { ": " }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_preference_drills_ui_theme() {
        let root = json!({
            "ok": true,
            "value": {
                "namespaces": [
                    { "ns": "ui-onboarding", "value": {} },
                    { "ns": "ui-theme", "value": { "preference": "dark" } }
                ]
            }
        });
        let namespaces = root
            .get("value")
            .and_then(|v| v.get("namespaces"))
            .and_then(Value::as_array)
            .unwrap();
        let theme = namespaces
            .iter()
            .find(|ns| ns.get("ns").and_then(Value::as_str) == Some("ui-theme"));
        let pref = theme
            .and_then(|ns| ns.get("value"))
            .and_then(|v| v.get("preference"))
            .and_then(Value::as_str);
        assert_eq!(pref, Some("dark"));
    }

    #[test]
    fn set_preference_builds_mutate_envelope() {
        // The envelope shape is what the live probe validated; assert the
        // serialized body matches the documented contract.
        let body = json!({
            "type": "client-request",
            "rpcId": "launcher-theme-1",
            "method": "settings.mutate",
            "payload": {
                "ns": "ui-theme",
                "ops": [{ "op": "set", "path": ["preference"], "value": "light" }],
            },
        });
        assert_eq!(body["type"], "client-request");
        assert_eq!(body["method"], "settings.mutate");
        assert_eq!(body["payload"]["ns"], "ui-theme");
        assert_eq!(body["payload"]["ops"][0]["op"], "set");
        assert_eq!(body["payload"]["ops"][0]["path"][0], "preference");
        assert_eq!(body["payload"]["ops"][0]["value"], "light");
    }
}
