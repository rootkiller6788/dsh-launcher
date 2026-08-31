//! Shell-side diagnostics.
//!
//! A `tracing_subscriber` that writes every event to a per-day `dsh-YYYY-MM-DD.log`
//! file, mirrors warn/error events into `dsh-YYYY-MM-DD.error.log` (the same split
//! dsh-desktop's `LogFileSink` / `log-level.ts` `isErrorType` applies), and echoes
//! to stderr for dev visibility.
//!
//! Only day-rolling is implemented here; the size / directory caps from the
//! reference `LogFileSink` are a later enhancement.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Local calendar date in `YYYY-MM-DD` (mirrors `localDateSuffix` in `log-files.ts`).
fn local_date_suffix() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// One append-only line writer for the current day's file.
///
/// Opens the file on every `write` (like the reference `appendFileSync` per-line
/// sink) and serializes with a shared lock so concurrent threads never interleave
/// a line. Appends are atomic on Windows (`FILE_APPEND_DATA`) and on Unix
/// (`O_APPEND`), so reopening per event is safe.
struct RollingLineWriter {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Write for RollingLineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => f,
            // Never take the subscriber down over a log write.
            Err(_) => return Ok(buf.len()),
        };
        match file.write(buf) {
            Ok(n) => Ok(n),
            Err(_) => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `tracing_subscriber::fmt::MakeWriter` that targets the day-rolling file set.
struct DailyFileWriter {
    dir: PathBuf,
    error: bool,
    lock: Arc<Mutex<()>>,
}

impl tracing_subscriber::fmt::MakeWriter<'_> for DailyFileWriter {
    type Writer = RollingLineWriter;

    fn make_writer(&self) -> Self::Writer {
        let date = local_date_suffix();
        let name = format!("dsh-{date}{}.log", if self.error { ".error" } else { "" });
        RollingLineWriter {
            path: self.dir.join(name),
            lock: self.lock.clone(),
        }
    }
}

/// Install the shell logger rooted at `app_data_dir/logs`. Idempotent: a second
/// call (e.g. a re-initialized test) is ignored by `try_init`.
pub fn init(app_data_dir: &Path) {
    let log_dir = app_data_dir.join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let lock = Arc::new(Mutex::new(()));

    let all = DailyFileWriter { dir: log_dir.clone(), error: false, lock: lock.clone() };
    let error = DailyFileWriter { dir: log_dir.clone(), error: true, lock };
    let stderr_writer = io::stderr;

    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let _ = tracing_subscriber::registry()
        // Everything -> dsh-YYYY-MM-DD.log
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(all)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
        )
        // warn + error -> dsh-YYYY-MM-DD.error.log
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(error)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::WARN),
        )
        // Mirror to the terminal (color only here).
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(stderr_writer)
                .with_ansi(true)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
        )
        .try_init();
}
