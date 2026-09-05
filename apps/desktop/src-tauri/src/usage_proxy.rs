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
            .or_else(|| value.get("requestId"))
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
    if let Some(value) = sse_accumulate_usage(response).or_else(|| sse_usage_value(response)) {
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

/// A stream frame's `usage` object — top-level (OpenAI / Anthropic
/// `message_delta`) or nested under `message` (Anthropic `message_start`).
fn usage_of(value: &Value) -> Option<&Value> {
    value
        .get("usage")
        .or_else(|| value.get("message").and_then(|m| m.get("usage")))
}

/// Merge token counts split across SSE events. Anthropic streams emit
/// `input_tokens` on `message_start` (nested under `message`) and
/// `output_tokens` on `message_delta` (top-level), so no single frame carries
/// both — unlike OpenAI's `include_usage` final chunk. This walks every frame,
/// takes the last non-null input/output each, and synthesizes a single
/// OpenAI-shaped `{ usage: { input_tokens, output_tokens, total_tokens } }`
/// value for [`maybe_record_usage_value`].
fn sse_accumulate_usage(response: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(response);
    let mut input: Option<u64> = None;
    let mut output: Option<u64> = None;
    let mut model: Option<String> = None;
    let mut id: Option<String> = None;
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
        let Some(usage) = usage_of(&value) else {
            continue;
        };
        // Take the *last* non-null input/output: Anthropic reports a
        // placeholder `output_tokens` on `message_start`, then the real value on
        // `message_delta`.
        if let Some(v) = first_u64(usage, INPUT_KEYS) {
            input = Some(v);
        }
        if let Some(v) = first_u64(usage, OUTPUT_KEYS) {
            output = Some(v);
        }
        if model.is_none() {
            model = value
                .get("model")
                .or_else(|| value.get("message").and_then(|m| m.get("model")))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if id.is_none() {
            id = value
                .get("id")
                .or_else(|| value.get("request_id"))
                .or_else(|| value.get("requestId"))
                .or_else(|| value.get("message").and_then(|m| m.get("id")))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    let input = input?;
    let output = output?;
    let mut usage = serde_json::Map::new();
    usage.insert("input_tokens".into(), Value::from(input));
    usage.insert("output_tokens".into(), Value::from(output));
    usage.insert("total_tokens".into(), Value::from(input + output));
    let mut obj = serde_json::Map::new();
    if let Some(m) = model {
        obj.insert("model".into(), Value::String(m));
    }
    if let Some(i) = id {
        obj.insert("id".into(), Value::String(i));
    }
    obj.insert("usage".into(), Value::Object(usage));
    Some(Value::Object(obj))
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
            .or_else(|| value.get("requestId"))
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
            level: launcher_core::LogLevel::Info,
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

const INPUT_KEYS: &[&str] = &["input_tokens", "prompt_tokens", "inputTokens", "promptTokens"];
const OUTPUT_KEYS: &[&str] = &[
    "output_tokens",
    "completion_tokens",
    "outputTokens",
    "completionTokens",
];

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let v = value.get(*key)?;
        if let Some(n) = v.as_u64() {
            return Some(n);
        }
        if let Some(n) = v.as_f64() {
            return Some(n as u64);
        }
        v.as_str().and_then(|s| s.trim().parse::<u64>().ok())
    })
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

    #[test]
    fn sse_accumulates_anthropic_split_usage() {
        // Anthropic splits usage: input on message_start, output on
        // message_delta. message_start also carries a placeholder
        // output_tokens=1 that must NOT win over the real message_delta value.
        let sse = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n";
        let value = sse_accumulate_usage(sse).unwrap();
        assert_eq!(value["usage"]["input_tokens"].as_u64(), Some(25));
        assert_eq!(value["usage"]["output_tokens"].as_u64(), Some(15));
        assert_eq!(value["usage"]["total_tokens"].as_u64(), Some(40));
        assert_eq!(value["model"].as_str(), Some("claude-sonnet-4-5"));
        assert_eq!(value["id"].as_str(), Some("msg_1"));
    }

    #[test]
    fn sse_accumulate_captures_request_id_variant() {
        let sse = b"data: {\"requestId\":\"req-9\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}\n\n";
        let value = sse_accumulate_usage(sse).unwrap();
        assert_eq!(value["id"].as_str(), Some("req-9"));
    }

    #[test]
    fn first_u64_coerces_float_and_string() {
        let value: Value = serde_json::from_str(
            r#"{"a": 12, "b": 3.0, "c": "7", "d": "x"}"#,
        )
        .unwrap();
        assert_eq!(first_u64(&value, &["a"]), Some(12));
        assert_eq!(first_u64(&value, &["b"]), Some(3));
        assert_eq!(first_u64(&value, &["c"]), Some(7));
        assert_eq!(first_u64(&value, &["d"]), None);
    }
}
