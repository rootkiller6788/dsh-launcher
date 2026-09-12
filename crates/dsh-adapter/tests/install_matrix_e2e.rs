// Real-machine install matrix (roadmap §11 Phase 4) — one file, one command.
//
//   cargo test -p dsh-adapter --test install_matrix_e2e -- --ignored --nocapture
//   (or run all e2e together:
//    cargo test -p dsh-adapter --tests -- --ignored --nocapture)
//
// Every row drives the *same* code the Market uses, with no UI:
//   `install_local` (shallow-clone → fingerprint → deterministic build → entry)
//   → `probe_mcp` on the recorded entry. Rows that cannot be deterministic go to
//   the honest no-AI-provider failure path — a machine WITH an AI provider key
//   wired would instead run `ai_resolve`; without one the row must fail readably,
//   never silently succeed.
//
// Real-repo rows (network: clone/npm/go/cargo) live in git_local_e2e.rs and
// probe_e2e.rs. This file uses LOCAL git fixtures so the *routing* of every
// class is exercised fast and network-independently where possible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dsh_adapter::mcp_local::install_local;
use dsh_adapter::mcp_prefetch::prefetch_mcp;
use dsh_adapter::mcp_probe::probe_mcp;
use launcher_core::{
    McpInstallManifest, McpLaunchSpec, McpServerRecord, MCP_STATE_ERROR, MCP_STATE_OK,
};

fn sink() -> launcher_core::process::LogSink {
    Arc::new(|line| eprintln!("[matrix] {}", line.line))
}

fn tmp(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ahl-matrix-{tag}-{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ))
}

/// Build a real local git repo (commit each run) so `shallow_clone` clones it
/// exactly like a github source-run — file layout + history included.
fn make_repo(dir: &Path, files: &BTreeMap<&str, &str>) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "matrix@test.local"]);
    git(dir, &["config", "user.name", "matrix"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "fixture"]);
}

fn git(cwd: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// install_local on a fixture repo; returns launch/base/how or the honest error.
/// install_local takes a clone *url*; a local path works and keeps the row
/// network-free. `repo` is the committed source, `target` is where it clones.
fn install_fixture(
    repo: &Path,
    target: &Path,
) -> Result<(dsh_adapter::mcp_local::LocalLaunch, PathBuf, String), String> {
    rt().block_on(install_local(
        &repo.to_string_lossy(),
        target,
        None,
        None,
        sink(),
    ))
}

// --- 1. node source-run: deterministic (npm/zero-dep) + real probe handshake ---

const NODE_SRV: &str = r#"const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
function send(o) { process.stdout.write(JSON.stringify(o) + '\n'); }
rl.on('line', (line) => {
  let m; try { m = JSON.parse(line); } catch { return; }
  if (m.method === 'initialize') {
    send({ jsonrpc: '2.0', id: m.id, result: {
      protocolVersion: (m.params && m.params.protocolVersion) || '2025-03-26',
      capabilities: { tools: {} },
      serverInfo: { name: 'matrix-srv', version: '1.0.0' } } });
  } else if (m.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: m.id, result: { tools: [
      { name: 'ping', description: 'p', inputSchema: { type: 'object', properties: {} } } ] } });
  } else if (m.id !== undefined) {
    send({ jsonrpc: '2.0', id: m.id, result: {} });
  }
});
"#;

#[test]
#[ignore = "node + npm on PATH; git fixture is local. Run: cargo test -p dsh-adapter --test install_matrix_e2e -- --ignored --nocapture"]
fn source_node_fixture_deterministic_probe_ok() {
    let repo = tmp("node-repo");
    make_repo(
        &repo,
        &BTreeMap::from([
            (
                "package.json",
                r#"{"name":"matrix-node-srv","version":"1.0.0","main":"server.js"}"#,
            ),
            ("server.js", NODE_SRV),
        ]),
    );
    let target = tmp("node-clone");
    let (launch, _base, how) = install_fixture(&repo, &target)
        .unwrap_or_else(|e| panic!("node source install failed: {e}"));
    eprintln!("how={how} entry={} {:?}", launch.command, launch.args);
    assert_eq!(how, "deterministic", "a package.json repo must not need AI");
    // Probe the recorded local entry `<node> <repo>/server.js` — what DSH spawns.
    let record = McpServerRecord {
        id: "local/node-srv".into(),
        server_name: "node-srv".into(),
        transport: "stdio".into(),
        command: launch.command.clone(),
        args: launch.args.clone(),
        env: launch.env.clone(),
        ..Default::default()
    };
    let state = rt().block_on(probe_mcp(&record, None, sink()));
    eprintln!(
        "probe node source: state={} error={:?}",
        state.state, state.error
    );
    assert_eq!(
        state.state, MCP_STATE_OK,
        "node fixture must handshake, err: {:?}",
        state.error
    );
    assert!(!state.tools.is_empty(), "expected tools/list answered");
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
}

// --- 2. python source-run: no uv on PATH → honest tool-missing error ---------

const PY_FIXTURE: &str = "[project]\nname = \"matrix-pysrv\"\nversion = \"0.1.0\"\n\n[project.scripts]\npysrv = \"matrix_pysrv.server:main\"\n";

#[test]
#[ignore = "git fixture local; branches on uv presence"]
fn source_python_fixture_toolchain_gate() {
    let uv_present = std::process::Command::new("uv")
        .arg("--version")
        .output()
        .is_ok();
    let repo = tmp("py-repo");
    make_repo(
        &repo,
        &BTreeMap::from([
            ("pyproject.toml", PY_FIXTURE),
            ("matrix_pysrv/__init__.py", ""),
            ("matrix_pysrv/server.py", "def main(): pass\n"),
        ]),
    );
    let target = tmp("py-clone");
    let result = install_fixture(&repo, &target);
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
    if !uv_present {
        // The honest gate this machine exercises today: a python source MCP on a
        // box without uv must fail readably, not hang or fake a record.
        let err = result.expect_err("no uv → deterministic python build must fail honestly");
        eprintln!("VERDICT(no-uv python source): {err}");
        assert!(
            err.contains("uv"),
            "expected a 'uv' toolchain hint, got: {err}"
        );
    } else {
        eprintln!("SKIP-success-path: uv present — python source success covered by real-repo e2e");
        let _ = result;
    }
}

// --- 3. rust source-run: deterministic cargo build (zero-dep fixture) --------

const CARGO_TOML: &str =
    "[package]\nname = \"matrixrs\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const RUST_MAIN: &str = "fn main() { std::process::exit(0) }\n";

#[test]
#[ignore = "cargo on PATH; zero-dep fixture builds in seconds"]
fn source_rust_fixture_deterministic_build() {
    if std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: cargo not on PATH");
        return;
    }
    let repo = tmp("rs-repo");
    make_repo(
        &repo,
        &BTreeMap::from([("Cargo.toml", CARGO_TOML), ("src/main.rs", RUST_MAIN)]),
    );
    let target = tmp("rs-clone");
    // install_local hands back the launch (not just base) — probe the build only.
    let (launch, base, how) = rt()
        .block_on(install_local(
            &repo.to_string_lossy(),
            &target,
            None,
            None,
            sink(),
        ))
        .unwrap_or_else(|e| panic!("rust source install failed: {e}"));
    eprintln!("how={how} entry={}", launch.command);
    assert_eq!(how, "deterministic");
    // Cargo.toml name → target/release/matrixrs[.exe], inside the clone.
    let bin = Path::new(&launch.command);
    assert!(
        bin.is_file(),
        "built rust binary missing: {}",
        launch.command
    );
    assert!(
        bin.starts_with(&base),
        "escaped clone dir: {}",
        launch.command
    );
    assert!(launch.args.is_empty(), "rust binary launch needs no args");
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
}

// --- 4. no fingerprint: no provider → honest failure (never silent success) --

#[test]
#[ignore = "git fixture local"]
fn source_no_fingerprint_without_provider_honest_error() {
    let repo = tmp("nf-repo");
    make_repo(&repo, &BTreeMap::from([("README.md", "# nothing here\n")]));
    let target = tmp("nf-clone");
    let err = install_fixture(&repo, &target).expect_err("README-only repo must fail honestly");
    eprintln!("VERDICT(no-fingerprint, no provider): {err}");
    assert!(
        err.contains("no AI provider"),
        "expected 'no AI provider is configured' honesty, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
}

// --- 5. Dockerfile-only: launcher never runs Docker → needs AI → honest error -

#[test]
#[ignore = "git fixture local"]
fn source_dockerfile_only_honest_no_docker() {
    let repo = tmp("dk-repo");
    make_repo(
        &repo,
        &BTreeMap::from([(
            "Dockerfile",
            "FROM node:20\nCMD [\"node\", \"server.js\"]\n",
        )]),
    );
    let target = tmp("dk-clone");
    let err = install_fixture(&repo, &target).expect_err("Dockerfile-only repo must fail honestly");
    eprintln!("VERDICT(dockerfile-only): {err}");
    assert!(
        err.contains("no AI provider"),
        "expected no-AI-provider honesty (repo needs AI to infer native entry), got: {err}"
    );
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
}

// --- 6. monorepo workspace: ambiguous → needs AI → honest error -------------

#[test]
#[ignore = "git fixture local"]
fn source_workspace_monorepo_honest_ambiguous() {
    let repo = tmp("ws-repo");
    make_repo(
        &repo,
        &BTreeMap::from([
            (
                "package.json",
                r#"{"name":"matrix-mono","private":true,"workspaces":["packages/*"]}"#,
            ),
            (
                "packages/a/package.json",
                r#"{"name":"@matrix/a","version":"1.0.0","main":"index.js"}"#,
            ),
        ]),
    );
    let target = tmp("ws-clone");
    let err = install_fixture(&repo, &target).expect_err("workspace root must be honest ambiguous");
    eprintln!("VERDICT(workspace monorepo): {err}");
    assert!(
        err.contains("no AI provider"),
        "expected honest no-AI failure for ambiguous workspace, got: {err}"
    );
    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&target);
}

// --- 7. registry warmable guard: a source-run manifest never cache-prefetches --

#[test]
#[ignore = "no network; fast"]
fn registry_prefetch_rejects_source_run_manifest() {
    let m = McpInstallManifest {
        runtime: "node".into(),
        method: "npm".into(),
        package: "github:acme/server".into(),
        launch: McpLaunchSpec {
            command: "npx".into(),
            args: vec!["-y".into(), "github:acme/server".into()],
        },
    };
    let err = rt()
        .block_on(prefetch_mcp(&m, None, sink()))
        .expect_err("source-run manifest must not be cache-fetched");
    eprintln!("VERDICT(prefetch git manifest): {err}");
    assert!(
        err.contains("source-run"),
        "expected source-run refusal, got: {err}"
    );
}

// --- 8. remote streamable-http: unreachable endpoint fails fast & honest -----

#[test]
#[ignore = "localhost connect; fast"]
fn remote_unreachable_http_fails_fast_honest() {
    let record = McpServerRecord {
        id: "local/nowhere".into(),
        server_name: "nowhere".into(),
        transport: "streamable-http".into(),
        // Port 1 refuses immediately — the connect probe must error readably,
        // not hang out the whole init window.
        url: "http://127.0.0.1:1/mcp".into(),
        ..Default::default()
    };
    let state = rt().block_on(probe_mcp(&record, None, sink()));
    eprintln!(
        "VERDICT(remote unreachable): state={} error={:?}",
        state.state, state.error
    );
    assert_eq!(
        state.state, MCP_STATE_ERROR,
        "unreachable remote must be an honest error"
    );
}
