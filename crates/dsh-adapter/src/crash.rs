//! Crash diagnosis rule engine.
//!
//! When a launch fails, the launcher already knows the process is gone; this
//! module tells it *why*, by matching the boot log against the failure modes
//! `dsh` actually emits. Ported from `3/zat`'s `src/rescue.js` `diagnoseCrash`
//! — 13+ ordered rules, each mapping a signature to a real GitHub issue and a
//! repair action. The rules are matched in order (one line → one hit, except
//! `plugin-failed`, which names every plugin in the list) and de-duplicated by
//! `(kind, plugin)` across the whole log, exactly like the JS original.
//!
//! The rule text is bound to specific dsh releases (0.6.x–1.5.7) and will rot
//! as dsh changes wording — that is accepted here (see `docs/absorb-plan.md`
//! §5) because each rule is backed by a real issue number and the classifier is
//! fail-open: a log it does not recognise yields no issues, never a false boot.
//!
//! Matching is hand-rolled substring search, no `regex` dependency, matching
//! the crate's parsing style (`launcher-core/src/redact.rs`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// A diagnosed crash class. The `kebab-case` serde names are the stable tokens
/// the frontend maps to localized copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashKind {
    MissingBundle,
    PluginFailed,
    BadProfile,
    SourceDeps,
    MissingModule,
    NativeDeps,
    ClientModuleMissing,
    BundleMismatch,
    SourceMixed,
    DuplicatePlugin,
    ToolMissing,
    CliArg,
    CliError,
}

/// The repair action a diagnosis recommends. `exclude-bundle` / `restore` are
/// the L1/L2 recovery-ladder actions; `reinstall` / `install-deps` /
/// `rebuild-source` are dependency-level; `restart` is a plain retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixAction {
    ExcludeBundle,
    Restore,
    InstallDeps,
    Reinstall,
    RebuildSource,
    Restart,
}

/// One diagnosed crash cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashIssue {
    pub kind: CrashKind,
    /// The offending plugin / bundle / dependency / flag name, when the failure
    /// names one; empty otherwise.
    pub plugin: String,
    /// Human-readable, actionable message.
    pub message: String,
    pub fix: FixAction,
}

/// Diagnose a crash from boot log lines. Fail-open: an unrecognised log yields
/// an empty list, never a false failure.
pub fn diagnose_crash<S: AsRef<str>>(lines: &[S]) -> Vec<CrashIssue> {
    let mut issues = Vec::new();
    let mut seen: HashSet<(CrashKind, String)> = HashSet::new();
    for raw in lines {
        let text = raw.as_ref();
        if text.is_empty() {
            continue;
        }
        classify_line(text, &mut seen, &mut issues);
    }
    issues
}

fn classify_line(
    text: &str,
    seen: &mut HashSet<(CrashKind, String)>,
    issues: &mut Vec<CrashIssue>,
) {
    let mut add = |kind: CrashKind, plugin: &str, message: String, fix: FixAction| {
        // Same kind + same plugin reported once across the whole log.
        if seen.insert((kind, plugin.to_string())) {
            issues.push(CrashIssue {
                kind,
                plugin: plugin.to_string(),
                message,
                fix,
            });
        }
    };

    // 1. Profile declares a bundle that is not installed (#880).
    if let Some(i) = find_ci(text, "cannot resolve profile bundle") {
        let plugin = quoted_after(text, i + "cannot resolve profile bundle".len()).unwrap_or("");
        if !plugin.is_empty() {
            add(
                CrashKind::MissingBundle,
                plugin,
                format!("profile declares bundle \"{plugin}\" but it is not installed, so boot aborted"),
                FixAction::ExcludeBundle,
            );
        }
        return;
    }

    // 2. One or more plugins failed to load and aborted boot.
    if let Some(i) = find_ci(text, "plugin(s) failed to load:") {
        let tail = &text[i + "plugin(s) failed to load:".len()..];
        let names = tail.split([';', '\n']).next().unwrap_or("");
        for name in names.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            add(
                CrashKind::PluginFailed,
                name,
                format!("plugin \"{name}\" failed to load and aborted boot"),
                FixAction::ExcludeBundle,
            );
        }
        return;
    }

    // 3. Malformed patch/config — only a rescue-point restore helps.
    if find_ci(text, "must be a top-level yaml array").is_some()
        || find_ci(text, "failed to parse ").is_some_and(|i| {
            let tail = &text[i + "failed to parse ".len()..];
            starts_with_ci(tail, "patches")
                || starts_with_ci(tail, "overlay")
                || starts_with_ci(tail, "config file")
        })
    {
        add(
            CrashKind::BadProfile,
            "",
            format!("profile config is malformed — {}", excerpt(text, 160)),
            FixAction::Restore,
        );
        return;
    }

    // 4/5. A missing package: a source checkout missing a dev dependency
    // (tsx / esbuild) → install-deps; anything else → exclude the bundle.
    let dep = find_ci(text, "cannot find package")
        .and_then(|i| quoted_after(text, i + "cannot find package".len()))
        .or_else(|| {
            find_ci(text, "err_module_not_found")
                .and_then(|i| quoted_after(text, i + "err_module_not_found".len()))
        });
    if let Some(dep) = dep {
        if is_source_dep(dep, text) {
            add(
                CrashKind::SourceDeps,
                dep,
                format!(
                    "source checkout is missing dev dependency \"{dep}\" — install source deps and restart"
                ),
                FixAction::InstallDeps,
            );
        } else {
            add(
                CrashKind::MissingModule,
                dep,
                format!("missing package \"{dep}\""),
                FixAction::ExcludeBundle,
            );
        }
        return;
    }

    // 6. Native dependency not compiled: a `.node` module is missing, or the
    // Node ABI does not match (NODE_MODULE_VERSION).
    if find_ci(text, "node_module_version").is_some() {
        add(
            CrashKind::NativeDeps,
            "",
            "native dependency not built (missing .node binary or Node ABI mismatch) — reinstall profile deps to recompile".to_string(),
            FixAction::Reinstall,
        );
        return;
    }
    if let Some(i) = find_ci(text, "cannot find module") {
        let module = quoted_after(text, i + "cannot find module".len()).unwrap_or("");
        if module.to_ascii_lowercase().ends_with(".node") {
            add(
                CrashKind::NativeDeps,
                "",
                "native dependency not built (missing .node binary) — reinstall profile deps to recompile".to_string(),
                FixAction::Reinstall,
            );
            return;
        }
    }

    // 7. Client plugin failed to load: package on disk but absent from the
    // module table (npm shape → reinstall deps; source shape → rebuild).
    if find_ci(text, "failed to import loader entry").is_some()
        || find_ci(text, "missed the module table").is_some()
        || find_ci(text, "dynamic dependency that did not arrive").is_some()
    {
        add(
            CrashKind::ClientModuleMissing,
            "",
            "client plugin failed to load (package present but not in the module table) — reinstall profile deps, or rebuild a source checkout".to_string(),
            FixAction::Reinstall,
        );
        return;
    }

    // 8. Bundle out of sync with this dsh version (unknown file extension).
    if let Some(i) = find_ci(text, "unknown file extension") {
        let ext = quoted_after(text, i + "unknown file extension".len())
            .map(|e| e.strip_prefix('.').unwrap_or(e))
            .unwrap_or("");
        add(
            CrashKind::BundleMismatch,
            "",
            format!(
                "profile bundle is out of sync with this dsh version (loading .{ext} failed) — reinstall profile deps and restart"
            ),
            FixAction::Reinstall,
        );
        return;
    }
    if find_ci(text, "err_unknown_file_extension").is_some() {
        add(
            CrashKind::BundleMismatch,
            "",
            "profile bundle is out of sync with this dsh version (unknown file extension) — reinstall profile deps and restart".to_string(),
            FixAction::Reinstall,
        );
        return;
    }

    // 9. Source + build output mixed after a rollback (loader called a function
    // that does not exist).
    if (find_ci(text, "plugin tree failed to load").is_some()
        || find_ci(text, "failed to apply loader entry").is_some())
        && find_ci(text, "is not a function").is_some()
    {
        add(
            CrashKind::SourceMixed,
            "",
            "source and build output are mixed after a dsh rollback (loader called a missing function) — clean and rebuild source".to_string(),
            FixAction::RebuildSource,
        );
        return;
    }

    // 10. Tool scheduler not registered (#1677 / #2130) — duplicate
    // @deepseek-ai/* deps, same class as bundle mismatch.
    if let Some(i) = find_ci(text, "cannot read properties of undefined") {
        let field = quoted_after(text, i + "cannot read properties of undefined".len())
            .unwrap_or("");
        if find_ci(text, "prepare").is_some()
            || find_ci(text, "toolruntime").is_some()
            || find_ci(text, "scheduler").is_some()
        {
            add(
                CrashKind::BundleMismatch,
                "",
                format!("tool scheduler not registered (reading '{field}') — duplicate @deepseek-ai/* deps, reinstall profile deps"),
                FixAction::Reinstall,
            );
            return;
        }
    }

    // 11. Duplicate loader entry (#3263 / #2889).
    if let Some(i) = find_ci(text, "duplicate loader entry id:") {
        let id = bare_after(text, i + "duplicate loader entry id:".len());
        if !id.is_empty() {
            add(
                CrashKind::DuplicatePlugin,
                id,
                format!("plugin \"{id}\" is registered twice (duplicate loader entry) — remove the duplicate registration"),
                FixAction::ExcludeBundle,
            );
        }
        return;
    }

    // 12. Toolchain command missing (#2990) — spawn ENOENT.
    if find_ci(text, "enoent").is_some()
        && (find_ci(text, "spawn").is_some()
            || find_ci(text, "bash").is_some()
            || find_ci(text, "exec").is_some()
            || find_ci(text, "createprocess").is_some())
    {
        add(
            CrashKind::ToolMissing,
            "",
            "toolchain command missing (spawn ENOENT) — the launcher re-bootstraps node/pnpm/npm/git, restart".to_string(),
            FixAction::Restart,
        );
        return;
    }

    // 13. Unsupported CLI flag — restart with adapted args.
    if let Some(i) = find_ci(text, "unknown option") {
        let flag = quoted_after(text, i + "unknown option".len()).unwrap_or("");
        add(
            CrashKind::CliArg,
            "",
            format!("flag \"{flag}\" is not supported by this dsh version — restart with adapted args"),
            FixAction::Restart,
        );
        return;
    }

    // 14. Bare CLI error line — not a connection refusal / port-in-use noise.
    if starts_with_ci(text, "error:")
        && find_ci(text, "econnrefused").is_none()
        && find_ci(text, "eaddrinuse").is_none()
    {
        add(CrashKind::CliError, "", excerpt(text, 200), FixAction::Restart);
    }
}

/// Case-insensitive byte index of `needle` in `haystack`. ASCII-lowercasing both
/// sides changes only single-byte `A-Z`→`a-z`, so byte offsets are preserved and
/// the returned index is valid in `haystack` itself.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase())
}

/// The quoted token (`'…'` or `"…"`) that first appears at or after `start`.
fn quoted_after(haystack: &str, start: usize) -> Option<&str> {
    let tail = haystack.get(start..)?;
    let q = tail.find(['\'', '"'])?;
    let quote = tail.as_bytes()[q];
    let open = q + 1;
    let close = tail[open..].find(quote as char)?;
    Some(&tail[open..open + close])
}

/// The bare token (`[A-Za-z0-9._-]+`) that starts at or after `start`, skipping
/// any separator (`duplicate loader entry id: dsh-market` → `dsh-market`).
fn bare_after(haystack: &str, start: usize) -> &str {
    let tail = haystack.get(start..).unwrap_or("").trim_start();
    let end = tail
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
        .map(|(i, _)| i)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn is_source_dep(dep: &str, text: &str) -> bool {
    let d = dep.to_ascii_lowercase();
    d == "tsx"
        || d == "esbuild"
        || d.contains("tsx/esm")
        || d.contains("tsx\\esm")
        || find_ci(text, "tsx/esm").is_some()
        || find_ci(text, "tsx\\esm").is_some()
}

fn excerpt(s: &str, max: usize) -> String {
    let stripped = s
        .strip_prefix("error:")
        .or_else(|| s.strip_prefix("Error:"))
        .unwrap_or(s)
        .trim();
    let mut out: String = stripped.chars().take(max).collect();
    if stripped.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(lines: &[&str]) -> Vec<CrashIssue> {
        diagnose_crash(lines)
    }

    fn kinds(lines: &[&str]) -> Vec<(CrashKind, String)> {
        diag(lines)
            .into_iter()
            .map(|i| (i.kind, i.plugin))
            .collect()
    }

    #[test]
    fn missing_bundle_captures_the_plugin_name() {
        let out = diag(&["Error: cannot resolve profile bundle \"dsh-broken-plugin\""]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CrashKind::MissingBundle);
        assert_eq!(out[0].plugin, "dsh-broken-plugin");
        assert_eq!(out[0].fix, FixAction::ExcludeBundle);
    }

    #[test]
    fn plugin_failed_names_every_plugin() {
        let out = diag(&["plugin(s) failed to load: dsh-a, dsh-b, dsh-c"]);
        let names: Vec<&str> = out.iter().map(|i| i.plugin.as_str()).collect();
        assert_eq!(names, vec!["dsh-a", "dsh-b", "dsh-c"]);
        assert!(out.iter().all(|i| i.kind == CrashKind::PluginFailed));
    }

    #[test]
    fn bad_profile_matches_both_signatures() {
        assert_eq!(
            kinds(&["must be a top-level YAML array"]),
            vec![(CrashKind::BadProfile, String::new())]
        );
        assert_eq!(
            kinds(&["Error: failed to parse patches"]),
            vec![(CrashKind::BadProfile, String::new())]
        );
        assert_eq!(
            kinds(&["failed to parse config file near line 3"]),
            vec![(CrashKind::BadProfile, String::new())]
        );
    }

    #[test]
    fn tsx_is_a_source_dep_others_are_missing_modules() {
        assert_eq!(
            kinds(&["Cannot find package 'tsx'"]),
            vec![(CrashKind::SourceDeps, "tsx".into())]
        );
        assert_eq!(
            kinds(&["Cannot find package 'dsh-whatever'"]),
            vec![(CrashKind::MissingModule, "dsh-whatever".into())]
        );
    }

    #[test]
    fn native_deps_matches_dot_node_and_abi() {
        assert_eq!(
            kinds(&["Cannot find module './build/Release/fs_ext.node'"]),
            vec![(CrashKind::NativeDeps, String::new())]
        );
        assert_eq!(
            kinds(&["NODE_MODULE_VERSION 108 mismatch"]),
            vec![(CrashKind::NativeDeps, String::new())]
        );
        // A plain (non-.node) missing module is not a native-dep signature.
        assert!(diag(&["Cannot find module 'express'"]).is_empty());
    }

    #[test]
    fn client_module_missing_matches_all_three_signatures() {
        for line in [
            "failed to import loader entry",
            "missed the module table",
            "a dynamic dependency that did not arrive",
        ] {
            assert_eq!(
                kinds(&[line]),
                vec![(CrashKind::ClientModuleMissing, String::new())],
                "line: {line}"
            );
        }
    }

    #[test]
    fn bundle_mismatch_ext_and_scheduler() {
        let out = diag(&["Unknown file extension \".css\""]);
        assert_eq!(out[0].kind, CrashKind::BundleMismatch);
        assert!(out[0].message.contains(".css"), "{}", out[0].message);
        assert_eq!(
            kinds(&["TypeError: Cannot read properties of undefined (reading 'prepare')"]),
            vec![(CrashKind::BundleMismatch, String::new())]
        );
    }

    #[test]
    fn source_mixed_requires_both_halves() {
        assert_eq!(
            kinds(&["plugin tree failed to load: ctx.subagents is not a function"]),
            vec![(CrashKind::SourceMixed, String::new())]
        );
        // "is not a function" alone is not enough to pin source mixing.
        assert!(diag(&["something is not a function"]).is_empty());
    }

    #[test]
    fn duplicate_plugin_captures_the_id() {
        let out = diag(&["duplicate loader entry id: dsh-market"]);
        assert_eq!(out[0].kind, CrashKind::DuplicatePlugin);
        assert_eq!(out[0].plugin, "dsh-market");
        assert_eq!(out[0].fix, FixAction::ExcludeBundle);
    }

    #[test]
    fn tool_missing_needs_enoent_plus_a_spawn_word() {
        assert_eq!(
            kinds(&["spawn bash ENOENT"]),
            vec![(CrashKind::ToolMissing, String::new())]
        );
        assert!(diag(&["ENOENT on its own"]).is_empty());
    }

    #[test]
    fn cli_arg_captures_the_flag() {
        let out = diag(&["unknown option '--no-open'"]);
        assert_eq!(out[0].kind, CrashKind::CliArg);
        assert_eq!(out[0].fix, FixAction::Restart);
        assert!(out[0].message.contains("--no-open"), "{}", out[0].message);
    }

    #[test]
    fn cli_error_is_a_catchall_but_skips_connection_noise() {
        assert_eq!(kinds(&["error: something bad happened"]).len(), 1);
        assert!(diag(&["error: connect ECONNREFUSED 127.0.0.1:3080"]).is_empty());
        assert!(diag(&["error: listen EADDRINUSE 0.0.0.0:3080"]).is_empty());
    }

    #[test]
    fn duplicates_collapse_across_lines() {
        let out = diag(&[
            "cannot resolve profile bundle \"dsh-x\"",
            "cannot resolve profile bundle \"dsh-x\"",
        ]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn unrecognised_log_yields_nothing() {
        assert!(diag(&["everything is fine", ""]).is_empty());
    }
}
