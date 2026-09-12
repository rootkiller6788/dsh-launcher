//! The app's one zip writer.
//!
//! Two features ship a zip to the user: the environment package (`.dshenv`, a
//! manifest only) and the diagnostic package (logs and reports). They contain
//! different things, but the *container* is the same decision every time —
//! deflate, entry names as paths, deterministic ordering — and a second writer
//! would be a second place for that decision to drift. The same reasoning as
//! "one reader per fact" that the health panel follows.
//!
//! Callers own the entry names and the bytes; this module owns the format.

use std::io::{Cursor, Write};

use anyhow::Context;
use zip::write::SimpleFileOptions;

use crate::error::AppError;

/// Build a zip from `(entry name, contents)` pairs, in the order given.
///
/// Entry names are paths inside the archive, so a section that belongs in a
/// subdirectory writes `sub/name.txt`. Deflate is used throughout: everything
/// here is text, and the packages are meant to be small enough to email.
pub(crate) fn write_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, AppError> {
    let mut out = Cursor::new(Vec::new());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut zip = zip::ZipWriter::new(&mut out);
    for (name, body) in entries {
        zip.start_file(*name, options)
            .with_context(|| format!("start {name}"))?;
        zip.write_all(body)
            .with_context(|| format!("write {name}"))?;
    }
    zip.finish().context("finish zip")?;
    Ok(out.into_inner())
}
