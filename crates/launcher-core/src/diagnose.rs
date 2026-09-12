//! The text half of the redacted diagnostic package (absorb-plan 2.6).
//!
//! A diagnostic package is a zip the user hands to someone else when asking for
//! help, so it is the one artifact that leaves the machine on purpose. Two
//! things therefore have to be true of it, and this module exists to make them
//! hard to get wrong:
//!
//! 1. **Nothing identifying leaves.** Two distinct leaks are handled —
//!    [`mask_user_paths`] replaces the user's home directory and username with
//!    placeholders (ported from `1/`'s `DiagnoseExport.Sanitize`), and
//!    [`redact_secrets`](crate::redact::redact_secrets) masks secret *values*.
//!    [`clean`] runs both, and is the only thing call sites should use: a call
//!    site that picks one of the two has already lost.
//! 2. **The package is still worth reading.** A masked-to-uselessness dump helps
//!    nobody, so the sections keep paths, versions, and messages — only the
//!    parts that identify *this user* are replaced.
//!
//! What never goes in is a policy of the command layer, not of this module, but
//! it is written down here because this is where a reader will look: no
//! `.credentials.yaml`, no session content, no MCP environment *values* (key
//! names only, same rule as the health checks), no plugin or skill payloads.
//!
//! Zip assembly is deliberately **not** here. It needs the `zip` crate, which
//! only the desktop app depends on; the app also owns the decision of where the
//! package lands. This module stays pure text so it can be tested without a
//! filesystem or a GUI.
//!
//! As elsewhere in this crate, matching is hand-rolled against a lowered copy
//! rather than pulling in `regex` (see `redact.rs` for the same reasoning).
//! Lowering is ASCII-only and therefore byte-length preserving, so offsets found
//! in the lowered copy are valid in the original.

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error_code::ErrorCode;
use crate::redact::redact_secrets;

/// What the user's home directory is replaced with.
pub const USER_PLACEHOLDER: &str = "%USER%";

/// What the bare username is replaced with when it appears as a path segment.
pub const USERNAME_PLACEHOLDER: &str = "USERNAME";

/// The current user's home directory, as the OS reports it.
///
/// `USERPROFILE` is preferred on Windows: `HOME` is frequently set to a
/// *different* value by Git Bash / MSYS shells, and masking the wrong directory
/// would leave the real one in place.
pub fn user_home() -> Option<PathBuf> {
    let keys: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    keys.iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Replace the user's home directory and username with placeholders.
///
/// Four replacements, applied in this order (all case-insensitive, matching
/// `1/`'s ordering so the same text masks the same way):
///
/// 1. the home directory itself → `%USER%`
/// 2. a literal `%USERPROFILE%` → `%USER%`
/// 3. a `~\` prefix → `%USER%\`
/// 4. `\<username>\` → `\USERNAME\`
///
/// Rule 4 exists because the first three only catch the forms the OS and the
/// shell produce. A username still survives in a short path, a profile name, or
/// a hand-written path that never included the full home directory. It requires
/// a separator on **both** sides, so it cannot fire on the username appearing as
/// an ordinary word.
///
/// Rule 3 deliberately covers only the backslash form. A bare `~` is left alone:
/// it is as likely to be prose or a version range as a path.
pub fn mask_user_paths<'a>(input: &'a str, home: &Path) -> Cow<'a, str> {
    let mut out: Option<String> = None;

    // 1) the home directory, in whichever separator style it was written.
    let home = home.as_os_str().to_string_lossy();
    let home = home.trim_end_matches(is_separator);
    if !home.is_empty() {
        // Both separator styles, at both ends: the separators *inside* the home
        // path and the one that *follows* it vary independently, so a forward-
        // slash spelling of a backslash-stored home (`C:/Users/ada/…`) would slip
        // through if only the trailing separator were varied.
        let mut spellings: Vec<String> = Vec::new();
        for separator in ['\\', '/'] {
            let spelled: String = home
                .chars()
                .map(|c| if is_separator(c) { separator } else { c })
                .collect();
            if !spellings.contains(&spelled) {
                spellings.push(spelled);
            }
        }
        for spelled in &spellings {
            for separator in ['\\', '/'] {
                let needle = format!("{spelled}{separator}");
                let with = format!("{USER_PLACEHOLDER}{separator}");
                if let Some(next) = replace_ci(current(&out, input), &needle, &with) {
                    out = Some(next);
                }
            }
        }
        // The home directory at the very end of the text has no separator after it.
        for spelled in &spellings {
            if let Some(next) = replace_ci(current(&out, input), spelled, USER_PLACEHOLDER) {
                out = Some(next);
            }
        }
    }

    // 2) the environment-variable spelling of the same thing.
    if let Some(next) = replace_ci(current(&out, input), "%USERPROFILE%", USER_PLACEHOLDER) {
        out = Some(next);
    }

    // 3) the tilde shorthand.
    if let Some(next) = replace_ci(current(&out, input), "~\\", &format!("{USER_PLACEHOLDER}\\")) {
        out = Some(next);
    }

    // 4) a bare username between two separators.
    if let Some(name) = home.rsplit(is_separator).next().filter(|n| !n.is_empty()) {
        let needle = format!("\\{name}\\");
        let with = format!("\\{USERNAME_PLACEHOLDER}\\");
        if let Some(next) = replace_ci(current(&out, input), &needle, &with) {
            out = Some(next);
        }
    }

    match out {
        Some(s) => Cow::Owned(s),
        None => Cow::Borrowed(input),
    }
}

/// The text as it stands after the replacements applied so far.
fn current<'a>(out: &'a Option<String>, input: &'a str) -> &'a str {
    out.as_deref().unwrap_or(input)
}

/// Both separator styles, since a Windows path reaches a log as whichever one
/// the writer happened to use.
fn is_separator(c: char) -> bool {
    c == '\\' || c == '/'
}

/// `input` with every case-insensitive occurrence of `needle` replaced by
/// `with`. Returns `None` when nothing matched, so callers can keep the
/// original borrow and skip an allocation.
fn replace_ci(input: &str, needle: &str, with: &str) -> Option<String> {
    if needle.is_empty() {
        return None;
    }
    let lowered = input.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();

    let mut out: Option<String> = None;
    let mut copied = 0usize;
    while let Some(at) = lowered[copied..].find(&needle) {
        let start = copied + at;
        let end = start + needle.len();
        let buf = out.get_or_insert_with(|| String::with_capacity(input.len()));
        buf.push_str(&input[copied..start]);
        buf.push_str(with);
        copied = end;
    }
    let buf = out.as_mut()?;
    buf.push_str(&input[copied..]);
    out
}

/// Everything a package section must pass through before it is written.
///
/// Paths first, then secrets: the two scanners look for different things, so the
/// order only matters in that both must run. `home` being `None` (no home
/// directory could be determined) skips masking rather than guessing — the
/// secret scan still runs, because a leaked credential is worse than a leaked
/// username.
pub fn clean(input: &str, home: Option<&Path>) -> String {
    let masked = match home {
        Some(home) => mask_user_paths(input, home).into_owned(),
        None => input.to_string(),
    };
    redact_secrets(&masked).into_owned()
}

/// The last `max` lines of `text`, newline-terminated.
///
/// Used for log tails so a package does not carry a whole log file. A file with
/// fewer lines is returned whole; `max == 0` yields an empty string.
pub fn tail_lines(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut lines: Vec<&str> = text.lines().rev().take(max).collect();
    lines.reverse();
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Read the tail of a file **without** taking or requiring an exclusive lock,
/// returning `Ok(None)` when the file does not exist.
///
/// The sharing detail is the whole reason this function exists rather than a
/// `read_to_string`. `1/` shipped `--diagnose` broken for its entire log tail
/// because its reader asked for a share mode the running service did not grant;
/// every export while the service ran produced an empty zip. The log a package
/// wants is by definition one that something else is writing, so this reads
/// through `File::open` (which requests shared read/write on Windows), never
/// truncates, and tolerates a torn tail.
///
/// `max_bytes` bounds the read: the file is seeked to `len - max_bytes` and the
/// resulting partial first line is dropped. A cut that lands inside a UTF-8
/// sequence is advanced to the next character boundary.
pub fn read_shared_tail(path: &Path, max_bytes: u64) -> std::io::Result<Option<String>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    // A tail that begins mid-line begins with debris: drop up to the first
    // newline. Only do this when the read actually started mid-file.
    let bytes = if start > 0 {
        match buf.iter().position(|&b| b == b'\n') {
            Some(at) => &buf[at + 1..],
            None => &buf[..],
        }
    } else {
        &buf[..]
    };
    // `from_utf8_lossy` after trimming to a char boundary, so a split sequence
    // becomes a replacement char rather than shifting every byte after it.
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(e) => {
            let valid = e.valid_up_to();
            String::from_utf8_lossy(&bytes[valid..]).into_owned()
        }
    };
    Ok(Some(text))
}

/// A failure count for one code, with the first message seen for it.
struct CodeTally {
    code: String,
    count: usize,
    first: String,
}

/// Summarise the launcher's coded failures (`[E2001] message`) by code.
///
/// This is what turns a log into something a reader can act on, and it is the
/// one section `1/` had no equivalent of: AHL already carries a stable code →
/// title → **next action** table ([`ErrorCode`]), so the summary can say what to
/// do about each failure, not just that it happened. It is also the same table
/// the banner and the Activity log use, so a code means one thing everywhere.
///
/// Ordered by descending count, then by code, so the heaviest failure is first
/// and the order is stable between exports.
///
/// A code the table does not know is still listed, with no title or next action:
/// it is evidence of a newer build (or a typo), and silently dropping it would
/// hide exactly the line worth looking at.
pub fn summarize_errors<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut tallies: Vec<CodeTally> = Vec::new();
    for line in lines {
        for (code, message) in coded_failures(line) {
            match tallies.iter_mut().find(|t| t.code == code) {
                Some(t) => t.count += 1,
                None => tallies.push(CodeTally {
                    code: code.to_string(),
                    count: 1,
                    first: message,
                }),
            }
        }
    }

    if tallies.is_empty() {
        return "(no coded failures in the supplied lines)\n".to_string();
    }
    tallies.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.code.cmp(&b.code)));

    let mut out = String::new();
    for t in &tallies {
        match ErrorCode::from_code(&t.code) {
            Some(code) => {
                out.push_str(&format!(
                    "[{}] x{} — {} ({})\n  next: {}\n",
                    t.code,
                    t.count,
                    code.title(),
                    code.category(),
                    code.next_action()
                ));
            }
            None => out.push_str(&format!(
                "[{}] x{} — not in this build's code table; it may come from a newer version\n",
                t.code, t.count
            )),
        }
        if !t.first.is_empty() {
            out.push_str(&format!("  seen: {}\n", t.first));
        }
    }
    out
}

/// Every `[Exxxx]` code in `line`, paired with the message that follows it.
///
/// A line may carry more than one (a message that quotes an earlier failure),
/// so this scans rather than matching once. The shape is fixed by
/// [`CodedError::log_line`](crate::error_code::CodedError::log_line): a bracketed
/// `E` and exactly four digits. Nothing else is accepted — a bracketed word is
/// not a code, and a five-digit number is not from this table.
fn coded_failures(line: &str) -> Vec<(&str, String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 6 <= bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        if bytes[i + 1] != b'E' || !bytes[i + 2..i + 6].iter().all(u8::is_ascii_digit) {
            i += 1;
            continue;
        }
        let end = i + 6;
        if bytes.get(end) != Some(&b']') {
            i += 1;
            continue;
        }
        let code = &line[i + 1..end];
        // The message is the rest of the line up to the next code, if any.
        let rest = line[end + 1..].trim_start();
        let message = rest
            .split_once('[')
            .map(|(head, _)| head.trim_end())
            .unwrap_or(rest)
            .to_string();
        out.push((code, message));
        i = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from(r"C:\Users\ada")
    }

    #[test]
    fn the_home_directory_is_masked_in_both_separator_styles() {
        assert_eq!(
            mask_user_paths(r"open C:\Users\ada\.dsh\config.yml", &home()),
            r"open %USER%\.dsh\config.yml"
        );
        assert_eq!(
            mask_user_paths("open C:/Users/ada/.dsh/config.yml", &home()),
            "open %USER%/.dsh/config.yml"
        );
    }

    #[test]
    fn masking_is_case_insensitive_and_survives_a_trailing_home() {
        // Windows paths arrive in whatever case the writer felt like.
        assert_eq!(
            mask_user_paths(r"c:\users\ADA\x", &home()),
            r"%USER%\x"
        );
        // A home directory at the very end of the text has no separator after it.
        assert_eq!(mask_user_paths(r"cwd is C:\Users\ada", &home()), "cwd is %USER%");
    }

    #[test]
    fn the_environment_variable_and_tilde_forms_are_masked() {
        assert_eq!(mask_user_paths("%USERPROFILE%\\.dsh", &home()), "%USER%\\.dsh");
        assert_eq!(mask_user_paths(r"~\logs\launcher.log", &home()), r"%USER%\logs\launcher.log");
    }

    #[test]
    fn a_bare_username_only_masks_as_a_path_segment() {
        // Between separators: masked.
        assert_eq!(
            mask_user_paths(r"D:\backup\ada\profile.json", &home()),
            r"D:\backup\USERNAME\profile.json"
        );
        // As an ordinary word: left alone. This is why rule 4 needs a separator
        // on both sides.
        assert_eq!(
            mask_user_paths("ada reported a failure", &home()),
            "ada reported a failure"
        );
        // A bare tilde is prose as often as it is a path.
        assert_eq!(mask_user_paths("~3 plugins", &home()), "~3 plugins");
    }

    #[test]
    fn clean_masks_paths_and_secrets_together() {
        let text = r"token=abc123 at C:\Users\ada\.dsh\config.yml";
        let out = clean(text, Some(&home()));
        assert!(!out.contains("abc123"), "secret value survived: {out}");
        assert!(!out.contains(r"C:\Users\ada"), "home path survived: {out}");
        assert!(out.contains("%USER%"), "expected the placeholder: {out}");
    }

    #[test]
    fn clean_still_masks_secrets_when_no_home_is_known() {
        let out = clean("api_key: sk-live-9000", None);
        assert_eq!(out, "api_key: ***");
    }

    #[test]
    fn tail_lines_keeps_the_end_and_terminates_it() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(tail_lines(text, 2), "c\nd\n");
        assert_eq!(tail_lines(text, 99), "a\nb\nc\nd\n");
        assert_eq!(tail_lines(text, 0), "");
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn read_shared_tail_reads_a_live_file_and_drops_the_torn_first_line() {
        let dir = std::env::temp_dir().join(format!("ahl-diagnose-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("launcher.log");
        std::fs::write(&path, "first-line\nsecond-line\nthird-line\n").unwrap();

        // A bound that lands mid-file drops the partial line it starts inside.
        let tail = read_shared_tail(&path, 12).unwrap().unwrap();
        assert_eq!(tail, "third-line\n");

        // A bound larger than the file returns it whole.
        let whole = read_shared_tail(&path, 4096).unwrap().unwrap();
        assert_eq!(whole, "first-line\nsecond-line\nthird-line\n");

        // Absent is not an error — a package is still worth producing.
        assert!(read_shared_tail(&dir.join("nope.log"), 64).unwrap().is_none());

        // Still readable while an append handle is open: the whole point.
        let mut writer = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        writeln!(writer, "fourth-line").unwrap();
        assert!(read_shared_tail(&path, 4096).unwrap().unwrap().contains("fourth-line"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_coded_line_becomes_a_code_with_its_message() {
        assert_eq!(
            coded_failures("[E1003] dsh exited 1"),
            vec![("E1003", "dsh exited 1".to_string())]
        );
        // A bracketed word is not a code, and the digit count is exact.
        assert!(coded_failures("[WARN] something").is_empty());
        assert!(coded_failures("[E10035] too many digits").is_empty());
        assert!(coded_failures("[E100] too few digits").is_empty());
        assert!(coded_failures("E1003 unbracketed").is_empty());
    }

    #[test]
    fn the_summary_orders_by_count_then_code_and_carries_the_next_action() {
        let lines = vec![
            "[E1003] first failure",
            "unrelated noise",
            "[E1003] second failure",
            "[E2001] npm registry unreachable",
        ];
        let out = summarize_errors(lines);
        let e1003 = out.find("[E1003] x2").expect("E1003 summarised");
        let e2001 = out.find("[E2001] x1").expect("E2001 summarised");
        assert!(e1003 < e2001, "heaviest failure should come first:\n{out}");
        // The first message for a code is the one kept.
        assert!(out.contains("seen: first failure"), "{out}");
        // The action table is joined in — the point of having codes at all.
        assert!(out.contains("next: Check the Activity log"), "{out}");
        assert!(out.contains("(runtime)"), "{out}");
    }

    #[test]
    fn a_code_outside_the_table_is_listed_rather_than_dropped() {
        let out = summarize_errors(["[E7777] from a newer build"]);
        assert!(out.contains("[E7777] x1"), "{out}");
        assert!(out.contains("newer version"), "{out}");
    }

    #[test]
    fn an_empty_supply_says_so_instead_of_returning_nothing() {
        assert!(summarize_errors(Vec::<&str>::new()).contains("no coded failures"));
    }
}
