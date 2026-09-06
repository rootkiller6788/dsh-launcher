// Real-machine Phase-4 regression: a NON-registry MCP server (go, only a github
// source, nothing published to npm/pypi) installs through the git-local branch
// the Market uses: `mcp_local::install_local` shallow-clones the repo, builds the
// deterministic entry with the local Go toolchain, and hands back an absolute
// local command — then `probe_mcp` launches that *built artifact* (never a live
// `go run` / `npx github:` fetch) and must answer initialize + tools/list.
//
// `bivex/kanboard-mcp` is chosen because it is a real go stdio MCP server whose
// startup needs no config/API key (backend is only contacted at tool-call time),
// and its dependency tree is small (mark3labs/mcp-go + 3 transitive), so the
// build is fast and reproducible. `#[ignore]` because it clones + builds over the
// network. Run explicitly:
//
//   cargo test -p dsh-adapter --test git_local_e2e -- --ignored --nocapture
use std::path::PathBuf;
use std::sync::Arc;

use dsh_adapter::mcp_local::install_local;
use dsh_adapter::mcp_probe::probe_mcp;
use launcher_core::{McpServerRecord, MCP_STATE_OK};

fn sink() -> launcher_core::process::LogSink {
    Arc::new(|line| eprintln!("[gitlocal] {}", line.line))
}

fn workdir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ahl-gitlocal-{tag}-{}", std::process::id()))
}

#[test]
#[ignore = "hits network/go build; run explicitly: cargo test -p dsh-adapter --test git_local_e2e -- --ignored --nocapture"]
fn go_non_registry_install_builds_and_probes_ok() {
    let target = workdir("kanboard");
    let _ = std::fs::remove_dir_all(&target);
    std::fs::create_dir_all(&target).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (launch, _repo_dir, how, probe_state) = rt.block_on(async {
        let url = "https://github.com/bivex/kanboard-mcp";
        let outcome = install_local(url, &target, None, None, sink()).await;
        let (launch, repo_dir, how) = match outcome {
            Ok(x) => x,
            Err(e) => {
                eprintln!("install_local FAILED: {e}");
                let _ = std::fs::remove_dir_all(&target);
                panic!("install_local failed: {e}");
            }
        };
        eprintln!("install_local ok: how={how} command={}", launch.command);
        // launch.command must point at a freshly-built artifact inside the clone.
        let bin = std::path::Path::new(&launch.command);
        assert!(bin.is_file(), "built binary missing: {}", launch.command);
        assert!(
            bin.starts_with(&repo_dir),
            "launch command escaped the clone dir: {}",
            launch.command
        );

        // Probe the *recorded local entry* — exactly what DSH would spawn.
        let record = McpServerRecord {
            id: "bivex/kanboard-mcp".into(),
            server_name: "kanboard".into(),
            transport: "stdio".into(),
            command: launch.command.clone(),
            args: launch.args.clone(),
            env: launch.env.clone(),
            enabled: true,
            ..McpServerRecord::default()
        };
        let state = probe_mcp(&record, None, sink()).await;
        (launch, repo_dir, how, state)
    });

    eprintln!("probe: state={} error={:?} tools={}", probe_state.state, probe_state.error, probe_state.tools.len());
    assert_eq!(probe_state.state, MCP_STATE_OK, "go-built server must probe ok, error: {:?}", probe_state.error);
    assert!(!probe_state.tools.is_empty(), "expected tools/list to be answered");
    assert_eq!(how, "deterministic", "a go.mod repo must take the deterministic branch, not ai-resolve");
    let _ = launch;
    let _ = std::fs::remove_dir_all(&target);
    assert!(!target.exists(), "cleanup must remove the whole clone dir");
}
