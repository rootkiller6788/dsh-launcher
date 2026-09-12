//! The profile's `pnpm-workspace.yaml`, and the one thing the launcher changes
//! in it: `allowBuilds`.
//!
//! pnpm refuses to run a dependency's build scripts (a `postinstall`/`prepare`,
//! which is how a git-hosted plugin compiles itself) until the package is
//! approved. When `dsh plugin` hits that, it prints its own instruction:
//!
//! > add the exact key pnpm printed above under `allowBuilds` in
//! > `<profileDir>/pnpm-workspace.yaml`, then re-run
//! > — `@deepseek-ai/dsh@0.1.5-rc.1`, `lib/plugin-Ddi42qoW.js`
//!
//! That is what makes writing this file the launcher *relaying* an instruction
//! dsh printed, rather than the launcher deciding what a profile should contain
//! — the line `docs/absorb-plan.md` §3 draws when it refuses the same project's
//! factory reset.
//!
//! Two properties of the key are worth knowing before touching it:
//!
//! - **Its shape is a pnpm major-version decision, not a stable name.** pnpm 10
//!   approves builds with the `onlyBuiltDependencies` *list*; pnpm 11 with the
//!   `allowBuilds` *map*. A file written with the wrong shape does not error —
//!   pnpm ignores the key it does not recognise and the builds stay blocked, so
//!   the failure mode is silence. See `docs/dsh-contract-inventory.md` #53.
//! - **It is not read at boot.** `rescue.rs` left this file out of the rescue
//!   set on those grounds, which was true while nothing the launcher did wrote
//!   to it. pnpm reads it on every `dsh plugin` invocation in the profile
//!   directory, so an entry here is now part of the launcher's changes.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use launcher_core::InstanceManifest;
use serde::Serialize;

use crate::DshAdapter;

/// The profile file pnpm reads build-script approvals from.
pub fn workspace_file(instance: &InstanceManifest) -> PathBuf {
    DshAdapter::profile_dir(instance).join("pnpm-workspace.yaml")
}

/// The single-slot backup written before an edit (see [`add_allow_build`]).
fn backup_file(instance: &InstanceManifest) -> PathBuf {
    workspace_file(instance).with_extension("yaml.bak")
}

/// What an [`add_allow_build`] call did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowBuildOutcome {
    /// `false` when the package was already approved (the call was a no-op).
    pub written: bool,
    /// The exact line added or rewritten, so the UI can show what changed
    /// rather than only that something did.
    pub line: String,
    /// Where the pre-edit bytes went, when a file existed to back up.
    pub backup: Option<PathBuf>,
}

/// Approve one package's build scripts by adding it to `allowBuilds`.
///
/// A **text-level** edit, not a parse-and-reserialize. This file is edited by
/// hand — dsh's own instruction above tells the user to edit it — so a round
/// trip through a YAML writer would drop their comments and reorder their keys
/// in order to change one line. The rules:
///
/// - A top-level `allowBuilds:` line (column 0) anchors the insertion; the new
///   entry goes directly beneath it, indented like the entries already there.
/// - No such key → the block is appended, so a profile that never had one gets a
///   valid file instead of a hand-rolled approximation of one.
/// - The key exists at column 0 but is not a bare mapping key (`allowBuilds: {}`)
///   → refuse. Guessing at a shape we did not expect is how a fix becomes a
///   second breakage.
/// - Idempotent: an entry already `true` writes nothing and says so. An entry
///   present but `false` is rewritten — a silent no-op there would look exactly
///   like the failure this action exists to fix.
///
/// The edited text is parsed and checked **before** anything is written, so an
/// edit that would produce a document without the intended entry leaves the
/// file untouched rather than half-changed.
pub fn add_allow_build(instance: &InstanceManifest, package: &str) -> Result<AllowBuildOutcome> {
    let package = package.trim();
    validate_package_name(package)?;
    let file = workspace_file(instance);
    let before = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
    };

    let line = format!("{}: true", yaml_key(package));
    let after = match insert_allow_build(&before, package, &line)? {
        Edit::Unchanged => {
            return Ok(AllowBuildOutcome {
                written: false,
                line,
                backup: None,
            })
        }
        Edit::Changed(text) => text,
    };
    // Parse what we are about to write, not what we just wrote: the cheapest
    // place to catch our own malformed edit is in memory, where nothing has
    // landed yet. What this cannot check is pnpm's acceptance of the key — its
    // name is pnpm's to change, which is the contract recorded in #53.
    verify(&after, package)?;

    let backup = if before.is_empty() {
        None
    } else {
        let backup = backup_file(instance);
        std::fs::write(&backup, before.as_bytes())
            .with_context(|| format!("write {}", backup.display()))?;
        Some(backup)
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&file, after.as_bytes()).with_context(|| format!("write {}", file.display()))?;

    Ok(AllowBuildOutcome {
        written: true,
        line,
        backup,
    })
}

/// Confirm the edited text is a YAML mapping with `allowBuilds.<package> == true`.
fn verify(text: &str, package: &str) -> Result<()> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).context("the edited pnpm-workspace.yaml is not valid YAML")?;
    if value.get("allowBuilds").and_then(|v| v.get(package)) != Some(&serde_yaml::Value::Bool(true))
    {
        bail!("the edit did not produce allowBuilds.{package} = true — not writing it");
    }
    Ok(())
}

/// Reject anything that is not a package name, because this string is written
/// into a YAML file as a key. A newline or a stray `:` would not fail — it would
/// produce a *different document* than the one intended, which is the same
/// "looks fine, means something else" failure this module exists to avoid.
fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty() {
        bail!("no package name given");
    }
    if package.len() > 214 {
        bail!("package name is longer than npm allows (214 characters)");
    }
    if !package
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '/'))
    {
        bail!("'{package}' is not a package name (expected letters, digits, . _ - @ /)");
    }
    Ok(())
}

/// The YAML spelling of a package name used as a key.
///
/// Plain where a plain scalar is legal, quoted where it is not: `@` and backtick
/// are YAML *reserved indicators* and cannot start a plain scalar, so the scoped
/// form `@scope/pkg` has to be quoted. Getting this wrong produces a file that
/// fails to parse — a loud failure, but one the user would meet instead of us.
fn yaml_key(package: &str) -> String {
    let plain_safe = package
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
        && package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if plain_safe {
        package.to_string()
    } else {
        format!("\"{package}\"")
    }
}

enum Edit {
    Unchanged,
    Changed(String),
}

/// Put `line` (`<key>: true`) under the top-level `allowBuilds:` mapping.
///
/// Splits on `\n` rather than `str::lines` so a `\r` stays attached to its line
/// and is written back: this file belongs to dsh and to whoever hand-edited it,
/// and normalizing its line endings is not part of approving a build script.
fn insert_allow_build(text: &str, package: &str, line: &str) -> Result<Edit> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.split('\n').collect();

    let bare: Vec<&str> = lines.iter().map(|l| l.trim_end_matches('\r')).collect();

    // A top-level `allowBuilds:` carrying a value (`allowBuilds: {}`) is a shape
    // we did not expect. Appending a second key of the same name would give the
    // file duplicate keys, and editing the value in place would mean converting
    // a flow mapping to a block one — guessing at what the user meant. Refusing
    // leaves them exactly where they were; a half-migrated profile is worse than
    // a refused edit.
    if bare
        .iter()
        .any(|l| l.starts_with("allowBuilds:") && *l != "allowBuilds:")
    {
        bail!("the profile's pnpm-workspace.yaml has a top-level `allowBuilds:` that is not a bare mapping key; add the entry by hand");
    }

    // The anchor has to be a *top-level* key: a same-named key nested under
    // another one is a different path, and it is not the one pnpm reads.
    let Some(anchor) = bare.iter().position(|l| *l == "allowBuilds:") else {
        return Ok(Edit::Changed(append_block(text, line, eol)));
    };

    // The block runs to the last indented content line under the anchor: the new
    // entry goes after the existing ones rather than above them, and a blank
    // line or a comment inside the block does not read as the end of it.
    let mut end = anchor + 1;
    for (i, l) in bare.iter().enumerate().skip(anchor + 1) {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        if !l.starts_with(' ') && !l.starts_with('\t') {
            break;
        }
        end = i + 1;
    }
    let block = &bare[anchor + 1..end];

    // Indentation is copied from the entries already there, so an edit to a
    // hand-formatted file keeps that file's formatting.
    let indent = block
        .iter()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l[..l.len() - l.trim_start().len()].to_string())
        .unwrap_or_else(|| detect_indent(&bare));

    for (i, l) in block.iter().enumerate() {
        let Some((key, value)) = entry(l) else {
            continue;
        };
        if key != package {
            continue;
        }
        // An inline comment is not part of the value: `esbuild: true # ok` is
        // already approved, and rewriting it would drop the comment.
        if value.split('#').next().unwrap_or("").trim() == "true" {
            return Ok(Edit::Unchanged);
        }
        // Approved with some other value (`false`, or something we do not
        // recognise): replace the line, rather than adding a second key and
        // leaving which one counts up to the parser.
        let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        out[anchor + 1 + i] = format!("{indent}{line}");
        return Ok(Edit::Changed(join(&out, eol)));
    }

    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(end, format!("{indent}{line}"));
    Ok(Edit::Changed(join(&out, eol)))
}

/// One line as `(key, value text)`, or `None` where it is not a mapping entry.
///
/// The key can be quoted (`"@scope/pkg": true` — see [`yaml_key`]) while the
/// value can itself contain a `:`, so "the first colon" is not the split in
/// every case.
fn entry(line: &str) -> Option<(String, String)> {
    let colon = if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        if !rest[end + 1..].starts_with(':') {
            return None;
        }
        1 + end
    } else {
        line.find(':')?
    };
    let key = line[..colon].trim().trim_matches('"').to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, line[colon + 1..].trim().to_string()))
}

/// Indentation for a fresh entry when the mapping has no entries to copy from:
/// the file's own narrowest indentation, so the new key lines up with whatever
/// is already there. Two spaces when there is nothing to go on.
///
/// Tabs are not a candidate — YAML forbids them as indentation — so a tab-led
/// line counts as unindented here and cannot become the style a block is
/// written in.
fn detect_indent(lines: &[&str]) -> String {
    let width = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .filter(|w| *w > 0)
        .min();
    " ".repeat(width.unwrap_or(2))
}

/// Append a whole `allowBuilds:` block to a file that has none.
fn append_block(text: &str, line: &str, eol: &str) -> String {
    let mut out = text.to_string();
    if !out.is_empty() {
        if !out.ends_with(eol) {
            out.push_str(eol);
        }
        // A blank line before a new top-level key, unless one is already there.
        if !out.ends_with(&format!("{eol}{eol}")) {
            out.push_str(eol);
        }
    }
    out.push_str(&format!("allowBuilds:{eol}  {line}{eol}"));
    out
}

/// Rebuild the text from lines split on `\n`, without inventing a trailing
/// newline the original did not have.
fn join(lines: &[String], eol: &str) -> String {
    lines.join(eol)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> (PathBuf, InstanceManifest, PathBuf) {
        let root = std::env::temp_dir().join(format!("ahl-pnpm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let manifest = InstanceManifest::new("default".into(), "Default".into(), &root);
        let profile = DshAdapter::profile_dir(&manifest);
        std::fs::create_dir_all(&profile).unwrap();
        (root, manifest, profile)
    }

    fn write(profile: &std::path::Path, text: &str) -> PathBuf {
        let file = profile.join("pnpm-workspace.yaml");
        std::fs::write(&file, text).unwrap();
        file
    }

    fn read(file: &std::path::Path) -> String {
        std::fs::read_to_string(file).unwrap()
    }

    #[test]
    fn appends_a_block_to_a_file_that_has_none() {
        let (root, m, profile) = setup("append");
        let file = write(&profile, "packages:\n  - \"apps/*\"\n");
        let out = add_allow_build(&m, "esbuild").unwrap();
        assert!(out.written);
        assert_eq!(out.line, "esbuild: true");
        assert_eq!(
            read(&file),
            "packages:\n  - \"apps/*\"\n\nallowBuilds:\n  esbuild: true\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inserts_under_an_existing_block_keeping_its_indentation() {
        let (root, m, profile) = setup("insert");
        let file = write(
            &profile,
            "# approval\nallowBuilds:\n    esbuild: true\n\nother: 1\n",
        );
        add_allow_build(&m, "sharp").unwrap();
        assert_eq!(
            read(&file),
            "# approval\nallowBuilds:\n    esbuild: true\n    sharp: true\n\nother: 1\n",
            "four-space indent copied, entry kept inside the block and above its end"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_block_adopts_the_files_own_indent_width() {
        let (root, m, profile) = setup("width");
        // `allowBuilds:` with nothing under it: there is no entry to copy an
        // indent from, so the file's other keys are what it goes by.
        let file = write(&profile, "packages:\n    - \"apps/*\"\nallowBuilds:\n");
        add_allow_build(&m, "esbuild").unwrap();
        assert_eq!(
            read(&file),
            "packages:\n    - \"apps/*\"\nallowBuilds:\n    esbuild: true\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_true_entry_is_a_no_op() {
        let (root, m, profile) = setup("noop");
        let before = "allowBuilds:\n  esbuild: true\n";
        let file = write(&profile, before);
        let out = add_allow_build(&m, "esbuild").unwrap();
        assert!(!out.written);
        assert_eq!(out.backup, None, "nothing changed, so nothing backed up");
        assert_eq!(read(&file), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_false_entry_is_rewritten_not_duplicated() {
        let (root, m, profile) = setup("false");
        let file = write(&profile, "allowBuilds:\n  esbuild: false\n");
        let out = add_allow_build(&m, "esbuild").unwrap();
        assert!(out.written);
        assert_eq!(read(&file), "allowBuilds:\n  esbuild: true\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scoped_names_are_quoted_because_they_have_to_be() {
        let (root, m, profile) = setup("scoped");
        let file = write(&profile, "allowBuilds:\n  esbuild: true\n");
        let out = add_allow_build(&m, "@scope/pkg").unwrap();
        assert_eq!(out.line, "\"@scope/pkg\": true");
        let text = read(&file);
        assert_eq!(
            text,
            "allowBuilds:\n  esbuild: true\n  \"@scope/pkg\": true\n"
        );
        // And what it produced is a mapping under that key — which an unquoted
        // `@scope/pkg` would not be.
        let value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(
            value.get("allowBuilds").and_then(|v| v.get("@scope/pkg")),
            Some(&serde_yaml::Value::Bool(true))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hostile_name_is_refused_rather_than_written() {
        let (root, m, profile) = setup("hostile");
        let file = write(&profile, "allowBuilds:\n  esbuild: true\n");
        for bad in [
            "",
            "  ",
            "esbuild\nallowBuilds:\n  evil: true",
            "esbuild: true",
            "a b",
            "pkg#comment",
            "pkg\"",
            &"x".repeat(215),
        ] {
            let err = add_allow_build(&m, bad).unwrap_err().to_string();
            assert!(
                err.contains("package name") || err.contains("no package name"),
                "{bad:?} → {err}"
            );
        }
        assert_eq!(
            read(&file),
            "allowBuilds:\n  esbuild: true\n",
            "nothing refused left a mark"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_nested_key_is_not_mistaken_for_the_top_level_one() {
        let (root, m, profile) = setup("nested");
        let file = write(&profile, "pnpm:\n  allowBuilds:\n    esbuild: true\n");
        add_allow_build(&m, "sharp").unwrap();
        // The nested mapping is left exactly as it was; the top-level block is
        // added, because that is the key pnpm reads.
        assert_eq!(
            read(&file),
            "pnpm:\n  allowBuilds:\n    esbuild: true\n\nallowBuilds:\n  sharp: true\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_key_that_is_not_a_bare_mapping_is_refused_with_the_file_untouched() {
        let (root, m, profile) = setup("inline");
        let before = "allowBuilds: {}\n";
        let file = write(&profile, before);
        let err = add_allow_build(&m, "esbuild").unwrap_err().to_string();
        assert!(err.contains("not a bare mapping key"), "{err}");
        assert_eq!(read(&file), before);
        assert!(
            !profile.join("pnpm-workspace.yaml.bak").exists(),
            "a refusal happens before the backup, so it leaves no trace"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_previous_bytes_are_kept_next_to_the_file() {
        let (root, m, profile) = setup("backup");
        write(&profile, "allowBuilds:\n  esbuild: true\n");
        let out = add_allow_build(&m, "sharp").unwrap();
        let backup = out.backup.expect("a file existed, so it was backed up");
        assert_eq!(backup, profile.join("pnpm-workspace.yaml.bak"));
        assert_eq!(read(&backup), "allowBuilds:\n  esbuild: true\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_does_not_exist_yet_gets_one() {
        let (root, m, profile) = setup("missing");
        let out = add_allow_build(&m, "esbuild").unwrap();
        assert!(out.written);
        assert_eq!(out.backup, None, "nothing existed to back up");
        assert_eq!(
            read(&profile.join("pnpm-workspace.yaml")),
            "allowBuilds:\n  esbuild: true\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn line_endings_and_a_missing_final_newline_survive() {
        let (root, m, profile) = setup("crlf");
        let file = write(&profile, "packages:\r\n  - \"apps/*\"");
        add_allow_build(&m, "esbuild").unwrap();
        let text = read(&file);
        assert_eq!(
            text, "packages:\r\n  - \"apps/*\"\r\n\r\nallowBuilds:\r\n  esbuild: true\r\n",
            "CRLF stays CRLF"
        );
        assert!(
            !text.contains("\n\n"),
            "no LF was introduced into a CRLF file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_trailing_newline_is_not_added_to_a_file_that_had_none() {
        let (root, m, profile) = setup("eof");
        let file = write(&profile, "allowBuilds:\n  esbuild: true");
        let out = add_allow_build(&m, "sharp").unwrap();
        assert!(out.written);
        assert_eq!(read(&file), "allowBuilds:\n  esbuild: true\n  sharp: true");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn yaml_key_quotes_only_where_a_plain_scalar_is_illegal() {
        assert_eq!(yaml_key("esbuild"), "esbuild");
        assert_eq!(yaml_key("foo-bar.baz"), "foo-bar.baz");
        assert_eq!(yaml_key("@scope/pkg"), "\"@scope/pkg\"");
        assert_eq!(yaml_key("a/b"), "\"a/b\"");
    }
}
