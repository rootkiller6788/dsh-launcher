//! Managed runtimes — versioned DSH + Node installs under `<root>/runtimes/`.
//!
//! ```text
//! runtimes/
//! ├── active            # plain text: the active DSH version (e.g. 0.1.0-rc.7)
//! ├── node/             # a Node runtime (node/node.exe)
//! └── dsh-<version>/    # a DSH runtime: apps/cli/lib/bin.js at its root
//! ```
//!
//! `install` imports a *working* DSH tree (an existing checkout) by copying it
//! into `runtimes/dsh-<version>/` with `robocopy /E /SL`, which preserves
//! pnpm's junction/symlink forest — the copy is the runtime, a self-contained
//! tree a clean machine can boot from. Later phases replace the import with a
//! pinned-release fetch; the layout and verify contract stay.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

pub const DSHDIR_PREFIX: &str = "dsh-";

/// One installed DSH runtime, as the UI lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEntry {
    pub version: String,
    /// Absolute path to `apps/cli/lib/bin.js`.
    pub bin_path: String,
    pub dir: String,
    pub verified: bool,
}

/// The outcome of a structural verify for a runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub version: String,
    pub node_ok: bool,
    pub node_version: Option<String>,
    pub dsh_ok: bool,
    pub dsh_version: Option<String>,
    pub message: String,
}

/// A Node runtime as reported to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub present: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub struct Runtimes {
    dir: PathBuf,
}

impl Runtimes {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("create runtimes dir {}", self.dir.display()))
    }

    pub fn dsh_dir(&self, version: &str) -> PathBuf {
        self.dir.join(format!("{DSHDIR_PREFIX}{version}"))
    }

    pub fn bin_path(&self, version: &str) -> PathBuf {
        self.dsh_dir(version).join("apps/cli/lib/bin.js")
    }

    /// The managed Node executable (`runtimes/node/node.exe`).
    pub fn managed_node_exe(&self) -> PathBuf {
        self.dir.join("node").join(node_exe_name())
    }

    /// `runtimes/active` — the version string of the active DSH runtime.
    pub fn active_version(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.join("active")).ok()?;
        let v = text.trim().to_string();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    }

    pub fn set_active(&self, version: &str) -> Result<()> {
        self.ensure_dir()?;
        if !self.dsh_dir(version).is_dir() {
            return Err(anyhow!("runtime {version} is not installed"));
        }
        std::fs::write(self.dir.join("active"), format!("{version}\n"))
            .with_context(|| format!("write {}", self.dir.join("active").display()))
    }

    /// Installed DSH runtimes, newest first.
    pub fn list(&self) -> Result<Vec<RuntimeEntry>> {
        self.ensure_dir()?;
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(DSHDIR_PREFIX) {
                continue;
            }
            let version = name[DSHDIR_PREFIX.len()..].to_string();
            let bin = self.bin_path(&version);
            out.push(RuntimeEntry {
                verified: bin.is_file(),
                version,
                bin_path: bin.display().to_string(),
                dir: entry.path().display().to_string(),
            });
        }
        out.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(out)
    }

    /// The runtime `verify`/`detect` would pick when nothing is marked active:
    /// the first installed, newest-first.
    pub fn pick_default_version(&self) -> Option<String> {
        self.list().ok().into_iter().flatten().next().map(|e| e.version)
    }

    /// Resolve which installed version should run: the explicit `active`
    /// pointer (if its bin still exists), else the newest installed.
    pub fn resolve_version(&self) -> Option<String> {
        self.active_version()
            .filter(|v| self.bin_path(v).is_file())
            .or_else(|| self.pick_default_version())
    }

    /// Install a working DSH tree by copying `source` into
    /// `runtimes/dsh-<version>/`. On Windows this is `robocopy /E /SL` so
    /// pnpm's junction/symlink forest survives intact; elsewhere a plain
    /// recursive copy. Fails if that version is already installed.
    pub fn install_from_source(&self, source: &Path, version: Option<&str>) -> Result<RuntimeEntry> {
        self.ensure_dir()?;
        let source = canonicalize_plain(source)?;
        let src_bin = source.join("apps/cli/lib/bin.js");
        if !src_bin.is_file() {
            return Err(anyhow!(
                "{} doesn't look like a DSH tree — no apps/cli/lib/bin.js",
                source.display()
            ));
        }
        let version = match version {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => read_cli_version(&src_bin)
                .ok_or_else(|| anyhow!("can't read a version from {} — pass one explicitly", src_bin.display()))?,
        };
        let dest = self.dsh_dir(&version);
        if dest.exists() {
            let bin = self.bin_path(&version);
            if bin.is_file() {
                return Err(anyhow!("runtime {version} is already installed — remove it first"));
            }
            // Stale/broken install (no runnable bin): replace it on import so a
            // fresh re-import can self-heal instead of blocking on a useless
            // dir. remove_dir_all removes junctions as links (does not follow
            // them), so a pnpm-junction tree — even one with reparse cycles —
            // is safe to drop.
            tracing::warn!(version, "runtime exists but is broken (no bin) — replacing on import");
            std::fs::remove_dir_all(&dest)?;
        }
        copy_tree(&source, &dest)?;
        let bin = self.bin_path(&version);
        if !bin.is_file() {
            // Roll back a partial copy so the manager never advertises a broken dir.
            let _ = std::fs::remove_dir_all(&dest);
            return Err(anyhow!(
                "copy finished but {} is missing — source tree may be incomplete",
                bin.display()
            ));
        }
        Ok(RuntimeEntry {
            verified: true,
            version,
            bin_path: bin.display().to_string(),
            dir: dest.display().to_string(),
        })
    }

    /// Structural verify: the bin exists, and if a node is available its
    /// `--version` runs.
    pub fn verify(&self, version: &str, node: Option<&Path>) -> Result<VerifyReport> {
        let bin = self.bin_path(version);
        let dsh_ok = bin.is_file();
        let mut report = VerifyReport {
            version: version.into(),
            node_ok: false,
            node_version: None,
            dsh_ok,
            dsh_version: if dsh_ok { read_cli_version(&bin) } else { None },
            message: String::new(),
        };
        match node {
            Some(n) if n.is_file() => match node_version(n) {
                Ok(v) => {
                    report.node_ok = true;
                    report.node_version = Some(v);
                }
                Err(e) => report.message = format!("node check failed: {e}"),
            },
            _ => report.message.push_str("no node available for verify"),
        }
        if !dsh_ok {
            report.message = format!("DSH bin missing: {}", bin.display());
        } else if report.node_ok {
            report.message = "ok".into();
        }
        report.message = report.message.trim().to_string();
        Ok(report)
    }

    /// Repair: if the runtime is broken and a source is given, reinstall it;
    /// otherwise re-verify and report what's broken.
    pub fn repair(
        &self,
        version: &str,
        source: Option<&Path>,
        node: Option<&Path>,
    ) -> Result<RuntimeEntry> {
        if self.verify(version, node)?.dsh_ok {
            let bin = self.bin_path(version);
            return Ok(RuntimeEntry {
                version: version.into(),
                bin_path: bin.display().to_string(),
                dir: self.dsh_dir(version).display().to_string(),
                verified: true,
            });
        }
        let Some(src) = source else {
            return Err(anyhow!("{version} is broken — point repair at a valid DSH source tree"));
        };
        let _ = std::fs::remove_dir_all(self.dsh_dir(version));
        self.install_from_source(src, Some(version))
    }

    pub fn remove(&self, version: &str) -> Result<()> {
        let dir = self.dsh_dir(version);
        if !dir.is_dir() {
            return Err(anyhow!("runtime {version} is not installed"));
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        // Keep the active pointer honest.
        if self.active_version().as_deref() == Some(version) {
            let _ = std::fs::remove_file(self.dir.join("active"));
        }
        Ok(())
    }
}

fn node_exe_name() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

/// `std::fs::canonicalize` + strip the `\\?\` prefix Windows adds to the
/// result — robocopy (and Node's resolver) cannot parse an extended-length
/// path. Non-Windows passes through unchanged.
fn canonicalize_plain(p: &Path) -> Result<PathBuf> {
    let canon = p
        .canonicalize()
        .with_context(|| format!("resolve {}", p.display()))?;
    let s = canon.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(stripped) => Ok(PathBuf::from(stripped)),
        None => Ok(canon),
    }
}

/// Run `node --version` (or any `--version` binary) and return the first line.
pub(crate) fn node_version(exe: &Path) -> Result<String> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .map_err(|e| anyhow!("spawn {}: {e}", exe.display()))?;
    if !out.status.success() {
        return Err(anyhow!("`{} --version` exited with {}", exe.display(), out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read the CLI version from `<cli>/package.json` next to the bin, offline.
fn read_cli_version(bin: &Path) -> Option<String> {
    // bin = <cli>/lib/bin.js → package.json at <cli>/package.json
    let pkg = bin.parent()?.parent()?.join("package.json");
    let text = std::fs::read_to_string(pkg).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("version")?.as_str().map(String::from)
}

/// Copy a tree, preserving symlinks. Windows: `robocopy /E /SL` — it handles
/// pnpm's junction forest and is fast (multithreaded, and it does not traverse
/// into the store through links, so no duplication). Other platforms: a plain
/// recursive copy that recreates symlinks.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // /XJ is non-negotiable: robocopy otherwise FOLLOWS junctions (pnpm's
        // node_modules links into the .pnpm store) and materializes the store
        // once per dependent — the copy can even recurse into itself and fill
        // the disk (seen in the field). Copy real files only, then recreate
        // the junctions in a second pass. /MT:16 keeps it fast.
        let status = std::process::Command::new("robocopy")
            .arg(src)
            .arg(dst)
            .args(["/E", "/SL", "/XJ", "/MT:16", "/R:1", "/W:1", "/NFL", "/NDL", "/NP", "/NJH", "/NJS"])
            .arg("/XD")
            .arg(".git")
            .arg(".turbo")
            .arg("target")
            .arg("/XF")
            .arg("*.tsbuildinfo")
            .arg("*.log")
            .status()
            .map_err(|e| anyhow!("failed to run robocopy: {e}"))?;
        // robocopy: 0–7 are success-ish, 8+ means real errors.
        if let Some(code) = status.code() {
            if code >= 8 {
                return Err(anyhow!("robocopy failed with exit code {code}"));
            }
        }
        // Robocopy skipped the junctions; recreate them so the copied tree's
        // node_modules resolve into its own .pnpm store.
        restore_junctions(src, dst)?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(dst)?;
        copy_tree_recursive(src, dst)
    }
}

#[cfg(not(windows))]
fn copy_tree_recursive(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = std::fs::symlink_metadata(&from)?;
        if ft.file_type().is_symlink() {
            let target = std::fs::read_link(&from)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to)?;
            continue;
        }
        if ft.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_tree_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Walk the source tree and recreate every junction (dir symlink) at the same
/// relative path in `dst`. A junction target that points *inside* `src` — pnpm
/// workspace links and the in-tree `.pnpm` store — is remapped to the matching
/// path inside `dst`, so the copied runtime is self-contained: its node_modules
/// resolve against its own copy and work on a clean machine with no source
/// checkout. A target outside `src` (a shared external store) is kept verbatim.
///
/// Creation is batched into a single PowerShell process (script on disk): each
/// per-link spawn costs seconds, and this machine has no symlink privilege, so
/// every link would otherwise take the slow fallback path.
#[cfg(windows)]
fn restore_junctions(src: &Path, dst: &Path) -> Result<()> {
    let mut links: Vec<(PathBuf, PathBuf)> = Vec::new(); // (dest_link, abs_target)
    let mut stack = vec![src.to_path_buf()];
    while let Some(from) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&from) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = std::fs::symlink_metadata(&path) else { continue };
            if ft.file_type().is_symlink() {
                let Ok(target) = std::fs::read_link(&path) else { continue };
                let Some(rel) = path.strip_prefix(src).ok() else { continue };
                let dl = dst.join(rel);
                if let Some(parent) = dl.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let link_dir = path.parent().unwrap_or(src);
                links.push((dl, remap_target(&target, link_dir, src, dst)));
                continue; // never descend into a link target
            }
            if ft.is_dir() {
                stack.push(path);
            }
        }
    }
    if links.is_empty() {
        return Ok(());
    }
    tracing::info!(count = links.len(), "recreating {} junctions", links.len());
    create_junctions_batch(&links)
}

/// Resolve a source junction target to where it must point in `dst`. A relative
/// target resolves against the link's own directory (Windows junction
/// semantics); the resulting in-`src` path becomes the matching path in `dst`.
/// Targets outside `src` stay verbatim.
#[cfg(windows)]
fn remap_target(target: &Path, link_dir: &Path, src: &Path, dst: &Path) -> PathBuf {
    let abs_src = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_dir.join(target)
    };
    match abs_src.strip_prefix(src) {
        Ok(rel) => dst.join(rel),
        Err(_) => abs_src,
    }
}

/// Create every junction in one PowerShell process. The command line caps at
/// ~32K chars, but a .ps1 file can hold thousands of creates, so the script is
/// written to a temp file and run with `-File`. Each create is wrapped in
/// try/catch (`-ErrorAction Stop` makes New-Item's failure a catchable
/// terminating error) so one bad link doesn't abort the rest; failures are
/// counted and reported.
#[cfg(windows)]
fn create_junctions_batch(links: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut script = String::from("$errs = @();\n");
    for (dest, target) in links {
        script.push_str(&format!(
            "try {{ New-Item -ItemType Junction -Path '{}' -Target '{}' -ErrorAction Stop | Out-Null }} catch {{ $errs += '{}' }}\n",
            dest.display(),
            target.display(),
            dest.display()
        ));
    }
    script.push_str("if ($errs.Count) { Write-Error \"$($errs.Count) junction(s) failed to create\"; exit 1 }\n");
    let script_path = std::env::temp_dir().join(format!("dsh-restore-junctions-{}.ps1", std::process::id()));
    std::fs::write(&script_path, script)?;
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script_path)
        .status()
        .map_err(|e| anyhow!("batch junction creation failed: {e}"))?;
    let _ = std::fs::remove_file(&script_path);
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "junction recreation failed (exit {:?}) — {} links requested",
            status.code(),
            links.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("dsh-runtimes-test-{}", std::process::id()))
    }

    fn fake_tree(dir: &Path) {
        let bin = dir.join("apps/cli/lib/bin.js");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "// fake dsh\n").unwrap();
        let pkg = dir.join("apps/cli/package.json");
        std::fs::write(
            &pkg,
            r#"{"name":"@deepseek-ai/dsh","version":"0.1.0-rc.7"}"#,
        )
        .unwrap();
    }

    #[test]
    fn list_empty_dir_is_empty() {
        let dir = tmp().join("empty");
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = Runtimes::new(dir.clone());
        assert!(mgr.list().unwrap().is_empty());
        assert!(mgr.resolve_version().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_reads_version_and_verifies() {
        let dir = tmp().join("install");
        let src = dir.join("src");
        fake_tree(&src);
        let mgr = Runtimes::new(dir.join("runtimes"));
        let entry = mgr.install_from_source(&src, None).unwrap();
        assert_eq!(entry.version, "0.1.0-rc.7");
        assert!(entry.verified);
        assert!(mgr.bin_path("0.1.0-rc.7").is_file());
        // duplicate install refused
        assert!(mgr.install_from_source(&src, None).is_err());
        // active pointer round-trip
        mgr.set_active("0.1.0-rc.7").unwrap();
        assert_eq!(mgr.active_version().as_deref(), Some("0.1.0-rc.7"));
        assert_eq!(mgr.resolve_version().as_deref(), Some("0.1.0-rc.7"));
        // list + verify
        let listed = mgr.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].verified);
        let rep = mgr.verify("0.1.0-rc.7", None).unwrap();
        assert!(rep.dsh_ok);
        assert!(!rep.node_ok);
        assert_eq!(rep.dsh_version.as_deref(), Some("0.1.0-rc.7"));
        // remove clears the active pointer
        mgr.remove("0.1.0-rc.7").unwrap();
        assert!(mgr.resolve_version().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_non_dsh_source() {
        let dir = tmp().join("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = Runtimes::new(dir.join("runtimes"));
        let err = mgr.install_from_source(&dir, None).unwrap_err();
        assert!(err.to_string().contains("apps/cli/lib/bin.js"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A broken/stale install dir (exists but no runnable bin) must be replaced
    /// on import, not block it; a *healthy* existing install must still refuse.
    #[test]
    fn install_replaces_broken_existing_dir() {
        let dir = tmp().join("replace");
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fake_tree(&src);
        let mgr = Runtimes::new(dir.join("runtimes"));
        assert!(mgr.install_from_source(&src, None).unwrap().verified);

        let bin = mgr.bin_path("0.1.0-rc.7");
        assert!(bin.is_file());
        // Break it: delete the bin, leaving an unusable stub.
        std::fs::remove_file(&bin).unwrap();
        // A second import must self-heal (replace), not error.
        assert!(mgr.install_from_source(&src, None).unwrap().verified);
        assert!(bin.is_file(), "replaced install must have the bin again");
        // A healthy install must still refuse.
        assert!(mgr.install_from_source(&src, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// remove_dir_all must not follow junctions: a reparse cycle (a/b -> b,
    /// b/a -> a) must not hang or recurse through the link targets.
    #[cfg(windows)]
    #[test]
    fn remove_dir_all_breaks_junction_cycles() {
        let dir = tmp().join("cycle");
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        make_junction(&a.join("b"), &b);
        make_junction(&b.join("a"), &a);
        std::fs::remove_dir_all(&a).unwrap();
        assert!(!a.exists(), "a must be removed");
        assert!(b.is_dir(), "b (junction target) must survive — links are removed, not followed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_reports_missing_bin() {
        let dir = tmp().join("missing");
        let _ = std::fs::create_dir_all(&dir);
        let mgr = Runtimes::new(dir.clone());
        let rep = mgr.verify("9.9.9", None).unwrap();
        assert!(!rep.dsh_ok);
        assert!(rep.message.contains("missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Create a real junction without SeCreateSymbolicLinkPrivilege, using the
    /// same inline PowerShell New-Item script as `create_junction`'s fallback
    /// (single-quoted paths — deterministic, no $args, no cmd /C re-parsing).
    #[cfg(windows)]
    fn make_junction(link: &std::path::Path, target: &std::path::Path) {
        let script = format!(
            "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
            link.display(),
            target.display()
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .expect("spawn powershell");
        assert!(
            out.status.success(),
            "make_junction failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Regression for the disk-full bug: robocopy must NOT follow junctions
    /// (which materializes the .pnpm store once per dependent), and the
    /// recreated junction must be a link again, not a materialized copy.
    #[cfg(windows)]
    #[test]
    fn copy_tree_recreates_junctions_without_following() {
        let dir = tmp().join("junctions");
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        let store = src.join("store/pkg");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("index.js"), "// pkg\n").unwrap();
        // pnpm-style junction: node_modules/pkg -> <abs>/src/store/pkg.
        let link = src.join("node_modules/pkg");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        make_junction(&link, &store);

        let dst = dir.join("dst");
        copy_tree(&src, &dst).unwrap();

        // The junction was recreated, not materialized as a real copy.
        let dl = dst.join("node_modules/pkg");
        assert!(
            std::fs::symlink_metadata(&dl).map(|m| m.file_type().is_symlink()).unwrap_or(false),
            "dest junction missing: {}",
            dl.display()
        );
        // And it resolves through the dest's OWN store copy: the recreated
        // junction's target is remapped inside dst, never back at the source.
        assert!(dl.join("index.js").is_file(), "junction target not reachable via dest");
        assert!(dst.join("store/pkg/index.js").is_file(), "store not copied");
        let recreated = std::fs::read_link(&dl).expect("read recreated junction target");
        assert!(
            recreated.starts_with(&dst),
            "junction must point into the copied tree (dst), got: {}",
            recreated.display()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
