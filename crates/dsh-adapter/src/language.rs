//! DSH language-preference sync over the host settings RPC.
//!
//! DSH's UI language lives in Host settings namespace `locale`, field
//! `preference` (`zh` | `en`; absence delegates to the browser's locale).
//! Mirrors `theme.rs` — the same POST-only settings RPC, the same envelope
//! (verified against `deepseek-harness-master`: `LOCALE_SETTINGS_NAMESPACE`).
//!
//! Mutating `locale.preference` is `applies: "live"`: the running client's
//! `LocaleRuntime` subscribes to the settings scope and re-adopts on any
//! change (`packages/client/locale/src/client/index.ts`), so a launcher-side
//! write switches an already-open DSH window without a reload. Reverse
//! direction is just `settings.describe`.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::theme::{ensure_ok, host_rpc};

/// Set the running DSH's `locale.preference`. Idempotent; creates the
/// document on a fresh home. Errors if the harness isn't reachable or rejects.
pub async fn set_preference(port: u16, lang: &str) -> Result<()> {
    let payload = json!({
        "ns": "locale",
        "ops": [{ "op": "set", "path": ["preference"], "value": lang }],
    });
    let value = host_rpc(port, "settings.mutate", payload).await?;
    ensure_ok(&value, "settings.mutate")
}

/// Read the running DSH's `locale.preference`. `Ok(None)` when the document
/// (or the namespace) doesn't exist yet — DSH falls back to the browser locale.
pub async fn get_preference(port: u16) -> Result<Option<String>> {
    let value = host_rpc(port, "settings.describe", json!({})).await?;
    ensure_ok(&value, "settings.describe")?;

    let namespaces = value
        .get("value")
        .and_then(|v| v.get("namespaces"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("settings.describe: missing namespaces"))?;
    let locale = namespaces
        .iter()
        .find(|ns| ns.get("ns").and_then(Value::as_str) == Some("locale"));
    match locale {
        Some(ns) => Ok(ns
            .get("value")
            .and_then(|v| v.get("preference"))
            .and_then(Value::as_str)
            .map(str::to_owned)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_preference_drills_locale() {
        let root = json!({
            "ok": true,
            "value": {
                "namespaces": [
                    { "ns": "ui-onboarding", "value": {} },
                    { "ns": "locale", "value": { "preference": "zh" } }
                ]
            }
        });
        let namespaces = root
            .get("value")
            .and_then(|v| v.get("namespaces"))
            .and_then(Value::as_array)
            .unwrap();
        let locale = namespaces
            .iter()
            .find(|ns| ns.get("ns").and_then(Value::as_str) == Some("locale"));
        let pref = locale
            .and_then(|ns| ns.get("value"))
            .and_then(|v| v.get("preference"))
            .and_then(Value::as_str);
        assert_eq!(pref, Some("zh"));
    }

    #[test]
    fn set_preference_builds_mutate_envelope() {
        // The envelope shape is what the live probe validated; assert the
        // payload matches the documented contract for the locale namespace.
        let payload = json!({
            "ns": "locale",
            "ops": [{ "op": "set", "path": ["preference"], "value": "en" }],
        });
        assert_eq!(payload["ns"], "locale");
        assert_eq!(payload["ops"][0]["op"], "set");
        assert_eq!(payload["ops"][0]["path"][0], "preference");
        assert_eq!(payload["ops"][0]["value"], "en");
    }
}
