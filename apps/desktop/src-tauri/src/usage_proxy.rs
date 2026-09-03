use std::collections::HashMap;
use std::sync::Arc;

use launcher_core::NewUsageRecord;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::state::AppState;

pub struct UsageProxy {
    pub base_url: String,
    pub shutdown: oneshot::Sender<()>,
}

pub async fn start(
    app: AppHandle,
    upstream_base: String,
    api_key: String,
    instance_id: String,
    api_key_alias: String,
    fallback_model: String,
) -> anyhow::Result<UsageProxy> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let (shutdown, mut shutdown_rx) = oneshot::channel::<()>();
    let ctx = Arc::new(ProxyContext {
        app,
        upstream_base: upstream_base.trim_end_matches('/').to_string(),
        api_key,
        instance_id,
        api_key_alias,
        fallback_model,
        client: reqwest::Client::new(),
    });
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _)) => {
                            let ctx = ctx.clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = handle(stream, ctx).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });
    Ok(UsageProxy {
        base_url: format!("http://127.0.0.1:{port}"),
        shutdown,
    })
}

struct ProxyContext {
    app: AppHandle,
    upstream_base: String,
    api_key: String,
    instance_id: String,
    api_key_alias: String,
    fallback_model: String,
    client: reqwest::Client,
}

async fn handle(mut stream: TcpStream, ctx: Arc<ProxyContext>) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 1024 * 1024 {
            write_response(&mut stream, 413, "Payload Too Large", b"").await?;
            return Ok(());
        }
    }
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = headers.lines();
    let Some(request_line) = lines.next() else {
        write_response(&mut stream, 400, "Bad Request", b"").await?;
        return Ok(());
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let path = parts.next().unwrap_or("/");
    let mut header_map = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            header_map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = header_map
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + len {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let mut body = buf
        .get(body_start..body_start + len)
        .unwrap_or_default()
        .to_vec();
    body = ensure_stream_usage(body);
    let upstream = format!("{}{}", ctx.upstream_base, path);
    emit_proxy_log(
        &ctx.app,
        &ctx.instance_id,
        &format!("usage proxy request {method} {path}"),
    );
    let mut req = ctx.client.request(method.parse()?, upstream);
    req = req.bearer_auth(&ctx.api_key);
    req = req.header(
        "content-type",
        header_map
            .get("content-type")
            .cloned()
            .unwrap_or_else(|| "application/json".into()),
    );
    let resp = req.body(body.clone()).send().await?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let bytes = resp.bytes().await?.to_vec();
    maybe_record_usage(&ctx, &body, &bytes);
    let status_text = status.canonical_reason().unwrap_or("OK");
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\naccess-control-allow-origin: *\r\n\r\n",
        status.as_u16(),
        status_text,
        content_type,
        bytes.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

fn maybe_record_usage(ctx: &ProxyContext, request: &[u8], response: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(response) else {
        if record_sse_usage(ctx, request, response) {
            return;
        }
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            "usage proxy response was not JSON or usage SSE",
        );
        return;
    };
    let Some(usage) = value.get("usage") else {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            "usage proxy response had no usage field",
        );
        return;
    };
    let input = first_u64(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "inputTokens",
            "promptTokens",
        ],
    );
    let output = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "outputTokens",
            "completionTokens",
        ],
    );
    let Some(input_tokens) = input else {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            "usage proxy usage field had no input tokens",
        );
        return;
    };
    let Some(output_tokens) = output else {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            "usage proxy usage field had no output tokens",
        );
        return;
    };
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| request_model(request))
        .unwrap_or_else(|| ctx.fallback_model.clone());
    let record = NewUsageRecord {
        instance_id: ctx.instance_id.clone(),
        timestamp: None,
        model,
        input_tokens,
        output_tokens,
        total_tokens: first_u64(usage, &["total_tokens", "totalTokens"]),
        cost: value
            .get("cost")
            .or_else(|| usage.get("cost"))
            .and_then(Value::as_f64),
        api_key_alias: ctx.api_key_alias.clone(),
        request_id: value
            .get("id")
            .or_else(|| value.get("request_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let state = ctx.app.state::<AppState>();
    if let Ok(Some(saved)) = state.usage.record(record) {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            &format!(
                "usage recorded {} tokens for {}",
                saved.total_tokens, saved.model
            ),
        );
        let _ = ctx.app.emit("usage-recorded", &saved);
    }
}

fn ensure_stream_usage(body: Vec<u8>) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    if value.get("stream").and_then(Value::as_bool) != Some(true) {
        return body;
    }
    let Some(obj) = value.as_object_mut() else {
        return body;
    };
    let entry = obj
        .entry("stream_options")
        .or_insert_with(|| Value::Object(Default::default()));
    if let Some(options) = entry.as_object_mut() {
        options.insert("include_usage".into(), Value::Bool(true));
    }
    serde_json::to_vec(&value).unwrap_or(body)
}

fn record_sse_usage(ctx: &ProxyContext, request: &[u8], response: &[u8]) -> bool {
    if let Some(value) = sse_usage_value(response) {
        return maybe_record_usage_value(ctx, request, &value);
    }
    emit_proxy_log(
        &ctx.app,
        &ctx.instance_id,
        "usage proxy SSE stream had no usage field",
    );
    false
}

fn sse_usage_value(response: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(response);
    for line in text.lines() {
        let Some(data) = line.trim_start().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let Some(usage) = value.get("usage") else {
            continue;
        };
        if usage.is_object() {
            return Some(value);
        }
    }
    None
}

fn maybe_record_usage_value(ctx: &ProxyContext, request: &[u8], value: &Value) -> bool {
    let Some(usage) = value.get("usage") else {
        return false;
    };
    let Some(usage_obj) = usage.as_object() else {
        return false;
    };
    let input = first_u64(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "inputTokens",
            "promptTokens",
        ],
    );
    let output = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "outputTokens",
            "completionTokens",
        ],
    );
    let Some(input_tokens) = input else {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            &format!(
                "usage proxy usage field had no input tokens; keys: {}",
                usage_obj.keys().cloned().collect::<Vec<_>>().join(",")
            ),
        );
        return false;
    };
    let Some(output_tokens) = output else {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            &format!(
                "usage proxy usage field had no output tokens; keys: {}",
                usage_obj.keys().cloned().collect::<Vec<_>>().join(",")
            ),
        );
        return false;
    };
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| request_model(request))
        .unwrap_or_else(|| ctx.fallback_model.clone());
    let record = NewUsageRecord {
        instance_id: ctx.instance_id.clone(),
        timestamp: None,
        model,
        input_tokens,
        output_tokens,
        total_tokens: first_u64(usage, &["total_tokens", "totalTokens"]),
        cost: value
            .get("cost")
            .or_else(|| usage.get("cost"))
            .and_then(Value::as_f64),
        api_key_alias: ctx.api_key_alias.clone(),
        request_id: value
            .get("id")
            .or_else(|| value.get("request_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let state = ctx.app.state::<AppState>();
    if let Ok(Some(saved)) = state.usage.record(record) {
        emit_proxy_log(
            &ctx.app,
            &ctx.instance_id,
            &format!(
                "usage recorded {} tokens for {}",
                saved.total_tokens, saved.model
            ),
        );
        let _ = ctx.app.emit("usage-recorded", &saved);
        return true;
    }
    false
}

fn emit_proxy_log(app: &AppHandle, instance_id: &str, line: &str) {
    let _ = app.emit(
        "logs",
        launcher_core::LogLine {
            stream: launcher_core::LogStream::Stdout,
            line: format!("{instance_id} · {line}"),
        },
    );
}

fn request_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn write_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_requests_are_marked_for_usage() {
        let body = br#"{"model":"deepseek-chat","stream":true,"messages":[]}"#.to_vec();
        let value: Value = serde_json::from_slice(&ensure_stream_usage(body)).unwrap();
        assert_eq!(
            value["stream_options"]["include_usage"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn sse_usage_skips_null_until_final_usage_object() {
        let sse = b"data: {\"id\":\"a\",\"usage\":null}\n\n\
data: {\"id\":\"a\",\"choices\":[]}\n\n\
data: {\"id\":\"a\",\"model\":\"deepseek-chat\",\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":7,\"total_tokens\":10}}\n\n\
data: [DONE]\n\n";
        let value = sse_usage_value(sse).unwrap();
        assert_eq!(value["usage"]["prompt_tokens"].as_u64(), Some(3));
        assert_eq!(value["usage"]["completion_tokens"].as_u64(), Some(7));
    }
}
