//! DSH settings-change subscription over the host WebSocket event stream.
//!
//! DSH's browser client receives server events over `ws://…/api/events.host`
//! (a plain HTTP GET is answered `426 upgrade required`). One of the forwarded
//! frames is `host/remote-event` carrying `settings/document-updated`
//! (`args = [namespace, revision]`) — emitted whenever the *raw* user section of
//! any settings namespace changes, whether or not the resolved value moved
//! (`packages/settings/settings/README.md`). The launcher keys appearance off
//! `ui-theme` and language off `locale`, so a change in the DSH window reaches
//! the launcher in one hop instead of the next poll.

use anyhow::Result;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

/// Namespaces whose `settings/document-updated` events the launcher forwards.
const APPEARANCE_NS: &str = "ui-theme";
const LANGUAGE_NS: &str = "locale";

/// Subscribe to the running DSH's host WebSocket event stream and call
/// `on_change(ns)` for every appearance/language document update. Runs until
/// `shutdown` fires. The apiproxy may not be ready when the port first opens and
/// the socket may drop mid-flight, so both the connect and read paths reconnect
/// on a short backoff instead of giving up. `on_change` receives `"ui-theme"` or
/// `"locale"`.
pub async fn watch_settings_changes(
    port: u16,
    mut shutdown: oneshot::Receiver<()>,
    mut on_change: impl FnMut(&str) + Send,
) -> Result<()> {
    let url = format!("ws://127.0.0.1:{port}/api/events.host");

    loop {
        // Connect, retrying while the apiproxy finishes booting. Only `shutdown`
        // breaks this loop — a refused/4xx answer is "not ready yet", not fatal.
        let mut ws = loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                r = tokio_tungstenite::connect_async(&url) => match r {
                    Ok((ws, _)) => break ws,
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                },
            }
        };
        // Read frames; a dropped socket reconnects (unless shutting down).
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    let _ = ws.close(None).await;
                    return Ok(());
                }
                msg = ws.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => handle_frame(&text, &mut on_change),
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        // Binary/ping/pong are not part of this protocol; skip.
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
    }
}

/// Parse one WebSocket text frame, looking for the `host/remote-event` payload
/// that carries `settings/document-updated` and forwarding appearance/language
/// updates. The frame is the `server-request` envelope: `{ payload: { type,
/// event, args } }`.
fn handle_frame(text: &str, on_change: &mut (impl FnMut(&str) + Send)) {
    let Ok(frame) = serde_json::from_str::<Value>(text) else { return };
    let Some(payload) = frame.get("payload") else { return };
    if payload.get("type").and_then(Value::as_str) != Some("host/remote-event") {
        return;
    }
    if payload.get("event").and_then(Value::as_str) != Some("settings/document-updated") {
        return;
    }
    let Some(args) = payload.get("args").and_then(Value::as_array) else { return };
    let Some(ns) = args.first().and_then(Value::as_str) else { return };
    if ns == APPEARANCE_NS || ns == LANGUAGE_NS {
        on_change(ns);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(args: Value) -> String {
        serde_json::json!({
            "type": "server-request",
            "rpcId": "x",
            "method": "host/remote-event",
            "payload": {
                "type": "host/remote-event",
                "event": "settings/document-updated",
                "args": args
            }
        })
        .to_string()
    }

    #[test]
    fn handle_frame_forwards_theme_and_language() {
        let mut seen = Vec::new();
        handle_frame(&frame(serde_json::json!(["ui-theme", 3])), &mut |ns| {
            seen.push(ns.to_string())
        });
        handle_frame(&frame(serde_json::json!(["locale", 1])), &mut |ns| {
            seen.push(ns.to_string())
        });
        assert_eq!(seen, vec!["ui-theme", "locale"]);
    }

    #[test]
    fn handle_frame_ignores_other_events_and_namespaces() {
        let mut seen = 0usize;
        let other_event = serde_json::json!({
            "type": "server-request",
            "method": "host/remote-event",
            "payload": {
                "type": "host/remote-event",
                "event": "llm/adapters-updated",
                "args": []
            }
        })
        .to_string();
        handle_frame(&other_event, &mut |_| seen += 1);
        let other_ns = frame(serde_json::json!(["llm-deepseek", 7]));
        handle_frame(&other_ns, &mut |_| seen += 1);
        let garbage = "not json".to_string();
        handle_frame(&garbage, &mut |_| seen += 1);
        assert_eq!(seen, 0);
    }
}
