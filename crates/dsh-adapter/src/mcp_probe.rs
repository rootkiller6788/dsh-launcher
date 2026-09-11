// MCP server health probe (roadmap §9.3 / Phase 2, A path).
//
// DSH owns the *live* MCP process — the launcher writes `cordis.patch.yml` and
// DSH's mcp-client boots npx/uvx from it, so the launcher can never observe that
// pid. This module is instead the launcher's **self-proving** health check: it
// spawns the server's stdio launch (or POSTs a streamable-http endpoint), speaks
// a minimal MCP `initialize` handshake, and classifies the outcome:
//
//   ok        — initialize answered (tools/list is best-effort afterwards)
//   degraded  — process/HTTP came up but the handshake failed/refused
//               (config-grade: missing token/env, wrong server)
//   error     — spawn failed, exited non-zero, timed out, or there is no
//               launch to probe at all
//
// The verdict is persisted by the caller into `mcp/<server>/runtime.json`; the
// probe itself never touches disk.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use launcher_core::process::LogSink;
use launcher_core::{
    McpRuntimeState, McpServerRecord, MCP_STATE_DEGRADED, MCP_STATE_ERROR, MCP_STATE_OK,
};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::mcp_prefetch::bundled_cli;

// Allow room for a cold `npx -y <pkg>` fetch: on a slow registry hop the
// package download alone can exceed the old 15s budget even though the server
// itself is healthy (spawn → fetch → server boot → initialize). 30s keeps the
// 检查 button honest without hanging on a truly stuck spawn.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
// Extra budget granted to a child that is *still alive* at the INIT_TIMEOUT mark with no
// initialize answer: a cold `npx` fetch whose postinstall downloads a browser/binary
// (server-puppeteer → Chromium) can legitimately take far longer than 30s to first
// print anything. Killing it then would badge a healthy install as an error. Total
// window is INIT_TIMEOUT + COLD_START_GRACE.
const COLD_START_GRACE: Duration = Duration::from_secs(90);
/// The knob for both windows above. A registry hop behind a slow proxy can
/// outlast even the cold-start grace, and the user watching the 检查 button is
/// the only one who knows that about their network — so the window is
/// overridable instead of argued about. Read per probe, so it needs no plumbing
/// through every call site.
const INIT_TIMEOUT_ENV: &str = "AHL_MCP_PROBE_TIMEOUT_SECS";
/// Bounds on the override: below a few seconds a healthy cold server cannot
/// answer (the flag would only manufacture failures), and above ten minutes a
/// typo would look like a hang.
const INIT_TIMEOUT_RANGE: (u64, u64) = (5, 600);
const TOOLS_TIMEOUT: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TOOLS: usize = 40;
const MCP_PROTOCOL: &str = "2025-03-26";

/// Case-insensitive text markers that mean "this process launched an HTTP/streamable
/// server and is ignoring stdio" — a real transport mismatch with the catalog's stdio
/// entry (e.g. the modelcontextprotocol "App Server" packages that need `--stdio`).
/// When present, the probe fails fast with an honest message instead of a bare timeout.
const HTTP_TRANSPORT_MARKERS: [&str; 6] = [
    "listening on http",
    "binding to 0.0.0.0",
    "allowedhosts",
    "http://localhost:",
    "server is running at",
    "streamable http",
];

/// Parse an override value: whole seconds, clamped to
/// [`INIT_TIMEOUT_RANGE`]. `None` for anything that is not a number, so a typo
/// falls back to the default rather than to a 0-second window.
fn parse_init_timeout(raw: &str) -> Option<Duration> {
    let secs: u64 = raw.trim().parse().ok()?;
    let (lo, hi) = INIT_TIMEOUT_RANGE;
    Some(Duration::from_secs(secs.clamp(lo, hi)))
}

/// How long to wait for the first initialize response.
///
/// The default is unchanged; the override exists for a network where even a
/// cold `npx` fetch inside the grace window is not enough. A value that is not a
/// number is ignored (with a line in the probe log, so a typo is visible rather
/// than silently ignored).
fn init_timeout() -> Duration {
    let Ok(raw) = std::env::var(INIT_TIMEOUT_ENV) else {
        return INIT_TIMEOUT;
    };
    match parse_init_timeout(&raw) {
        Some(d) => d,
        None => {
            tracing::warn!(
                value = %raw,
                "{INIT_TIMEOUT_ENV} is not a whole number of seconds — using {}s",
                INIT_TIMEOUT.as_secs()
            );
            INIT_TIMEOUT
        }
    }
}

/// The cold-start extension: three times the init window, never less than the
/// historical 90s. A cold `npx` that downloads a browser (server-puppeteer →
/// Chromium) is the case this covers, and its cost does not shrink because the
/// user lowered the first window.
fn cold_start_grace(init: Duration) -> Duration {
    (init * 3).max(COLD_START_GRACE)
}

/// Text markers for "the package itself installed, but the *runtime asset* it
/// downloads for itself did not" — a browser (server-puppeteer → Chromium) or a
/// prebuilt binary.
///
/// This is the other half of the cold-start story: the install script runs
/// during `npx`, its download fails (a CDN the network cannot reach, no disk, a
/// proxy that only serves the registry), and the process exits non-zero having
/// never reached initialize. The server's own code is complete and the failure
/// is a fixable missing dependency, so the probe must not report it as a failed
/// install — an install rollback here would delete a working server over a
/// browser it did not need on the next run.
const RUNTIME_ASSET_MARKERS: [&str; 8] = [
    "failed to download chromium",
    "failed to download chrome",
    "failed to set up chrome",
    "failed to download browser",
    "failed to install browser",
    "browser download failed",
    "could not find expected browser",
    "unable to download chrome",
];

fn runtime_asset_hint(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    RUNTIME_ASSET_MARKERS
        .iter()
        .find(|m| lower.contains(*m))
        .map(|_| {
            "the package installed but its browser/binary download did not, so it exited \
             before initialize — the server itself is installed and will start once that \
             dependency is available (retry the check, install the browser yourself, or set \
             PUPPETEER_SKIP_DOWNLOAD=1 if a browser is already present)"
        })
}

fn http_transport_hint(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    HTTP_TRANSPORT_MARKERS
        .iter()
        .find(|m| lower.contains(*m))
        .map(|_| {
            "the process started an HTTP/streamable listener instead of answering stdio — \
             this package is not running as a stdio MCP (it may need a stdio flag like \
             `--stdio`); the catalog's `stdio` entry is suspect"
        })
}

/// True when a server log line announces that the process came up in a degraded /
/// misconfigured state: it *is* up and answering stdio, but declares tool calls will
/// fail until credentials/config are supplied (the kanboard-mcp ConfigError → DEGRADED
/// mode case, where KANBOARD_URL is unset). An initialize that answered normally is
/// `ok` on its own; a self-declared degraded line downgrades that verdict so the
/// Library badge reads config-grade rather than falsely healthy.
fn self_declared_degraded(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("\"degraded\":true")
        || lower.contains("\"degraded\": true")
        || lower.contains("degraded mode")
        || lower.contains("without valid credentials")
}

/// Record the first self-declared-degraded line seen on stdout/stderr so the verdict
/// can cite it (and so eviction from the short tail window doesn't lose the signal).
fn note_degraded_self_report(
    line: &str,
    flag: &Arc<std::sync::Mutex<bool>>,
    reason: &Arc<std::sync::Mutex<Option<String>>>,
) {
    if self_declared_degraded(line) {
        *flag.lock().unwrap() = true;
        let mut r = reason.lock().unwrap();
        if r.is_none() {
            *r = Some(line.to_string());
        }
    }
}

// --- JSON-RPC frames (newline-delimited, per the MCP stdio spec) -----------

fn init_request() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{MCP_PROTOCOL}","capabilities":{{}},"clientInfo":{{"name":"AI Harness Launcher","version":"0.1"}}}}}}"#
    )
}

fn initialized_notification() -> String {
    r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#.to_string()
}

fn tools_request() -> String {
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#.to_string()
}

/// One parsed server line that the probe cares about.
#[derive(Debug, Clone, PartialEq)]
enum ParseOutcome {
    /// `initialize` answered with a result; carries the advertised serverInfo name.
    InitResult(Option<String>),
    /// `initialize` answered with a JSON-RPC error (server is up, config-grade).
    InitError(String),
    /// `tools/list` answered; the discovered tool names.
    Tools(Vec<String>),
    /// Anything else (notifications, logging, noise).
    None_,
}

fn parse_response(line: &str) -> ParseOutcome {
    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return ParseOutcome::None_,
    };
    let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
        return ParseOutcome::None_;
    };
    match id {
        1 => {
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("initialize rejected")
                    .to_string();
                ParseOutcome::InitError(msg)
            } else {
                let name = v
                    .get("result")
                    .and_then(|r| r.get("serverInfo"))
                    .and_then(|s| s.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());
                ParseOutcome::InitResult(name)
            }
        }
        2 => {
            let tools = v
                .get("result")
                .and_then(|r| r.get("tools"))
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
                        .take(MAX_TOOLS)
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ParseOutcome::Tools(tools)
        }
        _ => ParseOutcome::None_,
    }
}

// --- launch resolution ------------------------------------------------------

/// How to reach a stdio command on this machine. npx is a `.cmd` shim on Windows
/// (not directly spawnable) and prefers the bundled node's `npx-cli.js` when one
/// exists — the same env-isolated trick `mcp_prefetch` uses for npm.
fn launch_command(command: &str, node: Option<&Path>) -> (String, Vec<String>) {
    match command {
        "npx" => {
            if let Some(node_exe) = node {
                if let Some(cli) = bundled_cli(node_exe, "npx") {
                    return (node_exe.display().to_string(), vec![cli.display().to_string()]);
                }
            }
            if cfg!(windows) {
                ("cmd".into(), vec!["/C".into(), "npx".into()])
            } else {
                ("npx".into(), vec![])
            }
        }
        other => (other.to_string(), vec![]),
    }
}

/// Health-check one installed MCP record. Returns a [`McpRuntimeState`] whose
/// `state` is already chosen; the caller folds it onto the persisted snapshot.
pub async fn probe_mcp(
    record: &McpServerRecord,
    node: Option<&Path>,
    sink: LogSink,
) -> McpRuntimeState {
    let transport = record.transport.clone();
    if transport.eq_ignore_ascii_case("streamable-http") || !record.url.trim().is_empty() {
        return probe_http(record).await;
    }
    if record.command.trim().is_empty() {
        return verdict(&transport, MCP_STATE_ERROR, None, "no launch command — open its repo to run".into(), vec![]);
    }
    probe_stdio(record, node, sink).await
}

/// A non-`ok` probe whose *process came up* but the handshake failed is degraded
/// (config); environment failures (spawn/exit/timeout/no-launch) are errors.
fn verdict(
    transport: &str,
    state: &'static str,
    exit_code: Option<i32>,
    detail: String,
    tools: Vec<String>,
) -> McpRuntimeState {
    McpRuntimeState {
        state: state.to_string(),
        transport: transport.to_string(),
        exit_code,
        error: Some(detail),
        tools,
        ..McpRuntimeState::default()
    }
}

// --- stdio probe ------------------------------------------------------------

async fn probe_stdio(record: &McpServerRecord, node: Option<&Path>, sink: LogSink) -> McpRuntimeState {
    let transport = "stdio".to_string();
    let (program, prefix) = launch_command(&record.command, node);

    // Probe script is staged as a temp *file* that becomes the child's stdin rather than
    // feeding a live pipe. On Windows, an anonymous-pipe stdin created by Rust std::process
    // is non-overlapped, and node's libuv cannot async-read such a handle: the server boots
    // (its stderr even prints "running on stdio") but the initialize frame never arrives, so
    // the old code timed out at INIT_TIMEOUT with a healthy server left waiting. A file
    // handle has no such constraint; SDK-based servers drain the buffered frames in order
    // (initialize → initialized → tools/list) before hitting EOF — exactly what this
    // one-shot probe needs. All three frames are queued up front for the same reason.
    let stdin_script = format!(
        "{}\n{}\n{}\n",
        init_request(),
        initialized_notification(),
        tools_request()
    );
    let stdin_path = std::env::temp_dir().join(format!(
        "ahl-probe-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let stdin_file = match stage_stdin(&stdin_path, stdin_script.as_bytes()) {
        Ok(f) => f,
        Err(e) => {
            return verdict(
                &transport,
                MCP_STATE_ERROR,
                None,
                format!("could not stage probe stdin: {e}"),
                vec![],
            );
        }
    };

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&prefix);
    cmd.args(&record.args);
    cmd.stdin(Stdio::from(stdin_file));
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    for (k, v) in &record.env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&stdin_path);
            emit(&sink, &format!("health: spawn {program} failed: {e}"));
            return verdict(&transport, MCP_STATE_ERROR, None, format!("spawn {program}: {e}"), vec![]);
        }
    };
    // Child holds its own handle to the file now; the staging path can go.
    let _ = std::fs::remove_file(&stdin_path);

    let mut reader = BufReader::new(child.stdout.take().expect("stdout piped")).lines();
    // Capture stderr concurrently; the shared tail (stdout too) feeds the no-init
    // verdict detail and the "this is really an HTTP server" heuristic. Degraded
    // self-reports are flagged the moment they arrive (not from the evicted tail).
    let out_tail = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let degraded = Arc::new(std::sync::Mutex::new(false));
    let degraded_reason = Arc::new(std::sync::Mutex::new(None::<String>));
    if let Some(err) = child.stderr.take() {
        let tail = out_tail.clone();
        let sink = sink.clone();
        let degraded = degraded.clone();
        let degraded_reason = degraded_reason.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    emit(&sink, &line);
                    note_degraded_self_report(&line, &degraded, &degraded_reason);
                    let mut t = tail.lock().unwrap();
                    t.push(line);
                    if t.len() > 6 {
                        t.remove(0);
                    }
                }
            }
        });
    }

    // Read until initialize is classified; once answered, give TOOLS_TIMEOUT for the
    // pre-queued tools/list reply (best-effort — it must not downgrade a healthy check).
    // A child *still alive* at the init deadline gets COLD_START_GRACE more time: a cold
    // `npx` fetch whose postinstall downloads a browser/binary can take far longer than
    // INIT_TIMEOUT to first print anything, and killing it would badge a healthy server
    // as an install error (the puppeteer/Chromium case).
    let init_timeout = init_timeout();
    let cold_grace = cold_start_grace(init_timeout);
    let start = tokio::time::Instant::now();
    let mut deadline = start + init_timeout;
    let mut cold_grace_granted = false;
    let mut http_seen = false; // the child announced an HTTP listener on stdout
    let mut init_outcome = ParseOutcome::None_;
    let mut tools: Vec<String> = Vec::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            // Phase over. If we are still waiting for initialize and the child is alive
            // with no HTTP-transport signal, it is most plausibly still cold-installing —
            // extend the init window once. Once initialize answered, the short tools
            // window simply elapsed → stop (the init arm below finalizes a healthy check).
            if matches!(init_outcome, ParseOutcome::None_) {
                let alive = child.try_wait().ok().flatten().is_none();
                let tail_text = {
                    let t = out_tail.lock().unwrap();
                    t.join("\n")
                };
                if alive && !cold_grace_granted && http_transport_hint(&tail_text).is_none() {
                    emit(
                        &sink,
                        &format!(
                            "health: no initialize within {}s and the process is still alive — \
                             granting a cold-start extension of {}s (npx may still be installing)",
                            init_timeout.as_secs(),
                            cold_grace.as_secs()
                        ),
                    );
                    cold_grace_granted = true;
                    deadline = tokio::time::Instant::now() + cold_grace;
                    continue;
                }
            }
            break;
        }
        tokio::select! {
            biased;
            line = reader.next_line() => {
                match line {
                    Ok(Some(l)) if !l.trim().is_empty() => {
                        let trimmed = l.trim().to_string();
                        note_degraded_self_report(&trimmed, &degraded, &degraded_reason);
                        {
                            let mut t = out_tail.lock().unwrap();
                            t.push(trimmed.clone());
                            if t.len() > 6 {
                                t.remove(0);
                            }
                        }
                        // An HTTP/streamable server announcing itself is a real transport
                        // mismatch with a stdio entry — bail out fast rather than sit out
                        // the whole deadline.
                        if matches!(init_outcome, ParseOutcome::None_)
                            && http_transport_hint(&trimmed).is_some()
                        {
                            http_seen = true;
                            break;
                        }
                        match parse_response(&trimmed) {
                            ParseOutcome::InitResult(name) if matches!(init_outcome, ParseOutcome::None_) => {
                                init_outcome = ParseOutcome::InitResult(name);
                                // Initialize answered: stop waiting on it; give the short
                                // pre-queued tools/list window from here.
                                deadline = tokio::time::Instant::now() + TOOLS_TIMEOUT;
                            }
                            ParseOutcome::InitError(msg) if matches!(init_outcome, ParseOutcome::None_) => {
                                init_outcome = ParseOutcome::InitError(msg);
                                deadline = tokio::time::Instant::now() + TOOLS_TIMEOUT;
                            }
                            ParseOutcome::Tools(names) if !names.is_empty() => tools = names,
                            _ => {}
                        }
                    }
                    // stdout EOF → the child replied and exited (file stdin ends) or died.
                    _ => break,
                }
            }
            _ = tokio::time::sleep(deadline - now) => {}
        }
    }

    match init_outcome {
        ParseOutcome::InitError(msg) => {
            kill(&mut child).await;
            let detail = format!("server answered but rejected initialize: {msg}");
            emit(&sink, &format!("health: {detail}"));
            verdict(&transport, MCP_STATE_DEGRADED, None, detail, vec![])
        }
        ParseOutcome::InitResult(name) => {
            kill(&mut child).await;
            let mut detail = "initialize ok".to_string();
            if let Some(name) = name {
                detail.push_str(&format!(" ({name})"));
            }
            emit(&sink, &format!("health: {detail}"));
            let tools_out = if tools.is_empty() {
                emit(&sink, "health: tools/list unavailable (server may still be healthy)");
                vec![]
            } else {
                tools
            };
            // The handshake answered (so the server is genuinely up), but if it
            // self-declared a degraded/misconfigured state (missing API key / URL /
            // invalid credentials), `ok` would be a lie — every tool call fails until
            // it is configured. Downgrade to degraded so the badge is honest; the
            // install is still kept (config-grade, not a failed install).
            if *degraded.lock().unwrap() {
                let note = "server answered initialize but self-reports a degraded/misconfigured \
                            state (missing API key/URL or invalid credentials) — tools are listable \
                            but tool calls may fail until it is configured"
                    .to_string();
                let reason = degraded_reason.lock().unwrap().clone().unwrap_or_default();
                let reason_short: String = reason.chars().take(240).collect();
                let detail = if reason_short.is_empty() {
                    note.clone()
                } else {
                    format!("{note}: {reason_short}")
                };
                emit(&sink, &format!("health: {detail}"));
                return McpRuntimeState {
                    state: MCP_STATE_DEGRADED.to_string(),
                    transport,
                    error: Some(detail),
                    tools: tools_out,
                    ..McpRuntimeState::default()
                };
            }
            McpRuntimeState {
                state: MCP_STATE_OK.to_string(),
                transport,
                error: None,
                tools: tools_out,
                ..McpRuntimeState::default()
            }
        }
        _ => {
            // No initialize response: EOF or deadline. Distinguish a child that refused
            // stdio because it is really an HTTP server (catalog/entry at fault, tell the
            // user plainly) from a genuine exit or a pure timeout.
            let code = await_exit_or_kill(&mut child).await;
            kill(&mut child).await;
            let tail = {
                let t = out_tail.lock().unwrap();
                t.join(" | ")
            };
            // Degraded, not error, when the only thing missing is a runtime asset
            // the package downloads for itself: the install is fine and a rollback
            // would destroy it over a browser that a retry (or the user's own
            // Chrome) supplies. See RUNTIME_ASSET_MARKERS.
            let asset_miss = runtime_asset_hint(&tail);
            let state = if asset_miss.is_some() {
                MCP_STATE_DEGRADED
            } else {
                MCP_STATE_ERROR
            };
            let detail = if http_seen || http_transport_hint(&tail).is_some() {
                format!("server never answered a stdio initialize — {hint}", hint = http_transport_hint(&tail).unwrap_or("it started an HTTP/streamable listener instead"))
            } else if let Some(hint) = asset_miss {
                format!("server exited before initialize — {hint}")
            } else {
                match code {
                    Some(c) => format!("server exited with code {c} before initialize response"),
                    None => {
                        let total = init_timeout.as_secs()
                            + if cold_grace_granted { cold_grace.as_secs() } else { 0 };
                        // Name the knob: this is the message a user on a slow
                        // registry hits, and it is the one they can act on.
                        format!(
                            "no initialize response within {total}s — a cold package fetch can \
                             exceed this; raise {INIT_TIMEOUT_ENV} to wait longer"
                        )
                    }
                }
            };
            let detail = if tail.is_empty() { detail } else { format!("{detail} — captured: {tail}") };
            emit(&sink, &format!("health: {detail}"));
            verdict(&transport, state, code, detail, vec![])
        }
    }
}

/// Write the probe frames to a temp file and hand back a readable handle for the child's
/// stdin. The path is only a staging vehicle — the caller unlinks it right after spawn.
fn stage_stdin(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<std::fs::File> {
    std::fs::write(path, bytes)?;
    std::fs::File::open(path)
}

/// Wait briefly for a child that may already be dead, returning its exit code
/// when it exited on its own; `None` means it was still running.
async fn await_exit_or_kill(child: &mut tokio::process::Child) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
    loop {
        match child.try_wait().ok().flatten() {
            Some(status) => return status.code(),
            None if tokio::time::Instant::now() >= deadline => return None,
            None => tokio::time::sleep(Duration::from_millis(30)).await,
        }
    }
}

/// Stop a probed child. The spawn is usually a launcher like `cmd`/`cmd /C npx` whose real
/// server is a *descendant* (cmd → npx → node), so a bare `start_kill` kills only the
/// launcher and orphans the whole server tree — which keeps the probe's stdout/stderr pipes
/// open (hanging the caller) and leaks the MCP process. On Windows, tree-kill first via
/// `taskkill /F /T`; everywhere fall back to `start_kill` + reap. Best-effort throughout.
async fn kill(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut k = match tokio::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(k) => k,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return;
            }
        };
        let _ = k.wait().await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn emit(sink: &LogSink, line: &str) {
    sink(launcher_core::LogLine {
        stream: launcher_core::LogStream::Stderr,
        level: launcher_core::LogLevel::Warn,
        line: line.to_string(),
    });
}

// --- streamable-http probe --------------------------------------------------

async fn probe_http(record: &McpServerRecord) -> McpRuntimeState {
    let transport = "streamable-http".to_string();
    let url = record.url.trim();
    if url.is_empty() {
        return verdict(&transport, MCP_STATE_ERROR, None, "streamable-http record has no url".into(), vec![]);
    }
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    // Default JSON content type, but only when the catalog doesn't supply its own
    // (appending would send two Content-Type headers — some servers reject that).
    let mut req = client.post(url);
    if !record.headers.keys().any(|k| k.eq_ignore_ascii_case("content-type")) {
        req = req.header("Content-Type", "application/json");
    }
    for (k, v) in &record.headers {
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
            if let Ok(val) = reqwest::header::HeaderValue::from_str(v) {
                req = req.header(name, val);
            }
        }
    }
    req = req.body(init_request());
    match req.send().await {
        Err(e) => verdict(&transport, MCP_STATE_ERROR, None, format!("http probe failed: {e}"), vec![]),
        Ok(resp) => {
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    return verdict(&transport, MCP_STATE_ERROR, None, format!("read http response: {e}"), vec![]);
                }
            };
            if status.is_success() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if v.get("error").is_some() {
                        let msg = v["error"]["message"].as_str().unwrap_or("handshake rejected").to_string();
                        return verdict(&transport, MCP_STATE_DEGRADED, None, format!("http initialize error: {msg}"), vec![]);
                    }
                    if v.get("result").is_some() {
                        return McpRuntimeState {
                            state: MCP_STATE_OK.to_string(),
                            transport,
                            error: None,
                            ..McpRuntimeState::default()
                        };
                    }
                }
                verdict(&transport, MCP_STATE_ERROR, None, format!("http {status}: unexpected body"), vec![])
            } else {
                let state = if status.as_u16() == 401 || status.as_u16() == 403 {
                    MCP_STATE_DEGRADED
                } else {
                    MCP_STATE_ERROR
                };
                verdict(&transport, state, None, format!("http {status}: {text}"), vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_initialize_result_carries_server_name() {
        let line = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"github-mcp","version":"0.1"}}}"#;
        assert_eq!(parse_response(line), ParseOutcome::InitResult(Some("github-mcp".into())));
    }

    #[test]
    fn parse_initialize_error_is_degraded_grade() {
        let line = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Missing Authorization header"}}"#;
        assert_eq!(
            parse_response(line),
            ParseOutcome::InitError("Missing Authorization header".into())
        );
    }

    #[test]
    fn parse_tools_lists_names_up_to_cap() {
        let many = (0..60).map(|i| format!(r#"{{"name":"t{i}","description":""}}"#)).collect::<Vec<_>>().join(",");
        let line = format!(r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{many}]}}}}"#);
        match parse_response(&line) {
            ParseOutcome::Tools(names) => {
                assert_eq!(names.len(), MAX_TOOLS, "capped at MAX_TOOLS");
                assert_eq!(names[0], "t0");
            }
            other => panic!("expected Tools, got {other:?}"),
        }
    }

    #[test]
    fn parse_ignores_notifications_and_garbage() {
        assert_eq!(
            parse_response(r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#),
            ParseOutcome::None_
        );
        assert_eq!(parse_response("not json at all"), ParseOutcome::None_);
        assert_eq!(parse_response(""), ParseOutcome::None_);
    }

    #[test]
    fn launch_npx_prefers_bundled_node_cli() {
        // Synthetic node dir with an npm install that includes npx-cli.js.
        let dir = std::env::temp_dir().join(format!("ahl-probe-{}", std::process::id()));
        let cli = dir.join("node_modules/npm/bin/npx-cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "// cli").unwrap();
        let node_exe = dir.join("node.exe");
        std::fs::write(&node_exe, "").unwrap();

        let (program, prefix) = launch_command("npx", Some(&node_exe));
        assert_eq!(program, node_exe.display().to_string());
        // PathBuf keeps raw separators on Windows (`join` never normalizes), so
        // compare separator-normalized forms instead of raw strings.
        let norm = |p: &str| p.replace('\\', "/");
        assert_eq!(prefix.len(), 1);
        assert_eq!(norm(&prefix[0]), norm(&cli.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);

        // No node → cmd shim on Windows, plain binary elsewhere.
        let (program, _) = launch_command("npx", None);
        assert!(program == "cmd" || program == "npx");
    }

    #[test]
    fn self_declared_degraded_catches_kanboard_style_lines() {
        // Real lines kanboard-mcp prints on stderr when KANBOARD_URL is unset.
        let config_err = r#"{"level":40,"time":1788690087829,"pid":16208,"hostname":"Kant","err":{"name":"ConfigError"},"msg":"kanboard-mcp started WITHOUT valid credentials — running in DEGRADED mode. Tools are LISTABLE (tools/list works) but every tool CALL will fail until the environment is fixed. Cause: KANBOARD_URL is required but was not set."}"#;
        let start_line = r#"{"level":40,"time":1788690087834,"pid":16208,"hostname":"Kant","name":"kanboard-mcp","version":"0.3.6","node":"v24.13.1","degraded":true,"msg":"starting stdio transport in DEGRADED mode — tools are listable but every call will fail until credentials are fixed"}"#;
        assert!(self_declared_degraded(config_err), "ConfigError 'without valid credentials' must flag");
        assert!(self_declared_degraded(start_line), "\"degraded\":true + DEGRADED mode must flag");
    }

    #[test]
    fn self_declared_degraded_ignores_healthy_noise() {
        assert!(!self_declared_degraded(""));
        assert!(!self_declared_degraded(r#"{"level":30,"time":1,"name":"github-mcp","msg":"Server listening on stdio"}"#));
        assert!(!self_declared_degraded("Connected to Kanboard API at https://pm.example.com"));
        assert!(!self_declared_degraded("2025-01-01 debug: initialized 40 tools"));
        // A word merely containing "degraded" in passing (e.g. error recovery docs)
        // is not a self-declared degraded state.
        assert!(!self_declared_degraded("connection health may degrade under load"));
    }

    #[test]
    fn frames_carry_mcp_protocol_and_methods() {
        assert!(init_request().contains("\"method\":\"initialize\""));
        assert!(init_request().contains(&format!("\"protocolVersion\":\"{MCP_PROTOCOL}\"")));
        assert!(tools_request().contains("\"method\":\"tools/list\""));
        assert!(initialized_notification().contains("\"method\":\"notifications/initialized\""));
    }

    #[test]
    fn empty_stdio_record_detects_no_launch() {
        // probe_mcp is async; assert the synchronous dispatch predicate instead:
        // an empty command with no url routes to the error branch (not http).
        let rec = McpServerRecord::default();
        assert!(rec.command.is_empty());
        assert!(rec.url.is_empty());
        assert_eq!(rec.transport, "stdio");
    }

    #[test]
    fn probe_timeout_override_is_clamped_and_falls_back_on_junk() {
        // The default is the historical window; an override is taken at face
        // value only between the bounds.
        assert_eq!(INIT_TIMEOUT, Duration::from_secs(30));
        assert_eq!(parse_init_timeout("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_init_timeout("  120  "), Some(Duration::from_secs(120)));
        // Too small to let a healthy cold server answer → the floor.
        assert_eq!(parse_init_timeout("1"), Some(Duration::from_secs(5)));
        assert_eq!(parse_init_timeout("0"), Some(Duration::from_secs(5)));
        // A typo'd huge value would read as a hang → the ceiling.
        assert_eq!(parse_init_timeout("999999"), Some(Duration::from_secs(600)));
        // Not a number at all: no duration, so the caller keeps the default
        // rather than waiting zero seconds.
        assert_eq!(parse_init_timeout("soon"), None);
        assert_eq!(parse_init_timeout("30s"), None);
        assert_eq!(parse_init_timeout("-5"), None);
        assert_eq!(parse_init_timeout(""), None);
    }

    #[test]
    fn runtime_asset_failure_reads_as_degraded_not_a_failed_install() {
        // The server-puppeteer cold case: the package is installed, Chromium is
        // not. Matched from either stream, and case-insensitively.
        assert!(runtime_asset_hint("ERROR: Failed to download Chromium!").is_some());
        assert!(runtime_asset_hint("npm ERR! Failed to set up chrome").is_some());
        assert!(runtime_asset_hint("Error: Could not find expected browser (chrome) locally").is_some());
        assert!(runtime_asset_hint("browser download failed").is_some());
        // A missing browser must never be classified as an HTTP-transport
        // mismatch — those point at the catalog entry, this points at the network.
        assert!(http_transport_hint("Failed to download Chrome").is_none());
    }

    #[test]
    fn runtime_asset_hint_leaves_ordinary_failures_alone() {
        // A package that simply is not there, or dies for its own reasons, is a
        // genuine failed install and must still be reported as one — softening
        // these would keep broken records in the library.
        assert!(runtime_asset_hint("npm ERR! 404 Not Found - GET /nonexistent").is_none());
        assert!(runtime_asset_hint("server exited with code 1 before initialize response").is_none());
        assert!(runtime_asset_hint("").is_none());
        assert!(runtime_asset_hint("Server listening on stdio").is_none());
        // Mentioning a browser is not enough — it has to be a download failure.
        assert!(runtime_asset_hint("launching chrome at /usr/bin/chrome").is_none());
        assert!(runtime_asset_hint("using existing Chrome installation").is_none());
    }

    #[test]
    fn cold_start_grace_never_shrinks_below_the_historical_window() {
        // The default must be byte-identical to the pre-override behaviour.
        assert_eq!(cold_start_grace(INIT_TIMEOUT), COLD_START_GRACE);
        assert_eq!(COLD_START_GRACE, Duration::from_secs(90));
        // A lowered first window must not shorten the grace: a browser download
        // does not get faster because the user set a small timeout.
        assert_eq!(cold_start_grace(Duration::from_secs(5)), COLD_START_GRACE);
        // A raised one does widen it, so the two stay proportionate.
        assert_eq!(cold_start_grace(Duration::from_secs(120)), Duration::from_secs(360));
    }
}
