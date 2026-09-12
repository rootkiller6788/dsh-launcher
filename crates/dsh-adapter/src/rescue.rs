//! Rescue-point snapshot & restore for an instance's DSH profile.
//!
//! The launcher mutates a small set of profile files to install/toggle plugins,
//! skins and MCP servers (see `content.rs` / `lib.rs` patch compilation). A bad
//! change to any of them can brick the next boot, so every destructive action
//! should first snapshot the files it is about to touch — a "rescue point" —
//! and offer a one-step restore. Ported from `3/zat`'s `createRescueSnapshot`
//! (`src/rescue.js`), which keeps a *single latest* rescue point per terminal:
//! the same model here, one rescue point per instance.
//!
//! The files captured are the exact ones the launcher writes, nothing more:
//!
//! | rescue name                | live path (per instance)                     |
//! |----------------------------|----------------------------------------------|
//! | `package.json`             | `profiles/<profile>/package.json`            |
//! | `profile-cordis.patch.yml` | `profiles/<profile>/cordis.patch.yml`        |
//! | `home-cordis.patch.yml`    | `$DSH_HOME/cordis.patch.yml` (workspace)     |
//!
//! `cordis.yml` and `pnpm-workspace.yaml` — which `3/zat` also snapshots — are
//! deliberately absent: the source-checkout `dsh web` AHL runs never reads them
//! (see `docs/dsh-contract-inventory.md` 附一, "已核实不存在").

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use launcher_core::InstanceManifest;
use serde::{Deserialize, Serialize};

use crate::DshAdapter;

/// Metadata file name inside a rescue dir.
pub const SNAPSHOT_META: &str = "snapshot.json";

/// One profile file a rescue snapshot captures. `source` is the live path —
/// copied on snapshot, overwritten on restore. `name` is the file name inside
/// the rescue dir (the two `cordis.patch.yml` layers need distinct names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueFile {
    pub source: PathBuf,
    pub name: &'static str,
}

/// The profile files the launcher mutates that can brick a boot.
pub fn rescue_files(instance: &InstanceManifest) -> Vec<RescueFile> {
    let profile = DshAdapter::profile_dir(instance);
    let workspace = PathBuf::from(&instance.workspace);
    vec![
        RescueFile {
            source: profile.join("package.json"),
            name: "package.json",
        },
        RescueFile {
            source: profile.join("cordis.patch.yml"),
            name: "profile-cordis.patch.yml",
        },
        RescueFile {
            source: workspace.join("cordis.patch.yml"),
            name: "home-cordis.patch.yml",
        },
    ]
}

/// Metadata written as `snapshot.json` inside a rescue dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RescueSnapshotMeta {
    /// Epoch milliseconds when the snapshot was taken.
    pub at: u64,
    /// The `RescueFile::name` values actually captured.
    pub files: Vec<String>,
}

/// Whether a rescue point exists, and (if so) when it was taken and what it holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RescueStatus {
    pub exists: bool,
    #[serde(default)]
    pub at: u64,
    #[serde(default)]
    pub files: Vec<String>,
}

/// Copy the live profile files into `rescue_dir`, overwriting any prior rescue
/// point, and write `snapshot.json`. Fails when nothing is present to snapshot.
pub fn create_snapshot(files: &[RescueFile], rescue_dir: &Path) -> Result<RescueSnapshotMeta> {
    let captured: Vec<&RescueFile> = files.iter().filter(|f| f.source.is_file()).collect();
    if captured.is_empty() {
        bail!("profile has no rescue-able files to snapshot");
    }
    std::fs::create_dir_all(rescue_dir)
        .with_context(|| format!("create rescue dir {}", rescue_dir.display()))?;
    for f in &captured {
        std::fs::copy(&f.source, rescue_dir.join(f.name))
            .with_context(|| format!("snapshot {}", f.source.display()))?;
    }
    let meta = RescueSnapshotMeta {
        at: now_millis(),
        files: captured.iter().map(|f| f.name.to_string()).collect(),
    };
    write_meta(rescue_dir, &meta)?;
    Ok(meta)
}

/// Take a rescue point only when one does not already exist.
///
/// This is the one to call *before* a destructive change, and the asymmetry with
/// [`create_snapshot`] is deliberate. A rescue point is only worth restoring if
/// it holds a state that was known to boot, so the pair of rules is:
///
/// - refresh the point after every successful boot ([`create_snapshot`]), and
/// - create one before a change only if none exists yet ([`snapshot_if_absent`]).
///
/// If this overwrote instead, the *second* change made without an intervening
/// boot would capture the already-damaged files and destroy the only good copy —
/// exactly when the user needs it. Returns `Ok(None)` when a point was already
/// present (nothing written).
pub fn snapshot_if_absent(
    files: &[RescueFile],
    rescue_dir: &Path,
) -> Result<Option<RescueSnapshotMeta>> {
    if snapshot_status(rescue_dir).exists {
        return Ok(None);
    }
    create_snapshot(files, rescue_dir).map(Some)
}

/// Copy the rescue point back over the live profile files. Fails when there is
/// no rescue point (or its metadata is unreadable). Returns the restored
/// snapshot's metadata.
pub fn restore_snapshot(files: &[RescueFile], rescue_dir: &Path) -> Result<RescueSnapshotMeta> {
    let meta = read_meta(rescue_dir)?.context("no rescue point — create one first")?;
    let mut restored = 0usize;
    for f in files {
        let src = rescue_dir.join(f.name);
        if !src.is_file() {
            continue;
        }
        if let Some(parent) = f.source.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&src, &f.source).with_context(|| format!("restore {}", f.source.display()))?;
        restored += 1;
    }
    if restored == 0 {
        bail!("rescue point has no files to restore");
    }
    Ok(meta)
}

/// Report whether a rescue point exists. Corrupt metadata degrades to "absent"
/// rather than failing the caller.
pub fn snapshot_status(rescue_dir: &Path) -> RescueStatus {
    match read_meta(rescue_dir).ok().flatten() {
        Some(meta) => RescueStatus {
            exists: true,
            at: meta.at,
            files: meta.files,
        },
        None => RescueStatus {
            exists: false,
            at: 0,
            files: Vec::new(),
        },
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn read_meta(rescue_dir: &Path) -> Result<Option<RescueSnapshotMeta>> {
    let path = rescue_dir.join(SNAPSHOT_META);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("rescue point metadata {} is corrupt", path.display()))
}

fn write_meta(rescue_dir: &Path, meta: &RescueSnapshotMeta) -> Result<()> {
    let json = serde_json::to_string_pretty(meta).context("serialize rescue metadata")?;
    std::fs::write(rescue_dir.join(SNAPSHOT_META), format!("{json}\n"))
        .context("write rescue metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway temp dir, unique per tag + pid so parallel test binaries
    /// don't collide (matches the convention in `paths.rs` / `diagnostics.rs`).
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-rescue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    /// A minimal profile tree with `package.json` and both patch layers, so the
    /// full rescue file set is present.
    fn profile_tree(tag: &str) -> (PathBuf, Vec<RescueFile>) {
        let workspace = tmp_dir(tag);
        let profile = workspace.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("package.json"), r#"{"dsh":{"profile":{"bundles":[]}}}"#).unwrap();
        std::fs::write(profile.join("cordis.patch.yml"), "- id: a\n").unwrap();
        std::fs::write(workspace.join("cordis.patch.yml"), "- id: home-a\n").unwrap();
        let files = vec![
            RescueFile {
                source: profile.join("package.json"),
                name: "package.json",
            },
            RescueFile {
                source: profile.join("cordis.patch.yml"),
                name: "profile-cordis.patch.yml",
            },
            RescueFile {
                source: workspace.join("cordis.patch.yml"),
                name: "home-cordis.patch.yml",
            },
        ];
        (workspace, files)
    }

    #[test]
    fn create_and_restore_round_trips_all_three_files() {
        let (workspace, files) = profile_tree("roundtrip");
        let rescue_dir = workspace.join("rescue");

        let meta = create_snapshot(&files, &rescue_dir).unwrap();
        assert_eq!(
            meta.files,
            vec!["package.json", "profile-cordis.patch.yml", "home-cordis.patch.yml"]
        );
        assert!(rescue_dir.join("package.json").is_file());

        // Corrupt all three live files, then restore.
        for f in &files {
            std::fs::write(&f.source, "corrupted").unwrap();
        }
        let restored = restore_snapshot(&files, &rescue_dir).unwrap();
        assert_eq!(restored.at, meta.at);

        assert_eq!(
            std::fs::read_to_string(files[0].source.clone()).unwrap(),
            r#"{"dsh":{"profile":{"bundles":[]}}}"#
        );
        assert_eq!(
            std::fs::read_to_string(files[1].source.clone()).unwrap(),
            "- id: a\n"
        );
        assert_eq!(
            std::fs::read_to_string(files[2].source.clone()).unwrap(),
            "- id: home-a\n"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn create_snapshot_fails_when_nothing_to_snapshot() {
        let workspace = tmp_dir("empty");
        let files = vec![RescueFile {
            source: workspace.join("profiles").join("web").join("package.json"),
            name: "package.json",
        }];
        assert!(create_snapshot(&files, &workspace.join("rescue")).is_err());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn snapshot_if_absent_captures_once_and_never_overwrites() {
        let (workspace, files) = profile_tree("if-absent");
        let rescue_dir = workspace.join("rescue");

        // First change: no point yet → one is taken.
        let first = snapshot_if_absent(&files, &rescue_dir).unwrap();
        assert!(first.is_some());

        // The change lands (files are now modified).
        std::fs::write(&files[0].source, "modified-by-first-change").unwrap();

        // Second change with no intervening boot: the point already holds the
        // last known-good state and must survive untouched. Overwriting here
        // would capture the damage and destroy the only recoverable copy.
        assert!(snapshot_if_absent(&files, &rescue_dir).unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(rescue_dir.join("package.json")).unwrap(),
            r#"{"dsh":{"profile":{"bundles":[]}}}"#
        );

        // So restoring still gets back the pre-first-change state.
        restore_snapshot(&files, &rescue_dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&files[0].source).unwrap(),
            r#"{"dsh":{"profile":{"bundles":[]}}}"#
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn a_good_boot_refreshes_the_point_over_a_stale_one() {
        let (workspace, files) = profile_tree("refresh");
        let rescue_dir = workspace.join("rescue");

        snapshot_if_absent(&files, &rescue_dir).unwrap();
        // A change lands, then the instance boots successfully → the new state
        // is known-good and becomes the point to restore to.
        std::fs::write(&files[0].source, "known-good-after-boot").unwrap();
        create_snapshot(&files, &rescue_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(rescue_dir.join("package.json")).unwrap(),
            "known-good-after-boot"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn restore_snapshot_fails_without_a_snapshot() {
        let (workspace, files) = profile_tree("no-snapshot");
        assert!(restore_snapshot(&files, &workspace.join("rescue")).is_err());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn status_reflects_existence_and_corruption() {
        let (workspace, files) = profile_tree("status");
        let rescue_dir = workspace.join("rescue");

        assert!(!snapshot_status(&rescue_dir).exists);

        create_snapshot(&files, &rescue_dir).unwrap();
        let status = snapshot_status(&rescue_dir);
        assert!(status.exists);
        assert_eq!(status.files.len(), 3);

        // Corrupt metadata degrades to "absent" (status is read-only).
        std::fs::write(rescue_dir.join(SNAPSHOT_META), "not json").unwrap();
        assert!(!snapshot_status(&rescue_dir).exists);

        // But restore refuses to silently no-op on the same corruption.
        assert!(restore_snapshot(&files, &rescue_dir).is_err());
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn rescue_files_points_at_the_three_mutated_locations() {
        let workspace = tmp_dir("paths");
        let instance = InstanceManifest {
            id: "r".into(),
            name: "R".into(),
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
            workspace: workspace.display().to_string(),
        };
        let files = rescue_files(&instance);
        let names: Vec<&str> = files.iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            vec!["package.json", "profile-cordis.patch.yml", "home-cordis.patch.yml"]
        );
        assert_eq!(files[1].source, workspace.join("profiles").join("web").join("cordis.patch.yml"));
        assert_eq!(files[2].source, workspace.join("cordis.patch.yml"));
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
