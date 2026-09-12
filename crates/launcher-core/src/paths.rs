use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use directories::BaseDirs;

/// The on-disk layout for the whole launcher.
///
/// The data root resolves in this priority order:
/// 1. `AHL_HOME` env — an explicit override (dev/testing), never "portable".
/// 2. `AHL_PORTABLE` env set to a truthy value — data root = the exe's own dir.
/// 3. A `portable` / `.portable` marker file next to the exe — the green-edition
///    toggle: drop the exe in any folder, add an empty `portable` file beside it,
///    and every byte (runtimes, instances, settings, cache, logs, DB) stays in
///    that folder so the whole thing is movable on a USB stick.
/// 4. Default: `%LOCALAPPDATA%/AIHarnessLauncher` (Windows).
///
/// In portable mode the root IS the exe directory, and — because Tauri's
/// `resource_dir()` is also the exe's own directory on Windows — a bundled
/// `node/` next to the exe and the managed `runtimes/` under root naturally
/// sit side by side in the same folder.
///
/// ```text
/// <root>/
/// ├── settings.json       # app settings (dshPath override, lastInstance)
/// ├── providers.json      # provider metadata — NEVER the API key
/// ├── crash-signatures.json  # optional user crash rules (see dsh-adapter::crash)
/// ├── runtimes/           # managed runtimes
/// ├── instances/default/  # the (single) instance: instance.json + workspace/
/// ├── cache/
/// └── logs/launcher.log
/// ```
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    /// `true` when the data root lives next to the exe (env/marker portable mode)
    /// rather than the per-user app-data dir. UI shows a green-edition badge.
    pub portable: bool,
    pub settings: PathBuf,
    pub providers: PathBuf,
    /// User-editable crash signatures, merged under the built-in rule table.
    /// Absent by default; a missing file is the normal case, not an error.
    pub crash_signatures: PathBuf,
    pub runtimes: PathBuf,
    pub instances: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub launcher_log: PathBuf,
}

impl AppPaths {
    /// Canonical layout for a data root: every path the launcher owns hangs off
    /// `root`. The runtime resolver and the test helpers both build through here
    /// so the two layouts can never drift apart.
    pub fn rooted_at(root: PathBuf, portable: bool) -> Self {
        Self {
            settings: root.join("settings.json"),
            providers: root.join("providers.json"),
            crash_signatures: root.join("crash-signatures.json"),
            runtimes: root.join("runtimes"),
            instances: root.join("instances"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            launcher_log: root.join("logs").join("launcher.log"),
            root,
            portable,
        }
    }

    pub fn from_env() -> Result<Self> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()));
        let ahl_home = std::env::var("AHL_HOME").ok();
        let ahl_portable = std::env::var("AHL_PORTABLE").ok();
        let (base, portable) = resolve_root(
            exe_dir.as_deref(),
            ahl_home.as_deref(),
            ahl_portable.as_deref(),
        )
        .ok_or_else(|| anyhow!("cannot resolve base directories"))?;
        Ok(Self::rooted_at(base, portable))
    }

    /// Idempotently create every directory the launcher owns.
    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [
            &self.root,
            &self.runtimes,
            &self.instances,
            &self.cache,
            &self.logs,
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn default_instance_file(&self) -> PathBuf {
        self.instance_file("default")
    }

    /// Directory for an instance's whole tree (`instances/<id>/`).
    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances.join(id)
    }

    /// The instance's manifest file (`instances/<id>/instance.json`).
    pub fn instance_file(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("instance.json")
    }

    /// Per-MCP runtime dir under an instance (`instances/<id>/mcp/<server>/`),
    /// the Phase-2 home for a server's `runtime.json` and `logs/`. `server` is
    /// sanitized into a filesystem-safe segment (`owner/name` → `owner-name`).
    pub fn mcp_dir(&self, id: &str, server: &str) -> PathBuf {
        self.instance_dir(id)
            .join("mcp")
            .join(sanitize_mcp_segment(server))
    }

    /// A server's persisted health snapshot (`instances/<id>/mcp/<server>/runtime.json`).
    pub fn mcp_runtime_file(&self, id: &str, server: &str) -> PathBuf {
        self.mcp_dir(id, server).join("runtime.json")
    }

    /// A server's most recent health-check transcript (`…/mcp/<server>/logs/last.log`).
    pub fn mcp_log_file(&self, id: &str, server: &str) -> PathBuf {
        self.mcp_dir(id, server).join("logs").join("last.log")
    }

    /// An instance's rescue-point dir (`instances/<id>/rescue/`), holding the
    /// pre-change copy of the profile files the launcher mutates. One rescue
    /// point per instance — a new snapshot overwrites the previous one.
    pub fn rescue_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("rescue")
    }

    /// The SQLite file for launch history / index (`root/launcher.db`).
    pub fn db_file(&self) -> PathBuf {
        self.root.join("launcher.db")
    }

    /// The spawned-PID ledger (`root/spawned.pids`). Written on every launch,
    /// swept on the next one, so a hard-killed launcher can't leave an orphan
    /// harness tree behind.
    pub fn pid_ledger(&self) -> PathBuf {
        self.root.join("spawned.pids")
    }
}

/// Resolve the data root from explicit inputs (pure — no ambient env reads, so
/// tests can drive every branch). Returns `(root, portable)`.
///
/// Priority: `AHL_HOME` override → `AHL_PORTABLE` truthy → `portable` marker
/// beside the exe → the per-user app-data dir. `None` only when the fallback
/// needs `BaseDirs` and that fails.
fn resolve_root(
    exe_dir: Option<&Path>,
    ahl_home: Option<&str>,
    ahl_portable: Option<&str>,
) -> Option<(PathBuf, bool)> {
    if let Some(home) = ahl_home.filter(|h| !h.trim().is_empty()) {
        // An explicit AHL_HOME is a deliberate relocation — never portable.
        return Some((PathBuf::from(home), false));
    }
    if ahl_portable.is_some_and(is_truthy) {
        if let Some(dir) = exe_dir {
            return Some((dir.to_path_buf(), true));
        }
    }
    if let Some(dir) = exe_dir.filter(|d| portable_marker(d)) {
        return Some((dir.to_path_buf(), true));
    }
    let dirs = BaseDirs::new()?;
    Some((dirs.data_local_dir().join("AIHarnessLauncher"), false))
}

/// `true` when a `portable` or `.portable` marker file sits directly in `dir`.
fn portable_marker(dir: &Path) -> bool {
    dir.join("portable").is_file() || dir.join(".portable").is_file()
}

/// Truthy = non-empty and not one of `0`/`false`/`no`/`off` (case-insensitive).
fn is_truthy(v: &str) -> bool {
    !matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// Filesystem-safe folder segment for an MCP server identity. Catalog keys
/// carry a `/` (`owner/name`) which would nest directories; keep `[A-Za-z0-9._-]`,
/// collapse everything else to single `-`, trim, cap at 48 chars, `server` fallback.
pub fn sanitize_mcp_segment(server: &str) -> String {
    let mut out = String::with_capacity(server.len());
    let mut prev_sep = false;
    for c in server.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            out.push(c);
            prev_sep = false;
        } else if !prev_sep && !out.is_empty() {
            out.push('-');
            prev_sep = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.len() > 48 {
        let capped = out[..48].trim_end_matches('-').to_string();
        if capped.is_empty() {
            return "server".into();
        }
        return capped;
    }
    if out.is_empty() || out == "." || out == ".." {
        // `.`/`..` are reserved directory names (parent traversal) on Windows.
        "server".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway temp dir, cleaned by Rust's `remove_dir_all` (never shell
    /// tools). Unique per tag + pid so parallel test binaries don't collide.
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-paths-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    #[test]
    fn resolve_root_prefers_ahl_home_over_portable() {
        let exe_dir = tmp_dir("home-vs-portable");
        let home = tmp_dir("home-target");
        let (root, portable) =
            resolve_root(Some(&exe_dir), Some(home.to_str().unwrap()), Some("1")).unwrap();
        assert_eq!(root, home);
        assert!(
            !portable,
            "AHL_HOME is an explicit relocation, never portable"
        );
        let _ = std::fs::remove_dir_all(&exe_dir);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_root_portable_env_uses_exe_dir() {
        let exe_dir = tmp_dir("env-portable");
        let (root, portable) = resolve_root(Some(&exe_dir), None, Some("1")).unwrap();
        assert_eq!(root, exe_dir);
        assert!(portable);
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn resolve_root_marker_file_uses_exe_dir() {
        // Plain `portable` file hits first.
        let exe_dir = tmp_dir("marker");
        std::fs::write(exe_dir.join("portable"), "").unwrap();
        let (root, portable) = resolve_root(Some(&exe_dir), None, None).unwrap();
        assert_eq!(root, exe_dir);
        assert!(portable);
        let _ = std::fs::remove_dir_all(&exe_dir);

        // Dot variant also counts.
        let dot_dir = tmp_dir("dot-marker");
        std::fs::write(dot_dir.join(".portable"), "").unwrap();
        let (root, portable) = resolve_root(Some(&dot_dir), None, None).unwrap();
        assert_eq!(root, dot_dir);
        assert!(portable);
        let _ = std::fs::remove_dir_all(&dot_dir);
    }

    #[test]
    fn resolve_root_defaults_when_nothing() {
        let exe_dir = tmp_dir("default");
        let (root, portable) = resolve_root(Some(&exe_dir), None, None).unwrap();
        assert!(!portable);
        assert!(
            root.ends_with("AIHarnessLauncher"),
            "default root must be the per-user app-data dir, got {root:?}"
        );
        assert_ne!(root, exe_dir);
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn resolve_root_ignores_falsy_portable() {
        let exe_dir = tmp_dir("falsy");
        for value in ["0", "false", "no", "off", "FALSE", " 0 "] {
            let (root, portable) = resolve_root(Some(&exe_dir), None, Some(value)).unwrap();
            assert!(!portable, "AHL_PORTABLE={value:?} must be treated as falsy");
            assert!(
                root.ends_with("AIHarnessLauncher"),
                "falsy portable must fall back to app-data, got {root:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn is_truthy_accepts_1_yes_on_true() {
        for value in ["1", "yes", "on", "true", "TRUE", "Yes"] {
            assert!(is_truthy(value), "{value:?} should be truthy");
        }
    }

    #[test]
    fn mcp_runtime_paths_are_safe_and_scoped() {
        let paths = AppPaths {
            root: tmp_dir("mcp-dirs"),
            portable: false,
            settings: "s".into(),
            providers: "p".into(),
            crash_signatures: "cs".into(),
            runtimes: "r".into(),
            instances: "i".into(),
            cache: "c".into(),
            logs: "l".into(),
            launcher_log: "ll".into(),
        };
        let dir = paths.mcp_dir("default", "owner/server-git");
        let want = paths.instances.join("default").join("mcp").join("owner-server-git");
        assert_eq!(dir, want, "slash in identity must flatten to one segment");
        assert_eq!(
            paths.mcp_runtime_file("default", "owner/server-git"),
            dir.join("runtime.json")
        );
        assert_eq!(
            paths.mcp_log_file("default", "owner/server-git"),
            dir.join("logs").join("last.log")
        );
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn sanitize_mcp_segment_handles_hostile_input() {
        assert_eq!(sanitize_mcp_segment("owner/name"), "owner-name");
        assert_eq!(sanitize_mcp_segment("a::b"), "a-b");
        assert_eq!(sanitize_mcp_segment("@scope/pkg"), "scope-pkg", "leading separators dropped");
        assert_eq!(sanitize_mcp_segment(".."), "server", "only separators → fallback");
        assert_eq!(sanitize_mcp_segment("a"), "a");
        let long = "n".repeat(80);
        assert_eq!(sanitize_mcp_segment(&long).len(), 48);
    }
}
