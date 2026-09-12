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
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use launcher_core::process::{kill_tree, spawn_child_with_exit, ChildHandle, ExitSink, LogSink};
use launcher_core::runtime::RuntimeInfo;
use launcher_core::{
    AppSettings, InstanceManifest, LogLevel, LogLine, LogStream, ResolvedProvider, RuntimeAdapter,
};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub mod content;
pub mod crash;
pub mod diagnostics;
pub mod events;
pub mod health;
pub mod language;
pub mod llm;
pub mod mcp_import;
pub mod mcp_local;
pub mod mcp_prefetch;
pub mod mcp_probe;
pub mod mcp_resolver;
pub mod page_signature;
pub mod pnpm;
pub mod rescue;
pub mod runtimes;
pub mod safe_boot;
pub mod theme;
pub mod web_check;

pub use diagnostics::{BundleInfo, DiagnosticsReport, OrderViolation};
use runtimes::Runtimes;
use safe_boot::{SafeBundlePlan, SafeProfileVerdict, SafeTier};

/// The web profile's default port.
pub const DEFAULT_WEB_PORT: u16 = 3080;

/// One plugin as DSH sees it in a profile: a `dependencies` entry with its
/// enable state (`dsh.profile.bundles` membership).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub toggleable: bool,
    #[serde(default)]
    pub kind: InstalledPluginKind,
    #[serde(default)]
    pub source: InstalledPluginSource,
    #[serde(default)]
    pub entry_id: Option<String>,
    #[serde(default)]
    pub fiber_phase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InstalledPluginKind {
    #[default]
    Plugin,
    Theme,
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InstalledPluginSource {
    #[default]
    Profile,
    Inventory,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InventoryEntry {
    entry_id: String,
    module_name: String,
    enabled: bool,
    fiber_phase: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InventorySnapshot {
    entries: Vec<InventoryEntry>,
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

impl Default for DshAdapter {
    fn default() -> Self {
        Self::new()
    }
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
    /// The result is normalized with `canonicalize_for_node` so a `\\?\`
    /// verbatim prefix (which Tauri's `resource_dir()` returns on Windows) is
    /// stripped — `CreateProcessW` otherwise starts `node.exe` from a verbatim
    /// path and it dies with `0xc0000142` (STATUS_DLL_INIT_FAILED).
    pub fn resolve_node(&self, settings: &AppSettings) -> Option<PathBuf> {
        for cand in self.node_candidates(settings) {
            if cand.is_file() {
                return Some(Self::canonicalize_for_node(&cand));
            }
        }
        which::which("node")
            .ok()
            .map(|p| Self::canonicalize_for_node(&p))
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
        on_exit: Option<ExitSink>,
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
        let handle = spawn_child_with_exit(cmd, on_log, on_exit).await?;
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

    /// The instance's profile manifest as read from disk, or `None` when it is
    /// missing or unparseable. Read-only by design: DSH owns this file (it
    /// rewrites `dsh.profile.bundles` on every plugin operation), so the
    /// launcher never writes it — see [`safe_boot`] for the scratch profile
    /// that exists instead of editing this one.
    pub(crate) fn read_profile_manifest(instance: &InstanceManifest) -> Option<serde_json::Value> {
        let path = Self::profile_dir(instance).join("package.json");
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Installed packages as DSH sees them: profile dependencies plus bundled
    /// profile rows. User-installed bundle plugins are toggleable; built-in
    /// bundles and client/theme packages are visible assets but not toggleable
    /// through the user patch layer.
    pub fn installed_plugins(instance: &InstanceManifest) -> Vec<InstalledPlugin> {
        let Some(value) = Self::read_profile_manifest(instance) else {
            return Vec::new();
        };
        let deps: HashSet<String> = value
            .get("dependencies")
            .and_then(|d| d.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let bundles: Vec<String> = value
            .pointer("/dsh/profile/bundles")
            .and_then(|b| b.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut names: Vec<String> = deps.iter().cloned().collect();
        for bundle in &bundles {
            if !names.iter().any(|name| name == bundle) {
                names.push(bundle.clone());
            }
        }
        names.sort();
        let profile_dir = Self::profile_dir(instance);
        let disabled_ids = read_patch_disabled(&profile_dir.join("cordis.patch.yml"));
        let insert_names = read_insert_names(&profile_dir.join("cordis.patch.yml"));
        names
            .into_iter()
            .map(|name| {
                let in_bundles = bundles.iter().any(|b| b == &name);
                let ids = inserted_row_ids(&profile_dir, &name);
                let disabled = ids.iter().any(|id| disabled_ids.contains(id));
                let kind = installed_plugin_kind(&profile_dir, &name, !ids.is_empty());
                // A skin is a client plugin DSH will not auto-load; the
                // launcher mounts it by writing an insert row, so its `enabled`
                // state is "insert row present", not "installed". A skin that
                // declares `dsh.bundle` activates via bundles instead and keeps
                // plugin semantics.
                let skin_is_bundle = package_json(&profile_dir, &name)
                    .map(|p| p.pointer("/dsh/bundle").is_some())
                    .unwrap_or(false);
                let enabled = match kind {
                    InstalledPluginKind::Theme if !skin_is_bundle => insert_names.contains(&name),
                    // A `dsh.bundle` skin mounts as a profile bundle, so it
                    // toggles exactly like one: enabled until a `disabled:` row
                    // lands on one of its bundle entries.
                    InstalledPluginKind::Theme => {
                        if in_bundles {
                            !disabled
                        } else {
                            !ids.is_empty() && !disabled
                        }
                    }
                    InstalledPluginKind::Client => deps.contains(&name),
                    InstalledPluginKind::Plugin => {
                        if in_bundles {
                            !disabled
                        } else {
                            !ids.is_empty() && !disabled
                        }
                    }
                };
                // A client-plugin skin (no bundle) toggles through the insert
                // row, but `plugin_toggle` only routes there when the package
                // is registered in `skin_packages` — a stray package merely
                // *named* like a skin (e.g. a leftover dep whose repo root has
                // no package.json) must not offer a switch it cannot honor.
                let registered_skin = instance.skin_packages.iter().any(|sp| sp.package == name);
                InstalledPlugin {
                    name,
                    enabled,
                    toggleable: !ids.is_empty()
                        || (matches!(
                            kind,
                            InstalledPluginKind::Theme if !skin_is_bundle
                        ) && registered_skin),
                    kind,
                    source: InstalledPluginSource::Profile,
                    entry_id: None,
                    fiber_phase: None,
                }
            })
            .collect()
    }

    /// Live DSH Cordis Loader inventory, the same source as the stock
    /// Workspace Settings → Plugins → Plugin list tab.
    pub async fn plugin_inventory(port: u16) -> Result<Vec<InstalledPlugin>> {
        let payload = serde_json::json!({ "args": {} });
        let value = crate::theme::host_rpc(port, "pluginInventory/list", payload).await?;
        crate::theme::ensure_ok(&value, "pluginInventory/list")?;
        let mut body = value.get("value").cloned().ok_or_else(|| {
            anyhow!(
                "DSH returned no plugin inventory — the harness did not answer \
                     pluginInventory/list. Restart the instance; if it persists, check the DSH \
                     version in Settings → Runtime."
            )
        })?;
        if body.get("ok").and_then(|v| v.as_bool()).is_some() {
            crate::theme::ensure_ok(&body, "pluginInventory/list remote")?;
            body = body.get("value").cloned().ok_or_else(|| {
                anyhow!(
                    "DSH returned no plugin inventory over HTTP — the harness did not answer \
                         pluginInventory/list. Restart the instance; if it persists, check the DSH \
                         version in Settings → Runtime."
                )
            })?;
        }
        let snapshot: InventorySnapshot = serde_json::from_value(body)?;
        Ok(snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let name = inventory_short_name(&entry.module_name);
                InstalledPlugin {
                    kind: inventory_kind(&entry.module_name),
                    name,
                    enabled: entry.enabled,
                    toggleable: false,
                    source: InstalledPluginSource::Inventory,
                    entry_id: Some(entry.entry_id),
                    fiber_phase: entry.fiber_phase,
                }
            })
            .collect())
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
            // Nothing to fix, and saying so is the point: the switch is missing
            // because the *plugin* has no bundle rows, not because the launcher
            // failed. Its Enable/Disable has nothing to point at.
            return Err(anyhow!(
                "plugin '{name}' is not switchable — its package.json declares no `dsh.bundle`, \
                 so it has no bundle rows the patch layer could turn off. Nothing is broken and \
                 there is nothing to retry: this plugin is active whenever it is installed."
            ));
        }
        let patch_path = profile_dir.join("cordis.patch.yml");
        for id in ids {
            if !is_valid_row_id(&id) {
                return Err(anyhow!(
                    "this plugin's bundle declares a row id the patch layer cannot write \
                     ('{id}') — the plugin's own packaging is at fault rather than the launcher. \
                     Update the plugin, or install it from its own repo"
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
        let node = node.to_string_lossy().to_string();
        let mut full = vec![
            info.bin_path,
            "plugin".to_string(),
            "--profile".to_string(),
            instance.profile.clone(),
        ];
        full.extend(args.iter().cloned());
        let envs = vec![("DSH_HOME".to_string(), instance.workspace.clone())];
        run_timed(
            &node,
            &full,
            Path::new(&instance.workspace),
            &envs,
            on_log,
            INSTALL_TIMEOUT,
        )
        .await
        .map_err(|e| {
            let op = args.first().map(|a| a.as_str()).unwrap_or("");
            anyhow!("dsh plugin {op} {e}")
        })
    }

    /// Ask dsh whether a profile composes: `dsh --profile <name> --dump-config`.
    ///
    /// This runs dsh's own composition step — bundle resolution against the
    /// installation, the profile patch layer, the home patch layer — without
    /// booting: nothing mounts and no `!!js` expression is evaluated. So a
    /// bundle that cannot resolve or a patch row that cannot parse fails here,
    /// as a sentence from dsh, instead of as a boot the user has to interpret.
    ///
    /// Two deliberate choices:
    ///
    /// - **No API key in the child's environment.** Composition needs none, and
    ///   a diagnostic process has no business holding the user's credentials.
    /// - **`Ok(non-zero)` is a verdict, not an error.** `Err` means dsh never
    ///   got to speak (spawn failure or timeout) — the launcher must not report
    ///   a refusal it cannot substantiate.
    ///
    /// Not read-only, and not AHL's doing: `prepareProfile` links the
    /// installation into `$DSH_HOME/profiles/node_modules` on the way, which is
    /// the same thing a boot does. It touches no profile of the user's.
    pub async fn dump_profile_config(
        &self,
        settings: &AppSettings,
        instance: &InstanceManifest,
        profile: &str,
    ) -> Result<CapturedOutput, String> {
        let info = self.detect(settings).map_err(|e| format!("{e:#}"))?;
        let node = self
            .resolve_node(settings)
            .ok_or_else(|| "Node not found — can't run DSH".to_string())?;
        let node = node.to_string_lossy().to_string();
        let args = vec![
            info.bin_path,
            "--profile".to_string(),
            profile.to_string(),
            "--dump-config".to_string(),
        ];
        let envs = vec![("DSH_HOME".to_string(), instance.workspace.clone())];
        run_capture(
            &node,
            &args,
            Path::new(&instance.workspace),
            &envs,
            DUMP_CONFIG_TIMEOUT,
        )
        .await
    }

    /// Write the safe profile for `tier`, then let dsh judge it — the whole
    /// pre-flight in one call, in the order that makes it mean anything.
    ///
    /// Returns dsh's verdict alongside the plan whether or not it composed, so
    /// the caller can quote dsh verbatim when it did not. It does **not**
    /// launch: whether to boot a profile dsh just refused is the caller's
    /// decision, and it should be a decision made on the verdict.
    pub async fn prepare_safe_profile(
        &self,
        settings: &AppSettings,
        instance: &InstanceManifest,
        tier: SafeTier,
    ) -> Result<(SafeBundlePlan, SafeProfileVerdict)> {
        let plan = safe_boot::write_safe_profile(instance, tier)?;
        let captured = self
            .dump_profile_config(settings, instance, safe_boot::SAFE_PROFILE_NAME)
            .await
            .map_err(|e| anyhow!("dsh --dump-config {e}"))?;
        let verdict = safe_boot::classify_dump(captured.code, &captured.stdout, &captured.stderr);
        Ok((plan, verdict))
    }
}

// ---------------------------------------------------------------------------
// Timed sub-process runner shared by every git/pnpm call site.
//
// `git clone --depth 1` / `dsh plugin add` have no built-in timeout: when the
// TCP connection dies mid-transfer git's `index-pack` waits forever, which used
// to wedge the install job at `running` indefinitely (no failure, no retry).
// Every caller now runs through [`run_timed`], which kills the WHOLE process
// tree on expiry — a bare `start_kill()` would orphan git's
// `index-pack`/`fetch-pack`/`git-remote-https` grandchildren and the hang
// would survive the kill.
// ---------------------------------------------------------------------------

/// A shallow clone / fetch / checkout that must finish or die.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(180);

/// A `dsh plugin` (pnpm) install or from-source build that must finish or die.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

/// A log sink that discards every line (skill-clone, probes where output is
/// already routed elsewhere).
pub fn silent_log_sink() -> LogSink {
    std::sync::Arc::new(|_| {})
}

/// A short probe that must finish or die: `dsh --profile <x> --dump-config`
/// composes a profile without booting it (no mounts, no `!!js` evaluation), so
/// this is a cold node start plus file reads — generous at a minute.
pub const DUMP_CONFIG_TIMEOUT: Duration = Duration::from_secs(60);

/// Capture of a finished probe process.
pub struct CapturedOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `program` to completion and *capture* stdout/stderr, for callers that
/// need the child's words as data rather than as log lines (dsh's own verdict
/// on a profile). Same timeout and whole-tree-kill discipline as
/// [`run_timed`]: a probe that hangs must not hang the launcher, and killing
/// only the direct child would leave node's grandchildren behind.
///
/// `Ok` carries a non-zero exit: a refusal is a result. `Err` is reserved for
/// spawn failure and timeout, because neither is something the child said.
pub async fn run_capture(
    program: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
    timeout: Duration,
) -> Result<CapturedOutput, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("spawn {program}: {e}"))?;

    // Drain both pipes on their own tasks, like `run_timed` — reading them in
    // sequence would deadlock on a child that fills the other pipe first.
    let mut out_drain = None;
    if let Some(out) = child.stdout.take() {
        out_drain = Some(tokio::spawn(async move {
            let mut buf = String::new();
            let _ = BufReader::new(out).read_to_string(&mut buf).await;
            buf
        }));
    }
    let mut err_drain = None;
    if let Some(err) = child.stderr.take() {
        err_drain = Some(tokio::spawn(async move {
            let mut buf = String::new();
            let _ = BufReader::new(err).read_to_string(&mut buf).await;
            buf
        }));
    }

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let code = status
                .map_err(|e| format!("wait {program}: {e}"))?
                .code()
                .unwrap_or(1);
            let stdout = match out_drain {
                Some(drain) => drain.await.unwrap_or_default(),
                None => String::new(),
            };
            let stderr = match err_drain {
                Some(drain) => drain.await.unwrap_or_default(),
                None => String::new(),
            };
            Ok(CapturedOutput {
                code,
                stdout,
                stderr,
            })
        }
        Err(_) => {
            if let Some(pid) = child.id() {
                kill_tree(pid);
            }
            let _ = child.wait().await;
            if let Some(drain) = out_drain {
                let _ = drain.await;
            }
            if let Some(drain) = err_drain {
                let _ = drain.await;
            }
            Err(format!("{program} timed out after {}s", timeout.as_secs()))
        }
    }
}

/// Spawn `program`, stream stdout/stderr lines through `sink`, and wait with a
/// `timeout`. On expiry the whole process tree is killed (Windows
/// `taskkill /T /F`, elsewhere `killpg`) so grandchildren die too, then a
/// readable error is returned. Returns `Ok(exit_code)` — including non-zero —
/// so callers decide what a failure means; `Err` is reserved for spawn / wait /
/// timeout.
pub async fn run_timed(
    program: &str,
    args: &[String],
    cwd: &Path,
    envs: &[(String, String)],
    sink: LogSink,
    timeout: Duration,
) -> Result<i32, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.current_dir(cwd);
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

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let code = status
                .map_err(|e| format!("wait {program}: {e}"))?
                .code()
                .unwrap_or(1);
            for r in readers {
                let _ = r.await;
            }
            Ok(code)
        }
        Err(_) => {
            if let Some(pid) = child.id() {
                kill_tree(pid);
            }
            let _ = child.wait().await;
            for r in readers {
                let _ = r.await;
            }
            // A timeout is almost always the network, not the package: a first
            // install has to fetch every dependency, and `dsh plugin add` gives
            // no progress to watch. Say so, and name the one thing the user can
            // actually do about it.
            Err(format!(
                "{program} timed out after {}s and was stopped. The first install of a large \
                 plugin downloads its whole dependency tree, so a slow or blocked npm/git \
                 connection is the usual cause — check your network and Retry.",
                timeout.as_secs()
            ))
        }
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

fn package_json(profile_dir: &Path, name: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(
        profile_dir
            .join("node_modules")
            .join(name)
            .join("package.json"),
    )
    .ok()?;
    serde_json::from_str::<serde_json::Value>(&text).ok()
}

fn installed_plugin_kind(profile_dir: &Path, name: &str, toggleable: bool) -> InstalledPluginKind {
    let lower = name.to_lowercase();
    let Some(pkg) = package_json(profile_dir, name) else {
        return if !toggleable && (lower.contains("skin") || lower.contains("theme")) {
            InstalledPluginKind::Theme
        } else {
            InstalledPluginKind::Plugin
        };
    };
    let has_client = pkg.pointer("/dsh/client").is_some();
    let has_bundle = pkg.pointer("/dsh/bundle").is_some();
    let keywords = pkg
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let skinish = lower.contains("skin")
        || lower.contains("theme")
        || keywords
            .iter()
            .any(|k| matches!(k.as_str(), "skin" | "theme"));
    if has_client && skinish {
        InstalledPluginKind::Theme
    } else if has_client && !has_bundle {
        InstalledPluginKind::Client
    } else {
        InstalledPluginKind::Plugin
    }
}

fn inventory_short_name(module_name: &str) -> String {
    let unscoped = if let Some((_, tail)) = module_name.split_once('/') {
        tail
    } else {
        module_name
    };
    unscoped
        .strip_prefix("cordis:")
        .unwrap_or(unscoped)
        .strip_prefix("cordis-plugin-")
        .unwrap_or(unscoped)
        .strip_prefix("dsh-host-")
        .or_else(|| unscoped.strip_prefix("dsh-client-"))
        .or_else(|| unscoped.strip_prefix("dsh-"))
        .unwrap_or(unscoped)
        .to_string()
}

fn inventory_kind(module_name: &str) -> InstalledPluginKind {
    let lower = module_name.to_lowercase();
    if lower.contains("skin") || lower.contains("theme") {
        InstalledPluginKind::Theme
    } else if lower.contains("dsh-client-") {
        InstalledPluginKind::Client
    } else {
        InstalledPluginKind::Plugin
    }
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

/// The `name:` values nested under `- insert:` blocks — the packages a patch
/// layer currently mounts via insert rows. A client-plugin skin is "enabled"
/// iff its npm package name appears here (see [`installed_plugins`]).
fn read_insert_names(patch_path: &Path) -> HashSet<String> {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    parse_insert_names(&text)
}

fn parse_insert_names(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
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
        if let Some(name) = parse_name(trimmed) {
            if let Some(ins) = insert_indent {
                if indent > ins {
                    out.insert(name);
                }
            }
        }
    }
    out
}

fn parse_name(trimmed: &str) -> Option<String> {
    let t = trimmed.strip_prefix('-').unwrap_or(trimmed).trim();
    let rest = t.strip_prefix("name:")?.trim();
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

/// Append one top-level patch entry to `text`, handling the empty-list `[]`
/// placeholder the profile template ships (appending after it would produce two
/// top-level YAML documents — the loader refuses that). Pure — the disk writers
/// ([`append_patch_entry`], [`sync_mcp_patch`]) share this core. Port of
/// dsh-market's `appendPatchEntry`.
pub(crate) fn append_block_to_text(text: &str, block: &str) -> String {
    let core = text.trim();
    if core.is_empty() {
        return block.to_string();
    }
    let stripped = without_comment_lines(text).trim().to_string();
    let mut next = if stripped.is_empty() {
        // comments only — append after them
        text.to_string()
    } else if stripped == "[]" || stripped == "[ ]" {
        // comment out the empty-list placeholder and append
        comment_out_placeholder(text)
    } else {
        text.to_string()
    };
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(block);
    next
}

/// Disk wrapper for [`append_block_to_text`].
pub(crate) fn append_patch_entry(patch_path: &Path, block: &str) -> Result<()> {
    let text = std::fs::read_to_string(patch_path).unwrap_or_default();
    std::fs::write(patch_path, append_block_to_text(&text, block))
        .with_context(|| format!("write {}", patch_path.display()))
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
/// `withPlaceholderRestored`. `pub(crate)` so
/// [`sync_mcp_patch`](crate::content::sync_mcp_patch) can finish a compile
/// that left no content behind.
pub(crate) fn restore_placeholder(text: &str) -> String {
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

/// Remove every top-level `- insert:` block that inserts a launcher-owned MCP
/// row — a nested row whose `name:` is `'@deepseek-ai/dsh-mcp-client'` (the
/// region [`sync_mcp_patch`](crate::content::sync_mcp_patch) recompiles). The
/// block spans from its `- insert:` line to the next column-0 entry (or EOF).
/// Pure and line-based, like the other patch writers: plugin `- id:`/`disabled:`
/// rows, comments, and non-launcher `insert:` blocks pass through untouched,
/// and line endings survive the split/join. Returns the text unchanged when
/// there is nothing to remove.
pub(crate) fn remove_mcp_insert_blocks(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
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
            let owns_mcp = (i + 1..end).any(|k| {
                let content = lines[k].trim_end_matches('\r');
                let content = content.split('#').next().unwrap_or("").trim();
                let t = content.trim_start_matches('-').trim();
                let Some(rest) = t.strip_prefix("name:") else {
                    return false;
                };
                rest.trim().trim_matches(['"', '\'']) == "@deepseek-ai/dsh-mcp-client"
            });
            if owns_mcp {
                i = end;
                continue;
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    out.join("\n")
}

/// Remove every top-level `- insert:` block that inserts a launcher-owned skin
/// row — a nested row whose `id:` carries the `skin-` prefix
/// [`skin_id_from_package`](crate::content::skin_id_from_package) assigns. The
/// launcher owns the `skin-` id namespace, so removing by prefix (rather than
/// by package name) also clears the orphan block a removed skin leaves behind.
/// The block spans from its `- insert:` line to the next column-0 entry (or
/// EOF). Pure and line-based, matching [`remove_mcp_insert_blocks`]: MCP rows,
/// plugin rows, comments, and non-launcher `insert:` blocks pass through
/// untouched, and line endings survive the split/join.
pub(crate) fn remove_skin_insert_blocks(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        if line.starts_with("- insert:") {
            let mut end = i + 1;
            while end < lines.len() {
                let inner = lines[end].trim_end_matches('\r');
                if !inner.is_empty() && !inner.starts_with(' ') && !inner.starts_with('\t') {
                    break;
                }
                end += 1;
            }
            let owns_skin = (i + 1..end).any(|k| {
                let content = lines[k].trim_end_matches('\r');
                let content = content.split('#').next().unwrap_or("").trim();
                let t = content.trim_start_matches('-').trim();
                let Some(rest) = t.strip_prefix("id:") else {
                    return false;
                };
                rest.trim()
                    .trim_start_matches(['"', '\''])
                    .starts_with("skin-")
            });
            if owns_skin {
                i = end;
                continue;
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::SkinPackage;

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
    fn remove_mcp_insert_blocks_drops_only_launcher_owned_blocks() {
        let text = "\
# comment
- id: timer
  disabled: true
- insert:
    - id: user-row
      name: other-plugin
- insert:
    - id: mcp-a
      name: '@deepseek-ai/dsh-mcp-client'
- insert:
    - id: mcp-b
      name: '@deepseek-ai/dsh-mcp-client'
";
        let next = remove_mcp_insert_blocks(text);
        assert!(!next.contains("mcp-a"), "{next}");
        assert!(!next.contains("mcp-b"), "{next}");
        assert!(next.contains("# comment"), "{next}");
        assert!(next.contains("- id: timer\n  disabled: true"), "{next}");
        assert!(next.contains("user-row"), "{next}");
        assert_eq!(
            next.lines().filter(|l| l.starts_with("- insert:")).count(),
            1,
            "only the user insert block survives:\n{next}"
        );
    }

    #[test]
    fn remove_mcp_insert_blocks_is_idempotent_without_matches() {
        let text = "- id: timer\n  disabled: true\n# only comments\n";
        assert_eq!(remove_mcp_insert_blocks(text), text);
        assert_eq!(remove_mcp_insert_blocks(""), "");
    }

    #[test]
    fn remove_skin_insert_blocks_drops_skin_prefixed_blocks_only() {
        let text = "\
# comment
- id: timer
  disabled: true
- insert:
    - id: mcp-a
      name: '@deepseek-ai/dsh-mcp-client'
- insert:
    - id: skin-sakura
      name: dsh-skin-sakura
    - id: skin-dark
      name: dsh-skin-dark
";
        let next = remove_skin_insert_blocks(text);
        assert!(!next.contains("skin-sakura"), "{next}");
        assert!(!next.contains("skin-dark"), "{next}");
        assert!(next.contains("mcp-a"), "{next}");
        assert!(next.contains("- id: timer\n  disabled: true"), "{next}");
        assert_eq!(
            next.lines().filter(|l| l.starts_with("- insert:")).count(),
            1,
            "only the MCP insert block survives:\n{next}"
        );
    }

    #[test]
    fn remove_skin_insert_blocks_is_idempotent_without_matches() {
        let text = "- id: timer\n  disabled: true\n# only comments\n";
        assert_eq!(remove_skin_insert_blocks(text), text);
        assert_eq!(remove_skin_insert_blocks(""), "");
    }

    #[test]
    fn parse_insert_names_reads_names_under_insert() {
        let text = "\
- id: timer
  name: '@deepseek-ai/cordis-plugin-timer'
- insert:
    - id: skin-a
      name: dsh-skin-a
    - id: skin-b
      name: '@scope/dsh-skin-b'
";
        let names = parse_insert_names(text);
        assert!(names.contains("dsh-skin-a"));
        assert!(names.contains("@scope/dsh-skin-b"));
        assert!(!names.contains("@deepseek-ai/cordis-plugin-timer"));
    }

    /// A throwaway profile fixture with two installed skins: a `dsh.bundle`
    /// skin (catppuccin) and a client-plugin skin (sakura). Returns the ws dir
    /// the caller must clean up.
    fn skin_profile_fixture() -> (InstanceManifest, std::path::PathBuf) {
        let ws = std::env::temp_dir().join(format!("ahl-skin-sem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let profile = ws.join("profiles").join("web");
        for pkg in ["dsh-catppuccin", "dsh-skin-sakura"] {
            std::fs::create_dir_all(profile.join("node_modules").join(pkg)).unwrap();
        }
        // Both are deps; catppuccin declares a bundle (mounts via bundles).
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"dsh-catppuccin":"link:x","dsh-skin-sakura":"link:y"},"dsh":{"profile":{"bundles":["dsh-catppuccin"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            profile
                .join("node_modules")
                .join("dsh-catppuccin")
                .join("package.json"),
            r#"{"name":"dsh-catppuccin","keywords":["skin"],"dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        std::fs::write(
            profile
                .join("node_modules")
                .join("dsh-catppuccin")
                .join("cordis.patch.yml"),
            "- insert:\n    - id: catppuccin\n      name: 'dsh-catppuccin'\n",
        )
        .unwrap();
        std::fs::write(
            profile
                .join("node_modules")
                .join("dsh-skin-sakura")
                .join("package.json"),
            r#"{"name":"dsh-skin-sakura","dsh":{"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        let instance = InstanceManifest {
            id: "test".into(),
            name: "Test".into(),
            runtime: launcher_core::RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "default".into(),
            plugins: vec![],
            skills: vec![],
            mcp: vec![],
            skins: vec![],
            // Both skins are launcher-installed, so both are tracked in
            // `skin_packages` (the catalog key → package map). A client skin
            // only toggles through its insert row when registered here.
            skin_packages: vec![
                SkinPackage {
                    key: "zhijun-dai/Catppuccin-dsh-theme".into(),
                    package: "dsh-catppuccin".into(),
                    enabled: true,
                },
                SkinPackage {
                    key: "leo-aba/dsh-skin-sakura".into(),
                    package: "dsh-skin-sakura".into(),
                    enabled: false,
                },
            ],
            workspace: ws.display().to_string(),
        };
        (instance, ws)
    }

    fn installed_by_name(instance: &InstanceManifest, name: &str) -> InstalledPlugin {
        DshAdapter::installed_plugins(instance)
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("plugin {name} not in installed set"))
    }

    #[test]
    fn installed_plugins_bundle_skin_toggles_via_disabled_rows() {
        let (instance, ws) = skin_profile_fixture();
        let patch = DshAdapter::profile_dir(&instance).join("cordis.patch.yml");

        // No user rows yet → the bundle skin is on; the client skin has no
        // insert row so it is off (installed but not mounted).
        let cat = installed_by_name(&instance, "dsh-catppuccin");
        assert_eq!(cat.kind, InstalledPluginKind::Theme);
        assert!(cat.enabled, "bundle skin is enabled by default");
        assert!(cat.toggleable, "bundle skin has toggleable rows");
        let sakura = installed_by_name(&instance, "dsh-skin-sakura");
        assert_eq!(sakura.kind, InstalledPluginKind::Theme);
        assert!(!sakura.enabled, "client skin without insert row is off");
        assert!(sakura.toggleable, "client skin toggles via its insert row");

        // Disable the bundle skin the way plugin_toggle now does — write
        // `disabled: true` on one of its bundle rows.
        std::fs::write(&patch, "- id: catppuccin\n  disabled: true\n").unwrap();
        assert!(
            !installed_by_name(&instance, "dsh-catppuccin").enabled,
            "bundle skin disables via its bundle `disabled:` row"
        );

        // Enable the client skin the way plugin_toggle now does — write its
        // insert row into the user patch.
        std::fs::write(
            &patch,
            "- insert:\n    - id: skin-sakura\n      name: dsh-skin-sakura\n",
        )
        .unwrap();
        assert!(
            installed_by_name(&instance, "dsh-skin-sakura").enabled,
            "client skin enables via insert row presence"
        );
        // Removing the insert row turns it back off (disable).
        std::fs::write(&patch, "# empty\n").unwrap();
        assert!(
            !installed_by_name(&instance, "dsh-skin-sakura").enabled,
            "client skin disables by dropping its insert row"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn unregistered_ghost_skin_is_not_toggleable() {
        // A stale profile dependency whose repo root ships no package.json is
        // classified as a theme purely from its *name* — and since the launcher
        // never registered it in `skin_packages`, toggling it has no mechanism
        // (no bundle rows, no tracked insert row). It must not present a switch
        // that `plugin_toggle` would reject at runtime.
        let ws = std::env::temp_dir().join(format!("ahl-skin-ghost-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let profile = ws.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"leo-aba__dsh-skins":"link:x"},"dsh":{"profile":{"bundles":[]}}}"#,
        )
        .unwrap();
        let instance = InstanceManifest {
            id: "ghost".into(),
            name: "Ghost".into(),
            runtime: launcher_core::RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "default".into(),
            plugins: vec![],
            skills: vec![],
            mcp: vec![],
            skins: vec![],
            skin_packages: vec![],
            workspace: ws.display().to_string(),
        };

        let ghost = installed_by_name(&instance, "leo-aba__dsh-skins");
        assert_eq!(
            ghost.kind,
            InstalledPluginKind::Theme,
            "name-heuristic theme"
        );
        assert!(
            !ghost.toggleable,
            "unregistered ghost skin must not offer a toggle it cannot honor"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn append_block_to_text_comments_out_empty_placeholder() {
        let next = append_block_to_text("# template\n[]\n", "- insert:\n    - id: x\n");
        assert!(next.contains("# []"), "{next}");
        assert!(next.contains("- insert:\n    - id: x\n"), "{next}");
        assert!(
            !next.contains("\n[]\n"),
            "placeholder must not remain active:\n{next}"
        );
    }

    #[test]
    fn append_block_to_text_handles_empty_and_comments_only() {
        assert_eq!(append_block_to_text("", "- insert:\n"), "- insert:\n");
        let comments = "# just comments\n";
        let next = append_block_to_text(comments, "- insert:\n");
        assert!(next.starts_with("# just comments\n"), "{next}");
        assert!(next.ends_with("- insert:\n"), "{next}");
        // Content (a plugin row) is left alone and the block appended after.
        let rows = "- id: timer\n  disabled: true\n";
        let next = append_block_to_text(rows, "- insert:\n");
        assert_eq!(next, rows.to_string() + "- insert:\n");
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
        std::fs::write(
            &path,
            "- id: a\n  disabled: true\n- id: b\n  disabled: false\n",
        )
        .unwrap();

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
    // Two real-machine e2e tests touch the SAME managed-runtime dir
    // (`runtimes/dsh-0.1.0-rc.7`): the import test *replaces* it (delete +
    // recopy, tens of seconds) while the stop/start test *spawns* from it. cargo
    // runs the tests in one process, in parallel — so they race and round-1
    // boots die mid-copy ("did not boot within 60s"). Serialize both behind one
    // lock; either alone is fast and clean.
    static REAL_E2E_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    #[ignore = "requires the sibling deepseek-harness-master checkout"]
    fn import_real_master_and_detect_managed() {
        let _guard = REAL_E2E_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(root) = std::env::var_os("LOCALAPPDATA") else {
            eprintln!("no LOCALAPPDATA — skipping");
            return;
        };
        let runtimes_dir = PathBuf::from(root)
            .join("AIHarnessLauncher")
            .join("runtimes");
        let master =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness-master");
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
        let entry = mgr
            .install_from_source(&master, None)
            .expect("import master");
        eprintln!("imported {} -> {}", entry.version, entry.dir);
        mgr.set_active(&entry.version).expect("set active");

        let adapter = DshAdapter::configured(runtimes_dir, None);
        let settings = AppSettings::default();
        let (bin, source) = adapter
            .resolve_bin(&settings)
            .expect("managed bin resolves");
        assert_eq!(
            source, "managed",
            "detect must prefer the managed runtime over the dev tree"
        );
        assert_eq!(bin, mgr.bin_path(&entry.version));

        let info = adapter
            .detect(&settings)
            .expect("detect with managed runtime");
        assert_eq!(info.source, "managed");
        assert_eq!(info.version, entry.version);
        assert!(
            info.node_version.starts_with('v'),
            "node version = {}",
            info.node_version
        );
        // Path-separator agnostic: Windows paths use backslashes.
        assert!(
            info.bin_path
                .replace('\\', "/")
                .ends_with("apps/cli/lib/bin.js"),
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
        let (bin2, source) = adapter
            .resolve_bin(&settings)
            .expect("managed bin resolves");
        assert_eq!(source, "managed");
        assert_eq!(bin2, mgr.bin_path("0.2.0-test"));

        let info = adapter
            .detect(&settings)
            .expect("detect with managed runtime");
        assert_eq!(info.source, "managed");
        assert_eq!(info.version, "0.2.0-test");
        // Node resolves from the dev vendored copy (or PATH fallback), and the
        // version string is the node `--version` output.
        assert!(
            !info.node_version.is_empty(),
            "node_version = {}",
            info.node_version
        );
        assert!(info.node_path.is_some(), "node_path must be reported");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_bin_prefers_settings_override() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-ovr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("apps/cli/lib/bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "// override dsh\n").unwrap();

        let adapter = DshAdapter::new(); // no runtimes / resource dir
        let settings = AppSettings {
            dsh_path: Some(bin.display().to_string()),
            ..Default::default()
        };

        let (resolved, source) = adapter
            .resolve_bin(&settings)
            .expect("settings override resolves");
        assert_eq!(
            source, "override",
            "settings.dsh_path must be the top layer"
        );
        assert_eq!(resolved, bin);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_bin_resolves_bundled_from_resource_dir() {
        let dir = std::env::temp_dir().join(format!("dsh-adapter-bndl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resource_dir = dir.join("resources");
        let bin = resource_dir.join("dsh/apps/cli/lib/bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "// bundled dsh\n").unwrap();

        // Empty managed-runtimes dir → the bundled layer is the first hit.
        let adapter = DshAdapter::configured(dir.join("runtimes"), Some(resource_dir));
        let settings = AppSettings::default();

        let (resolved, source) = adapter
            .resolve_bin(&settings)
            .expect("bundled bin resolves");
        assert_eq!(
            source, "bundled",
            "resource_dir/dsh must resolve before managed"
        );
        assert_eq!(resolved, bin);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dev layer (debug builds only) points at a sibling `deepseek-harness-master`
    /// checkout. Creating that checkout is intrusive, so assert the candidate list
    /// shape instead of a full resolve; the priority-over-dev behaviour is proven
    /// by `detect_resolves_managed_after_synthetic_install` (managed wins).
    #[test]
    fn dev_candidates_point_at_sibling_checkout() {
        let adapter = DshAdapter::new();
        let cands = adapter.dev_candidates();
        assert!(!cands.is_empty(), "dev candidates must not be empty");
        let first = cands[0].to_string_lossy().replace('\\', "/");
        assert!(
            first.ends_with("deepseek-harness-master/apps/cli/lib/bin.js"),
            "first candidate = {first}"
        );
    }

    /// Real P1 acceptance: boot the *actual* managed DSH and stop it 10 times
    /// in a row, asserting every stop tears the whole tree down (the launcher's
    /// spawned pid — and everything it forked — is gone). `#[ignore]` because
    /// it needs the P0 managed runtime and ~a minute of real boots.
    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires the P0 managed runtime; 10 real boot/stop cycles"]
    #[allow(clippy::await_holding_lock)] // intentional: serialize with the import e2e (REAL_E2E_LOCK)
    async fn real_dsh_stop_start_10_rounds_no_scars() {
        // Serialize with import_real_master_and_detect_managed (see the lock's
        // comment): never spawn a runtime while the other test is replacing it.
        let _guard = REAL_E2E_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let runtimes_dir = PathBuf::from(root)
            .join("AIHarnessLauncher")
            .join("runtimes");
        let mgr = Runtimes::new(runtimes_dir.clone());
        if mgr.resolve_version().is_none() {
            eprintln!("no managed runtime installed — skipping real E2E");
            return;
        }
        let adapter = DshAdapter::configured(runtimes_dir, None);
        let settings = AppSettings::default();
        let (_bin, source) = adapter.resolve_bin(&settings).expect("bin resolves");
        assert_eq!(
            source, "managed",
            "E2E must run the managed runtime, not the dev tree"
        );

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
            skins: vec![],
            skin_packages: vec![],
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
            let mut handle = match adapter
                .launch(&settings, &instance, &env, on_log, None)
                .await
            {
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
                    tail.into_iter()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .join("\n")
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
