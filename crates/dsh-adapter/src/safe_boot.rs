//! Safe-boot profile — a scratch profile dsh boots *instead of* the user's own.
//!
//! Ported from `1/`'s safe mode (`SafeProfileBuilder` + `BootRecoveryPolicy`),
//! reduced to what AHL can honestly do. The problem it solves: one bad plugin or
//! one bad patch row can keep the user's profile from ever reaching a usable UI,
//! and by then the only surface left to fix it from is the launcher — which
//! cannot guess which row is the bad one.
//!
//! Three rules this module holds to:
//!
//! - **The user's profile is never touched.** The safe profile is a *different*
//!   directory ([`safe_profile_dir`]) holding a single generated `package.json`.
//!   Nothing here opens the user's `package.json`, `cordis.patch.yml` or
//!   `pnpm-workspace.yaml` for writing, so whatever safe mode does, the user's
//!   own profile is still exactly what dsh boots once safe mode is left.
//! - **Only a manifest is written.** Bundles, tiers, nothing else — a generated
//!   `cordis.patch.yml` would be AHL inventing patch rows for dsh to apply.
//!   Verified against the shipped runtime (`app-boot` 0.1.0-rc.7): `loadProfile`
//!   reads the profile patch layer as `existsSync(patchPath) ? load : []`, so a
//!   *missing* file means "no overlay", while a *present* file that fails to
//!   parse throws. Writing nothing is the branch with nothing to get wrong.
//! - **Disabled is not deleted.** Every bundle the user had stays in their
//!   profile; a tier only decides what *this boot* resolves. Leaving safe mode
//!   needs no undo, because nothing was done.
//!
//! `--profile .ahl-safe` is what makes any of this authoritative: it is dsh's
//! own flag, so the profile dsh reads is the one AHL prepared — and dsh still
//! gets the last word on whether that profile composes.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use launcher_core::InstanceManifest;
use serde::{Deserialize, Serialize};

use crate::DshAdapter;

/// Name of the scratch profile. Leading dot keeps it beside dsh's own
/// `profiles/` convention without ever colliding with a profile a user typed;
/// dsh's name validation allows it (it rejects only `''`, `/`, `\`, `.`, `..`
/// and `node_modules`).
pub const SAFE_PROFILE_NAME: &str = ".ahl-safe";

/// The smallest bundle set that still serves the web UI: dsh's own `web`
/// template (`PROFILE_TEMPLATES.web.bundles`), used verbatim. Both resolve from
/// the installation anchor, so no `pnpm install` is needed for a hand-written
/// manifest.
pub const MINIMAL_BUNDLES: [&str; 2] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

/// The scope AHL treats as "shipped by DSH, not by a user". Everything outside
/// it is a candidate for the cause of a broken boot, and the reason Tier 1 can
/// drop it without pretending to know which package is at fault.
const FIRST_PARTY_PREFIX: &str = "@deepseek-ai/";

/// Which rung of the recovery ladder a boot is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SafeTier {
    /// L1 — every first-party bundle the user's profile lists, plus the minimal
    /// pair. Keeps their first-party UI additions; drops everything else.
    Plugins,
    /// L2 — the minimal pair only. The end of the ladder: if dsh cannot boot
    /// this, nothing AHL generated was the cause, and the honest move is to
    /// say so rather than escalate further.
    Minimal,
}

impl SafeTier {
    /// The next, narrower tier — the ladder's only allowed direction.
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Plugins => Some(Self::Minimal),
            Self::Minimal => None,
        }
    }
}

/// What a safe-boot write did: the tier, what it will resolve, and what the
/// user's profile listed that this tier leaves out. `dropped` is what the UI
/// owes the user in words — "these are not loaded right now", never "these are
/// disabled", because nothing in their profile changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeBundlePlan {
    pub tier: SafeTier,
    pub bundles: Vec<String>,
    pub dropped: Vec<String>,
}

/// The user's own bundle list, in order, deduped, blank entries ignored.
/// A missing or unreadable profile yields an empty list rather than an error:
/// safe mode exists for exactly the case where reading the profile is the
/// thing that failed.
pub fn user_bundles(manifest: Option<&serde_json::Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(list) = manifest
        .and_then(|m| m.pointer("/dsh/profile/bundles"))
        .and_then(|b| b.as_array())
    else {
        return out;
    };
    for name in list.iter().filter_map(|v| v.as_str()) {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|e| e == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Which bundles a boot at `tier` should resolve.
///
/// Tier 1 keeps first-party rows in the user's own order and force-inserts the
/// minimal pair (so a profile that had somehow lost `dsh-web-app` still gets a
/// UI). Note what it does *not* do: it never asserts that a first-party row is
/// resolvable from the safe profile. A first-party-scoped plugin installed from
/// npm lives in the user's profile `node_modules`, which the safe profile does
/// not have — dsh says so itself ("cannot resolve profile bundle …"), and that
/// verdict is what moves the ladder to Tier 2.
pub fn plan_bundles(manifest: Option<&serde_json::Value>, tier: SafeTier) -> SafeBundlePlan {
    let user = user_bundles(manifest);
    let bundles: Vec<String> = match tier {
        SafeTier::Plugins => {
            let mut out: Vec<String> = user
                .iter()
                .filter(|n| n.starts_with(FIRST_PARTY_PREFIX))
                .cloned()
                .collect();
            for name in MINIMAL_BUNDLES {
                if !out.iter().any(|e| e == name) {
                    out.push(name.to_string());
                }
            }
            out
        }
        SafeTier::Minimal => MINIMAL_BUNDLES.iter().map(|s| s.to_string()).collect(),
    };
    let dropped = user.into_iter().filter(|n| !bundles.contains(n)).collect();
    SafeBundlePlan {
        tier,
        bundles,
        dropped,
    }
}

/// `$DSH_HOME/profiles/.ahl-safe` for this instance — a sibling of the user's
/// own profile directory, never a parent or child of it.
pub fn safe_profile_dir(instance: &InstanceManifest) -> PathBuf {
    PathBuf::from(&instance.workspace)
        .join("profiles")
        .join(SAFE_PROFILE_NAME)
}

/// Write the safe profile's manifest for `tier`, overwriting any previous safe
/// profile, and report what it will resolve.
///
/// The write is a temp-file rename so a boot racing a rewrite never reads half
/// a manifest. Returns the plan so the caller can log and show it — the file
/// alone cannot say which bundles were left out.
pub fn write_safe_profile(instance: &InstanceManifest, tier: SafeTier) -> Result<SafeBundlePlan> {
    let user_manifest = DshAdapter::read_profile_manifest(instance);
    let plan = plan_bundles(user_manifest.as_ref(), tier);

    let dir = safe_profile_dir(instance);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create safe profile dir {}", dir.display()))?;

    // Shape mirrors what dsh's own `initProfile` writes, minus `dependencies`:
    // with no dependencies there is nothing for pnpm to install, and the
    // bundles resolve from the installation anchor.
    let manifest = serde_json::json!({
        "name": SAFE_PROFILE_NAME,
        "private": true,
        "dsh": { "profile": { "bundles": &plan.bundles } },
    });
    let body =
        serde_json::to_string_pretty(&manifest).context("serialize the safe profile manifest")?;

    let path = dir.join("package.json");
    let tmp = dir.join("package.json.ahl-tmp");
    std::fs::write(&tmp, body.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("install {}", path.display()))?;
    Ok(plan)
}

/// Remove the safe profile directory, returning the boot to the user's own
/// profile. A missing directory is `Ok` — leaving safe mode must not depend on
/// safe mode ever having run.
pub fn remove_safe_profile(instance: &InstanceManifest) -> Result<()> {
    let dir = safe_profile_dir(instance);
    // Guard rather than trust: the only thing this function may ever delete is
    // the directory whose final component is exactly the safe profile name.
    if dir.file_name().and_then(|n| n.to_str()) != Some(SAFE_PROFILE_NAME) {
        bail!("refusing to remove {}", dir.display());
    }
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A throwaway temp dir, unique per tag + pid so parallel test binaries
    /// don't collide (matches the convention in `rescue.rs` / `paths.rs`).
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-safe-boot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    /// An instance rooted at `workspace`, with the default `web` profile.
    fn manifest(workspace: &Path) -> InstanceManifest {
        InstanceManifest {
            id: "s".into(),
            name: "S".into(),
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
        }
    }

    fn user_manifest(bundles: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "dsh": { "profile": { "bundles": bundles } } })
    }

    #[test]
    fn tier_one_keeps_first_party_and_forces_the_minimal_pair() {
        let m = user_manifest(serde_json::json!([
            "@deepseek-ai/dsh-base",
            "third-party/evil",
            "@deepseek-ai/dsh-things"
        ]));
        let plan = plan_bundles(Some(&m), SafeTier::Plugins);
        // User order preserved, the missing half of the minimal pair appended.
        assert_eq!(
            plan.bundles,
            vec![
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-things",
                "@deepseek-ai/dsh-web-app"
            ]
        );
        assert_eq!(plan.dropped, vec!["third-party/evil"]);
    }

    #[test]
    fn tier_two_is_only_the_minimal_pair() {
        let m = user_manifest(serde_json::json!([
            "@deepseek-ai/dsh-base",
            "@deepseek-ai/dsh-things"
        ]));
        let plan = plan_bundles(Some(&m), SafeTier::Minimal);
        assert_eq!(plan.bundles.to_vec(), MINIMAL_BUNDLES.to_vec());
        // `dropped` is "the user's rows this boot does not resolve", not "the
        // rows Tier 2 excludes": `dsh-base` is in the pair, so it is not
        // dropped, and saying otherwise would overstate what the user loses.
        assert_eq!(plan.dropped, vec!["@deepseek-ai/dsh-things"]);
    }

    #[test]
    fn a_missing_or_malformed_profile_still_boots_the_minimal_pair() {
        // Reading the profile is the thing that failed — that must not fail
        // safe mode. Both tiers degrade to the pair with nothing dropped.
        for tier in [SafeTier::Plugins, SafeTier::Minimal] {
            let plan = plan_bundles(None, tier);
            assert_eq!(plan.bundles.to_vec(), MINIMAL_BUNDLES.to_vec(), "{tier:?}");
            assert!(plan.dropped.is_empty(), "{tier:?}");
        }
        // A bundles key that isn't an array is the same case.
        let weird = user_manifest(serde_json::json!("dsh-base"));
        assert_eq!(
            plan_bundles(Some(&weird), SafeTier::Plugins)
                .bundles
                .to_vec(),
            MINIMAL_BUNDLES.to_vec()
        );
    }

    #[test]
    fn duplicates_and_blanks_are_collapsed() {
        let m = user_manifest(serde_json::json!([
            "@deepseek-ai/dsh-base",
            "@deepseek-ai/dsh-base",
            "   ",
            7
        ]));
        let plan = plan_bundles(Some(&m), SafeTier::Plugins);
        assert_eq!(plan.bundles.to_vec(), MINIMAL_BUNDLES.to_vec());
    }

    #[test]
    fn the_ladder_only_narrows() {
        assert_eq!(SafeTier::Plugins.next(), Some(SafeTier::Minimal));
        assert_eq!(SafeTier::Minimal.next(), None);
    }

    #[test]
    fn writing_the_safe_profile_leaves_the_user_profile_byte_identical() {
        let root = tmp_dir("write");
        let instance = manifest(&root);
        let user_dir = root.join("profiles").join("web");
        std::fs::create_dir_all(&user_dir).unwrap();
        let user_files = [
            (
                "package.json",
                r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","third-party/evil"]}}}"#,
            ),
            ("cordis.patch.yml", "- id: evil\n  disabled: true\n"),
            ("pnpm-workspace.yaml", "packages:\n  - .\n"),
        ];
        for (name, body) in user_files {
            std::fs::write(user_dir.join(name), body).unwrap();
        }
        let before: Vec<Option<String>> = user_files
            .iter()
            .map(|(name, _)| std::fs::read_to_string(user_dir.join(name)).ok())
            .collect();
        let home_patch = root.join("cordis.patch.yml");
        std::fs::write(&home_patch, "- id: home\n").unwrap();

        let plan = write_safe_profile(&instance, SafeTier::Plugins).unwrap();

        // The generated profile is its own directory with exactly one file…
        let safe_dir = safe_profile_dir(&instance);
        assert_eq!(safe_dir, root.join("profiles").join(SAFE_PROFILE_NAME));
        let mut entries: Vec<String> = std::fs::read_dir(&safe_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["package.json"]);
        // …holding exactly the planned bundles, and no dependencies to install.
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(safe_dir.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(
            written.pointer("/dsh/profile/bundles"),
            Some(&serde_json::to_value(&plan.bundles).unwrap())
        );
        assert!(written.get("dependencies").is_none(), "{written}");

        // …and the user's profile is byte for byte what it was, with no new
        // files beside it (the temp file was renamed, not left behind).
        let after: Vec<Option<String>> = user_files
            .iter()
            .map(|(name, _)| std::fs::read_to_string(user_dir.join(name)).ok())
            .collect();
        assert_eq!(before, after);
        assert_eq!(
            std::fs::read_to_string(&home_patch).unwrap(),
            "- id: home\n"
        );
        let mut profile_entries: Vec<String> = std::fs::read_dir(root.join("profiles"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        profile_entries.sort();
        assert_eq!(
            profile_entries,
            vec![SAFE_PROFILE_NAME.to_string(), "web".to_string()]
        );
    }

    #[test]
    fn removing_an_absent_safe_profile_is_not_an_error() {
        let root = tmp_dir("remove");
        let instance = manifest(&root);
        std::fs::create_dir_all(root.join("profiles").join("web")).unwrap();
        remove_safe_profile(&instance).unwrap();
        write_safe_profile(&instance, SafeTier::Minimal).unwrap();
        remove_safe_profile(&instance).unwrap();
        // The user's profile is still there; only the scratch dir went away.
        assert!(root.join("profiles").join("web").is_dir());
        assert!(!safe_profile_dir(&instance).exists());
    }
}
