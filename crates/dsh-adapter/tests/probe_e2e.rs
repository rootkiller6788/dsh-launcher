// End-to-end sanity of probe_stdio's stdin delivery against a REAL MCP server.
//
// Regression for the Windows pipe-stdin bug: Rust std::process anonymous-pipe stdin is
// non-overlapped, node's libuv can't async-read it, so initialize never reached the server
// and every probe timed out with "running on stdio" on stderr. The fix stages the frames in
// a temp file that becomes the child's stdin. These tests assert probe_mcp classifies a
// real SDK server as ok — #[ignore] because they need network/npx and node on PATH.
use std::sync::Arc;

use dsh_adapter::mcp_probe::probe_mcp;
use launcher_core::{McpServerRecord, MCP_STATE_ERROR, MCP_STATE_OK};

fn sink() -> launcher_core::process::LogSink {
    Arc::new(|line| eprintln!("[probe] {}", line.line))
}

fn github_record(cmd: &str, args: Vec<String>) -> McpServerRecord {
    McpServerRecord {
        id: "modelcontextprotocol/server-github".into(),
        server_name: "github".into(),
        transport: "stdio".into(),
        command: cmd.into(),
        args,
        enabled: true,
        ..McpServerRecord::default()
    }
}

fn path_node() -> Option<std::path::PathBuf> {
    // Resolve node the same way the app's fallback does when no vendored node has npm:
    // look for node.exe on PATH.
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for cand in ["node.exe", "node"] {
            let p = dir.join(cand);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_real_github_server_via_cmd_npx() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(async {
        // node=None → launch_command falls back to `cmd /C npx` on Windows.
        probe_mcp(
            &github_record(
                "npx",
                vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            ),
            None,
            sink(),
        )
        .await
    });
    eprintln!("VERDICT(no-node): {}", state.state);
    assert_eq!(state.state, MCP_STATE_OK, "error: {:?}", state.error);
    assert!(
        !state.tools.is_empty(),
        "expected tools/list to be answered"
    );
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_real_github_server_via_path_node() {
    let Some(node) = path_node() else {
        eprintln!("SKIP: no node on PATH");
        return;
    };
    eprintln!("node: {}", node.display());
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(async {
        // node with an npm install → launch_command uses `node <npx-cli.js>`.
        probe_mcp(
            &github_record(
                "npx",
                vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            ),
            Some(&node),
            sink(),
        )
        .await
    });
    eprintln!("VERDICT(path-node): {}", state.state);
    assert_eq!(state.state, MCP_STATE_OK, "error: {:?}", state.error);
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_puppeteer() {
    probe_pkg("@modelcontextprotocol/server-puppeteer");
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_wiki_explorer() {
    // Without `--stdio` the package boots a streamable-HTTP app server on :3001 and never
    // answers a stdio initialize; with it, it runs as a proper stdio MCP server.
    probe_pkg_args(
        "@modelcontextprotocol/server-wiki-explorer",
        vec!["--stdio"],
    );
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_wiki_explorer_without_stdio_flag_fails_fast() {
    // Guard for the HTTP-transport heuristic (B): the ext-apps package defaults to an
    // HTTP listener. A stdio probe must fail FAST with an explicit message instead of
    // sitting out the 30s init window with a bare "no initialize" timeout.
    let rec = github_record(
        "npx",
        vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-wiki-explorer".to_string(),
        ],
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(async { probe_mcp(&rec, None, sink()).await });
    eprintln!(
        "VERDICT(no-stdio-flag): state={} error={:?}",
        state.state, state.error
    );
    assert_eq!(state.state, MCP_STATE_ERROR, "error: {:?}", state.error);
    let err = state.error.as_deref().unwrap_or("");
    assert!(
        err.contains("HTTP"),
        "expected transport hint in error, got: {err}"
    );
}

#[test]
#[ignore = "hits network/npx; run explicitly: cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture"]
fn probe_memory() {
    probe_pkg("@modelcontextprotocol/server-memory");
}

fn probe_pkg(pkg: &str) {
    probe_pkg_args(pkg, vec![]);
}

fn probe_pkg_args(pkg: &str, extra: Vec<&str>) {
    let mut args = vec!["-y".to_string(), pkg.to_string()];
    args.extend(extra.into_iter().map(String::from));
    let rec = github_record("npx", args);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let state = rt.block_on(async { probe_mcp(&rec, None, sink()).await });
    eprintln!(
        "VERDICT({pkg}): state={} error={:?} tools={}",
        state.state,
        state.error,
        state.tools.len()
    );
}

#[test]
#[ignore = "spawn-mechanism A/B against a real package"]
fn std_vs_tokio_spawn_puppeteer_piped() {
    let pkg = "@modelcontextprotocol/server-puppeteer";
    let frames = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"r","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#;
    let path = std::env::temp_dir().join(format!("ahl-ab2-{}.jsonl", std::process::id()));
    std::fs::write(&path, frames).unwrap();

    // --- std::process, piped, drained by threads ---
    {
        use std::io::{BufRead as _, BufReader};
        let f = std::fs::File::open(&path).unwrap();
        let mut sc = std::process::Command::new("cmd");
        sc.args(["/C", "npx", "-y", pkg]);
        sc.stdin(std::process::Stdio::from(f))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = sc.spawn().unwrap();
        let so = child.stdout.take().unwrap();
        let se = child.stderr.take().unwrap();
        let t1 = std::thread::spawn(move || {
            for l in BufReader::new(so).lines().map_while(Result::ok) {
                eprintln!("[std-out] {l}");
            }
        });
        let t2 = std::thread::spawn(move || {
            for l in BufReader::new(se).lines().map_while(Result::ok) {
                eprintln!("[std-err] {l}");
            }
        });
        let code = child.wait().unwrap();
        eprintln!("STD-piped exit: {:?}", code.code());
        let _ = t1.join();
        let _ = t2.join();
    }
    // --- tokio::process, piped, drained async ---
    {
        use tokio::io::{AsyncBufReadExt as _, BufReader as TBuf};
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let f2 = std::fs::File::open(&path).unwrap();
            let mut tc = tokio::process::Command::new("cmd");
            tc.args(["/C", "npx", "-y", pkg]);
            tc.stdin(std::process::Stdio::from(f2))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = tc.spawn().unwrap();
            let so = child.stdout.take().unwrap();
            let se = child.stderr.take().unwrap();
            let h1 = tokio::spawn(async move {
                let mut r = TBuf::new(so).lines();
                while let Ok(Some(l)) = r.next_line().await {
                    eprintln!("[tok-out] {l}");
                }
            });
            let h2 = tokio::spawn(async move {
                let mut r = TBuf::new(se).lines();
                while let Ok(Some(l)) = r.next_line().await {
                    eprintln!("[tok-err] {l}");
                }
            });
            let status = child.wait().await.unwrap();
            eprintln!("TOKIO-piped exit: {:?}", status.code());
            let _ = h1.await;
            let _ = h2.await;
        });
    }
    let _ = std::fs::remove_file(&path);
}

/// A registry-backed python (uvx) MCP on a machine WITHOUT uv must fail the
/// install-time download with a readable "uv not found" error — never silently
/// write the config row or hang. This machine has no uv on PATH, so the run
/// exercises the real honest-failure branch; if uv *is* installed the test skips
/// (that path is covered by warmable/classify unit tests + CI elsewhere).
#[test]
#[ignore = "network-independent but environment-dependent (uv on PATH); run explicitly"]
fn prefetch_uv_without_uv_reports_honest_error() {
    use dsh_adapter::mcp_prefetch::prefetch_mcp;
    use launcher_core::{McpInstallManifest, McpLaunchSpec};

    let uv_present = std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_ok();
    if uv_present {
        eprintln!("SKIP: uv present on PATH — not the no-uv environment this test guards");
        return;
    }
    let m = McpInstallManifest {
        method: "uv".into(),
        package: "mcp-server-git".into(),
        launch: McpLaunchSpec {
            command: "uvx".into(),
            args: vec!["mcp-server-git".into()],
        },
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt.block_on(async { prefetch_mcp(&m, None, sink()).await.unwrap_err() });
    eprintln!("VERDICT(no-uv prefetch): {err}");
    assert!(
        err.contains("uv not found"),
        "expected a readable 'uv not found' error, got: {err}"
    );
}
