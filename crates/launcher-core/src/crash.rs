//! Crash reporting — a panic hook that writes a standalone crash report.
//!
//! The human-readable report lands in `logs/crash-<timestamp>.txt` (under the
//! app data dir) next to the rolling launcher log. It carries the panic
//! message, the source location, and a captured backtrace, then re-invokes the
//! previous hook so the default stderr print still happens and the panic
//! propagates as usual.
//!
//! When telemetry consent is on (`telemetry_consent`), a second, *minimal*
//! sidecar `crash-<timestamp>.json` is written alongside — only the panic
//! message, source location, thread, and occurred-at time. That sidecar is the
//! only input the telemetry uploader (`telemetry.rs`) ever reads: with consent
//! off, no uploadable crash data exists on disk at all.

use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::telemetry::CrashEvent;

/// Install the panic hook. Safe to call once at startup; a second call replaces
/// the first and keeps chaining through `take_hook`.
pub fn install_panic_hook(logs_dir: std::path::PathBuf, telemetry_consent: Arc<AtomicBool>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = payload_str(info);
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let occurred_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let ts = timestamp();
        // Consent can flip mid-session via the Preferences toggle; read it live
        // so a crash honours the choice in effect at crash time.
        if telemetry_consent.load(Ordering::Relaxed) {
            let _ = write_sidecar(
                &logs_dir,
                &ts,
                &payload,
                &location,
                thread_name,
                occurred_at,
            );
        }
        write_report(
            &logs_dir,
            &ts,
            &payload,
            &location,
            thread_name,
            &std::backtrace::Backtrace::force_capture().to_string(),
        );
        previous(info);
    }));
}

/// Format `now_utc()` as a filename-safe timestamp (`YYYY-MM-DD_HH-MM-SS`).
fn timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    )
}

/// Write one human-readable crash report to `logs/crash-<ts>.txt`. Never
/// panics — a failing write only falls back to stderr so the hook can't turn a
/// crash into a different crash. Returns the report path on success.
fn write_report(
    logs_dir: &std::path::Path,
    ts: &str,
    payload: &str,
    location: &str,
    thread: &str,
    backtrace: &str,
) -> Option<std::path::PathBuf> {
    let report = format!(
        "panic: {payload}\nlocation: {location}\nthread: {thread}\n\nbacktrace:\n{backtrace}\n"
    );
    let path = logs_dir.join(format!("crash-{ts}.txt"));
    write_to(&path, report.as_bytes())
}

/// Write the minimal telemetry sidecar to `logs/crash-<ts>.json`. Same
/// never-panic contract as `write_report`.
fn write_sidecar(
    logs_dir: &std::path::Path,
    ts: &str,
    payload: &str,
    location: &str,
    thread: &str,
    occurred_at: i64,
) -> Option<std::path::PathBuf> {
    let event = CrashEvent {
        occurred_at,
        message: payload.to_string(),
        location: location.to_string(),
        thread: thread.to_string(),
    };
    let Ok(body) = serde_json::to_vec(&event) else {
        return None;
    };
    let path = logs_dir.join(format!("crash-{ts}.json"));
    write_to(&path, &body)
}

/// Create parent dirs, write bytes, sync — with only stderr on failure.
fn write_to(path: &std::path::Path, bytes: &[u8]) -> Option<std::path::PathBuf> {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("crash report: cannot create {} ({e})", dir.display());
            return None;
        }
    }
    match std::fs::File::create(path).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    }) {
        Ok(()) => {
            eprintln!("crash report written to {}", path.display());
            Some(path.to_path_buf())
        }
        Err(e) => {
            eprintln!("crash report write failed for {} ({e})", path.display());
            None
        }
    }
}

/// The panic payload as a string — `&str` and `String` cover the normal cases;
/// anything else gets a type name so the report still says something useful.
fn payload_str(info: &PanicHookInfo) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        format!("<non-string payload: {:?}>", info.payload().type_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_filename_safe() {
        let a = timestamp();
        assert!(!a.contains(':'), "colons break Windows filenames: {a}");
        assert!(
            !a.contains('/') && !a.contains('\\'),
            "slashes break filenames: {a}"
        );
        assert_eq!(a.len(), "YYYY-MM-DD_HH-MM-SS".len());
        let year: i32 = a[..4].parse().expect("year");
        assert!((2020..=2100).contains(&year), "unexpected year {year}");
    }

    #[test]
    fn write_report_creates_crash_file() {
        let dir = std::env::temp_dir().join(format!("ahl-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_report(
            &dir,
            "2026-01-01_00-00-00",
            "boom",
            "src/main.rs:1:1",
            "main",
            "frame0",
        )
        .expect("report written");
        assert!(path.starts_with(&dir));
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(text.contains("panic: boom"));
        assert!(text.contains("location: src/main.rs:1:1"));
        assert!(text.contains("thread: main"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_written_only_under_consent() {
        let dir = std::env::temp_dir().join(format!("ahl-crashsidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No consent → only the .txt exists; nothing uploadable on disk.
        let ts = "2026-01-02_03-04-05";
        write_report(&dir, ts, "boom", "l", "main", "bt");
        assert!(!dir.join(format!("crash-{ts}.json")).exists());
        // Consent → both files; the sidecar carries exactly the minimal fields.
        let sidecar = write_sidecar(&dir, ts, "boom", "l:1:1", "main", 42).expect("sidecar");
        assert_eq!(sidecar, dir.join(format!("crash-{ts}.json")));
        let parsed: CrashEvent =
            serde_json::from_slice(&std::fs::read(sidecar).expect("readable")).expect("parse");
        assert_eq!(parsed.message, "boom");
        assert_eq!(parsed.location, "l:1:1");
        assert_eq!(parsed.thread, "main");
        assert_eq!(parsed.occurred_at, 42);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
