//! DSH adapter — the harness-specific knowledge behind the launcher.
//!
//! Everything DSH-specific lives here: how to find it, what environment it
//! needs, and how to spawn it. The launcher core stays harness-agnostic.
//!
//! Facts this adapter encodes (verified against `deepseek-harness-master`):
//! - The CLI entry is a Node script, `apps/cli/lib/bin.js`, package
//!   `@deepseek-ai/dsh`.
//! - `dsh web` (alias of `--profile web`) serves the UI; the launcher runs it
//!   with `--port 0` so a free port is picked and the URL line
//!   (`dsh web: http://127.0.0.1:<port>…`) is printed for the launcher to open
//!   DSH in its own window instead of the browser. The source checkout's web
//!   app never opens a browser itself (no `--no-open` flag — that's a newer
//!   vendored `@deepseek-ai/dsh` feature).
//! - `DEEPSEEK_API_KEY` / `DEEPSEEK_BASE_URL` are read from the *inherited*
//!   process environment (base URL can never come from a `.env` file).
//! - `$DSH_HOME` isolates profiles/config per instance; a fresh empty
//!   `$DSH_HOME` is materialized by the `web` profile template on first boot.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use launcher_core::process::{spawn_child, ChildHandle, LogSink};
use launcher_core::runtime::RuntimeInfo;
use launcher_core::{
    AppSettings, InstanceManifest, LogLine, LogStream, ResolvedProvider, RuntimeAdapter,
};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

pub mod content;
pub mod diagnostics;
pub mod llm;
pub mod runtimes;
pub mod theme;

pub use diagnostics::{BundleInfo, DiagnosticsReport, OrderViolation};
use runtimes::Runtimes;

/// The web profile's default port.
pub const DEFAULT_WEB_PORT: u16 = 3080;

/// One plugin as DSH sees it in a profile: a `dependencies` entry with its
/// enable state (`dsh.profile.bundles` membership).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub name: String,
    pub enabled: bool,
}

/// One installed plugin's update status (npm `latest` vs installed version).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub name: String,
    pub installed: String,
    pub latest: String,
    pub updatable: bool,
}

pub struct DshAdapter {
    /// Managed runtimes (`<root>/runtimes`) — set by the app shell from `AppPaths`.
    runtimes: Option<Runtimes>,
    /// The app's resource dir, where a bundled `node/` and `dsh/` live in the
    /// packaged install. Set by the Tauri shell once the app handle exists.
    resource_dir: Option<PathBuf>,
}

impl DshAdapter {
    pub fn new() -> Self {
        Self {
            runtimes: None,
            resource_dir: None,
        }
    }

    /// Build with the managed-runtimes dir plus the app's resource dir (where a
    /// bundled `node/` and `dsh/` live in the packaged install).
    pub fn configured(runtimes_dir: PathBuf, resource_dir: Option<PathBuf>) -> Self {
        Self {
            runtimes: Some(Runtimes::new(runtimes_dir)),
            resource_dir,
        }
    }

    /// The managed-runtimes dir, if the shell configured one.
    pub fn runtimes(&self) -> Option<&Runtimes> {
        self.runtimes.as_ref()
    }

    /// Resolve the DSH CLI entry, in priority order:
    /// settings override → `DSH_CLI_BIN` env → bundled `dsh/` under resources →
    /// managed `runtimes/dsh-<ver>/` → (debug builds only) the sibling source
    /// tree → `dsh` on PATH. Returns the bin plus a label the UI can show.
    fn resolve_bin(&self, settings: &AppSettings) -> Option<(PathBuf, &'static str)> {
        if let Some(p) = settings.dsh_path.as_deref() {
            let b = PathBuf::from(p);
            if b.is_file() {
                return Some((b, "override"));
            }
        }
        if let Ok(p) = std::env::var("DSH_CLI_BIN") {
            let b = PathBuf::from(p);
            if b.is_file() {
                return Some((b, "override"));
            }
        }
        if let Some(dir) = &self.resource_dir {
            let b = dir.join("dsh").join("apps/cli/lib/bin.js");
            if b.is_file() {
                return Some((b, "bundled"));
            }
        }
        if let Some(rt) = &self.runtimes {
            if let Some(ver) = rt.resolve_version() {
                let b = rt.bin_path(&ver);
                if b.is_file() {
                    return Some((b, "managed"));
                }
            }
        }
        if cfg!(debug_assertions) {
            for cand in self.dev_candidates() {
                if cand.is_file() {
                    return Some((cand, "dev"));
                }
            }
        }
        if let Ok(b) = which::which("dsh") {
            return Some((b, "path"));
        }
        None
    }

    /// Node executable candidates, in priority order: `settings.node_path` →
    /// bundled `node/` under resources → dev `vendor/node` → managed
    /// `runtimes/node/` → `node` on PATH.
    fn node_candidates(&self, settings: &AppSettings) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(p) = settings.node_path.as_deref() {
            out.push(PathBuf::from(p));
        }
        if let Some(dir) = &self.resource_dir {
            out.push(dir.join("node").join(crate::runtimes::node_exe_name()));
        }
        // Dev: the vendored copy beside the tauri source, before any managed
        // install. Two `..` up (crates/dsh-adapter → launcher root), then apps/…
        out.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/desktop/src-tauri/vendor/node")
                .join(crate::runtimes::node_exe_name()),
        );
        if let Some(rt) = &self.runtimes {
            out.push(rt.managed_node_exe());
        }
        out
    }

    /// The first Node candidate that exists on disk, else `node` on PATH.
    pub fn resolve_node(&self, settings: &AppSettings) -> Option<PathBuf> {
        for cand in self.node_candidates(settings) {
            if cand.is_file() {
                return Some(cand);
            }
        }
        which::which("node").ok()
    }

    /// Locate Node and read its `--version`. Returns `(version, exe path)`.
    pub fn node_info(&self, settings: &AppSettings) -> Result<(String, PathBuf)> {
        let node = self.resolve_node(settings).ok_or_else(|| {
            anyhow!("Node not found. Install Node, or add a managed runtime in Settings → Runtime.")
        })?;
        let version = runtimes::node_version(&node)?;
        Ok((version, node))
    }

    /// Repo-relative fallbacks so `tauri dev` works without configuration.
    fn dev_candidates(&self) -> Vec<PathBuf> {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![manifest.join("../../../deepseek-harness-master/apps/cli/lib/bin.js")];
        if let Ok(cwd) = std::env::current_dir() {
            out.push(cwd.join("deepseek-harness-master/apps/cli/lib/bin.js"));
            out.push(cwd.join("../deepseek-harness-master/apps/cli/lib/bin.js"));
            out.push(cwd.join("../../deepseek-harness-master/apps/cli/lib/bin.js"));
        }
        out
    }

    /// Read the version from the CLI package.json next to the bin, offline.
    fn read_version(bin: &Path) -> Option<String> {
        // bin = <cli>/lib/bin.js → package.json at <cli>/package.json
        let pkg = bin.parent()?.parent()?.join("package.json");
        let text = std::fs::read_to_string(pkg).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value.get("version")?.as_str().map(String::from)
    }

    /// `std::fs::canonicalize` on Windows returns `\\?\`-prefixed extended
    /// paths, which Node's CJS resolver mangles (`\\?\D:\…` → `D:\?\D:\…` →
    /// EISDIR on `D:`). Strip the prefix so the path Node receives is a normal
    /// absolute path. Any other platform passes through unchanged.
    fn canonicalize_for_node(bin: &Path) -> PathBuf {
        let canon = std::fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
        let s = canon.to_string_lossy();
        match s.strip_prefix(r"\\?\") {
            Some(stripped) => PathBuf::from(stripped),
            None => canon,
        }
    }
}

impl RuntimeAdapter for DshAdapter {
    fn id(&self) -> &'static str {
        "dsh"
    }

    fn detect(&self, settings: &AppSettings) -> Result<RuntimeInfo> {
        let (node_version, node_path) = self.node_info(settings)?;
        let (bin, source) = self.resolve_bin(settings).ok_or_else(|| {
            anyhow!(
                "DSH not found. Install a runtime in Settings → Runtime, or set its \
                 CLI path in Settings (e.g. …/deepseek-harness-master/apps/cli/lib/bin.js)"
            )
        })?;
        let bin = Self::canonicalize_for_node(&bin);
        let version = Self::read_version(&bin).unwrap_or_else(|| "unknown".into());
        Ok(RuntimeInfo {
            id: self.id().into(),
            version,
            bin_path: bin.display().to_string(),
            node_version,
            node_path: Some(node_path.display().to_string()),
            source: source.into(),
        })
    }

    fn build_env(
        &self,
        provider: &ResolvedProvider,
        instance: &InstanceManifest,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();
        env.insert("DEEPSEEK_API_KEY".into(), provider.api_key.clone());
        if let Some(base) = provider.profile.base_url.as_deref() {
            if !base.trim().is_empty() {
                env.insert("DEEPSEEK_BASE_URL".into(), base.to_string());
            }
        }
        env.insert("DSH_HOME".into(), instance.workspace.clone());
        env.insert("DSH_TELEMETRY_DISABLED".into(), "1".into());
        Ok(env)
    }

    async fn launch(
        &self,
        settings: &AppSettings,
        instance: &InstanceManifest,
        env: &HashMap<String, String>,
        on_log: LogSink,
    ) -> Result<ChildHandle> {
        let info = self.detect(settings)?;
        // Spawn through the resolved Node executable (bundled / managed / PATH),
        // never a bare `node` — PATH noise or a wedged env must not matter.
        let node = self
            .resolve_node(settings)
            .ok_or_else(|| anyhow!("Node not found — can't run DSH"))?;
        let mut cmd = tokio::process::Command::new(&node);
        cmd.arg(&info.bin_path);
        // The launcher renders DSH in its own window. `--port 0` picks a free
        // port (the CLI prints `dsh web: http://127.0.0.1:<port>…` on stdout,
        // which the launcher parses) instead of the fixed 3080, avoiding
        // collisions between instances. The CLI's web app never opens a
        // browser itself, so no `--no-open` is needed (that flag is specific
        // to the newer vendored `@deepseek-ai/dsh`, not the source checkout).
        cmd.arg("web");
        cmd.arg("--host");
        cmd.arg("127.0.0.1");
        cmd.arg("--port");
        cmd.arg("0");
        cmd.kill_on_drop(true);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.current_dir(&instance.workspace);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let handle = spawn_child(cmd, on_log).await?;
        tracing::info!(pid = handle.pid, version = %info.version, "dsh web launched");
        Ok(handle)
    }
}

impl DshAdapter {
    /// The profile directory for an instance (`$DSH_HOME/profiles/<profile>`).
    pub fn profile_dir(instance: &InstanceManifest) -> PathBuf {
        PathBuf::from(&instance.workspace)
            .join("profiles")
            .join(&instance.profile)
    }

    fn read_profile_manifest(instance: &InstanceManifest) -> Option<serde_json::Value> {
        let path = Self::profile_dir(instance).join("package.json");
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Installed plugins: `enabled` = in `dsh.profile.bundles` AND not disabled
    /// by the user patch layer. In-box template bundles live in `bundles` but
    /// never `dependencies`, so they are naturally excluded.
    pub fn installed_plugins(instance: &InstanceManifest) -> Vec<InstalledPlugin> {
        let Some(value) = Self::read_profile_manifest(instance) else {
            return Vec::new();
        };
        let deps: Vec<String> = value
            .get("dependencies")
            .and_then(|d| d.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let bundles: Vec<String> = value
            .pointer("/dsh/profile/bundles")
            .and_then(|b| b.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let profile_dir = Self::profile_dir(instance);
        let disabled_ids = read_patch_disabled(&profile_dir.join("cordis.patch.yml"));
        deps.into_iter()
            .map(|name| {
                let in_bundles = bundles.iter().any(|b| b == &name);
                let disabled = inserted_row_ids(&profile_dir, &name)
                    .iter()
                    .any(|id| disabled_ids.contains(id));
                InstalledPlugin {
                    name,
                    enabled: in_bundles && !disabled,
                }
            })
            .collect()
    }

    /// The installed version of a plugin (from its `node_modules` package.json).
    pub fn installed_version(instance: &InstanceManifest, name: &str) -> Option<String> {
        let pkg = Self::profile_dir(instance)
            .join("node_modules")
            .join(name)
            .join("package.json");
        let text = std::fs::read_to_string(pkg).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.get("version")?.as_str().map(String::from)
    }

    /// Enable/disable a plugin through the profile's user patch layer
    /// (`cordis.patch.yml`) — the mechanism dsh-market uses. Disable appends a
    /// `- id: <row>` + `disabled: true` entry; enable *removes* that entry
    /// (force-enabling with `disabled: false` only when a lower layer holds the
    /// row down). This is what makes disable stick: `dsh plugin`'s
    /// `reconcilePlugins` rewrites `dsh.profile.bundles` on every plugin op, so
    /// a bundles edit is undone next run — the patch layer is not. The
    /// dependency itself stays put; the change survives restarts.
    pub fn set_plugin_enabled(
        instance: &InstanceManifest,
        name: &str,
        enabled: bool,
    ) -> Result<()> {
        let profile_dir = Self::profile_dir(instance);
        let ids = inserted_row_ids(&profile_dir, name);
        if ids.is_empty() {
            return Err(anyhow!(
                "plugin '{name}' has no toggleable bundle rows (it may not declare a dsh.bundle)"
            ));
        }
        let patch_path = profile_dir.join("cordis.patch.yml");
        for id in ids {
            if !is_valid_row_id(&id) {
                return Err(anyhow!(
                    "row id '{id}' has characters the patch layer cannot write"
                ));
            }
            if enabled {
                enable_row(&patch_path, &id)?;
            } else {
                disable_row(&patch_path, &id)?;
            }
        }
        Ok(())
    }

    /// The row ids a plugin's bundle patch inserts — what the patch layer can
    /// toggle. Read *before* uninstalling, when `node_modules` still holds the
    /// package's patch files.
    pub fn plugin_row_ids(instance: &InstanceManifest, name: &str) -> Vec<String> {
        inserted_row_ids(&Self::profile_dir(instance), name)
    }

    /// Remove a plugin's patch-layer toggle rows (uninstall cleanup), restoring
    /// the empty-list placeholder if nothing else remains. A removed plugin
    /// must not leave orphan `disabled:` rows the next boot trips over.
    pub fn remove_patch_rows(instance: &InstanceManifest, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        remove_row_blocks(&Self::profile_dir(instance).join("cordis.patch.yml"), ids)?;
        Ok(())
    }

    /// Run `dsh plugin --profile <profile> <args>` against an instance's
    /// `$DSH_HOME`, streaming stdout/stderr to `on_log` and returning the exit
    /// code once the child finishes (installs are long-running pnpm jobs).
    pub async fn run_plugin_command(
        &self,
        settings: &AppSettings,
        instance: &InstanceManifest,
        args: &[String],
        on_log: LogSink,
    ) -> Result<i32> {
        let info = self.detect(settings)?;
        // Spawn through the resolved Node executable (bundled / managed / PATH),
        // never a bare `node` — PATH noise or a wedged env must not matter.
        let node = self
            .resolve_node(settings)
            .ok_or_else(|| anyhow!("Node not found — can't run DSH"))?;
        let mut cmd = tokio::process::Command::new(&node);
        cmd.arg(&info.bin_path);
        cmd.arg("plugin");
        cmd.arg("--profile");
        cmd.arg(&instance.profile);
        for a in args {
            cmd.arg(a);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.current_dir(&instance.workspace);
        cmd.env("DSH_HOME", &instance.workspace);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to spawn dsh plugin: {e}"))?;
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            let sink = on_log.clone();
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
                                    line,
                                });
                            }
                        }
                    }
                }
            }));
        }
        if let Some(err) = child.stderr.take() {
            let sink = on_log.clone();
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
                                    line,
                                });
                            }
                        }
                    }
                }
            }));
        }
        let status = child
            .wait()
            .await
            .map_err(|e| anyhow!("wait dsh plugin: {e}"))?;
        for r in readers {
            let _ = r.await;
        }
        Ok(status.code().unwrap_or(1))
    }
}

/// The row ids a package's bundle patch inserts (the ids nested under an
/// `insert:` block), read from its declared `dsh.bundle.patch` and its
/// conventional root `cordis.patch.yml`. These are the ids the user patch
/// layer targets with `disabled: true`.
fn inserted_row_ids(profile_dir: &Path, name: &str) -> Vec<String> {
    let pkg_dir = profile_dir.join("node_modules").join(name);
    let mut ids = Vec::new();
    let declared = std::fs::read_to_string(pkg_dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/dsh/bundle/patch")
                .and_then(|p| p.as_str())
                .map(String::from)
        });
    if let Some(rel) = declared {
        if let Ok(text) = std::fs::read_to_string(pkg_dir.join(&rel)) {
            ids.extend(parse_inserted_ids(&text));
        }
    }
    if let Ok(text) = std::fs::read_to_string(pkg_dir.join("cordis.patch.yml")) {
        ids.extend(parse_inserted_ids(&text));
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Line-wise extraction of the `id:` values nested under an `insert:` block —
/// a faithful port of dsh-market's `parsePatchRows` (src/profile.ts), which
/// matters because a bundle patch also carries rows that merely reconfigure
/// *other* plugins, and those must never be disabled.
fn parse_inserted_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut insert_indent: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if let Some(ins) = insert_indent {
            if indent <= ins && !is_row_line(trimmed) {
                insert_indent = None;
            }
        }
        if is_insert_line(trimmed) {
            insert_indent = Some(indent);
            continue;
        }
        if let Some(id) = parse_id(trimmed) {
            if let Some(ins) = insert_indent {
                if indent > ins && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

fn is_insert_line(trimmed: &str) -> bool {
    let t = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
    matches!(t.strip_prefix("insert:"), Some(rest) if rest.trim().is_empty())
}

fn is_row_line(trimmed: &str) -> bool {
    let t = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
    ["id:", "name:", "config:"].iter().any(|k| t.starts_with(k))
}

fn parse_id(trimmed: &str) -> Option<String> {
    let t = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
    let rest = t.strip_prefix("id:")?.trim();
    let rest = rest.trim_start_matches(['"', '\'']);
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .unwrap_or(rest.len());
    let val = &rest[..end];
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

/// What the user patch layer currently says about each row: ids it disables
/// (`disabled: true`) and ids it force-enables (`disabled: false`). Line-wise
/// on purpose, matching dsh-market's `readUserPatchState` — the file may hold
/// shapes a strict YAML parse rejects, but a plain `- id: X` + `disabled:`
/// pair is enough. Only top-level rows count (insert-block rows are indented).
#[derive(Default)]
struct PatchState {
    disables: HashSet<String>,
    forced: HashSet<String>,
}

fn read_patch_state(patch_path: &Path) -> PatchState {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    let mut state = PatchState::default();
    let lines: Vec<&str> = text.split('\n').collect();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim_end_matches('\r');
        // Top-level rows only: a `- id:` at column 0 (rows nested under
        // `- insert:` are indented and must never be read as disable rows).
        if !line.starts_with("- id:") {
            continue;
        }
        let Some(id) = parse_id(line) else { continue };
        let next = lines
            .get(i + 1)
            .copied()
            .unwrap_or("")
            .split('#')
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .trim();
        match next.strip_prefix("disabled:") {
            Some(v) if v.trim() == "true" => {
                state.disables.insert(id);
            }
            Some(v) if v.trim() == "false" => {
                state.forced.insert(id);
            }
            _ => {}
        }
    }
    state
}

/// The ids the user patch layer disables (top-level `- id: X` + `disabled:
/// true` entries).
fn read_patch_disabled(patch_path: &Path) -> HashSet<String> {
    read_patch_state(patch_path).disables
}

/// Row ids the patch layer can write: plain unquoted YAML scalars — the same
/// `ROW_ID_RE` dsh-market enforces before touching the file.
fn is_valid_row_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn row_block(id: &str, disabled: bool) -> String {
    format!(
        "- id: {id}\n  disabled: {}\n",
        if disabled { "true" } else { "false" }
    )
}

/// Strip full-line comments (lines whose first non-whitespace char is `#`),
/// keeping every line that carries content.
fn without_comment_lines(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with('#') {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Append one top-level patch entry, handling the empty-list `[]` placeholder
/// the profile template ships (appending after it would produce two top-level
/// YAML documents — the loader refuses that). Port of dsh-market's
/// `appendPatchEntry`.
pub(crate) fn append_patch_entry(patch_path: &Path, block: &str) -> Result<()> {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    let core = text.trim();
    if core.is_empty() {
        return std::fs::write(patch_path, block)
            .with_context(|| format!("write {}", patch_path.display()));
    }
    let stripped = without_comment_lines(&text).trim().to_string();
    let mut next = if stripped.is_empty() {
        // comments only — append after them
        text
    } else if stripped == "[]" || stripped == "[ ]" {
        // comment out the empty-list placeholder and append
        comment_out_placeholder(&text)
    } else {
        text
    };
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(block);
    std::fs::write(patch_path, next).with_context(|| format!("write {}", patch_path.display()))
}

/// Replace the template's top-level `[]` placeholder with a `# []` comment so a
/// block item can be appended after it.
fn comment_out_placeholder(text: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut done = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if !done && (trimmed == "[]" || trimmed == "[ ]") {
            result.push("# []".to_string());
            done = true;
        } else {
            result.push(line.to_string());
        }
    }
    result.join("\n")
}

/// Remove every top-level `- id: <id>` block whose following line is
/// `disabled: <value>` for one of `values`. Returns the new text and whether
/// anything was removed. Line endings (`\r\n`) survive the split/join.
fn remove_blocks(text: &str, id: &str, values: &[&str]) -> (String, bool) {
    let mut removed = false;
    let mut out: Vec<String> = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        let is_target = line.starts_with("- id:") && parse_id(line).as_deref() == Some(id);
        let mut dropped = false;
        if is_target {
            if let Some(next_raw) = lines.get(i + 1) {
                let next = next_raw.trim_end_matches('\r').trim();
                if let Some(v) = next.strip_prefix("disabled:") {
                    if values.contains(&v.trim()) {
                        removed = true;
                        dropped = true;
                    }
                }
            }
        }
        if dropped {
            i += 2; // skip the id line and its disabled line
        } else {
            out.push(lines[i].to_string());
            i += 1;
        }
    }
    (out.join("\n"), removed)
}

/// Put the empty-list `[]` placeholder back when nothing else is left. After
/// the template's placeholder is commented out and the last block is removed,
/// the file is pure comments — not a top-level array, which DSH refuses to
/// boot ("must be a top-level YAML array"). Port of dsh-market's
/// `withPlaceholderRestored`.
fn restore_placeholder(text: &str) -> String {
    if without_comment_lines(text).trim() != "" {
        return text.to_string();
    }
    let mut result: Vec<String> = Vec::new();
    let mut revivified = false;
    for line in text.split('\n') {
        let after_hash = line.trim().trim_start_matches('#').trim();
        if !revivified && (after_hash == "[]" || after_hash == "[ ]") {
            result.push("[]".to_string());
            revivified = true;
        } else {
            result.push(line.to_string());
        }
    }
    if revivified {
        return result.join("\n");
    }
    if text.is_empty() || text.ends_with('\n') {
        format!("{text}[]\n")
    } else {
        format!("{text}\n[]\n")
    }
}

/// Disable one row: append `- id: X` + `disabled: true` (idempotent — a row
/// already disabled is left alone).
fn disable_row(patch_path: &Path, id: &str) -> Result<()> {
    if read_patch_state(patch_path).disables.contains(id) {
        return Ok(());
    }
    append_patch_entry(patch_path, &row_block(id, true))
}

/// Enable one row: remove its `disabled: true` block (restoring the `[]`
/// placeholder if that empties the file); otherwise force-enable with
/// `disabled: false` when a lower layer holds it down. Port of dsh-market's
/// `enableRow`.
fn enable_row(patch_path: &Path, id: &str) -> Result<()> {
    let state = read_patch_state(patch_path);
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    let (after_remove, removed) = remove_blocks(&text, id, &["true"]);
    if removed {
        return std::fs::write(patch_path, restore_placeholder(&after_remove))
            .with_context(|| format!("write {}", patch_path.display()));
    }
    if state.forced.contains(id) {
        return Ok(());
    }
    append_patch_entry(patch_path, &row_block(id, false))
}

/// Remove every disable/force block for `ids` — the uninstall cleanup, so a
/// removed plugin leaves no orphan rows. Port of dsh-market's `removeRowBlocks`.
fn remove_row_blocks(patch_path: &Path, ids: &[String]) -> Result<()> {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    let mut next = text;
    let mut changed = false;
    for id in ids {
        let (after, removed) = remove_blocks(&next, id, &["true", "false"]);
        if removed {
            next = after;
            changed = true;
        }
    }
    if changed {
        std::fs::write(patch_path, restore_placeholder(&next))
            .with_context(|| format!("write {}", patch_path.display()))?;
    }
    Ok(())
}

/// Remove a whole top-level `- insert:` block that contains a row with id
/// `row_id` — the MCP-uninstall cleanup (each MCP install appends its own
/// insert block). The block spans from its `- insert:` line to the next
/// column-0 entry (or EOF). Line-based, like the other patch writers; restores
/// the `[]` placeholder if that empties the file.
pub(crate) fn remove_insert_block(patch_path: &Path, row_id: &str) -> Result<()> {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(());
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut removed = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if line.starts_with("- insert:") {
            // Block end (exclusive): the next column-0 line after this one.
            let mut end = i + 1;
            while end < lines.len() {
                let inner = lines[end].trim_end_matches('\r');
                if !inner.is_empty() && !inner.starts_with(' ') && !inner.starts_with('\t') {
                    break;
                }
                end += 1;
            }
            let contains = (i + 1..end).any(|k| {
                let inner = lines[k].trim_end_matches('\r').trim_start();
                parse_id(inner).as_deref() == Some(row_id)
            });
            if contains {
                removed = true;
                i = end;
                continue;
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    if removed {
        std::fs::write(patch_path, restore_placeholder(&out.join("\n")))
            .with_context(|| format!("write {}", patch_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inserted_ids_reads_ids_under_insert() {
        let text = "\
- id: timer
  name: '@deepseek-ai/cordis-plugin-timer'
- insert:
    - id: my-plugin
      name: my-pkg
    - id: other
      config:
        x: 1
- id: sibling
  name: unrelated
";
        assert_eq!(parse_inserted_ids(text), vec!["my-plugin", "other"]);
    }

    #[test]
    fn toggle_round_trip_removes_disable_on_enable() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");

        // Start from the profile template's empty-list placeholder.
        std::fs::write(&path, "# template\n[]\n").unwrap();

        disable_row(&path, "a").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# []"));
        assert!(text.contains("- id: a\n  disabled: true\n"));
        assert!(read_patch_state(&path).disables.contains("a"));

        enable_row(&path, "a").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("disabled: true"));
        assert!(!read_patch_state(&path).disables.contains("a"));
        // The placeholder is restored so the file stays a valid top-level array.
        assert!(text.trim().contains("[]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enable_without_disable_force_enables() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-test-f{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");

        enable_row(&path, "b").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("- id: b\n  disabled: false\n"));
        assert!(read_patch_state(&path).forced.contains("b"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_row_blocks_drops_both_states_and_restores_placeholder() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-test-r{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");
        std::fs::write(&path, "- id: a\n  disabled: true\n- id: b\n  disabled: false\n").unwrap();

        remove_row_blocks(&path, &["a".to_string(), "b".to_string()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("disabled:"));
        assert!(text.trim().contains("[]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_patch_disabled_extracts_disabled_ids() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");
        std::fs::write(
            &path,
            "- id: a\n  disabled: true\n- id: b\n  disabled: false\n- id: c\n",
        )
        .unwrap();
        let set = read_patch_disabled(&path);
        assert!(set.contains("a"));
        assert!(!set.contains("b"));
        assert!(!set.contains("c"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real-world P0 acceptance: import the sibling `deepseek-harness-master`
    /// checkout into the *actual* launcher runtimes dir, mark it active, and
    /// confirm the whole resolve chain lands on `managed` (not the dev tree).
    /// `#[ignore]` because it needs that checkout and copies a lot of disk.
    #[test]
    #[ignore = "requires the sibling deepseek-harness-master checkout"]
    fn import_real_master_and_detect_managed() {
        let Some(root) = std::env::var_os("LOCALAPPDATA") else {
            eprintln!("no LOCALAPPDATA — skipping");
            return;
        };
        let runtimes_dir = PathBuf::from(root).join("AIHarnessLauncher").join("runtimes");
        let master = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness-master");
        if !master.join("apps/cli/lib/bin.js").is_file() {
            eprintln!("sibling checkout missing — skipping real import");
            return;
        }
        let mgr = Runtimes::new(runtimes_dir.clone());
        // Clean the test's own target so a stale half-copy can't block a fresh
        // import (install_from_source refuses healthy existing installs).
        // remove_dir_all removes junctions as links — the source checkout is
        // never touched through a reparse point.
        let _ = std::fs::remove_dir_all(runtimes_dir.join("dsh-0.1.0-rc.7"));
        let entry = mgr.install_from_source(&master, None).expect("import master");
        eprintln!("imported {} -> {}", entry.version, entry.dir);
        mgr.set_active(&entry.version).expect("set active");

        let adapter = DshAdapter::configured(runtimes_dir, None);
        let settings = AppSettings::default();
        let (bin, source) = adapter.resolve_bin(&settings).expect("managed bin resolves");
        assert_eq!(source, "managed", "detect must prefer the managed runtime over the dev tree");
        assert_eq!(bin, mgr.bin_path(&entry.version));

        let info = adapter.detect(&settings).expect("detect with managed runtime");
        assert_eq!(info.source, "managed");
        assert_eq!(info.version, entry.version);
        assert!(info.node_version.starts_with('v'), "node version = {}", info.node_version);
        // Path-separator agnostic: Windows paths use backslashes.
        assert!(
            info.bin_path.replace('\\', "/").ends_with("apps/cli/lib/bin.js"),
            "bin_path = {}",
            info.bin_path
        );
    }

    /// Fast chain test (no real checkout needed): install a synthetic DSH tree
    /// into a temp runtimes dir, mark it active, and confirm the whole adapter
    /// resolves to `managed` — including the vendored Node.
    #[test]
    fn detect_resolves_managed_after_synthetic_install() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-chain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        let bin = src.join("apps/cli/lib/bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "// fake dsh\n").unwrap();
        let pkg = src.join("apps/cli/package.json");
        std::fs::write(
            &pkg,
            r#"{"name":"@deepseek-ai/dsh","version":"0.2.0-test"}"#,
        )
        .unwrap();

        let runtimes_dir = dir.join("runtimes");
        let mgr = Runtimes::new(runtimes_dir.clone());
        let entry = mgr.install_from_source(&src, None).unwrap();
        assert_eq!(entry.version, "0.2.0-test");
        mgr.set_active("0.2.0-test").unwrap();

        let adapter = DshAdapter::configured(runtimes_dir, None);
        let settings = AppSettings::default();
        let (bin2, source) = adapter.resolve_bin(&settings).expect("managed bin resolves");
        assert_eq!(source, "managed");
        assert_eq!(bin2, mgr.bin_path("0.2.0-test"));

        let info = adapter.detect(&settings).expect("detect with managed runtime");
        assert_eq!(info.source, "managed");
        assert_eq!(info.version, "0.2.0-test");
        // Node resolves from the dev vendored copy (or PATH fallback), and the
        // version string is the node `--version` output.
        assert!(!info.node_version.is_empty(), "node_version = {}", info.node_version);
        assert!(info.node_path.is_some(), "node_path must be reported");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real P1 acceptance: boot the *actual* managed DSH and stop it 10 times
    /// in a row, asserting every stop tears the whole tree down (the launcher's
    /// spawned pid — and everything it forked — is gone). `#[ignore]` because
    /// it needs the P0 managed runtime and ~a minute of real boots.
    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the P0 managed runtime; 10 real boot/stop cycles"]
    async fn real_dsh_stop_start_10_rounds_no_scars() {
        use std::sync::Arc;
        use std::time::Duration;

        async fn wait_dead(pid: u32, timeout: Duration) -> bool {
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                if !launcher_core::process::pid_alive(pid) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            false
        }

        let Some(root) = std::env::var_os("LOCALAPPDATA") else {
            eprintln!("no LOCALAPPDATA — skipping");
            return;
        };
        let runtimes_dir = PathBuf::from(root).join("AIHarnessLauncher").join("runtimes");
        let mgr = Runtimes::new(runtimes_dir.clone());
        if mgr.resolve_version().is_none() {
            eprintln!("no managed runtime installed — skipping real E2E");
            return;
        }
        let adapter = DshAdapter::configured(runtimes_dir, None);
        let settings = AppSettings::default();
        let (_bin, source) = adapter.resolve_bin(&settings).expect("bin resolves");
        assert_eq!(source, "managed", "E2E must run the managed runtime, not the dev tree");

        let ws = std::env::temp_dir().join(format!("ahl-p1-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();

        let instance = InstanceManifest {
            id: "p1-e2e".into(),
            name: "P1 E2E".into(),
            runtime: launcher_core::RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "e2e".into(),
            plugins: vec![],
            skills: vec![],
            mcp: vec![],
            workspace: ws.display().to_string(),
        };
        let provider = ResolvedProvider {
            profile: launcher_core::ProviderProfile {
                id: "e2e".into(),
                name: "E2E".into(),
                base_url: None,
                model: None,
                models: vec![],
            },
            api_key: "sk-dummy-not-validated-at-boot".into(),
        };
        let env = adapter.build_env(&provider, &instance).unwrap();

        for round in 1..=10 {
            let (url_tx, mut url_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let on_log: LogSink = {
                let url_tx = url_tx.clone();
                let line_tx = line_tx.clone();
                Arc::new(move |line: LogLine| {
                    let text = line.line;
                    if text.contains("dsh web") && text.contains("http://127.0.0.1:") {
                        let _ = url_tx.send(text.clone());
                    }
                    let _ = line_tx.send(text);
                })
            };
            let mut handle = match adapter.launch(&settings, &instance, &env, on_log).await {
                Ok(h) => h,
                Err(e) => panic!("round {round}: launch failed: {e}"),
            };
            let pid = handle.pid;

            // The production readiness signal: DSH prints its web URL. Give the
            // first boot time to materialize the web profile.
            let url = tokio::time::timeout(Duration::from_secs(60), url_rx.recv()).await;
            if url.is_err() {
                let mut tail = Vec::new();
                while let Ok(l) = line_rx.try_recv() {
                    tail.push(l);
                }
                panic!(
                    "round {round}: dsh did not boot within 60s (pid {pid}). last logs:\n{}",
                    tail.into_iter().rev().take(20).collect::<Vec<_>>().join("\n")
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            handle.stop().await.expect("stop");

            assert!(
                wait_dead(pid, Duration::from_secs(5)).await,
                "round {round}: launcher-spawned pid {pid} survived stop — tree not torn down"
            );
        }

        let _ = std::fs::remove_dir_all(&ws);
    }
}
