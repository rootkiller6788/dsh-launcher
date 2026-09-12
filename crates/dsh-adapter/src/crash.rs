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
//! [`diagnose_crash_with`] narrows the exposure: a reworded failure can be
//! caught by a signature from `crash-signatures.json` ([`load_signatures`])
//! without shipping a new launcher build.
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
    diagnose_crash_with(lines, &[])
}

/// Diagnose a crash, with user-supplied signatures checked after the built-ins.
///
/// The built-in table is bound to specific dsh releases and will rot as dsh
/// rewords its failures (see the module note). `extras` is the escape hatch: a
/// signature added here catches a boot whose wording the built-ins no longer
/// match, without shipping a new build of the launcher.
///
/// Built-ins win. A line the built-in table already classified is not passed to
/// the extras, so the shipped behaviour cannot be shadowed from a data file —
/// the extras can only ever *add* diagnoses to a log that would otherwise come
/// back empty.
pub fn diagnose_crash_with<S: AsRef<str>>(lines: &[S], extras: &[ExtraSignature]) -> Vec<CrashIssue> {
    let mut issues = Vec::new();
    let mut seen: HashSet<(CrashKind, String)> = HashSet::new();
    for raw in lines {
        let text = raw.as_ref();
        if text.is_empty() {
            continue;
        }
        if classify_specific(text, &mut seen, &mut issues) == Verdict::Recognised {
            continue;
        }
        if !classify_with_extras(text, extras, &mut seen, &mut issues) {
            classify_catch_all(text, &mut seen, &mut issues);
        }
    }
    issues
}

/// A user-supplied crash signature, loaded from JSON (see [`load_signatures`]).
///
/// Deliberately a flat conjunction of substrings rather than a pattern language:
/// the built-ins need bespoke logic (splitting a plugin list, the `.node` suffix,
/// the tsx source-dep check) that no reasonable data format expresses, so the
/// extras cover the common case — "dsh now says X instead of Y" — and leave the
/// rest to code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtraSignature {
    pub kind: CrashKind,
    pub fix: FixAction,
    /// Every one of these must appear in the line, case-insensitively.
    pub contains: Vec<String>,
    #[serde(default)]
    pub capture: Capture,
    /// Message shown to the user. `{name}` is replaced with the captured token
    /// (empty when `capture` is `none`).
    pub message: String,
}

/// Which token a signature reads out of the line as the offending name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capture {
    /// No name to capture.
    #[default]
    None,
    /// The first `'…'` or `"…"` token after `contains`'s last substring.
    Quoted,
    /// The first `[A-Za-z0-9._-]+` run after `contains`'s last substring.
    Bare,
}

/// Apply user signatures to one line. Returns whether any matched, so the caller
/// knows to skip the generic catch-all.
fn classify_with_extras(
    text: &str,
    extras: &[ExtraSignature],
    seen: &mut HashSet<(CrashKind, String)>,
    issues: &mut Vec<CrashIssue>,
) -> bool {
    let mut matched_any = false;
    for sig in extras {
        // An entry with no conditions would match every line; refuse rather than
        // let a malformed file fire a diagnosis on a healthy boot.
        if sig.contains.is_empty() {
            continue;
        }
        let mut search_from = 0usize;
        let mut last_end = 0usize;
        let matched = sig.contains.iter().all(|needle| {
            match find_ci(&text[search_from..], needle) {
                Some(i) => {
                    last_end = search_from + i + needle.len();
                    search_from = last_end;
                    true
                }
                None => false,
            }
        });
        if !matched {
            continue;
        }
        matched_any = true;
        let plugin = match sig.capture {
            Capture::None => String::new(),
            Capture::Quoted => quoted_after(text, last_end).unwrap_or("").to_string(),
            Capture::Bare => bare_after(text, last_end).to_string(),
        };
        let key = (sig.kind, plugin.clone());
        if !seen.insert(key) {
            continue;
        }
        issues.push(CrashIssue {
            kind: sig.kind,
            plugin: plugin.clone(),
            message: sig.message.replace("{name}", &plugin),
            fix: sig.fix,
        });
    }
    matched_any
}

/// Load extra signatures from a JSON file.
///
/// Fail-soft at every level, because this runs on the failure path: a missing
/// file, unreadable file, or malformed JSON yields no extras rather than an
/// error that would replace the diagnosis with a complaint about the diagnosis.
/// Entries that do not parse are skipped individually, so one bad signature in
/// an otherwise good file costs only that signature.
///
/// The file is the JSON array itself (`[ {"kind": …}, … ]`) or an object with a
/// `signatures` array; both are accepted because hand-editing is the point.
pub fn load_signatures(path: &std::path::Path) -> Vec<ExtraSignature> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_signatures(&text)
}

/// Parse the signatures document. Split out from [`load_signatures`] so the
/// parsing rules are testable without a filesystem.
pub fn parse_signatures(text: &str) -> Vec<ExtraSignature> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let array = match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut map) => {
            match map.remove("signatures") {
                Some(serde_json::Value::Array(items)) => items,
                _ => return Vec::new(),
            }
        }
        _ => return Vec::new(),
    };
    array
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

/// What the built-in rule table made of one log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// A specific rule recognised the line's shape; it is not offered to the extras.
    Recognised,
    /// Nothing specific matched; a user signature may still claim it.
    Open,
}

fn classify_specific(
    text: &str,
    seen: &mut HashSet<(CrashKind, String)>,
    issues: &mut Vec<CrashIssue>,
) -> Verdict {
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
            return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
    }
    if find_ci(text, "err_unknown_file_extension").is_some() {
        add(
            CrashKind::BundleMismatch,
            "",
            "profile bundle is out of sync with this dsh version (unknown file extension) — reinstall profile deps and restart".to_string(),
            FixAction::Reinstall,
        );
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
            return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
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
        return Verdict::Recognised;
    }
    Verdict::Open
}

/// The generic "some error happened" rule, applied only to a line no specific rule
/// and no user signature recognised.
///
/// Kept out of the specific table on purpose: it matches nearly every `error:` line,
/// so if it ran there it would claim exactly the lines an [`ExtraSignature`] exists to
/// catch (dsh reworded a failure the built-ins no longer know).
fn classify_catch_all(
    text: &str,
    seen: &mut HashSet<(CrashKind, String)>,
    issues: &mut Vec<CrashIssue>,
) {
    let mut add = |kind: CrashKind, plugin: &str, message: String, fix: FixAction| {
        if seen.insert((kind, plugin.to_string())) {
            issues.push(CrashIssue { kind, plugin: plugin.to_string(), message, fix });
        }
    };
    // Any `error:` line that is not connection noise: a port-race refusal is the
    // launcher's own retry case, not a crash cause.
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

    fn diag<S: AsRef<str>>(lines: &[S]) -> Vec<CrashIssue> {
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

    // ---- user-supplied signatures (the escape hatch for reworded dsh output) ----

    fn extra(
        kind: CrashKind,
        fix: FixAction,
        contains: &[&str],
        capture: Capture,
        message: &str,
    ) -> ExtraSignature {
        ExtraSignature {
            kind,
            fix,
            contains: contains.iter().map(|s| (*s).to_string()).collect(),
            capture,
            message: message.to_string(),
        }
    }

    #[test]
    fn extra_signature_catches_reworded_dsh_output() {
        // Stand-in for a dsh release that reworded its missing-bundle failure:
        // no built-in rule matches, which is exactly the rot this exists for.
        let lines = vec!["boot aborted: profile refers to unavailable addon 'dsh-x'".to_string()];
        assert!(diag(&lines).is_empty(), "built-ins must not recognise this");
        let extras = vec![extra(
            CrashKind::MissingBundle,
            FixAction::ExcludeBundle,
            &["profile refers to unavailable addon"],
            Capture::Quoted,
            "addon \"{name}\" is not installed",
        )];
        let out = diagnose_crash_with(&lines, &extras);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CrashKind::MissingBundle);
        assert_eq!(out[0].plugin, "dsh-x");
        assert_eq!(out[0].message, "addon \"dsh-x\" is not installed");
    }

    #[test]
    fn extra_requires_every_contains_in_order() {
        let two = vec![extra(
            CrashKind::CliError,
            FixAction::Restart,
            &["alpha", "beta"],
            Capture::None,
            "both seen",
        )];
        assert_eq!(diagnose_crash_with(&["alpha then beta".to_string()], &two).len(), 1);
        // A conjunction is ordered: "beta … alpha" is not the shape it describes,
        // and one half alone is not a match.
        assert!(diagnose_crash_with(&["beta then alpha".to_string()], &two).is_empty());
        assert!(diagnose_crash_with(&["only alpha".to_string()], &two).is_empty());
    }

    #[test]
    fn capture_modes_read_the_token_after_the_match() {
        let quoted = vec![extra(
            CrashKind::MissingModule,
            FixAction::ExcludeBundle,
            &["addon"],
            Capture::Quoted,
            "quoted {name}",
        )];
        assert_eq!(
            diagnose_crash_with(&["addon \"pkg-a\" failed".to_string()], &quoted)[0].plugin,
            "pkg-a"
        );

        let bare = vec![extra(
            CrashKind::MissingModule,
            FixAction::ExcludeBundle,
            &["addon"],
            Capture::Bare,
            "bare {name}",
        )];
        assert_eq!(
            diagnose_crash_with(&["addon pkg-b failed".to_string()], &bare)[0].plugin,
            "pkg-b"
        );

        // `none` has no name to read; the placeholder still has to disappear.
        let none = vec![extra(
            CrashKind::MissingModule,
            FixAction::ExcludeBundle,
            &["addon"],
            Capture::None,
            "nothing {name}here",
        )];
        let out = diagnose_crash_with(&["addon pkg-b failed".to_string()], &none);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].plugin, "");
        assert_eq!(out[0].message, "nothing here");
    }

    #[test]
    fn builtins_take_precedence_over_extras() {
        // A conflicting signature, deliberately a different kind so a leak shows
        // up as a second issue rather than being hidden by de-duplication.
        let extras = vec![extra(
            CrashKind::CliError,
            FixAction::Restart,
            &["cannot resolve profile bundle"],
            Capture::Quoted,
            "extra wins",
        )];
        let out = diagnose_crash_with(
            &["Error: cannot resolve profile bundle \"dsh-x\"".to_string()],
            &extras,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CrashKind::MissingBundle);
        assert_eq!(out[0].fix, FixAction::ExcludeBundle);
    }

    #[test]
    fn an_extra_claims_an_error_line_the_catchall_would_otherwise_take() {
        let text = "error: EPERM unlinking the profile lock".to_string();
        // Without a signature this is the generic CliError — the catch-all must
        // not get to it first, or every new `error:` wording is unreachable.
        assert_eq!(
            kinds(&[text.as_str()]),
            vec![(CrashKind::CliError, String::new())]
        );
        let extras = vec![extra(
            CrashKind::ToolMissing,
            FixAction::Restart,
            &["eperm unlinking"],
            Capture::None,
            "profile lock is held by another process",
        )];
        let out = diagnose_crash_with(&[text], &extras);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CrashKind::ToolMissing);
    }

    #[test]
    fn extras_dedupe_against_each_other() {
        let extras = vec![
            extra(CrashKind::MissingModule, FixAction::ExcludeBundle, &["addon"], Capture::Bare, "one {name}"),
            extra(CrashKind::MissingModule, FixAction::ExcludeBundle, &["addon"], Capture::Bare, "two {name}"),
        ];
        let out = diagnose_crash_with(&["addon pkg-a".to_string()], &extras);
        assert_eq!(out.len(), 1, "same kind + plugin is reported once");
        assert_eq!(out[0].message, "one pkg-a");
    }

    #[test]
    fn a_signature_with_no_conditions_is_refused() {
        // An empty conjunction would match every line, including a healthy boot's.
        let extras = vec![extra(
            CrashKind::CliError,
            FixAction::Restart,
            &[],
            Capture::None,
            "matches anything",
        )];
        assert!(diagnose_crash_with(&["everything is fine".to_string()], &extras).is_empty());
    }

    #[test]
    fn parse_signatures_accepts_both_document_shapes() {
        let body = r#"{"kind":"cli-error","fix":"restart","contains":["x"],"message":"m"}"#;
        assert_eq!(parse_signatures(&format!("[{body}]")).len(), 1);
        assert_eq!(parse_signatures(&format!("{{\"signatures\":[{body}]}}")).len(), 1);
    }

    #[test]
    fn parse_signatures_reads_capture_and_defaults_it() {
        let with = r#"[{"kind":"missing-module","fix":"exclude-bundle","contains":["a"],"capture":"bare","message":"{name}"}]"#;
        assert_eq!(parse_signatures(with)[0].capture, Capture::Bare);
        let without = r#"[{"kind":"missing-module","fix":"exclude-bundle","contains":["a"],"message":"{name}"}]"#;
        assert_eq!(parse_signatures(without)[0].capture, Capture::None);
    }

    #[test]
    fn parse_signatures_skips_bad_entries_and_bad_documents() {
        let good = r#"{"kind":"cli-error","fix":"restart","contains":["x"],"message":"m"}"#;
        // One unparsable entry costs only that entry.
        let doc = format!("[{good}, {{\"kind\":\"cli-error\"}}, {good}]");
        assert_eq!(parse_signatures(&doc).len(), 2);
        // Fail-soft at the document level: this runs on the failure path, so a
        // broken file must not replace the diagnosis with a complaint about it.
        assert!(parse_signatures("not json").is_empty());
        assert!(parse_signatures("\"a string\"").is_empty());
        assert!(parse_signatures("{\"other\":[]}").is_empty());
        // A misspelled key is a typo, not an extension point.
        let typo = r#"[{"kind":"cli-error","fix":"restart","contains":["x"],"message":"m","capture2":"bare"}]"#;
        assert!(parse_signatures(typo).is_empty());
    }

    #[test]
    fn load_signatures_is_fail_soft_on_a_missing_file() {
        let missing = std::env::temp_dir().join("ahl-crash-signatures-does-not-exist.json");
        let _ = std::fs::remove_file(&missing);
        assert!(load_signatures(&missing).is_empty());

        let path =
            std::env::temp_dir().join(format!("ahl-crash-sig-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"[{"kind":"cli-error","fix":"restart","contains":["boom"],"message":"m"}]"#,
        )
        .expect("write temp signature file");
        assert_eq!(load_signatures(&path).len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
