// MCP local (git true-)install + AI fallback (roadmap Phase 4).
//
// A non-registry, non-remote MCP server is **built from source at install time**
// instead of being written as an `npx github:…` / `uvx --from git+…` pseudo
// command that DSH re-fetches (and often fails to build) on first launch. The
// launcher shallow-clones the repo into the instance's per-server `mcp/` dir,
// fingerprints it (`go.mod` / `Cargo.toml` / `package.json` / `pyproject…`),
// installs/builds the deterministic entry, and records an **absolute local
// command** that DSH (and the install-time health probe) can spawn directly.
//
// When the repo is too ambiguous for the deterministic pass (no-fingerprint,
// monorepo, unusual layout), the **AI fallback** reads the *actually cloned*
// files and picks a runnable entry. It never invents a shell command: it may
// only choose a language + a repo-relative entry file, which we then run through
// our own interpreters/builds — every path is validated to live inside the
// clone, spawned without a shell, and the install-time probe is the terminal
// judge. `git clone` is always shallow (`--depth 1`).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use launcher_core::process::LogSink;
use launcher_core::{market::npm_registry, LogLevel, LogLine, LogStream, ResolvedProvider};

// Single source of truth for the durations lives in lib.rs (`run_timed`).
pub(crate) const CLONE_TIMEOUT: Duration = crate::GIT_TIMEOUT;
pub(crate) const BUILD_TIMEOUT: Duration = crate::INSTALL_TIMEOUT;
/// `.venv`/`node_modules` presence is cheap and decisive; install/build output
/// streams to the Activity log. Non-zero exit or timeout is a hard error.
const SKIP_DIRS: [&str; 8] = [
    ".git", "node_modules", ".venv", "venv", "target", "dist", ".tox", "__pycache__",
];

// --- public surface ---------------------------------------------------------

/// A ready-to-record local launch: `command` is an **absolute** program path the
/// launcher (and DSH) can spawn directly; `args` carry absolute entry paths and
/// literal flags; `env` merges onto the record. `transport` stays `stdio`.
#[derive(Debug, Clone)]
pub struct LocalLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Why the deterministic pass could not finish. `NeedsAi` is *not* a failure —
/// it hands off to [`ai_resolve`]. The other two are hard, readable failures.
pub(crate) enum ResolveFail {
    /// Deterministic resolution saw nothing concrete to run — the AI may help.
    NeedsAi(String),
    /// A required toolchain is missing (go/cargo/uv) — the AI cannot conjure it.
    ToolMissing(String),
    /// An install/build step failed; its output already streamed to the sink.
    Failed(String),
}

impl ResolveFail {
    pub(crate) fn into_err(self) -> String {
        match self {
            ResolveFail::NeedsAi(detail) => detail,
            ResolveFail::ToolMissing(detail) => detail,
            ResolveFail::Failed(detail) => detail,
        }
    }
}

/// The whole local-install orchestration the install job runs once:
/// shallow clone → fingerprint-base → deterministic build → (AI fallback).
/// Returns the launch to record plus the resolved base dir (for logs / cleanup).
pub async fn install_local(
    url: &str,
    target: &Path,
    node: Option<&Path>,
    provider: Option<&ResolvedProvider>,
    sink: LogSink,
) -> Result<(LocalLaunch, PathBuf, String), String> {
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!("clone: git clone --depth 1 {url}"),
    });
    shallow_clone(url, target, sink.clone()).await?;

    let sub = github_subpath(url);
    let base = repo_base(target, sub.as_deref());
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!("repo: fingerprint base {}", base.display()),
    });

    match build_local(&base, node, sink.clone()).await {
        Ok(launch) => Ok((launch, base, "deterministic".to_string())),
        Err(ResolveFail::NeedsAi(summary)) => {
            sink(LogLine {
                stream: LogStream::Stdout,
                level: LogLevel::Info,
                line: format!("ai-resolve: deterministic pass ambiguous — {summary}"),
            });
            let Some(provider) = provider else {
                return Err(format!(
                    "{summary} — no AI provider is configured, so this repo's entry cannot be \
                     inferred. Install the Go/Rust/uv toolchain if it needs one, or add an API \
                     key in Settings → Providers and retry."
                ));
            };
            let launch = ai_resolve(&base, provider, node, sink).await?;
            Ok((launch, base, format!("ai-resolve ({summary})")))
        }
        Err(ResolveFail::ToolMissing(detail)) => Err(detail),
        Err(ResolveFail::Failed(detail)) => Err(detail),
    }
}

// --- github url parsing -----------------------------------------------------

/// Parse a catalog GitHub url into `(clone_url, repo_leaf, subdir)` — the clone
/// is always of the repo *root*; `subdir` is whatever `/tree/<ref>/<path>` or
/// `/blob/<ref>/<path>` names (the monorepo subdirectory the server lives in).
pub fn github_source(url: &str) -> Option<(String, String, Option<String>)> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let segs = rest.split('/').collect::<Vec<_>>();
    let owner = segs.first()?.to_string();
    if owner.is_empty() {
        return None;
    }
    let repo = segs.get(1).map(|s| s.trim_end_matches(".git").to_string())?;
    if repo.is_empty() || repo == "tree" || repo == "blob" {
        return None;
    }
    let mut sub = None;
    if segs.len() > 2 && (segs[2] == "tree" || segs[2] == "blob") {
        // `/tree/<ref>/<a>/<b>` or `/blob/<ref>/<a>/<b>` — skip the first two
        // (`tree`/`blob`, the ref) and treat the rest as the subdir.
        if segs.len() > 4 {
            let joined = segs[4..].join("/");
            if !joined.is_empty() {
                sub = Some(joined);
            }
        }
    }
    let clone = format!("https://github.com/{owner}/{repo}");
    Some((clone, repo.to_string(), sub))
}

fn github_subpath(url: &str) -> Option<String> {
    github_source(url).and_then(|(_, _, sub)| sub)
}

/// The directory the fingerprint/build runs in: the URL-named subdir when it
/// exists inside the clone (monorepo), else the clone root.
fn repo_base(root: &Path, sub: Option<&str>) -> PathBuf {
    match sub {
        Some(s) if !s.is_empty() => {
            let cand = root.join(s);
            if cand.is_dir() {
                return cand;
            }
            root.to_path_buf()
        }
        _ => root.to_path_buf(),
    }
}

// --- shallow clone ----------------------------------------------------------

async fn shallow_clone(url: &str, target: &Path, sink: LogSink) -> Result<(), String> {
    // A retry after a half-failed earlier attempt may have left the dir.
    if target.exists() {
        std::fs::remove_dir_all(target).map_err(|e| format!("clean stale clone: {e}"))?;
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create clone dir: {e}"))?;
    }
    let mut args: Vec<String> = vec!["clone".into(), "--depth".into(), "1".into(), "--".into()];
    args.push(url.to_string());
    args.push(path_arg(target));
    match run_cmd("git", &args, target.parent().unwrap_or(target), &[], sink, CLONE_TIMEOUT).await
    {
        Ok(()) => Ok(()),
        Err(e) => Err(format!(
            "git clone {url} failed — is git on PATH, the repo public, and is github.com \
             reachable? {e}"
        )),
    }
}

// --- parsers (pure, ported from scripts/resolver/analyze-repo.mjs) ----------

/// The package.json fields a local node build needs.
#[derive(Debug, Default, PartialEq)]
struct PkgInfo {
    name: String,
    private: bool,
    workspaces: bool,
    /// Resolved entry relative to the package dir (bin path → main → index.js).
    entry: Option<String>,
    /// `scripts.build` present (a dist build is likely needed before launch).
    has_build: bool,
    /// Any real dependency map (dependencies / devDependencies, non-empty).
    has_deps: bool,
}

fn parse_package_json_full(text: &str) -> Option<PkgInfo> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = v.as_object()?;
    let name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let private = obj
        .get("private")
        .and_then(|p| p.as_bool())
        .unwrap_or(false);
    let workspaces = obj.get("workspaces").is_some_and(|w| match w {
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(m) => !m.is_empty(),
        _ => false,
    });
    // bin: string → entry path; object → the key matching `name`, else the first.
    let mut entry: Option<String> = None;
    if let Some(bin) = obj.get("bin") {
        if let Some(s) = bin.as_str() {
            entry = Some(s.trim_start_matches("./").to_string());
        } else if let Some(m) = bin.as_object() {
            let chosen = m
                .get(&name)
                .or_else(|| m.values().next())
                .and_then(|p| p.as_str())
                .map(|s| s.trim_start_matches("./").to_string());
            entry = chosen;
        }
    }
    if entry.is_none() {
        if let Some(main) = obj.get("main").and_then(|m| m.as_str()) {
            entry = Some(main.trim_start_matches("./").to_string());
        }
    }
    let has_build = obj
        .get("scripts")
        .and_then(|s| s.get("build"))
        .and_then(|b| b.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    let has_deps = ["dependencies", "devDependencies"].iter().any(|k| {
        obj.get(*k).is_some_and(|d| match d {
            serde_json::Value::Object(m) => !m.is_empty(),
            _ => false,
        })
    });
    Some(PkgInfo {
        name,
        private,
        workspaces,
        entry,
        has_build,
        has_deps,
    })
}

/// `[project] name` + the `[project.scripts]` console-script names.
#[derive(Debug, Default, PartialEq)]
struct PyInfo {
    name: Option<String>,
    scripts: Vec<String>,
}

fn parse_pyproject_full(text: &str) -> Option<PyInfo> {
    let mut table: Option<String> = None;
    let mut name: Option<String> = None;
    let mut scripts: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            table = Some(line[1..line.len() - 1].trim().to_string());
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let val = line[eq + 1..].trim();
        match table.as_deref() {
            Some("project") if key == "name" => {
                name = Some(unquote_toml(val));
            }
            Some("project.scripts") => {
                let n = unquote_toml(key);
                if !n.is_empty() {
                    scripts.push(n);
                }
            }
            _ => {}
        }
    }
    name.map(|name| PyInfo { name: Some(name), scripts })
}

fn unquote_toml(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"') || (b[0] == b'\'' && b[s.len() - 1] == b'\'') {
            return s[1..s.len() - 1].trim().to_string();
        }
    }
    s.to_string()
}

fn setup_name(text: &str) -> Option<String> {
    // `name="pkg"` / `name = 'pkg'` inside a setup() call — simple scan.
    let mut rest = text;
    while let Some(needle) = rest.find("name") {
        let after = &rest[needle + 4..];
        let after = after.trim_start_matches([' ', '\t', '\n']);
        let after = after.strip_prefix('=')?;
        let after = after.trim_start_matches([' ', '\t', '\n']);
        let quote = after.chars().next()?;
        if quote == '"' || quote == '\'' {
            let end = after[1..].find(quote)?;
            let val = after[1..=end].trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
        rest = after;
    }
    None
}

/// `[package] name` from a Cargo.toml (top-level only — workspace roots return
/// `None`, which sends the repo to the AI since the binary name is ambiguous).
fn cargo_package_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_package = line[1..line.len() - 1].trim() == "package";
            continue;
        }
        if in_package {
            let Some(eq) = line.find('=') else { continue };
            if line[..eq].trim() == "name" {
                let v = unquote_toml(&line[eq + 1..]);
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

// --- deterministic ladder ---------------------------------------------------

/// Fingerprint the base dir and build the deterministic local entry. The order
/// prefers compiled languages (their fingerprints are unambiguous) over the
/// scripted heuristics, and ends in `NeedsAi` for anything ambiguous.
async fn build_local(
    base: &Path,
    node: Option<&Path>,
    sink: LogSink,
) -> Result<LocalLaunch, ResolveFail> {
    if base.join("go.mod").is_file() {
        return build_go(base, sink).await;
    }
    if base.join("Cargo.toml").is_file() {
        return build_rust(base, sink).await;
    }
    if base.join("package.json").is_file() {
        return build_node(base, node, sink).await;
    }
    if base.join("pyproject.toml").is_file()
        || base.join("setup.py").is_file()
        || base.join("requirements.txt").is_file()
    {
        return build_python(base, sink).await;
    }
    if base.join("Dockerfile").is_file() {
        return Err(ResolveFail::NeedsAi(
            "repo declares only a Dockerfile — the launcher never runs Docker; the AI reads its \
             CMD/ENTRYPOINT to infer a native entry"
                .into(),
        ));
    }
    Err(ResolveFail::NeedsAi(
        "repo has no recognizable package manifest (package.json / pyproject / Cargo / go.mod)".into(),
    ))
}

async fn build_go(base: &Path, sink: LogSink) -> Result<LocalLaunch, ResolveFail> {
    let go = which::which("go").map_err(|_| {
        ResolveFail::ToolMissing(
            "this repo is a Go MCP server — Go is not on PATH. Install Go (https://go.dev/dl) \
             and retry"
                .into(),
        )
    })?;
    let bin_dir = base.join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| ResolveFail::Failed(format!("mkdir bin: {e}")))?;
    let name = bin_slug(&leaf_name(base));
    let out = bin_dir.join(exe_name(&name));
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!("build: go build -o {} .", out.display()),
    });
    let args: Vec<String> = vec!["build".into(), "-o".into(), path_arg(&out), ".".into()];
    run_cmd(
        go.to_str().unwrap_or("go"),
        &args,
        base,
        &[],
        sink,
        BUILD_TIMEOUT,
    )
    .await
    .map_err(|e| ResolveFail::Failed(format!("go build failed: {e}")))?;
    if !out.is_file() {
        return Err(ResolveFail::Failed(
            "go build finished without producing a binary".into(),
        ));
    }
    Ok(LocalLaunch {
        command: path_arg(&out),
        args: vec![],
        env: HashMap::new(),
    })
}

async fn build_rust(base: &Path, sink: LogSink) -> Result<LocalLaunch, ResolveFail> {
    let cargo = which::which("cargo").map_err(|_| {
        ResolveFail::ToolMissing(
            "this repo is a Rust MCP server — Cargo is not on PATH. Install Rust \
             (https://rustup.rs) and retry"
                .into(),
        )
    })?;
    let toml = std::fs::read_to_string(base.join("Cargo.toml"))
        .map_err(|e| ResolveFail::Failed(format!("read Cargo.toml: {e}")))?;
    let crate_name = cargo_package_name(&toml)
        .ok_or_else(|| ResolveFail::NeedsAi("Cargo.toml has no [package] name (workspace root)".into()))?;
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: "build: cargo build --release".into(),
    });
    let args = vec!["build".to_string(), "--release".to_string()];
    run_cmd(
        cargo.to_str().unwrap_or("cargo"),
        &args,
        base,
        &[],
        sink,
        BUILD_TIMEOUT,
    )
    .await
    .map_err(|e| ResolveFail::Failed(format!("cargo build failed: {e}")))?;
    let out = base.join("target").join("release").join(exe_name(&crate_name));
    if !out.is_file() {
        return Err(ResolveFail::Failed(format!(
            "cargo build finished without producing target/release/{crate_name}"
        )));
    }
    Ok(LocalLaunch {
        command: path_arg(&out),
        args: vec![],
        env: HashMap::new(),
    })
}

async fn build_node(
    base: &Path,
    node: Option<&Path>,
    sink: LogSink,
) -> Result<LocalLaunch, ResolveFail> {
    let pkg = read_pkg(base, "package.json unreadable")?;
    if pkg.workspaces {
        return Err(ResolveFail::NeedsAi(
            "repo root is a workspace monorepo (multiple packages) — which one is the MCP \
             server is ambiguous"
                .into(),
        ));
    }
    let node_abs = node_or_resolve(node).ok_or_else(|| {
        ResolveFail::ToolMissing("Node not found — cannot run a node-based MCP".into())
    })?;
    node_deps(base, node, sink.clone()).await?;
    let Some(entry_rel) = pkg.entry.clone() else {
        // `main` absent → node's own default is index.js.
        if base.join("index.js").is_file() {
            return Ok(node_launch(&node_abs, &base.join("index.js")));
        }
        return Err(ResolveFail::NeedsAi(
            "package.json declares no bin/main and no index.js".into(),
        ));
    };
    let entry = base.join(&entry_rel);
    if !entry.is_file() {
        // A bin that needs `npm run build` to produce its dist entry.
        if pkg.has_build {
            run_npm(base, &["run".into(), "build".into()], node, sink).await.map_err(
                |e| ResolveFail::Failed(format!("npm run build failed: {e}")),
            )?;
        }
        if !entry.is_file() {
            return Err(ResolveFail::NeedsAi(format!(
                "package entry '{entry_rel}' does not exist even after install/build — the repo \
                 likely needs a custom build step"
            )));
        }
    }
    Ok(node_launch(&node_abs, &entry))
}

fn node_launch(node_abs: &Path, entry: &Path) -> LocalLaunch {
    LocalLaunch {
        command: path_arg(node_abs),
        args: vec![path_arg(entry)],
        env: HashMap::new(),
    }
}

/// Ensure `base/node_modules` exists (npm install when missing). `base` must
/// carry a package.json — callers check before routing here.
///
/// A zero-dependency server (self-contained .js, nothing to import) needs no
/// install at all — and npm's handling of an empty deps map varies by version
/// (some never create `node_modules`), so demanding the directory would
/// false-fail an otherwise runnable repo.
async fn node_deps(base: &Path, node: Option<&Path>, sink: LogSink) -> Result<(), ResolveFail> {
    if base.join("node_modules").is_dir() {
        return Ok(());
    }
    let text = std::fs::read_to_string(base.join("package.json"))
        .map_err(|e| ResolveFail::Failed(format!("read package.json: {e}")))?;
    if !parse_package_json_full(&text).is_some_and(|p| p.has_deps) {
        return Ok(());
    }
    run_npm(base, &["install".to_string()], node, sink)
        .await
        .map_err(|e| ResolveFail::Failed(format!("npm install failed: {e}")))?;
    if !base.join("node_modules").is_dir() {
        return Err(ResolveFail::Failed(
            "npm install finished without creating node_modules".into(),
        ));
    }
    Ok(())
}

async fn run_npm(
    base: &Path,
    tail: &[String],
    node: Option<&Path>,
    sink: LogSink,
) -> Result<(), String> {
    let (program, mut prefix) = crate::mcp_prefetch::npm_program(node);
    prefix.extend(tail.iter().cloned());
    prefix.extend([
        "--no-audit".to_string(),
        "--no-fund".to_string(),
        "--loglevel=error".to_string(),
    ]);
    let envs: Vec<(String, String)> = vec![
        ("npm_config_registry".into(), npm_registry()),
        ("npm_config_update_notifier".into(), "false".into()),
    ];
    run_cmd(&program, &prefix, base, &envs, sink, BUILD_TIMEOUT).await
}

async fn build_python(base: &Path, sink: LogSink) -> Result<LocalLaunch, ResolveFail> {
    // A console-script entry (`[project.scripts]`) is the only deterministic
    // python launch — anything else (a bare script, a `-m` module) is ambiguous
    // and goes to the AI, which picks the exact file.
    let pyproject = std::fs::read_to_string(base.join("pyproject.toml"))
        .ok()
        .and_then(|t| parse_pyproject_full(&t));
    let script = pyproject
        .as_ref()
        .and_then(|p| p.scripts.first().cloned())
        .or_else(|| {
            std::fs::read_to_string(base.join("setup.py"))
                .ok()
                .and_then(|t| setup_name(&t))
        });
    let Some(script) = script else {
        let has_pyproject = base.join("pyproject.toml").is_file();
        let has_setup = base.join("setup.py").is_file();
        let reason = if has_pyproject {
            "pyproject declares no [project.scripts] console entry"
        } else if has_setup {
            "setup.py has no runnable entry point"
        } else {
            "requirements.txt only — no declared entry"
        };
        return Err(ResolveFail::NeedsAi(reason.into()));
    };
    let venv = base.join(".venv");
    let py = ensure_python_env(&venv, base, sink).await?;
    // Console script the editable install generated inside the venv.
    let console = venv_console(&venv, &script);
    if !console.is_file() {
        return Err(ResolveFail::Failed(format!(
            "uv install did not produce the '{script}' console entry at {} — the repo may need \
             a different entry",
            console.display()
        )));
    }
    // Keep the venv python as the interpreter, run the console script path.
    let _ = py;
    Ok(LocalLaunch {
        command: path_arg(&console),
        args: vec![],
        env: HashMap::new(),
    })
}

/// Create/ensure a venv at `venv` with the repo's python deps installed
/// (`-e .` for pyproject/setup.py projects, `-r requirements.txt` otherwise),
/// returning the venv's interpreter path. Shared by the deterministic console
/// path and the AI file-entry path.
async fn ensure_python_env(
    venv: &Path,
    base: &Path,
    sink: LogSink,
) -> Result<PathBuf, ResolveFail> {
    let uv = which::which("uv").map_err(|_| {
        ResolveFail::ToolMissing(
            "this is a Python MCP server — uv is not on PATH. Install uv \
             (https://docs.astral.sh/uv/) and retry"
                .into(),
        )
    })?;
    let py = venv_python(venv);
    if !py.is_file() {
        sink(LogLine {
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            line: format!("build: uv venv {}", venv.display()),
        });
        let args = vec!["venv".to_string(), path_arg(venv)];
        run_cmd(
            uv.to_str().unwrap_or("uv"),
            &args,
            base,
            &[],
            sink.clone(),
            BUILD_TIMEOUT,
        )
        .await
        .map_err(|e| ResolveFail::Failed(format!("uv venv failed: {e}")))?;
    }
    let py = venv_python(venv);
    if !py.is_file() {
        return Err(ResolveFail::Failed("uv venv left no interpreter".into()));
    }
    let py_arg = path_arg(&py);
    if base.join("pyproject.toml").is_file() || base.join("setup.py").is_file() {
        sink(LogLine {
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            line: "build: uv pip install -e . (project deps)".into(),
        });
        let args = vec![
            "pip".into(),
            "install".into(),
            "--python".into(),
            py_arg.clone(),
            "-e".into(),
            ".".into(),
        ];
        run_cmd(
            uv.to_str().unwrap_or("uv"),
            &args,
            base,
            &[],
            sink.clone(),
            BUILD_TIMEOUT,
        )
        .await
        .map_err(|e| ResolveFail::Failed(format!("uv pip install -e . failed: {e}")))?;
    } else if base.join("requirements.txt").is_file() {
        sink(LogLine {
            stream: LogStream::Stdout,
            level: LogLevel::Info,
            line: "build: uv pip install -r requirements.txt".into(),
        });
        let args = vec![
            "pip".into(),
            "install".into(),
            "--python".into(),
            py_arg.clone(),
            "-r".into(),
            path_arg(&base.join("requirements.txt")),
        ];
        run_cmd(
            uv.to_str().unwrap_or("uv"),
            &args,
            base,
            &[],
            sink,
            BUILD_TIMEOUT,
        )
        .await
        .map_err(|e| ResolveFail::Failed(format!("uv pip install -r failed: {e}")))?;
    }
    Ok(py)
}

fn venv_python(venv: &Path) -> PathBuf {
    let win = venv.join("Scripts").join("python.exe");
    if win.is_file() {
        win
    } else {
        venv.join("bin").join("python")
    }
}

fn venv_console(venv: &Path, name: &str) -> PathBuf {
    let win = venv.join("Scripts").join(exe_name(name));
    if win.is_file() {
        win
    } else {
        venv.join("bin").join(name)
    }
}

fn read_pkg(base: &Path, missing_msg: &str) -> Result<PkgInfo, ResolveFail> {
    let detail = ResolveFail::NeedsAi(missing_msg.to_string());
    let text = std::fs::read_to_string(base.join("package.json")).map_err(|_| detail)?;
    parse_package_json_full(&text).ok_or_else(|| ResolveFail::NeedsAi(missing_msg.to_string()))
}

fn node_or_resolve(node: Option<&Path>) -> Option<PathBuf> {
    node.map(Path::to_path_buf).or_else(resolve_node)
}

fn resolve_node() -> Option<PathBuf> {
    which::which("node").ok()
}

// --- streamed runner --------------------------------------------------------

/// Spawn `program + args` in `cwd`, stream stdout(stderr) to the sink, wait with
/// a hard timeout. Non-zero exit / timeout / spawn failure is an `Err(String)`.
/// A thin wrapper over [`crate::run_timed`], which on expiry kills the whole
/// process tree (not just the direct child — a bare `start_kill()` would orphan
/// `index-pack`/`fetch-pack` grandchildren and the hang would survive).
async fn run_cmd(
    program: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
    sink: LogSink,
    timeout: Duration,
) -> Result<(), String> {
    let code = crate::run_timed(program, args, cwd, envs, sink, timeout).await?;
    if code == 0 {
        Ok(())
    } else {
        Err(format!("{program} exited with code {code}"))
    }
}

// --- AI fallback ------------------------------------------------------------

/// A strict, structured AI suggestion. The model never hands us a shell line —
/// only a language + a repo-relative entry file, which we run through our own
/// validated interpreters/builds.
struct Suggestion {
    kind: String,
    entry: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    note: Option<String>,
}

impl Suggestion {
    fn parse(raw: &str) -> Result<Suggestion, String> {
        let s = raw.trim();
        let s = s
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let (a, b) = (s.find('{'), s.rfind('}'));
        let slice = match (a, b) {
            (Some(a), Some(b)) if b > a => &s[a..=b],
            _ => s,
        };
        let v: serde_json::Value = serde_json::from_str(slice)
            .map_err(|e| format!("suggestion is not JSON: {e}"))?;
        let kind = v
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if kind != "node" && kind != "python" {
            return Err(format!(
                "AI suggested an unsupported runtime '{kind}' — only 'node' or 'python' are allowed"
            ));
        }
        let entry = v
            .get("entry")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if entry.is_empty() {
            return Err("AI gave no entry file".into());
        }
        let args = v
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let env = v
            .get("env")
            .and_then(|e| e.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let note = v
            .get("note")
            .and_then(|n| n.as_str())
            .map(String::from);
        Ok(Suggestion {
            kind,
            entry,
            args,
            env,
            note,
        })
    }
}

/// Ask the configured provider's LLM to pick a runnable entry from the cloned
/// files, then validate + build it through our own toolchain.
async fn ai_resolve(
    base: &Path,
    provider: &ResolvedProvider,
    node: Option<&Path>,
    sink: LogSink,
) -> Result<LocalLaunch, String> {
    let context = repo_context(base);
    let system = "\
You analyze a cloned MCP (Model Context Protocol) server repository and decide how to run its \
stdio entrypoint on a developer machine. Rules:\n\
- Output ONLY a JSON object — no prose, no markdown fences.\n\
- It must run today with only a package manager install (no Docker, no system packages).\n\
- \"kind\": exactly \"node\" (needs npm install) or \"python\" (needs uv/venv).\n\
- \"entry\": a path RELATIVE to the listed repo dir of the file to execute. For node prefer the \
package.json \"bin\"/\"main\"/index entry; for python the main script (server.py / __main__.py / \
a named *.py). Never point outside the repo.\n\
- \"args\": extra literal argv (flags only, e.g. [\"--port\", \"3100\"]); omit for none.\n\
- \"env\": only values the server needs that the repo cannot self-supply.\n\
- \"note\": one short English sentence on your choice.\n\
Reply exactly: {\"kind\":\"node|python\",\"entry\":\"relative/path\",\"args\":[],\"env\":{},\"note\":\"...\"}";
    let user = format!(
        "Cloned repo dir: {}\n\n{}",
        path_arg(base),
        context
    );
    let raw = launcher_core::llm::chat(provider, system, &user)
        .await
        .map_err(|e| format!("AI call failed: {e}"))?;
    let sug = Suggestion::parse(&raw)?;
    let note = sug.note.clone().unwrap_or_default();
    sink(LogLine {
        stream: LogStream::Stdout,
        level: LogLevel::Info,
        line: format!(
            "ai-resolve: {} entry '{}'{}",
            sug.kind,
            sug.entry,
            if note.is_empty() { String::new() } else { format!(" — {note}") }
        ),
    });

    // Validate: entry must be a repo-relative path that stays inside the clone.
    let entry_path = resolve_inside(base, &sug.entry)?;

    match sug.kind.as_str() {
        "node" => {
            if !base.join("package.json").is_file() {
                return Err("AI chose node but the repo has no package.json".into());
            }
            let node_abs = node_or_resolve(node).ok_or_else(|| {
                "Node not found — cannot run a node-based MCP".to_string()
            })?;
            node_deps(base, node, sink.clone())
                .await
                .map_err(|e| format!("install node deps: {}", e.into_err()))?;
            let pkg = read_pkg(base, "package.json unreadable")
                .map_err(|e| e.into_err())?;
            if !entry_path.is_file() && pkg.has_build {
                run_npm(base, &["run".into(), "build".into()], node, sink)
                    .await
                    .map_err(|e| format!("npm run build failed: {e}"))?;
            }
            if !entry_path.is_file() {
                return Err(format!(
                    "AI entry '{}' does not exist after install/build — suggestion rejected",
                    sug.entry
                ));
            }
            let mut launch = node_launch(&node_abs, &entry_path);
            launch.args.extend(sug.args);
            launch.env.extend(sug.env);
            Ok(launch)
        }
        "python" => {
            let venv = base.join(".venv");
            let py = ensure_python_env(&venv, base, sink)
                .await
                .map_err(|e| e.into_err())?;
            if !entry_path.is_file() {
                return Err(format!(
                    "AI entry '{}' does not exist — suggestion rejected",
                    sug.entry
                ));
            }
            let mut launch = LocalLaunch {
                command: path_arg(&py),
                args: vec![path_arg(&entry_path)],
                env: HashMap::new(),
            };
            launch.args.extend(sug.args);
            launch.env.extend(sug.env);
            Ok(launch)
        }
        _ => Err("unsupported AI runtime".into()),
    }
}

/// Resolve a repo-relative path to an absolute candidate inside `base`,
/// rejecting absolute paths and any `..` traversal.
fn resolve_inside(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() {
        return Err("empty entry path".into());
    }
    let p = PathBuf::from(rel);
    // Reject absolute, root-relative and drive-qualified paths explicitly — on
    // Windows `is_absolute()` treats a bare `\foo` as drive-relative, so check
    // the components directly rather than the platform flag.
    for comp in p.components() {
        match comp {
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(format!("AI entry '{rel}' escapes the repo"));
            }
            _ => {}
        }
    }
    let cand = base.join(&p);
    if let (Ok(b), Ok(c)) = (std::fs::canonicalize(base), std::fs::canonicalize(&cand)) {
        if !c.starts_with(&b) {
            return Err(format!("AI entry '{rel}' escapes the repo"));
        }
    }
    Ok(cand)
}

/// A compact, token-bounded picture of the clone: the file tree plus the heads
/// of the decisive manifests — enough for the model to name a real entry.
fn repo_context(base: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut count = 0usize;
    collect_tree(base, &mut lines, 0, &mut count, 300);
    lines.push(String::new());
    lines.push("---- key file contents (head) ----".into());

    let mut heads = Vec::new();
    for name in [
        "package.json",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        "Cargo.toml",
        "go.mod",
        "Dockerfile",
    ] {
        if base.join(name).is_file() {
            heads.push(PathBuf::from(name));
        }
    }
    // A few likely script entrypoints at shallow depth.
    if let Ok(entries) = std::fs::read_dir(base) {
        let mut py: Vec<PathBuf> = Vec::new();
        let mut js: Vec<PathBuf> = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|x| x.to_str()) {
                    let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let likely = matches!(
                        fname,
                        "main.py" | "app.py" | "server.py" | "cli.py" | "__main__.py"
                            | "main.js" | "index.js" | "server.js" | "cli.js"
                    );
                    match ext {
                        "py" if likely => py.push(p),
                        "js" if likely => js.push(p),
                        _ => {}
                    }
                }
            }
        }
        py.sort();
        js.sort();
        heads.extend(py.into_iter().take(3));
        heads.extend(js.into_iter().take(3));
    }
    // src/ subdir scripts (workspace-ish layouts).
    for sub in ["src", "lib"] {
        let dir = base.join(sub);
        if dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut scripts = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && matches!(
                                p.extension().and_then(|x| x.to_str()),
                                Some("py") | Some("js") | Some("ts")
                            )
                    })
                    .collect::<Vec<_>>();
                scripts.sort();
                heads.extend(scripts.into_iter().take(2));
            }
        }
    }

    let mut total = 0usize;
    for rel in heads {
        let text = std::fs::read_to_string(base.join(&rel)).unwrap_or_default();
        let head: String = text.lines().take(200).collect::<Vec<_>>().join("\n");
        let shown: String = head.chars().take(6000).collect();
        if shown.trim().is_empty() {
            continue;
        }
        total += shown.chars().count();
        lines.push(format!("==== {} ====", rel.display()));
        lines.push(shown);
        if total > 20_000 {
            break;
        }
    }
    lines.join("\n")
}

fn collect_tree(dir: &Path, out: &mut Vec<String>, depth: usize, count: &mut usize, cap: usize) {
    if depth > 5 || *count >= cap {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    for p in names {
        if *count >= cap {
            return;
        }
        let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if SKIP_DIRS.contains(&fname) {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(dir) {
                out.push(format!("{}/", rel.display()));
            }
            *count += 1;
            collect_tree(&p, out, depth + 1, count, cap);
        } else {
            if let Ok(rel) = p.strip_prefix(dir) {
                out.push(rel.display().to_string());
            }
            *count += 1;
        }
    }
}

// --- tiny helpers -----------------------------------------------------------

fn leaf_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "server".into())
}

/// Filesystem/executable-safe short name (go/cargo binaries, exe extensions).
fn bin_slug(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let s = s.trim_matches(['-', '_']);
    if s.is_empty() {
        "server".into()
    } else {
        s.to_string()
    }
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Path as a child-process argument: strip a Windows `\\?\` verbatim prefix
/// (Node's CJS resolver mangles it), pass everything else through unchanged.
fn path_arg(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s.to_string(),
    }
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_source_parses_plain_and_subdirs() {
        assert_eq!(
            github_source("https://github.com/acme/server").unwrap(),
            ("https://github.com/acme/server".to_string(), "server".into(), None)
        );
        let (url, repo, sub) =
            github_source("https://github.com/acme/server.git").unwrap();
        assert_eq!(url, "https://github.com/acme/server");
        assert_eq!(repo, "server");
        assert_eq!(sub, None);
        // blob subpath → subdir after skipping tree/blob + ref.
        let (_, _, sub) = github_source("https://github.com/acme/mono/blob/main/packages/mcp-srv")
            .unwrap();
        assert_eq!(sub.as_deref(), Some("packages/mcp-srv"));
        let (_, _, sub) =
            github_source("https://github.com/acme/mono/tree/dev/server").unwrap();
        assert_eq!(sub.as_deref(), Some("server"));
        // Non-github / non-parseable.
        assert!(github_source("https://example.com/x").is_none());
        assert!(github_source("git@github.com:acme/server.git").is_none());
    }

    #[test]
    fn parse_package_json_entry_resolution() {
        let string_bin = r#"{ "name": "@acme/srv", "version": "1", "bin": "./dist/cli.js" }"#;
        let p = parse_package_json_full(string_bin).unwrap();
        assert_eq!(p.name, "@acme/srv");
        assert_eq!(p.entry.as_deref(), Some("dist/cli.js"));
        assert!(!p.has_build);

        // Zero-dependency self-contained server — `has_deps` must be false so the
        // local build skips npm install (npm may never create node_modules for an
        // empty deps map → regression: false-failed runnable repos).
        let zero_dep = r#"{ "name": "s", "main": "server.js" }"#;
        assert!(!parse_package_json_full(zero_dep).unwrap().has_deps);
        let with_deps = r#"{ "name": "s", "dependencies": { "fastmcp": "^2" }, "devDependencies": {} }"#;
        assert!(parse_package_json_full(with_deps).unwrap().has_deps);

        let obj_bin = r#"{ "name": "x", "bin": { "other": "a.js", "x": "b.js" } }"#;
        let p = parse_package_json_full(obj_bin).unwrap();
        assert_eq!(p.entry.as_deref(), Some("b.js"), "name-key wins");

        let main_only = r#"{ "name": "y", "main": "lib/index.js" }"#;
        let p = parse_package_json_full(main_only).unwrap();
        assert_eq!(p.entry.as_deref(), Some("lib/index.js"));

        let ws = r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#;
        let p = parse_package_json_full(ws).unwrap();
        assert!(p.workspaces);
        assert!(p.private);

        assert!(parse_package_json_full("not json").is_none());
        // A version-only object is not a usable server manifest (no bin/main).
        let bare = parse_package_json_full("{\"version\":\"1\"}").unwrap();
        assert!(bare.entry.is_none() && bare.name.is_empty() && !bare.has_build);
    }

    #[test]
    fn parse_pyproject_scripts_and_name() {
        let text = r#"[project]
name = "mcp-local"
[project.scripts]
mcp-local = "pkg.main:main"
cli = "pkg.cli:run"
"#;
        let p = parse_pyproject_full(text).unwrap();
        assert_eq!(p.name.as_deref(), Some("mcp-local"));
        assert_eq!(p.scripts, vec!["mcp-local", "cli"]);
        assert!(parse_pyproject_full("[build-system]\nrequires=[]\n").is_none());
    }

    #[test]
    fn setup_and_cargo_name_parse() {
        assert_eq!(setup_name("setup(name=\"mcp-git\", version=\"0.1\")").unwrap(), "mcp-git");
        let cargo = "[package]\nname = \"mcp-rs\"\nversion = \"0.1.0\"\n\n[workspace]\nmembers=[]";
        assert_eq!(cargo_package_name(cargo).unwrap(), "mcp-rs");
        // Workspace root (no [package]) → None.
        assert_eq!(cargo_package_name("[workspace]\nmembers=[\"a\"]"), None);
    }

    #[test]
    fn resolve_inside_rejects_escapes_and_absolutes() {
        let base = std::path::Path::new("/tmp/repo");
        assert_eq!(resolve_inside(base, "src/main.py").unwrap(), base.join("src/main.py"));
        assert!(resolve_inside(base, "../etc/passwd").is_err());
        assert!(resolve_inside(base, "/etc/passwd").is_err());
        assert!(resolve_inside(base, "C:\\Windows\\x").is_err());
        assert!(resolve_inside(base, "a/../../b").is_err());
        assert!(resolve_inside(base, "").is_err());
    }

    #[test]
    fn bin_slug_and_exe_slug_are_safe() {
        assert_eq!(bin_slug("my-server"), "my-server");
        assert_eq!(bin_slug("a b/c"), "abc");
        assert_eq!(bin_slug("///"), "server");
        if cfg!(windows) {
            assert_eq!(exe_name("srv"), "srv.exe");
        } else {
            assert_eq!(exe_name("srv"), "srv");
        }
    }

    #[test]
    fn venv_paths_follow_platform_layout() {
        let dir = std::env::temp_dir().join(format!("ahl-venv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scripts = dir.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("python.exe"), "").unwrap();
        std::fs::write(scripts.join("srv.exe"), "").unwrap();
        let venv = std::path::PathBuf::from(&dir);
        assert_eq!(venv_python(&venv), scripts.join("python.exe"));
        assert_eq!(venv_console(&venv, "srv"), scripts.join(exe_name("srv")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggestion_parse_validates_runtime() {
        let good = r#"{"kind":"python","entry":"server.py","args":[],"env":{},"note":"ok"}"#;
        let s = Suggestion::parse(good).unwrap();
        assert_eq!(s.kind, "python");
        assert_eq!(s.entry, "server.py");

        assert!(Suggestion::parse(r#"{"kind":"go","entry":"main.go"}"#).is_err());
        assert!(Suggestion::parse(r#"{"kind":"node"}"#).is_err(), "entry required");
        assert!(Suggestion::parse("not json").is_err());

        // Fenced replies are tolerated.
        let fenced = format!("```json\n{good}\n```");
        assert!(Suggestion::parse(&fenced).is_ok());
    }
}
