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
    // The response is forwarded chunk by chunk, so a small write per token
    // must go out immediately — with Nagle on, the OS would sit on it for up to
    // ~40 ms waiting for company, which is exactly the latency this proxy is
    // supposed to preserve.
    let _ = stream.set_nodelay(true);
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
    let mut resp = req.body(body.clone()).send().await?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let status_text = status.canonical_reason().unwrap_or("OK");
    // 1xx/204/304 carry no body, and RFC 7230 forbids framing them with
    // `transfer-encoding` — a client that honours the spec would wait for a
    // chunk terminator the status says cannot exist. Answer in one piece, the
    // way the pre-streaming proxy did.
    if status.is_informational()
        || status == reqwest::StatusCode::NO_CONTENT
        || status == reqwest::StatusCode::NOT_MODIFIED
    {
        let head = format!(
            "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: 0\r\naccess-control-allow-origin: *\r\n\r\n",
            status.as_u16(),
            status_text,
            content_type
        );
        stream.write_all(head.as_bytes()).await?;
        return Ok(());
    }

    // No `content-length`: the body is forwarded as it arrives, so its length
    // is not known when the head has to go out. Chunked framing lets the client
    // start rendering the first token while the rest is still in flight, which
    // is the whole point — buffering here turned every streamed reply into
    // "spinner, then the entire answer at once".
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ntransfer-encoding: chunked\r\naccess-control-allow-origin: *\r\n\r\n",
        status.as_u16(),
        status_text,
        content_type
    );
    stream.write_all(head.as_bytes()).await?;

    let mut tee = UsageTee::for_content_type(&content_type);
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            // Clean end of body: terminate the chunked framing.
            Ok(None) => {
                stream.write_all(b"0\r\n\r\n").await?;
                break;
            }
            Err(e) => {
                // The head is long gone, so the status cannot be corrected. Stop
                // without the terminator: the client sees a truncated body
                // rather than a body that claims to have ended cleanly.
                emit_proxy_log(
                    &ctx.app,
                    &ctx.instance_id,
                    &format!("usage proxy upstream stream failed mid-response: {e}"),
                );
                return Ok(());
            }
        };
        if chunk.is_empty() {
            continue;
        }
        let size = format!("{:x}\r\n", chunk.len());
        stream.write_all(size.as_bytes()).await?;
        stream.write_all(&chunk).await?;
        stream.write_all(b"\r\n").await?;
        tee.push(&chunk);
    }
    tee.record(&ctx, &body);
    Ok(())
}

/// Collects just enough of a forwarded response to record usage for it,
/// without holding the body back from the client.
///
/// An SSE stream cannot be summarised from a slice at the end — it is never
/// held as one — and it cannot be summarised from a tail either: Anthropic
/// reports `input_tokens` on the *first* frame and `output_tokens` on the last,
/// so either end dropped is a lost record. Hence folding frames as they pass.
/// Non-SSE bodies are ordinary JSON, where usage is one object somewhere in the
/// middle of a small document, so those are buffered whole — exactly what the
/// proxy did before it streamed, and no extra memory on top.
enum UsageTee {
    Sse(SseUsage),
    Whole(Vec<u8>),
}

impl UsageTee {
    fn for_content_type(content_type: &str) -> Self {
        if content_type.starts_with("text/event-stream") {
            Self::Sse(SseUsage::default())
        } else {
            Self::Whole(Vec::new())
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        match self {
            Self::Sse(acc) => acc.push(chunk),
            Self::Whole(buf) => buf.extend_from_slice(chunk),
        }
    }

    fn record(self, ctx: &ProxyContext, request: &[u8]) {
        match self {
            Self::Sse(acc) => {
                if let Some(value) = acc.finish() {
                    maybe_record_usage_value(ctx, request, &value);
                } else {
                    emit_proxy_log(
                        &ctx.app,
                        &ctx.instance_id,
                        "usage proxy SSE stream had no usage field",
                    );
                }
            }
            Self::Whole(bytes) => maybe_record_usage(ctx, request, &bytes),
        }
    }
}

/// Folds SSE frames into one usage value as the bytes flow past.
///
/// Only complete lines are considered, so a frame split across two chunks is
/// still read correctly; `finish` flushes whatever the last chunk left
/// unterminated.
#[derive(Default)]
struct SseUsage {
    /// Bytes of the newest chunk that did not yet end in a newline.
    pending: Vec<u8>,
    input: Option<u64>,
    output: Option<u64>,
    model: Option<String>,
    id: Option<String>,
    /// The first frame that carried a usage *object* — the fallback when no
    /// merged pair can be formed (see [`SseUsage::finish`]).
    first: Option<Value>,
}

impl SseUsage {
    fn push(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.frame(&line[..line.len() - 1]);
        }
    }

    fn frame(&mut self, line: &[u8]) {
        let Ok(text) = std::str::from_utf8(line) else {
            return;
        };
        let Some(data) = text.trim_start().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let Some(usage) = usage_of(&value) else {
            return;
        };
        // Take the *last* non-null input/output: Anthropic reports a
        // placeholder `output_tokens` on `message_start`, then the real value on
        // `message_delta`.
        if let Some(v) = first_u64(usage, INPUT_KEYS) {
            self.input = Some(v);
        }
        if let Some(v) = first_u64(usage, OUTPUT_KEYS) {
            self.output = Some(v);
        }
        if self.model.is_none() {
            self.model = value
                .get("model")
                .or_else(|| value.get("message").and_then(|m| m.get("model")))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if self.id.is_none() {
            self.id = value
                .get("id")
                .or_else(|| value.get("request_id"))
                .or_else(|| value.get("requestId"))
                .or_else(|| value.get("message").and_then(|m| m.get("id")))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if self.first.is_none() && usage.is_object() {
            self.first = Some(value);
        }
    }

    /// Drop anything the last chunk left unterminated, running it through the
    /// same parse (a stream may end without a final newline).
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let tail = std::mem::take(&mut self.pending);
        self.frame(&tail);
    }

    /// Both halves seen, synthesised into one OpenAI-shaped value.
    fn merged(&mut self) -> Option<Value> {
        self.flush();
        let (input, output) = (self.input?, self.output?);
        let mut usage = serde_json::Map::new();
        usage.insert("input_tokens".into(), Value::from(input));
        usage.insert("output_tokens".into(), Value::from(output));
        usage.insert("total_tokens".into(), Value::from(input + output));
        let mut obj = serde_json::Map::new();
        if let Some(m) = &self.model {
            obj.insert("model".into(), Value::String(m.clone()));
        }
        if let Some(i) = &self.id {
            obj.insert("id".into(), Value::String(i.clone()));
        }
        obj.insert("usage".into(), Value::Object(usage));
        Some(Value::Object(obj))
    }

    /// The merged value when both halves arrived, else the first frame that
    /// carried a usage object.
    fn finish(mut self) -> Option<Value> {
        if let Some(merged) = self.merged() {
            return Some(merged);
        }
        self.flush();
        self.first
    }
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
        let _ = ctx.app.emit(crate::commands::process::USAGE_EVENT, &saved);
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

/// Fold a whole SSE body through the same accumulator the streaming path uses.
/// Reaching here means the body arrived in one piece (a non-SSE content-type
/// that still turned out to be SSE), so the folding is just not chunk-aligned.
fn sse_usage_fold(response: &[u8]) -> Option<Value> {
    let mut acc = SseUsage::default();
    acc.push(response);
    acc.finish()
}

fn record_sse_usage(ctx: &ProxyContext, request: &[u8], response: &[u8]) -> bool {
    if let Some(value) = sse_usage_fold(response) {
        return maybe_record_usage_value(ctx, request, &value);
    }
    emit_proxy_log(
        &ctx.app,
        &ctx.instance_id,
        "usage proxy SSE stream had no usage field",
    );
    false
}

/// A stream frame's `usage` object — top-level (OpenAI / Anthropic
/// `message_delta`) or nested under `message` (Anthropic `message_start`).
fn usage_of(value: &Value) -> Option<&Value> {
    value
        .get("usage")
        .or_else(|| value.get("message").and_then(|m| m.get("usage")))
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
        let _ = ctx.app.emit(crate::commands::process::USAGE_EVENT, &saved);
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

    /// Fold a whole SSE body the way the streaming path folds it chunk by
    /// chunk: the merge of a split `input_tokens`/`output_tokens` pair.
    fn merged(response: &[u8]) -> Option<Value> {
        let mut acc = SseUsage::default();
        acc.push(response);
        acc.merged()
    }

    /// The fallback when no merge is possible: the first frame whose `usage` is
    /// an object, so a `"usage":null` frame is skipped rather than recorded.
    fn first_usage_frame(response: &[u8]) -> Option<Value> {
        let mut acc = SseUsage::default();
        acc.push(response);
        acc.flush();
        acc.first
    }

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
        let value = first_usage_frame(sse).unwrap();
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
        let value = merged(sse).unwrap();
        assert_eq!(value["usage"]["input_tokens"].as_u64(), Some(25));
        assert_eq!(value["usage"]["output_tokens"].as_u64(), Some(15));
        assert_eq!(value["usage"]["total_tokens"].as_u64(), Some(40));
        assert_eq!(value["model"].as_str(), Some("claude-sonnet-4-5"));
        assert_eq!(value["id"].as_str(), Some("msg_1"));
    }

    #[test]
    fn sse_accumulate_captures_request_id_variant() {
        let sse = b"data: {\"requestId\":\"req-9\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}\n\n";
        let value = merged(sse).unwrap();
        assert_eq!(value["id"].as_str(), Some("req-9"));
    }

    #[test]
    fn sse_fold_matches_whole_body_parse_when_chunks_split_frames() {
        let sse = b"event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n";
        let whole = merged(sse).unwrap();
        // Feed the same bytes one at a time, so every frame boundary lands
        // mid-chunk: the fold may not depend on chunk alignment.
        let mut acc = SseUsage::default();
        for byte in sse {
            acc.push(std::slice::from_ref(byte));
        }
        assert_eq!(acc.finish().unwrap(), whole);
    }

    #[test]
    fn sse_fold_reads_a_final_frame_without_a_trailing_newline() {
        let mut acc = SseUsage::default();
        acc.push(b"data: {\"usage\":{\"input_tokens\":4,\"output_tokens\":6}}");
        let value = acc.finish().unwrap();
        assert_eq!(value["usage"]["input_tokens"].as_u64(), Some(4));
        assert_eq!(value["usage"]["output_tokens"].as_u64(), Some(6));
    }

    #[test]
    fn sse_fold_falls_back_to_the_first_usage_object() {
        // Only one half of the split pair ever arrives, so no merge is possible
        // and the first frame carrying a usage *object* is the record — the
        // `"usage":null` frame before it must not be mistaken for one.
        let mut acc = SseUsage::default();
        acc.push(b"data: {\"model\":\"m\",\"usage\":null}\n\n");
        acc.push(b"data: {\"model\":\"m\",\"usage\":{\"prompt_tokens\":3}}\n\n");
        let value = acc.finish().unwrap();
        assert_eq!(value["usage"]["prompt_tokens"].as_u64(), Some(3));
        assert_eq!(value["model"].as_str(), Some("m"));
    }

    #[test]
    fn usage_tee_routes_on_content_type() {
        let mut json = UsageTee::for_content_type("application/json; charset=utf-8");
        json.push(b"{\"us");
        json.push(b"age\":{}}");
        let UsageTee::Whole(bytes) = json else {
            panic!("a JSON content-type must be buffered, not folded as SSE");
        };
        assert_eq!(bytes, b"{\"usage\":{}}");

        let sse = UsageTee::for_content_type("text/event-stream; charset=utf-8");
        assert!(matches!(sse, UsageTee::Sse(_)));
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
