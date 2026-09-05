//! Crash telemetry — opt-in, minimal, default-off.
//!
//! The panic hook (`crash.rs`) writes a *sidecar* `crash-<ts>.json` only while
//! the user has consented in Preferences. This module is the only reader of
//! those sidecars: on the launch after a crash it POSTs them to the user's own
//! endpoint as one small JSON document and — only on a confirmed 2xx — deletes
//! the sidecars so they are not re-uploaded on the next boot.
//!
//! What is transmitted is strictly bounded:
//!   - launcher version + OS
//!   - per crash: panic message, source location, thread, occurred-at
//!
//! It never sends session content, conversation text, logs, API keys, or the
//! local backtrace (paths inside a backtrace can leak machine identity).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One crash as recorded while consent was on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashEvent {
    pub occurred_at: i64,
    pub message: String,
    pub location: String,
    pub thread: String,
}

const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Crash sidecars present in `logs_dir` (`crash-*.json`), oldest first.
pub fn collect_pending(logs_dir: &Path) -> Vec<(PathBuf, CrashEvent)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("crash-") || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Ok(event) = serde_json::from_str::<CrashEvent>(&text) {
            found.push((entry.path(), event));
        }
    }
    // Zero-padded timestamps sort lexically as chronological.
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The exact wire document sent to the ingest endpoint.
pub fn payload(version: &str, os: &str, events: &[CrashEvent]) -> Value {
    json!({
        "app": "ai-harness-launcher",
        "version": version,
        "os": os,
        "crashes": events,
    })
}

/// Upload every pending crash sidecar to `endpoint`. Returns how many were
/// sent. A non-2xx reply or a network error leaves the sidecars in place so the
/// next launch retries; nothing is deleted unless the upload is confirmed.
pub async fn flush(
    logs_dir: &Path,
    endpoint: &str,
    version: &str,
    os: &str,
) -> anyhow::Result<usize> {
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        anyhow::bail!("telemetry endpoint must be http(s), got {endpoint:?}");
    }
    let pending = collect_pending(logs_dir);
    if pending.is_empty() {
        return Ok(0);
    }
    // Sidecars are tiny; cloning keeps `payload` a plain owned-slice builder.
    let events: Vec<CrashEvent> = pending.iter().map(|(_, e)| e.clone()).collect();
    let client = reqwest::Client::builder().timeout(SEND_TIMEOUT).build()?;
    let resp = client
        .post(endpoint)
        .json(&payload(version, os, &events))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("telemetry endpoint replied {}", resp.status());
    }
    for (path, _) in &pending {
        let _ = std::fs::remove_file(path);
    }
    Ok(pending.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-telemetry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_sidecar(dir: &Path, ts: &str) -> PathBuf {
        let path = dir.join(format!("crash-{ts}.json"));
        let event = CrashEvent {
            occurred_at: 1_725_000_000,
            message: format!("boom@{ts}"),
            location: "src/main.rs:1:1".into(),
            thread: "main".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&event).expect("serialize")).expect("write");
        path
    }

    #[test]
    fn collect_pending_filters_and_sorts() {
        let dir = temp_dir("collect");
        write_sidecar(&dir, "2026-01-01_00-00-01");
        let second = write_sidecar(&dir, "2026-01-01_00-00-02");
        // Non-crash JSON and the human-readable .txt must be ignored.
        std::fs::write(dir.join("other.json"), b"{}").unwrap();
        std::fs::write(dir.join("crash-2026-01-01_00-00-01.txt"), b"panic: boom").unwrap();
        // Malformed crash JSON must be skipped, not fatal.
        std::fs::write(dir.join("crash-2026-01-01_00-00-03.json"), b"not json").unwrap();

        let pending = collect_pending(&dir);
        assert_eq!(pending.len(), 2, "only the two valid sidecars");
        assert_eq!(pending[1].0, second, "sorted by timestamp");
        assert_eq!(pending[0].1.message, "boom@2026-01-01_00-00-01");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn payload_shape_is_minimal() {
        let events = [CrashEvent {
            occurred_at: 9,
            message: "boom".into(),
            location: "a.rs:1:1".into(),
            thread: "main".into(),
        }];
        let p = payload("0.1.0", "windows", &events);
        let obj = p.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["app", "crashes", "os", "version"]);
        assert_eq!(obj["version"], "0.1.0");
        assert_eq!(obj["os"], "windows");
        let crash = obj["crashes"][0].as_object().expect("crash object");
        let mut ckeys: Vec<&str> = crash.keys().map(|s| s.as_str()).collect();
        ckeys.sort_unstable();
        assert_eq!(ckeys, vec!["location", "message", "occurredAt", "thread"]);
        assert_eq!(crash["message"], "boom");
        // The backtrace never travels; this payload carries no extra fields.
        assert!(!p.to_string().contains("backtrace"));
    }

    #[tokio::test]
    async fn flush_to_unreachable_port_keeps_sidecar() {
        let dir = temp_dir("fail");
        let path = write_sidecar(&dir, "2026-01-01_00-00-01");
        // Bind then drop so the port is closed — a refused connection.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let res = flush(&dir, &format!("http://{addr}/crash"), "0.1.0", "windows").await;
        assert!(res.is_err(), "refused connection must surface an error");
        assert!(
            path.exists(),
            "failed upload must keep the sidecar for retry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_rejects_non_http_endpoint() {
        let dir = temp_dir("scheme");
        write_sidecar(&dir, "2026-01-01_00-00-01");
        let res = flush(&dir, "ftp://example.com/x", "0.1.0", "windows").await;
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_posts_and_clears_on_2xx() {
        let dir = temp_dir("ok");
        let path = write_sidecar(&dir, "2026-01-01_00-00-01");
        assert!(path.exists());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 2048];
            let body_len;
            loop {
                let n = sock.read(&mut tmp).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..end]);
                    body_len = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    while buf.len() < end + 4 + body_len {
                        let n = sock.read(&mut tmp).await.expect("read body");
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[end + 4..]).to_string();
                    let _ = sock
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await;
                    return body;
                }
            }
            String::new()
        });

        let sent = flush(&dir, &format!("http://{addr}/crash"), "0.1.0", "windows")
            .await
            .expect("flush succeeds");
        assert_eq!(sent, 1);

        let body = server.await.expect("server task");
        assert!(
            body.contains("\"app\":\"ai-harness-launcher\""),
            "body: {body}"
        );
        assert!(body.contains("\"version\":\"0.1.0\""), "body: {body}");
        assert!(
            body.contains("\"message\":\"boom@2026-01-01_00-00-01\""),
            "body: {body}"
        );
        assert!(body.contains("\"thread\":\"main\""), "body: {body}");
        assert!(!path.exists(), "confirmed upload removes the sidecar");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_with_no_pending_is_noop() {
        let dir = temp_dir("empty");
        let sent = flush(&dir, "https://example.invalid/x", "0.1.0", "windows")
            .await
            .expect("empty flush never touches the network");
        assert_eq!(sent, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
