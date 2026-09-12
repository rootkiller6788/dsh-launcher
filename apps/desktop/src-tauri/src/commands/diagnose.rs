//! The redacted diagnostic package (absorb-plan 2.6).
//!
//! A user who wants help has to send someone a description of a machine nobody
//! else can see. This turns that into one zip: the environment, the launcher's
//! own state, the log tails, and a summary of the coded failures — with the
//! user's home directory and every secret value masked out on the way
//! ([`launcher_core::diagnose::clean`]).
//!
//! Ported from `1/`'s `DiagnoseExport`, which had the same shape. Two things
//! were changed on the way in:
//!
//! - **Nothing is dumped wholesale.** `1/` collected a fixed list of environment
//!   variables and AHL collects a different fixed list, but neither dumps the
//!   process environment: `PATH`, `*_TOKEN`, and whatever the shell happens to
//!   export are how a "safe" archive stops being safe.
//! - **The failure summary carries the next action.** `1/` printed a code and a
//!   description; AHL already has a code → title → *next action* table
//!   ([`launcher_core::ErrorCode`]) used by the banner and the Activity log, so
//!   the package can say what to do about each failure instead of only naming it.
//!
//! What is never in the package, and why:
//!
//! | excluded | why |
//! |---|---|
//! | `.credentials.yaml`, the provider vault | credentials |
//! | MCP environment **values** | they are the secrets; key names only |
//! | session transcripts, usage records | private conversation content |
//! | plugin / skill payloads, `node_modules` | size, and not diagnostic |
//!
//! The package is written where the user asked for it and is never uploaded by
//! the launcher. Crash telemetry is separate, opt-in, and off by default.

use std::path::Path;

use dsh_adapter::diagnostics::diagnose_profile;
use dsh_adapter::health::health_report;
use dsh_adapter::rescue::snapshot_status;
use launcher_core::diagnose::{clean, read_shared_tail, summarize_errors, tail_lines, user_home};
use launcher_core::{diagnostics::check_tool, InstanceManifest, RuntimeAdapter};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::paths::{downloads_dir, slug};
use crate::commands::settings::settings_snapshot;
use crate::error::AppError;
use crate::jobs::{run_instance_job, HeavyJobKind};
use crate::state::AppState;

/// How much of `launcher.log` a package carries, and how much of it is read.
///
/// Bounded on both sides: the read is bounded so a multi-gigabyte log does not
/// have to be slurped to produce a 2000-line tail, and the tail is bounded so
/// the package stays small enough to attach to a message.
const LOG_TAIL_LINES: usize = 2000;
const LOG_READ_BYTES: u64 = 4 * 1024 * 1024;

/// How many crash reports to include, newest first.
const MAX_CRASH_REPORTS: usize = 5;

/// Crash reports older than this are left out.
///
/// A package is about the failure that just happened; a report from three weeks
/// ago is noise, and it is the one section whose size is not bounded by a
/// constant.
const CRASH_REPORT_MAX_AGE_SECS: u64 = 14 * 24 * 60 * 60;

/// One line handed over from the Activity panel.
///
/// The launcher's coded failures (`[E2001] message`) exist only in that
/// in-memory view: `emit_log_at` streams them to the frontend and never writes
/// them to a file. So the frontend has to supply them for the package to be able
/// to summarise anything — reading only from disk would produce an empty
/// `errors.txt` and look like a healthy machine.
#[derive(Debug, Clone, Deserialize)]
pub struct ActivityLine {
    pub level: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExportResult {
    /// Where the package landed.
    pub path: String,
    pub bytes: u64,
    /// How many files are inside it.
    pub entries: usize,
    /// How many distinct coded failures the summary found — `0` means the
    /// supplied Activity lines had none, not that nothing was collected.
    pub coded_failures: usize,
}

/// Write a redacted diagnostic package next to the user's other downloads.
///
/// Read-only with respect to the instance: it opens files, and writes only the
/// archive. It still takes the heavy-job gate, for the same reason
/// `profile_diagnostics` does — a profile read that lands in the middle of an
/// install would describe a half-written state as if it were the whole truth.
#[tauri::command]
pub async fn export_diagnostics(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    activity: Vec<ActivityLine>,
) -> Result<DiagnosticsExportResult, AppError> {
    let job_id = id.clone();
    let settings = settings_snapshot(&state)?;
    run_instance_job(&state, &app, &job_id, HeavyJobKind::Diagnostics, || async {
        // Loaded once, and passed down: the name below and every section that
        // describes the instance must be of the same read.
        let instance = InstanceManifest::get(&state.paths, &id)?;
        let sections = collect(&state, &settings, &instance, &activity)?;
        let coded_failures = sections.coded_failures;
        let entries = sections.files.len();

        let path = downloads_dir().join(format!(
            "ahl-diagnose-{}-{}.zip",
            slug(&instance.name, "instance"),
            launcher_core::now_secs()
        ));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::msg(format!("create {}: {e}", parent.display())))?;
        }
        let bytes = write_package(&sections.files)?;
        std::fs::write(&path, &bytes)
            .map_err(|e| AppError::msg(format!("write {}: {e}", path.display())))?;

        Ok(DiagnosticsExportResult {
            path: path.display().to_string(),
            bytes: bytes.len() as u64,
            entries,
            coded_failures,
        })
    })
    .await
}

/// The finished package: files ready to zip, plus the count the UI reports.
pub(crate) struct Package {
    /// `(entry name, contents)`, in the order they should appear.
    pub files: Vec<(String, String)>,
    pub coded_failures: usize,
}

/// Build every section of the package.
///
/// Separate from the command so a failed boot can dump the same package without
/// a frontend (see `commands::process`): the evidence for a crash is most
/// valuable exactly when nobody has opened a dialog.
pub(crate) fn collect(
    state: &AppState,
    settings: &launcher_core::settings::AppSettings,
    instance: &InstanceManifest,
    activity: &[ActivityLine],
) -> Result<Package, AppError> {
    let profile_dir = dsh_adapter::DshAdapter::profile_dir(instance);
    let rescue_dir = state.paths.rescue_dir(&instance.id);
    let home = user_home();
    let mask = |text: &str| clean(text, home.as_deref());

    // Masked as one block rather than a line at a time: a secret cannot span a
    // line boundary, so the result is the same, and one pass over the joined text
    // is cheaper than one per line.
    let activity_body = mask(&activity_body(activity));

    // Ordered the way a reader works through them: what this is, what the machine
    // is, what the launcher thinks, what happened, and the summary.
    let errors_body = mask(&summarize_errors(activity.iter().map(|l| l.line.as_str())));
    let mut files: Vec<(String, String)> = vec![
        ("README.md".into(), README.to_string()),
        (
            "env.txt".into(),
            mask(&env_section(state, settings, instance, &profile_dir)),
        ),
        (
            "state.txt".into(),
            mask(&state_section(state, settings, instance, &rescue_dir)),
        ),
        ("activity.txt".into(), activity_body),
        ("errors.txt".into(), errors_body),
    ];

    // The launcher's own tracing log. Read through a shared handle: something
    // else is writing it, and a package that can only be produced while the app
    // is closed is a package nobody produces.
    let log = state.paths.launcher_log.clone();
    let log_body = match read_shared_tail(&log, LOG_READ_BYTES) {
        Ok(Some(text)) => mask(&tail_lines(&text, LOG_TAIL_LINES)),
        Ok(None) => format!("(no launcher log at {})\n", mask(&log.display().to_string())),
        Err(e) => format!("(launcher log unreadable: {e})\n"),
    };
    files.push(("log.txt".into(), log_body));

    for (name, body) in crash_reports(&state.paths.logs, MAX_CRASH_REPORTS) {
        files.push((format!("crashes/{name}"), mask(&body)));
    }

    // Counted from the same text the package carries, so the number the UI shows
    // and the section a reader opens cannot disagree. Every code block starts
    // with `[<code>]`, and the "none found" placeholder does not.
    let coded_failures = files
        .iter()
        .find(|(name, _)| name == "errors.txt")
        .map(|(_, body)| body.lines().filter(|l| l.starts_with('[')).count())
        .unwrap_or(0);

    Ok(Package {
        files,
        coded_failures,
    })
}

/// Zip a package's files.
pub(crate) fn write_package(files: &[(String, String)]) -> Result<Vec<u8>, AppError> {
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, body)| (name.as_str(), body.as_bytes()))
        .collect();
    crate::zip::write_zip(&entries)
}

/// What the package is, what was masked, and what was left out.
///
/// In the package rather than only in the code because the person who receives
/// it is not the person who built it, and "is this safe to forward?" should be
/// answerable by reading the archive.
const README: &str = "\
# AI Harness Launcher — diagnostic package

Produced by the launcher's \"Export diagnostic package\" action, or written
automatically under `diagnostics/` after a boot failed.

## Contents

| file | what it is |
|---|---|
| `env.txt` | OS, launcher version, tool versions, the environment variables that change what gets launched, and the paths in play |
| `state.txt` | the instance manifest, settings, rescue-point status, health report, and profile diagnostics |
| `activity.txt` | the Activity panel's warnings and errors |
| `errors.txt` | those lines summarised by error code, with the code's meaning and its recommended next action |
| `log.txt` | the tail of the launcher's own log |
| `crashes/` | recent launcher crash reports, if any |

## What was removed

- The user's home directory and username, replaced with `%USER%` / `USERNAME`.
- Secret *values*: anything after a key named token, secret, password,
  authorization, api key, or similar is replaced with `***`.
- MCP environment variables appear as **names only** — never values.

## What was never collected

Credentials, session transcripts, usage records, plugin or skill payloads, and
`node_modules`. The launcher does not upload this file anywhere; sending it is
the user's decision.
";

/// The Activity lines as text, oldest first, one per line.
///
/// Levels are kept because a warning and an error read very differently in a
/// package, and the level is one of the few facts the frontend holds that the
/// backend cannot reconstruct from disk.
fn activity_body(activity: &[ActivityLine]) -> String {
    if activity.is_empty() {
        return "(the Activity panel had no warnings or errors to include)\n".to_string();
    }
    let mut out = String::new();
    for line in activity {
        out.push_str(&format!("[{}] {}\n", line.level, line.line));
    }
    out
}

/// Environment facts: what the machine is, what the launcher is, what it found.
fn env_section(
    state: &AppState,
    settings: &launcher_core::settings::AppSettings,
    instance: &InstanceManifest,
    profile_dir: &Path,
) -> String {
    let mut out = String::new();
    out.push_str("== machine ==\n");
    out.push_str(&format!("os: {} ({})\n", std::env::consts::OS, std::env::consts::ARCH));
    if let Some(version) = sysinfo::System::long_os_version() {
        out.push_str(&format!("osVersion: {version}\n"));
    }
    out.push_str(&format!("launcherVersion: {}\n", env!("CARGO_PKG_VERSION")));

    out.push_str("\n== tools ==\n");
    let node = check_tool("node", "--version");
    out.push_str(&format!("node: {}\n", describe(&node)));
    let git = check_tool("git", "--version");
    out.push_str(&format!("git: {}\n", describe(&git)));
    match state.adapter.detect(settings) {
        Ok(info) => {
            out.push_str(&format!(
                "dsh: {} ({} from {}), node {}\n",
                info.version, info.bin_path, info.source, info.node_version
            ));
            if let Some(path) = &info.node_path {
                out.push_str(&format!("dshNode: {path}\n"));
            }
        }
        Err(e) => out.push_str(&format!("dsh: not resolvable — {e}\n")),
    }
    // What the *launcher* would resolve, which is the pair that actually gets
    // launched and can differ from what `detect` reports.
    out.push_str(&format!(
        "resolvedNode: {}\n",
        state
            .adapter
            .resolve_node(settings)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into())
    ));

    out.push_str("\n== environment ==\n");
    out.push_str("(only the variables that change what gets launched)\n");
    // A named list, never the whole environment: `PATH`, `*_TOKEN`, and whatever
    // else the shell exports is how an archive stops being safe to send.
    for key in [
        "AHL_HOME",
        "AHL_PORTABLE",
        "AHL_CONTENT_URL",
        "DSH_CLI_BIN",
        "DSH_HOME",
        "DSH_WEB_PORT",
        "USERPROFILE",
        "LOCALAPPDATA",
    ] {
        match std::env::var(key) {
            Ok(v) if !v.trim().is_empty() => out.push_str(&format!("{key}={v}\n")),
            _ => out.push_str(&format!("{key}=\n")),
        }
    }

    out.push_str("\n== paths ==\n");
    out.push_str(&format!("dataRoot: {}\n", state.paths.root.display()));
    out.push_str(&format!("instanceId: {}\n", instance.id));
    out.push_str(&format!("workspace: {}\n", instance.workspace));
    out.push_str(&format!("profile: {}\n", profile_dir.display()));
    out.push_str(&format!("logs: {}\n", state.paths.logs.display()));
    out
}

/// Launcher state: what the instance *is*, and what the launcher thinks of it.
fn state_section(
    state: &AppState,
    settings: &launcher_core::settings::AppSettings,
    instance: &InstanceManifest,
    rescue_dir: &Path,
) -> String {
    let mut out = String::new();
    let mut section = |name: &str, body: String| {
        out.push_str(&format!("--- {name} ---\n{body}\n"));
    };

    section(
        "settings",
        serde_json::to_string_pretty(settings).unwrap_or_else(|e| format!("(unserialisable: {e})")),
    );
    section(
        "instance",
        serde_json::to_string_pretty(instance).unwrap_or_else(|e| format!("(unserialisable: {e})")),
    );
    section(
        "rescuePoint",
        serde_json::to_string_pretty(&snapshot_status(rescue_dir))
            .unwrap_or_else(|e| format!("(unserialisable: {e})")),
    );
    section(
        "health",
        serde_json::to_string_pretty(&health_report(
            &state.adapter,
            settings,
            instance,
            rescue_dir,
        ))
        .unwrap_or_else(|e| format!("(unserialisable: {e})")),
    );
    section(
        "profileDiagnostics",
        serde_json::to_string_pretty(&diagnose_profile(instance))
            .unwrap_or_else(|e| format!("(unserialisable: {e})")),
    );
    out
}

/// `present` + version + note, on one line.
fn describe(item: &launcher_core::diagnostics::EnvItem) -> String {
    if !item.present {
        return item
            .note
            .clone()
            .unwrap_or_else(|| "not found".to_string());
    }
    match &item.version {
        Some(v) => v.clone(),
        None => "present".to_string(),
    }
}

/// Recent crash reports from the logs directory, newest first.
///
/// Returns `(file name, contents)`. Bounded by both count and age: this is the
/// one section whose size is not set by a constant, and a package that carries
/// twenty megabytes of old panics helps nobody. Newest-first by name is enough
/// because the name is `crash-<timestamp>`, so the lexical order is the time
/// order.
fn crash_reports(logs_dir: &Path, max: usize) -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir(logs_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.starts_with("crash-") && (n.ends_with(".txt") || n.ends_with(".json")))
        .collect();
    names.sort();
    names.reverse();

    let newest = names.first().and_then(|n| crash_timestamp(n));
    names
        .into_iter()
        .filter(|n| match (crash_timestamp(n), newest) {
            (Some(at), Some(newest)) => newest.saturating_sub(at) <= CRASH_REPORT_MAX_AGE_SECS,
            // A name we cannot date is kept: dropping it would silently omit the
            // report we failed to parse, which is the one worth seeing.
            _ => true,
        })
        .take(max)
        .filter_map(|name| {
            let text = std::fs::read_to_string(logs_dir.join(&name)).ok()?;
            Some((name, text))
        })
        .collect()
}

/// The epoch seconds in a `crash-<ts>.txt` / `.json` name.
fn crash_timestamp(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("crash-")?;
    let digits = rest.split('.').next()?;
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_core::diagnostics::EnvItem;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-diagnose-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn line(level: &str, line: &str) -> ActivityLine {
        ActivityLine {
            level: level.into(),
            line: line.into(),
        }
    }

    #[test]
    fn activity_lines_carry_their_level() {
        let body = activity_body(&[line("warn", "slow"), line("error", "[E1003] dsh exited 1")]);
        assert_eq!(body, "[warn] slow\n[error] [E1003] dsh exited 1\n");
    }

    #[test]
    fn an_empty_activity_buffer_says_so_rather_than_looking_clean() {
        // Distinguishing "nothing failed" from "nothing was supplied" matters:
        // the package is evidence, and a silent empty file reads as the former.
        assert!(activity_body(&[]).contains("no warnings or errors"));
    }

    /// A stand-in "now", so the age window is exercised without a clock.
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn crash_reports_keep_the_newest_and_drop_the_ancient() {
        let dir = tmp_dir("crashes");
        let recent = NOW - 86_400; // a day old: inside the window
        let ancient = NOW - 30 * 86_400; // a month old: outside it
        for ts in [ancient, recent, NOW] {
            std::fs::write(dir.join(format!("crash-{ts}.txt")), format!("report {ts}")).unwrap();
        }
        // Unrelated files in the same folder are not crash reports.
        std::fs::write(dir.join("launcher.log"), "not a report").unwrap();

        let reports = crash_reports(&dir, MAX_CRASH_REPORTS);
        let names: Vec<&str> = reports.iter().map(|(n, _)| n.as_str()).collect();
        let newest = format!("crash-{NOW}.txt");
        let day_old = format!("crash-{recent}.txt");
        // Newest first, the ancient one gone, the ordinary log never included.
        assert_eq!(names, vec![newest.as_str(), day_old.as_str()]);
        assert_eq!(reports[0].1, format!("report {NOW}"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_reports_are_capped_by_count() {
        let dir = tmp_dir("capped");
        for offset in 0..5u64 {
            std::fs::write(dir.join(format!("crash-{}.txt", NOW + offset)), "x").unwrap();
        }
        assert_eq!(crash_reports(&dir, 3).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_undatable_crash_report_is_kept_rather_than_dropped() {
        let dir = tmp_dir("undatable");
        // A name we cannot parse must not be silently omitted — it is the one
        // most likely to be worth looking at.
        std::fs::write(dir.join("crash-torn-name.txt"), "kept").unwrap();
        std::fs::write(dir.join(format!("crash-{NOW}.txt")), "kept too").unwrap();
        let names: Vec<String> = crash_reports(&dir, MAX_CRASH_REPORTS)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(names.contains(&"crash-torn-name.txt".to_string()), "{names:?}");
        assert!(
            names.contains(&format!("crash-{NOW}.txt")),
            "{names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_logs_directory_is_not_an_error() {
        let dir = tmp_dir("absent");
        assert!(crash_reports(&dir.join("nope"), MAX_CRASH_REPORTS).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_timestamps_are_read_from_the_name() {
        assert_eq!(crash_timestamp("crash-1757654321.txt"), Some(1757654321));
        assert_eq!(crash_timestamp("crash-1757654321.json"), Some(1757654321));
        assert_eq!(crash_timestamp("crash-panic.txt"), None);
        assert_eq!(crash_timestamp("launcher.log"), None);
    }

    #[test]
    fn describe_reports_absence_with_the_note() {
        let missing = EnvItem {
            name: "git".into(),
            present: false,
            version: None,
            note: Some("not on PATH".into()),
        };
        assert_eq!(describe(&missing), "not on PATH");

        let found = EnvItem {
            name: "node".into(),
            present: true,
            version: Some("v22.1.0".into()),
            note: None,
        };
        assert_eq!(describe(&found), "v22.1.0");
    }

    #[test]
    fn the_package_zips_its_entries_under_the_names_it_was_given() {
        let files = vec![
            ("env.txt".to_string(), "a=1\n".to_string()),
            ("crashes/crash-1.txt".to_string(), "boom".to_string()),
        ];
        let bytes = write_package(&files).expect("zip");
        assert!(bytes.starts_with(b"PK"), "not a zip");

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("open");
        assert_eq!(archive.len(), 2);
        // Subdirectory names are preserved as paths inside the archive.
        archive.by_name("crashes/crash-1.txt").expect("nested entry");
    }

    #[test]
    fn coded_failures_are_counted_from_the_section_the_reader_opens() {
        // The count the UI shows and the file the reader opens are the same text,
        // so they cannot disagree.
        let activity = vec![
            line("error", "[E1003] dsh exited 1"),
            line("error", "[E1003] again"),
            line("error", "[E2001] npm unreachable"),
            line("warn", "unrelated"),
        ];
        let summary = summarize_errors(activity.iter().map(|l| l.line.as_str()));
        let counted = summary.lines().filter(|l| l.starts_with('[')).count();
        assert_eq!(counted, 2, "two distinct codes:\n{summary}");

        let empty = summarize_errors(Vec::<&str>::new());
        assert_eq!(empty.lines().filter(|l| l.starts_with('[')).count(), 0);
    }
}
