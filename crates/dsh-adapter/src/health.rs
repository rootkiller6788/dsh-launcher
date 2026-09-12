//! Instance health checks — "what is wrong right now", before it becomes a crash.
//!
//! [`crate::crash`] reads a boot log *after* a failure; this reads the instance
//! *before* one, and both speak the same vocabulary (`exclude-bundle`,
//! `restore`) so the UI can offer the same buttons. Everything here is
//! read-only and re-measured on every call: no cache, because the whole value is
//! telling the user the state of a thing they just changed.
//!
//! Deliberately **not** a port of `dsh-manager`'s check list. That list is
//! larger than its implementation — `disk-space` is a hard-coded `'info'` stub
//! that never measures disk, and several documented checks do not exist. A check
//! that cannot point at the line of code that measures it is worse than no check
//! at all: it reports "healthy" for something nobody looked at. So the rule here
//! is one check = one real measurement, and where AHL cannot measure, there is no
//! check (see `docs/absorb-plan.md` §2 Phase 2.4).
//!
//! Reuse over reinvention: the profile checks read what
//! [`crate::diagnostics`] already resolves (bundle resolution, duplicate entry
//! ids), and the repairs offered are the commands that already exist
//! (`rescue_restore`, `rescue_create`, `set_plugin_enabled`) — a health check
//! never introduces a new mutation path.

use std::path::Path;

use launcher_core::{AppSettings, InstanceManifest};
use serde::{Deserialize, Serialize};

use crate::content::{entry_artifact_exists, package_mounts_client, skin_has_bundle};
use crate::diagnostics::diagnose_profile;
use crate::rescue::snapshot_status;
use crate::DshAdapter;

/// How bad a check's finding is. `Ok` is the only status that means "measured,
/// and fine" — a check that could not run reports [`HealthStatus::Warn`], not
/// `Ok`, so an unmeasurable thing never reads as healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    Ok,
    Warn,
    Fail,
}

impl HealthStatus {
    /// Severity rank, for picking the report's worst finding.
    fn rank(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Warn => 1,
            Self::Fail => 2,
        }
    }
}

/// Which part of the instance a check looks at. The UI groups by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthGroup {
    /// Node + the DSH CLI the launcher would actually run.
    Runtime,
    /// The profile's own files: the two patch layers and `package.json`.
    Profile,
    /// The bundle stack — what boot will try to mount.
    Plugins,
    /// MCP server records in the instance manifest.
    Mcp,
    /// The rescue point that makes a `restore` possible.
    Rescue,
}

/// A repair the launcher can carry out itself. Each maps to a command that
/// already exists — this is not a new mutation surface, it is a pointer at the
/// existing one, so a fix button here can never do something the corresponding
/// command could not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthFix {
    /// Overwrite the profile files from the rescue point (`rescue_restore`).
    RestoreRescue,
    /// Take a rescue point now (`rescue_create`).
    CreateRescue,
    /// Disable a plugin / bundle's patch rows (`set_plugin_enabled(false)`) —
    /// the same write the pre-boot quarantine performs.
    ExcludeBundle,
}

/// One measured finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    /// Stable token (`bundles-unloadable`), the key the UI localizes.
    pub id: String,
    pub group: HealthGroup,
    pub status: HealthStatus,
    /// What was measured, specifically — names, counts, paths. Never a canned
    /// "looks fine"; an empty `detail` would mean nothing was measured.
    pub detail: String,
    /// Repairs that apply, in the order to try them. Empty when the launcher has
    /// no safe action — an honest dead end beats a button that cannot work.
    pub fixes: Vec<HealthFix>,
    /// The names a `fixes` entry acts on (plugin / bundle names). Empty for
    /// fixes that take no target.
    pub targets: Vec<String>,
}

impl HealthCheck {
    fn new(id: &str, group: HealthGroup, status: HealthStatus, detail: String) -> Self {
        Self {
            id: id.to_string(),
            group,
            status,
            detail,
            fixes: Vec::new(),
            targets: Vec::new(),
        }
    }

    fn with_fix(mut self, fix: HealthFix, targets: Vec<String>) -> Self {
        self.fixes.push(fix);
        self.targets = targets;
        self
    }
}

/// The whole picture, worst finding first-class so the UI does not re-derive it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub checks: Vec<HealthCheck>,
    pub worst: HealthStatus,
}

impl HealthReport {
    /// Does the report contain a finding of this status or worse?
    pub fn has_at_least(&self, status: HealthStatus) -> bool {
        self.worst.rank() >= status.rank()
    }
}

/// Measure an instance. `rescue_dir` is the instance's rescue-point directory
/// (`AppPaths::rescue_dir`); passed in rather than derived so this module stays
/// ignorant of the launcher's on-disk layout.
pub fn health_report(
    adapter: &DshAdapter,
    settings: &AppSettings,
    instance: &InstanceManifest,
    rescue_dir: &Path,
) -> HealthReport {
    let mut checks = Vec::new();
    checks.extend(runtime_checks(adapter, settings));
    checks.extend(profile_checks(instance));
    checks.extend(bundle_checks(instance));
    checks.extend(mcp_checks(instance));
    checks.push(rescue_check(rescue_dir));

    let worst = checks
        .iter()
        .map(|c| c.status)
        .max_by_key(|s| s.rank())
        .unwrap_or(HealthStatus::Ok);
    HealthReport { checks, worst }
}

/// Node + the DSH CLI. Both are resolved the same way `launch` resolves them, so
/// a green light here means the launch path has its two inputs.
fn runtime_checks(adapter: &DshAdapter, settings: &AppSettings) -> Vec<HealthCheck> {
    let mut out = Vec::new();

    let node = adapter.resolve_node(settings);
    out.push(match &node {
        Some(path) => HealthCheck::new(
            "runtime-node",
            HealthGroup::Runtime,
            HealthStatus::Ok,
            path.display().to_string(),
        ),
        None => HealthCheck::new(
            "runtime-node",
            HealthGroup::Runtime,
            HealthStatus::Fail,
            "no Node executable found (checked the settings override, the bundled and \
             managed runtimes, and PATH) — nothing can boot without one"
                .to_string(),
        ),
    });

    // Node is reported separately, so a missing Node does not also have to be
    // said again here; what this answers is "is there a DSH CLI to run".
    match adapter.resolve_bin(settings) {
        Some((bin, source)) => out.push(HealthCheck::new(
            "runtime-dsh-bin",
            HealthGroup::Runtime,
            HealthStatus::Ok,
            format!("{} ({source})", bin.display()),
        )),
        None => out.push(HealthCheck::new(
            "runtime-dsh-bin",
            HealthGroup::Runtime,
            HealthStatus::Fail,
            "no DSH CLI entry found — set one in Settings, or install a runtime".to_string(),
        )),
    }

    out
}

/// The two patch layers and the profile manifest. These are the files the
/// launcher rewrites, so they are the ones a bad write can brick — and all three
/// have a rescue-point copy, which is why every failure here offers `restore`.
fn profile_checks(instance: &InstanceManifest) -> Vec<HealthCheck> {
    let profile_dir = DshAdapter::profile_dir(instance);
    let workspace = Path::new(&instance.workspace);
    let mut out = Vec::new();

    let manifest = profile_dir.join("package.json");
    out.push(match std::fs::read_to_string(&manifest) {
        Err(e) => HealthCheck::new(
            "profile-manifest",
            HealthGroup::Profile,
            HealthStatus::Fail,
            format!("{} is unreadable ({e})", manifest.display()),
        )
        .with_fix(HealthFix::RestoreRescue, Vec::new()),
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Err(e) => HealthCheck::new(
                "profile-manifest",
                HealthGroup::Profile,
                HealthStatus::Fail,
                format!("{} is not valid JSON: {e}", manifest.display()),
            )
            .with_fix(HealthFix::RestoreRescue, Vec::new()),
            Ok(value) if value.get("dsh").is_none() => HealthCheck::new(
                "profile-manifest",
                HealthGroup::Profile,
                HealthStatus::Warn,
                format!(
                    "{} parses but has no `dsh` section — the profile may not be \
                     materialized yet",
                    manifest.display()
                ),
            ),
            Ok(_) => HealthCheck::new(
                "profile-manifest",
                HealthGroup::Profile,
                HealthStatus::Ok,
                manifest.display().to_string(),
            ),
        },
    });

    // A patch layer must be a top-level YAML sequence (the shape DSH's patch
    // reader expects) — a scalar or mapping here is exactly the
    // `must be a top-level YAML array` boot failure, caught one step earlier.
    for (id, path) in [
        ("profile-patch", profile_dir.join("cordis.patch.yml")),
        ("home-patch", workspace.join("cordis.patch.yml")),
    ] {
        out.push(patch_check(id, &path));
    }

    out
}

/// One `cordis.patch.yml` layer: parseable, and a sequence (or empty).
fn patch_check(id: &'static str, path: &Path) -> HealthCheck {
    if !path.is_file() {
        // Absent is legitimate: a fresh profile has neither layer until the
        // launcher writes one. Not a finding.
        return HealthCheck::new(
            id,
            HealthGroup::Profile,
            HealthStatus::Ok,
            format!("{} — not present (nothing to patch yet)", path.display()),
        );
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return HealthCheck::new(
            id,
            HealthGroup::Profile,
            HealthStatus::Fail,
            format!("{} is unreadable", path.display()),
        )
        .with_fix(HealthFix::RestoreRescue, Vec::new());
    };
    // An empty file deserializes to `None`, which DSH treats as "nothing
    // mounted" — the same as a `[]` placeholder, so it is not a failure.
    match serde_yaml::from_str::<serde_yaml::Value>(&text) {
        Err(e) => HealthCheck::new(
            id,
            HealthGroup::Profile,
            HealthStatus::Fail,
            format!("{} is not valid YAML: {e}", path.display()),
        )
        .with_fix(HealthFix::RestoreRescue, Vec::new()),
        Ok(value) if !matches!(value, serde_yaml::Value::Null | serde_yaml::Value::Sequence(_)) => {
            HealthCheck::new(
                id,
                HealthGroup::Profile,
                HealthStatus::Fail,
                format!(
                    "{} is not a top-level YAML array — DSH's patch reader will reject \
                     this profile at boot",
                    path.display()
                ),
            )
            .with_fix(HealthFix::RestoreRescue, Vec::new())
        }
        Ok(_) => HealthCheck::new(
            id,
            HealthGroup::Profile,
            HealthStatus::Ok,
            path.display().to_string(),
        ),
    }
}

/// What boot would mount. Reuses [`diagnose_profile`] for bundle resolution and
/// duplicate entry ids rather than re-reading the profile a second way — two
/// readers would eventually disagree, and the panel would then contradict the
/// Diagnostics one.
fn bundle_checks(instance: &InstanceManifest) -> Vec<HealthCheck> {
    let report = diagnose_profile(instance);
    let mut out = Vec::new();

    let unresolved: Vec<String> = report
        .bundles
        .iter()
        .filter(|b| !b.resolved)
        .map(|b| b.name.clone())
        .collect();
    out.push(if unresolved.is_empty() {
        HealthCheck::new(
            "bundles-resolved",
            HealthGroup::Plugins,
            HealthStatus::Ok,
            format!(
                "{} bundle{} resolved",
                report.bundles.len(),
                if report.bundles.len() == 1 { "" } else { "s" }
            ),
        )
    } else {
        // No repair offered: a bundle that is *listed* but not on disk has no
        // patch rows to disable (its package directory is what supplies them), so
        // the launcher cannot switch it off — that needs `dsh plugin remove`.
        // Restoring the profile files would not bring the package back either.
        // Saying so is more useful than a button that fails.
        HealthCheck::new(
            "bundles-resolved",
            HealthGroup::Plugins,
            HealthStatus::Fail,
            format!(
                "declared in dsh.profile.bundles but not installed: {} — boot aborts on a \
                 missing bundle",
                unresolved.join(", ")
            ),
        )
    });

    let unloadable: Vec<String> = report
        .bundles
        .iter()
        .filter(|b| b.resolved && bundle_needs_artifact(instance, &b.name))
        .map(|b| b.name.clone())
        .collect();
    out.push(if unloadable.is_empty() {
        HealthCheck::new(
            "bundles-loadable",
            HealthGroup::Plugins,
            HealthStatus::Ok,
            "every client bundle has its built entry".to_string(),
        )
    } else {
        HealthCheck::new(
            "bundles-loadable",
            HealthGroup::Plugins,
            HealthStatus::Fail,
            format!(
                "installed but missing the built entry they import: {} — boot dies \
                 ERR_MODULE_NOT_FOUND importing these",
                unloadable.join(", ")
            ),
        )
        .with_fix(HealthFix::ExcludeBundle, unloadable)
    });

    out.push(if report.duplicates.is_empty() {
        HealthCheck::new(
            "entry-duplicates",
            HealthGroup::Plugins,
            HealthStatus::Ok,
            "no loader entry id is mounted twice".to_string(),
        )
    } else {
        // Which layer should give way is a judgement the launcher cannot make
        // for the user, so this is a finding without a fix.
        HealthCheck::new(
            "entry-duplicates",
            HealthGroup::Plugins,
            HealthStatus::Fail,
            format!(
                "mounted by more than one layer: {} — DSH refuses to start on a \
                 duplicate entry id",
                report.duplicates.join(", ")
            ),
        )
    });

    out
}

/// Is this bundle a web-app client whose built entry artifact is missing?
///
/// A bundle that mounts a client (`dsh.client`) has its own patch row importing
/// the package at boot, so it must resolve; a resource-only bundle never does.
/// The same predicate the pre-boot quarantine acts on
/// ([`crate::content::quarantine_unloadable_client_bundles`]) — reported here
/// before the boot that would trip over it.
fn bundle_needs_artifact(instance: &InstanceManifest, name: &str) -> bool {
    let dir = DshAdapter::profile_dir(instance)
        .join("node_modules")
        .join(name);
    skin_has_bundle(instance, name)
        && package_mounts_client(&dir)
        && !entry_artifact_exists(&dir)
}

/// MCP server records, from the manifest alone — no probe, no network. What it
/// can catch is a record that cannot work as written: a stdio server with no
/// command, an http server with no URL, and a declared-required env key with no
/// value. Key *names* are safe to print; values never appear here.
fn mcp_checks(instance: &InstanceManifest) -> Vec<HealthCheck> {
    let enabled: Vec<&launcher_core::McpServerRecord> =
        instance.mcp.iter().filter(|r| r.enabled).collect();
    let mut out = Vec::new();

    let malformed: Vec<String> = enabled
        .iter()
        .filter(|r| match r.transport.as_str() {
            "streamable-http" => r.url.trim().is_empty(),
            // Anything not marked http is launched as a stdio child, which
            // needs a command to spawn.
            _ => r.command.trim().is_empty(),
        })
        .map(|r| r.id.clone())
        .collect();
    out.push(if enabled.is_empty() {
        HealthCheck::new(
            "mcp-launch-shape",
            HealthGroup::Mcp,
            HealthStatus::Ok,
            "no MCP servers enabled".to_string(),
        )
    } else if malformed.is_empty() {
        HealthCheck::new(
            "mcp-launch-shape",
            HealthGroup::Mcp,
            HealthStatus::Ok,
            format!("{} enabled server(s) have a launch definition", enabled.len()),
        )
    } else {
        HealthCheck::new(
            "mcp-launch-shape",
            HealthGroup::Mcp,
            HealthStatus::Fail,
            format!(
                "no command (stdio) or URL (http) to launch: {}",
                malformed.join(", ")
            ),
        )
    });

    // Declared-but-unset env. Install copies the catalog's `requiredEnv` onto the
    // record; a key the user never filled in stays absent (or empty), and the
    // server then fails at connect time with whatever its own message is.
    let mut unset: Vec<String> = Vec::new();
    for record in &enabled {
        let missing: Vec<&str> = record
            .required_env
            .iter()
            .map(|req| req.key.as_str())
            .filter(|key| {
                record
                    .env
                    .get(*key)
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect();
        if !missing.is_empty() {
            unset.push(format!("{} ({})", record.id, missing.join(", ")));
        }
    }
    out.push(if enabled.is_empty() {
        HealthCheck::new(
            "mcp-env",
            HealthGroup::Mcp,
            HealthStatus::Ok,
            "no MCP servers enabled".to_string(),
        )
    } else if unset.is_empty() {
        HealthCheck::new(
            "mcp-env",
            HealthGroup::Mcp,
            HealthStatus::Ok,
            "every declared environment variable has a value".to_string(),
        )
    } else {
        HealthCheck::new(
            "mcp-env",
            HealthGroup::Mcp,
            HealthStatus::Warn,
            format!("declared environment variables with no value: {}", unset.join("; ")),
        )
    });

    out
}

/// The rescue point. Absent is a `warn`, not a failure: the launcher takes one
/// before any destructive change, so a fresh instance legitimately has none — but
/// until it exists, a `restore` has nothing to restore from.
fn rescue_check(rescue_dir: &Path) -> HealthCheck {
    let status = snapshot_status(rescue_dir);
    if status.exists {
        HealthCheck::new(
            "rescue-point",
            HealthGroup::Rescue,
            HealthStatus::Ok,
            format!(
                "{} file(s) captured at {}",
                status.files.len(),
                status.at
            ),
        )
    } else {
        HealthCheck::new(
            "rescue-point",
            HealthGroup::Rescue,
            HealthStatus::Warn,
            "no rescue point yet — a bad change could not be undone until one exists"
                .to_string(),
        )
        .with_fix(HealthFix::CreateRescue, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::{InstanceManifest, McpEnvRequirement, McpServerRecord, RuntimeRef};

    /// A throwaway workspace with an instance manifest pointing at it.
    fn instance(tag: &str) -> (std::path::PathBuf, InstanceManifest) {
        let root = std::env::temp_dir()
            .join(format!("ahl-health-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        let instance = InstanceManifest {
            id: tag.into(),
            name: "Health".into(),
            runtime: RuntimeRef {
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
        (root, instance)
    }

    fn profile_dir(instance: &InstanceManifest) -> std::path::PathBuf {
        DshAdapter::profile_dir(instance)
    }

    /// The report for an instance, without the environment-dependent runtime
    /// checks — those delegate to `resolve_node` / `resolve_bin`, which report
    /// on the machine running the test.
    fn report(instance: &InstanceManifest, rescue_dir: &Path) -> Vec<HealthCheck> {
        let adapter = DshAdapter::configured(
            std::env::temp_dir().join("ahl-health-runtimes"),
            None,
        );
        health_report(&adapter, &AppSettings::default(), instance, rescue_dir)
            .checks
            .into_iter()
            .filter(|c| c.group != HealthGroup::Runtime)
            .collect()
    }

    fn check<'a>(checks: &'a [HealthCheck], id: &str) -> &'a HealthCheck {
        checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check {id} in {:?}", ids(checks)))
    }

    fn ids(checks: &[HealthCheck]) -> Vec<&str> {
        checks.iter().map(|c| c.id.as_str()).collect()
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn every_report_covers_each_group_at_least_once() {
        let (root, instance) = instance("coverage");
        let checks = report(&instance, &root.join("rescue"));
        for group in [
            HealthGroup::Profile,
            HealthGroup::Plugins,
            HealthGroup::Mcp,
            HealthGroup::Rescue,
        ] {
            assert!(
                checks.iter().any(|c| c.group == group),
                "no check for {group:?}: {:?}",
                ids(&checks)
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_profile_manifest_is_a_restorable_failure() {
        let (root, instance) = instance("no-manifest");
        let checks = report(&instance, &root.join("rescue"));
        let c = check(&checks, "profile-manifest");
        assert_eq!(c.status, HealthStatus::Fail);
        assert_eq!(c.fixes, vec![HealthFix::RestoreRescue]);
        assert!(c.detail.contains("unreadable"), "{}", c.detail);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_json_and_yaml_are_failures_with_a_restore() {
        let (root, instance) = instance("malformed");
        let profile = profile_dir(&instance);
        write(&profile.join("package.json"), "{ this is not json");
        // A scalar where DSH's patch reader wants a top-level array.
        write(&profile.join("cordis.patch.yml"), "dsh:\n  profile: web\n");

        let checks = report(&instance, &root.join("rescue"));
        for id in ["profile-manifest", "profile-patch"] {
            let c = check(&checks, id);
            assert_eq!(c.status, HealthStatus::Fail, "{id}: {}", c.detail);
            assert_eq!(c.fixes, vec![HealthFix::RestoreRescue], "{id}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_absent_patch_layer_is_not_a_finding() {
        let (root, instance) = instance("no-patch");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        let checks = report(&instance, &root.join("rescue"));
        let c = check(&checks, "profile-patch");
        assert_eq!(c.status, HealthStatus::Ok, "{}", c.detail);
        assert!(c.fixes.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_empty_patch_layer_parses_as_nothing_mounted() {
        let (root, instance) = instance("empty-patch");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        write(&profile_dir(&instance).join("cordis.patch.yml"), "[]\n");
        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "profile-patch").status, HealthStatus::Ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_declared_but_absent_bundle_fails_with_no_false_fix() {
        let (root, instance) = instance("missing-bundle");
        write(
            &profile_dir(&instance).join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["dsh-ghost"]}}}"#,
        );
        let checks = report(&instance, &root.join("rescue"));
        let c = check(&checks, "bundles-resolved");
        assert_eq!(c.status, HealthStatus::Fail);
        assert!(c.detail.contains("dsh-ghost"), "{}", c.detail);
        // No patch rows can exist without the package directory, so there is no
        // disable to offer — and restoring would not bring the package back.
        assert!(c.fixes.is_empty(), "must not offer a fix that cannot work");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_installed_client_bundle_without_its_entry_offers_disable() {
        let (root, instance) = instance("unloadable");
        write(
            &profile_dir(&instance).join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["dsh-skin"]}}}"#,
        );
        let nm = profile_dir(&instance).join("node_modules").join("dsh-skin");
        write(
            &nm.join("package.json"),
            r#"{"main":"lib/index.js","dsh":{"bundle":{"patch":"cordis.patch.yml"},"client":{"entry":"lib/index.js"}}}"#,
        );

        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "bundles-resolved").status, HealthStatus::Ok);
        let c = check(&checks, "bundles-loadable");
        assert_eq!(c.status, HealthStatus::Fail, "{}", c.detail);
        assert_eq!(c.fixes, vec![HealthFix::ExcludeBundle]);
        assert_eq!(c.targets, vec!["dsh-skin"]);

        // With the declared entry on disk it is loadable, and the finding clears.
        write(&nm.join("lib/index.js"), "export {}\n");
        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "bundles-loadable").status, HealthStatus::Ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_resource_only_bundle_is_exempt_from_the_entry_check() {
        let (root, instance) = instance("resource-only");
        write(
            &profile_dir(&instance).join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["dsh-theme"]}}}"#,
        );
        // A `dsh.bundle` with no `dsh.client`: nothing imports the package, so a
        // missing build artifact is not a boot problem.
        write(
            &profile_dir(&instance)
                .join("node_modules")
                .join("dsh-theme")
                .join("package.json"),
            r#"{"dsh":{"bundle":{"patch":"cordis.patch.yml"}}}"#,
        );
        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "bundles-loadable").status, HealthStatus::Ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_duplicate_entry_id_is_a_failure_without_a_fix() {
        let (root, instance) = instance("dup");
        write(
            &profile_dir(&instance).join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["a","b"]}}}"#,
        );
        // Both bundles insert the same loader row.
        for name in ["a", "b"] {
            let dir = profile_dir(&instance).join("node_modules").join(name);
            write(
                &dir.join("package.json"),
                r#"{"dsh":{"bundle":{"patch":"cordis.patch.yml"}}}"#,
            );
            write(
                &dir.join("cordis.patch.yml"),
                "- insert:\n    - id: shared-row\n      name: something\n",
            );
        }
        let checks = report(&instance, &root.join("rescue"));
        let c = check(&checks, "entry-duplicates");
        assert_eq!(c.status, HealthStatus::Fail);
        assert!(c.detail.contains("shared-row"), "{}", c.detail);
        assert!(c.fixes.is_empty(), "which layer gives way is not ours to pick");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_records_are_checked_for_launch_shape_and_unset_env() {
        let (root, mut instance) = instance("mcp");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        instance.mcp = vec![
            McpServerRecord {
                id: "owner/no-command".into(),
                server_name: "no-command".into(),
                transport: "stdio".into(),
                // command deliberately empty
                ..McpServerRecord::default()
            },
            McpServerRecord {
                id: "owner/http-no-url".into(),
                server_name: "http-no-url".into(),
                transport: "streamable-http".into(),
                ..McpServerRecord::default()
            },
            McpServerRecord {
                id: "owner/needs-key".into(),
                server_name: "needs-key".into(),
                transport: "stdio".into(),
                command: "npx".into(),
                required_env: vec![
                    McpEnvRequirement {
                        key: "KANBOARD_URL".into(),
                        label: None,
                        secret: false,
                    },
                    McpEnvRequirement {
                        key: "KANBOARD_TOKEN".into(),
                        label: None,
                        secret: true,
                    },
                ],
                ..McpServerRecord::default()
            },
            // Disabled servers are not booted, so they are not checked.
            McpServerRecord {
                id: "owner/off".into(),
                transport: "stdio".into(),
                enabled: false,
                ..McpServerRecord::default()
            },
        ];

        let checks = report(&instance, &root.join("rescue"));
        let shape = check(&checks, "mcp-launch-shape");
        assert_eq!(shape.status, HealthStatus::Fail);
        assert!(shape.detail.contains("owner/no-command"), "{}", shape.detail);
        assert!(shape.detail.contains("owner/http-no-url"), "{}", shape.detail);
        assert!(!shape.detail.contains("owner/off"), "disabled must not be checked");

        let env = check(&checks, "mcp-env");
        assert_eq!(env.status, HealthStatus::Warn);
        assert!(env.detail.contains("needs-key"), "{}", env.detail);
        assert!(env.detail.contains("KANBOARD_URL"), "{}", env.detail);
        // Names only — a value must never reach a report that gets screenshotted.
        assert!(!env.detail.contains("KANBOARD_TOKEN="), "{}", env.detail);

        // Filling both in clears it.
        if let Some(record) = instance.mcp.iter_mut().find(|r| r.id == "owner/needs-key") {
            record.env.insert("KANBOARD_URL".into(), "https://kb".into());
            record.env.insert("KANBOARD_TOKEN".into(), "t".into());
        }
        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "mcp-env").status, HealthStatus::Ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_empty_env_value_counts_as_unset() {
        let (root, mut instance) = instance("mcp-blank-env");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        let mut record = McpServerRecord {
            id: "owner/blank".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            required_env: vec![McpEnvRequirement {
                key: "API_KEY".into(),
                label: None,
                secret: true,
            }],
            ..McpServerRecord::default()
        };
        record.env.insert("API_KEY".into(), "   ".into());
        instance.mcp = vec![record];
        let checks = report(&instance, &root.join("rescue"));
        assert_eq!(check(&checks, "mcp-env").status, HealthStatus::Warn);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_rescue_point_is_a_warning_until_one_exists() {
        let (root, instance) = instance("rescue");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        let rescue_dir = root.join("rescue");

        let checks = report(&instance, &rescue_dir);
        let c = check(&checks, "rescue-point");
        assert_eq!(c.status, HealthStatus::Warn);
        assert_eq!(c.fixes, vec![HealthFix::CreateRescue]);

        let files = crate::rescue::rescue_files(&instance);
        crate::rescue::create_snapshot(&files, &rescue_dir).expect("snapshot");
        let checks = report(&instance, &rescue_dir);
        assert_eq!(check(&checks, "rescue-point").status, HealthStatus::Ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worst_reports_the_most_severe_finding() {
        let (root, instance) = instance("worst");
        write(&profile_dir(&instance).join("package.json"), r#"{"dsh":{}}"#);
        // No rescue point → warn at worst.
        let adapter =
            DshAdapter::configured(std::env::temp_dir().join("ahl-health-runtimes"), None);
        let r = health_report(&adapter, &AppSettings::default(), &instance, &root.join("rescue"));
        assert!(r.has_at_least(HealthStatus::Warn));

        // A malformed profile file outranks it.
        write(&profile_dir(&instance).join("cordis.patch.yml"), "not: [a, sequence\n");
        let r = health_report(&adapter, &AppSettings::default(), &instance, &root.join("rescue"));
        assert_eq!(r.worst, HealthStatus::Fail);
        let _ = std::fs::remove_dir_all(root);
    }
}
