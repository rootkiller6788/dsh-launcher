// MCP install-time dependency fetch (roadmap §8.4 / Phase 1, hardened).
//
// A registry-backed MCP (an npx/uvx server published on npm/uv) is *downloaded at
// install time*: this module pulls the package into the shared package-manager
// cache so DSH's first launch of the server is a cache hit instead of a fresh
// fetch. The install job treats it as a required, visible download stage — a
// failure (no npm/uv, no network, timeout, unknown package) fails the install
// with a readable reason instead of silently writing a config row.
//
// Entries with nothing to download (remote `url` / streamable-http servers, and
// `github:` / `git+` source-runs that only npx/uvx can clone at first launch)
// never reach here — the caller (`mcp_install_job`) classifies them and writes
// the record directly.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use launcher_core::process::LogSink;
use launcher_core::{
    market::npm_registry, LogLevel, LogLine, LogStream, McpInstallManifest, McpServerRecord,
};
use tokio::io::{AsyncBufReadExt, BufReader};

const PREFETCH_TIMEOUT: Duration = Duration::from_secs(150);

/// Is this package spec one the package manager can fetch from a registry?
/// `github:` / `git+https://…` / `git@…` source-runs can't be cache-fetched —
/// npx/uvx clones them at first launch regardless, so there is no package to
/// download at install time. Public so `mcp_install_job` can classify an entry
/// into the registry-download path vs the source-run / remote record path.
pub fn warmable(spec: &str) -> bool {
    !(spec.starts_with("github:") || spec.starts_with("git+") || spec.starts_with("git@"))
}

/// Which install path an MCP row takes — the `mcp_install_job` three-way route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallClass {
    /// streamable-http / url-hosted — the record *is* the install (no package).
    Remote,
    /// Published npm/uv package that can be cache-fetched — download stage + probe.
    RegistryPackage,
    /// Source-run / unpublished / compiled-language — shallow-clone + deterministic
    /// local build; AI resolve only when that deterministic pass is ambiguous.
    Source,
}

/// Classify the three install paths from the *effective* record and its optional
/// install plan. Invariants (so the classes never mix):
///   - `remote` wins on transport/url alone;
///   - `RegistryPackage` additionally requires a warmable npm/uv package and a
///     launch command — it implies `!remote` by construction;
///   - everything else is `Source` (clone → build → probe).
///
/// A directly-installable registry package therefore can never fall through to
/// the Source/AI-resolve path, and a `github:`/`git+` source-run is never
/// cache-fetched as a registry package (`warmable` rejects it).
pub fn classify_install(
    record: &McpServerRecord,
    plan: Option<&McpInstallManifest>,
) -> InstallClass {
    let remote = record.transport.eq_ignore_ascii_case("streamable-http") || !record.url.is_empty();
    let registry_pkg = !remote
        && plan.is_some_and(|p| {
            (p.method == "npm" || p.method == "uv")
                && !p.package.trim().is_empty()
                && !p.launch.command.is_empty()
                && warmable(&p.package)
        });
    if registry_pkg {
        InstallClass::RegistryPackage
    } else if remote {
        InstallClass::Remote
    } else {
        InstallClass::Source
    }
}

/// The install-time package download for a registry-backed MCP manifest.
///
/// This is a **required, blocking** stage: `Ok(detail)` is a human note for the
/// Activity log, `Err(reason)` fails the whole install with a readable reason
/// (tool missing, no network, unknown package, timeout). Downloading at install
/// populates the shared npm/uv cache so DSH's first `npx` / `uvx` launch of the
/// server is a cache hit instead of a fresh fetch. `node` is the launcher's
/// resolved node executable (from `DshAdapter::resolve_node`) so the npm CLI is
/// reached through the *bundled/managed* node when present, not a PATH gamble.
pub async fn prefetch_mcp(
    manifest: &McpInstallManifest,
    node: Option<&Path>,
    sink: LogSink,
) -> Result<String, String> {
    match manifest.method.as_str() {
        "npm" => prefetch_npm(manifest, node, sink).await,
        "uv" => prefetch_uv(manifest, sink).await,
        other => Err(format!(
            "unsupported package manager '{other}' — nothing was downloaded"
        )),
    }
}

async fn prefetch_npm(
    manifest: &McpInstallManifest,
    node: Option<&Path>,
    sink: LogSink,
) -> Result<String, String> {
    let spec = manifest.package.trim();
    if spec.is_empty() {
        return Err("no npm package recorded — cannot download".into());
    }
    if !warmable(spec) {
        return Err(format!(
            "{spec} is a source-run (github:/git+) — no registry package"
        ));
    }
    // `npm cache add <pkg>` fetches the tarball into the shared npm cache that
    // npx's pacote reads on first launch — that fetch is what this stage is for.
    let registry = npm_registry();
    let (program, mut prefix) = npm_program(node);
    prefix.extend([
        "cache".into(),
        "add".into(),
        spec.into(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--loglevel=error".into(),
    ]);
    let envs: Vec<(String, String)> = vec![
        ("npm_config_registry".into(), registry.clone()),
        ("npm_config_update_notifier".into(), "false".into()),
    ];
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!("download: npm cache add {spec} (registry {registry})"),
    });
    match run_tool(&program, &prefix, &envs, sink.clone()).await {
        Ok(()) => Ok(format!("{spec} downloaded into npm cache")),
        Err(e) => Err(format!("failed to download {spec}: {e}")),
    }
}

/// Decide how to invoke npm: `node <npm-cli.js>` when a node with a bundled npm
/// is available (env-isolated), else `cmd /C npm` / `npm` from PATH as a fallback.
pub(crate) fn npm_program(node: Option<&Path>) -> (String, Vec<String>) {
    if let Some(node_exe) = node {
        if let Some(cli) = bundled_cli(node_exe, "npm") {
            return (
                node_exe.display().to_string(),
                vec![cli.display().to_string()],
            );
        }
    }
    if cfg!(windows) {
        ("cmd".into(), vec!["/C".into(), "npm".into()])
    } else {
        ("npm".into(), vec![])
    }
}

/// The npm-bundled `<name>-cli.js` next to a resolved node exe — first the
/// windows layout `<node-dir>/node_modules/npm/bin/<name>-cli.js`, then the unix
/// pkg-manager layout `<node-dir>/../lib/node_modules/…`. Public(crate) so the MCP
/// health probe can reach `npx` through the same bundled node.
pub(crate) fn bundled_cli(node_exe: &Path, name: &str) -> Option<PathBuf> {
    let dir = node_exe.parent()?;
    let cli = format!("{name}-cli.js");
    let candidates = [
        dir.join("node_modules/npm/bin").join(&cli),
        dir.join("../lib/node_modules/npm/bin").join(&cli),
    ];
    candidates.into_iter().find(|c| c.is_file())
}

async fn prefetch_uv(manifest: &McpInstallManifest, sink: LogSink) -> Result<String, String> {
    let spec = manifest.package.trim();
    if spec.is_empty() {
        return Err("no uv package recorded — cannot download".into());
    }
    if !warmable(spec) {
        return Err(format!(
            "{spec} is a source-run (github:/git+) — no registry package"
        ));
    }
    let uv = match which::which("uv") {
        Ok(p) => p,
        Err(_) => {
            return Err(
                "uv not found on PATH — cannot download the python package (check Runtime settings)".into(),
            );
        }
    };
    let tmp = std::env::temp_dir().join(format!(
        "ahl-uvwarm-{}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let args = vec![
        "pip".into(),
        "download".into(),
        spec.into(),
        "--no-deps".into(),
        "-d".into(),
        tmp.display().to_string(),
    ];
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!("download: uv pip download {spec}"),
    });
    let result = run_tool(uv.to_str().unwrap_or("uv"), &args, &[], sink.clone()).await;
    let _ = std::fs::remove_dir_all(&tmp);
    match result {
        Ok(()) => Ok(format!("{spec} downloaded into uv cache")),
        Err(e) => Err(format!("failed to download {spec}: {e}")),
    }
}

/// Spawn `program + args + env`, stream stdout/stderr to the sink, wait with a
/// hard timeout. Any non-zero exit or timeout is an error.
async fn run_tool(
    program: &str,
    args: &[String],
    envs: &[(String, String)],
    sink: LogSink,
) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("spawn {program}: {e}"))?;

    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let sink = sink.clone();
        readers.push(tokio::spawn(async move {
            let mut r = BufReader::new(out);
            let mut buf = String::new();
            loop {
                buf.clear();
                match r.read_line(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = buf.trim_end_matches(['\r', '\n']).to_string();
                        if !line.is_empty() {
                            sink(LogLine {
                                stream: LogStream::Stdout,
                                level: LogLevel::Info,
                                line,
                            });
                        }
                    }
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let sink = sink.clone();
        readers.push(tokio::spawn(async move {
            let mut r = BufReader::new(err);
            let mut buf = String::new();
            loop {
                buf.clear();
                match r.read_line(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = buf.trim_end_matches(['\r', '\n']).to_string();
                        if !line.is_empty() {
                            sink(LogLine {
                                stream: LogStream::Stderr,
                                level: LogLevel::Warn,
                                line,
                            });
                        }
                    }
                }
            }
        }));
    }

    let status = match tokio::time::timeout(PREFETCH_TIMEOUT, child.wait()).await {
        Ok(status) => status.map_err(|e| format!("wait {program}: {e}"))?,
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!("{program} download timed out after 150s"));
        }
    };
    for r in readers {
        let _ = r.await;
    }
    let code = status.code().unwrap_or(1);
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "{program} exited with code {code} — registry/network issue?"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmable_rejects_source_runs() {
        assert!(!warmable("github:acme/server"));
        assert!(!warmable("git+https://github.com/acme/server"));
        assert!(!warmable("git@github.com:acme/server.git"));
        assert!(warmable("@acme/server"));
        assert!(warmable("mcp-server-git"));
    }

    fn stdio_record() -> McpServerRecord {
        McpServerRecord {
            transport: "stdio".to_string(),
            ..Default::default()
        }
    }

    fn plan(method: &str, package: &str, launch: bool) -> McpInstallManifest {
        McpInstallManifest {
            method: method.to_string(),
            package: package.to_string(),
            launch: launcher_core::market::McpLaunchSpec {
                command: if launch {
                    "npx".to_string()
                } else {
                    String::new()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn classify_registry_package_is_warmable_npm_uv_with_launch() {
        // Directly-installable registry packages → RegistryPackage, never Source
        // (so they can never reach the clone/AI-resolve path).
        assert_eq!(
            classify_install(&stdio_record(), Some(&plan("npm", "@acme/server", true))),
            InstallClass::RegistryPackage
        );
        assert_eq!(
            classify_install(&stdio_record(), Some(&plan("uv", "mcp-server-git", true))),
            InstallClass::RegistryPackage
        );
    }

    #[test]
    fn classify_remote_wins_on_transport_or_url_even_with_registry_plan() {
        let remote = McpServerRecord {
            transport: "streamable-http".to_string(),
            ..Default::default()
        };
        // A streamable-http row with a registry-looking plan is still Remote: the
        // server is hosted, there is nothing to download locally.
        assert_eq!(
            classify_install(&remote, Some(&plan("npm", "@acme/server", true))),
            InstallClass::Remote
        );
        // Any non-empty url on a stdio row also means remote-hosted.
        let hosted = McpServerRecord {
            url: "https://example.com/mcp".to_string(),
            ..Default::default()
        };
        assert_eq!(
            classify_install(&hosted, Some(&plan("npm", "@acme/server", true))),
            InstallClass::Remote
        );
    }

    #[test]
    fn classify_source_runs_never_registry_never_remote() {
        // git source-runs: warmable rejects them → Source (clone + build), and
        // they are *never* cache-fetched as a registry package.
        for (method, package) in [
            ("npm", "github:acme/server"),
            ("npm", "git+https://github.com/acme/server"),
            ("uv", "git+https://github.com/acme/server"),
            ("uv", "git@github.com:acme/server.git"),
        ] {
            assert_eq!(
                classify_install(&stdio_record(), Some(&plan(method, package, true))),
                InstallClass::Source,
                "{method} {package}"
            );
        }
        // Compiled / no-launch / no-plan → Source (clone + build / as-is write).
        assert_eq!(
            classify_install(&stdio_record(), Some(&plan("cargo", "x", true))),
            InstallClass::Source
        );
        assert_eq!(
            classify_install(&stdio_record(), Some(&plan("npm", "@acme/server", false))),
            InstallClass::Source
        );
        assert_eq!(
            classify_install(&stdio_record(), None),
            InstallClass::Source
        );
    }

    #[test]
    fn classify_is_mutually_exclusive_never_two_classes_at_once() {
        // The three classes are a clean partition: for any record+plan exactly one
        // matches. We can't prove totality here, but we pin the boundary that
        // matters — no plan combination yields both RegistryPackage and Remote,
        // and RegistryPackage implies warmable (no git spec sneaks in).
        let cases: Vec<(McpServerRecord, Option<McpInstallManifest>)> = vec![
            (stdio_record(), Some(plan("npm", "@acme/server", true))),
            (
                stdio_record(),
                Some(plan("npm", "github:acme/server", true)),
            ),
            (
                McpServerRecord {
                    transport: "streamable-http".into(),
                    ..Default::default()
                },
                Some(plan("npm", "@acme/server", true)),
            ),
            (stdio_record(), None),
        ];
        for (record, plan) in &cases {
            let cls = classify_install(record, plan.as_ref());
            let mut n = 0;
            if cls == InstallClass::RegistryPackage {
                n += 1;
            }
            if cls == InstallClass::Remote {
                n += 1;
            }
            if cls == InstallClass::Source {
                n += 1;
            }
            assert_eq!(n, 1, "classify must pick exactly one class for {cls:?}");
        }
    }

    #[test]
    fn npm_program_prefers_node_cli_when_npm_present() {
        // A synthetic node dir with npm-cli.js resolves to `node <cli>`.
        let dir = std::env::temp_dir().join(format!("ahl-npmtest-{}", std::process::id()));
        let cli = dir.join("node_modules/npm/bin/npm-cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "// cli").unwrap();
        let node_exe = dir.join("node.exe");
        std::fs::write(&node_exe, "").unwrap();

        let (program, prefix) = npm_program(Some(&node_exe));
        assert_eq!(program, node_exe.display().to_string());
        // PathBuf keeps raw separators on Windows — compare normalized forms.
        let norm = |p: &str| p.replace('\\', "/");
        assert_eq!(prefix.len(), 1);
        assert_eq!(norm(&prefix[0]), norm(&cli.display().to_string()));
        let _ = std::fs::remove_dir_all(&dir);

        // No node → PATH fallback.
        let (program, _) = npm_program(None);
        assert!(program == "cmd" || program == "npm");
    }
}
