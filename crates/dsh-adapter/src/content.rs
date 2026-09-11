//! Content installers for the Market's non-plugin kinds.
//!
//! Skills are directories under `$DSH_HOME/skills/` — a `SKILL.md` (or a
//! directory-bundle `<name>/SKILL.md`) discovered by DSH's `skill-filesystem`
//! plugin. There is no npm install and no enable toggle; the launcher lands
//! every file of the skill, each atomically (write to a `.tmp` sibling, then
//! rename), and records `{source, hash, installed}` provenance in
//! `InstanceManifest.skills` — the SHA-256 of the `SKILL.md` is the update
//! signal DSH exposes.
//!
//! A skill ships more than its `SKILL.md`: `references/`, `tools/`, `scripts/`
//! and `templates/` sit beside it and are opened by relative path, so install
//! clones the source repo and lands the skill's whole directory. The catalog's
//! pre-resolved raw `fetch` URL is the fallback for a source that cannot be
//! cloned — it can only ever deliver the one file it points at.
//!
//! (MCP connection records live in `InstanceManifest.mcp` — the single source
//! of truth — and are compiled into `cordis.patch.yml` as a whole by
//! [`sync_mcp_patch`]; install/uninstall/disable mutate the record then
//! regenerate.)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use launcher_core::github;
use launcher_core::{
    sha256_hex, InstanceManifest, McpEnvRequirement, McpServerRecord, RegistryPlugin, SkillRecord,
    SkinPackage,
};
use serde_yaml::Value as Yaml;

use crate::{DshAdapter, InstalledPlugin};

/// Canonical install id for a skill entry (`owner/name`), matching what the
/// manifest stores and the frontend compares. Two authors may publish a skill
/// with the same short name, so the id keeps the author.
pub fn skill_id(entry: &RegistryPlugin) -> String {
    entry.key()
}

/// The on-disk directory name for a skill id (`owner/name` → `owner-name`).
fn skill_dir_name(id: &str) -> String {
    id.replace('/', "-")
}

/// The skills root for an instance (`$DSH_HOME/skills`).
fn skills_dir(instance: &InstanceManifest) -> PathBuf {
    PathBuf::from(&instance.workspace).join("skills")
}

/// Installed skills, straight from the manifest — each carrying its `{source,
/// hash, installed}` provenance (the only index; skills are plain files, not
/// npm packages).
pub fn installed_skills(instance: &InstanceManifest) -> Vec<SkillRecord> {
    instance.skills.clone()
}

/// The URL a skill's `SKILL.md` is (re)fetched from — the pre-resolved raw
/// `fetch` URL when the catalog pinned one, else the source repo. Drives the
/// update check's "what's upstream now?" probe.
pub fn skill_source(entry: &RegistryPlugin) -> Option<String> {
    entry
        .fetch
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            let url = entry.url.trim();
            (!url.is_empty()).then(|| url.to_string())
        })
}

/// Install a skill into `$DSH_HOME/skills/<id>/` as a whole directory: every
/// file the source ships, each landed atomically. Returns the provenance record
/// (source + `SKILL.md` SHA-256); the command layer stamps `installed`.
pub async fn install_skill(
    instance: &InstanceManifest,
    entry: &RegistryPlugin,
    mirror: bool,
) -> Result<SkillRecord> {
    let bundle = fetch_skill_bundle(entry, mirror).await?;
    let id = skill_id(entry);
    let dir = skills_dir(instance).join(skill_dir_name(&id));
    land_bundle(&dir, &bundle)?;
    Ok(SkillRecord {
        id,
        source: bundle.source().to_string(),
        hash: bundle.hash(),
        installed: 0,
    })
}

/// SHA-256 (hex) of the `SKILL.md` a skill's source currently serves — the
/// update check's "has the author changed it upstream?" probe, resolved so it
/// agrees with what an install would land.
///
/// `source` is whatever the record captured: a raw `SKILL.md` URL from the
/// catalog's `fetch`, or a repo URL. Both are resolved to *`SKILL.md` content*.
/// A repo URL is deliberately not fetched as a page — that hashed the HTML of
/// the repo's landing page, which is stable and meaningless, so the check
/// silently reported "up to date" forever. And when nothing raw resolves, the
/// repo is cloned and asked directly: a source whose raw URL 404s (the file
/// moved, or the skill never sat at the repo root) is a probe the user cannot
/// act on, not a verdict that there is nothing to update.
pub async fn fetch_skill_hash(source: &str, name: &str, mirror: bool) -> Result<String> {
    for url in raw_skill_md_candidates(source) {
        if let Ok(text) = fetch_text(&url, mirror).await {
            return Ok(sha256_hex(text.as_bytes()));
        }
    }
    let resolved = resolve_source(source).ok_or_else(|| {
        anyhow!("skill source {source} is not a fetchable SKILL.md URL or a github repo")
    })?;
    let files = clone_bundle(&resolved.repo, resolved.dir.as_deref(), name, mirror).await?;
    Ok(SkillBundle::new(files, source.to_string())?.hash())
}

/// The raw `SKILL.md` URLs to try for a recorded source, cheapest first.
///
/// Only a URL that already points at the file qualifies. A *repo* URL gets no
/// candidate at all: guessing its root `SKILL.md` is wrong for any monorepo that
/// keeps a `SKILL.md` at its root alongside nested skills — `anthropics/skills`
/// does exactly that — and the wrong skill's hash is worse than paying for the
/// clone that resolves it exactly.
fn raw_skill_md_candidates(source: &str) -> Vec<String> {
    let source = source.trim();
    if source.starts_with("https://raw.githubusercontent.com/") || github_repo(source).is_none() {
        return vec![source.to_string()];
    }
    Vec::new()
}

/// Where a skill source lives: the repo to clone, and the skill's directory
/// inside it when the URL pinned one. `None` when the source is not a github
/// URL at all (a gist, a plain file host) — the only case where fetching a
/// single raw file is the sole option.
struct SkillSource {
    repo: String,
    dir: Option<String>,
}

/// Resolve a catalog entry's recorded source — a raw `fetch` URL or a repo URL
/// — to something clonable. Both shapes name the same repo; only the raw one
/// knows where inside it the skill sits.
fn resolve_source(url: &str) -> Option<SkillSource> {
    let url = url.trim().trim_end_matches('/');
    if let Some(dir) = fetch_dir_in_repo(Some(url)) {
        let rest = url.strip_prefix("https://raw.githubusercontent.com/")?;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|part| !part.is_empty())?;
        let repo = parts.next().filter(|part| !part.is_empty())?;
        return Some(SkillSource {
            repo: format!("{owner}/{repo}"),
            dir: Some(dir),
        });
    }
    github_repo(url).map(|repo| SkillSource { repo, dir: None })
}

/// SHA-256 of an installed skill's `SKILL.md` on disk (`None` when the file is
/// missing). The update check uses it as the "current" baseline for legacy
/// records that predate hash tracking.
pub fn skill_disk_hash(instance: &InstanceManifest, id: &str) -> Result<Option<String>> {
    let path = skills_dir(instance)
        .join(skill_dir_name(id))
        .join("SKILL.md");
    if path.is_file() {
        Ok(Some(launcher_core::file_sha256(&path)?))
    } else {
        Ok(None)
    }
}

/// Bring a skill up to the version its source currently serves, re-landing the
/// whole directory. Returns `Ok(None)` when there is genuinely nothing to do —
/// the `SKILL.md` hash still matches `current_hash` *and* the files on disk
/// already match upstream — so "update" stays idempotent; `Ok(Some(record))`
/// after re-landing the bundle (the command layer stamps `installed` and
/// persists the record).
pub async fn update_skill(
    instance: &InstanceManifest,
    entry: &RegistryPlugin,
    current_hash: &str,
    mirror: bool,
) -> Result<Option<SkillRecord>> {
    let bundle = fetch_skill_bundle(entry, mirror).await?;
    let hash = bundle.hash();
    let id = skill_id(entry);
    let dir = skills_dir(instance).join(skill_dir_name(&id));
    if !current_hash.is_empty() && hash == current_hash && bundle_matches_disk(&dir, &bundle) {
        return Ok(None);
    }
    land_bundle(&dir, &bundle)?;
    Ok(Some(SkillRecord {
        id,
        source: bundle.source().to_string(),
        hash,
        installed: 0,
    }))
}

/// Land a whole skill directory: every file written atomically, then anything
/// the new version no longer carries is pruned — a reference the author deleted
/// upstream must not linger on disk for the skill to find and follow.
fn land_bundle(dir: &Path, bundle: &SkillBundle) -> Result<()> {
    // `SKILL.md` goes last. Its presence is what `skill_disk_state` reads as
    // "installed", so landing it last means an install interrupted halfway
    // reads as *not installed* rather than as a skill whose `references/` are
    // still missing.
    for (path, bytes) in bundle.files().filter(|(path, _)| *path != "SKILL.md") {
        write_atomic(dir, path, bytes)?;
    }
    write_atomic(dir, "SKILL.md", bundle.skill_md())?;
    prune_stale(dir, bundle)
}

/// Whether `dir` already holds exactly this bundle — same file set, same bytes.
/// Compared on content rather than on the `SKILL.md` hash alone so an update
/// triggered by any other reason still picks up a changed sibling file.
fn bundle_matches_disk(dir: &Path, bundle: &SkillBundle) -> bool {
    let mut expected: Vec<(&str, &[u8])> = bundle.files().collect();
    expected.sort_by(|a, b| a.0.cmp(b.0));
    let mut on_disk: Vec<(String, Vec<u8>)> = walk_files(dir)
        .into_iter()
        .filter_map(|path| Some((rel_path(dir, &path)?, std::fs::read(&path).ok()?)))
        .collect();
    on_disk.sort_by(|a, b| a.0.cmp(&b.0));
    expected.len() == on_disk.len()
        && expected
            .iter()
            .zip(&on_disk)
            .all(|((path, bytes), (disk_path, disk))| *path == disk_path && *bytes == disk.as_slice())
}

/// Remove files under `dir` that `bundle` does not carry, then any directory
/// the removals emptied.
fn prune_stale(dir: &Path, bundle: &SkillBundle) -> Result<()> {
    let keep: HashSet<&str> = bundle.files().map(|(path, _)| path).collect();
    for path in walk_files(dir) {
        let Some(rel) = rel_path(dir, &path) else {
            continue;
        };
        if keep.contains(rel.as_str()) {
            continue;
        }
        std::fs::remove_file(&path).with_context(|| format!("prune {}", path.display()))?;
    }
    prune_empty_dirs(dir);
    Ok(())
}

/// Recursively drop directories that no longer hold anything (`remove_dir`
/// fails on a non-empty one, which is exactly the check needed).
fn prune_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            prune_empty_dirs(&path);
            let _ = std::fs::remove_dir(&path);
        }
    }
}

/// Atomically land `bytes` at `dir/rel` by writing a same-directory `.tmp`
/// sibling first, then renaming over the target (same-volume rename is atomic).
/// Mirrors the `.part`-then-rename discipline of
/// `launcher_core::download_file` for the buffered skill path. `rel` may be a
/// nested `/`-separated path; its parent directories are created as needed.
fn write_atomic(dir: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let dest = safe_join(dir, rel)?;
    let parent = dest.parent().unwrap_or(dir);
    std::fs::create_dir_all(parent)?;
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("skill-file");
    let tmp = parent.join(format!(
        "{file_name}.tmp-{}-{:x}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    if dest.exists() {
        std::fs::remove_file(&dest).with_context(|| format!("replace {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("finalize {}", dest.display()))?;
    Ok(())
}

/// Join a bundle's relative path onto `dir`, refusing anything that could land
/// outside it. The walk that produces these paths only ever yields names it
/// read from a directory, so this is a guard against a crafted repository
/// rather than against a plausible one.
fn safe_join(dir: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = dir.to_path_buf();
    let mut parts = 0;
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            return Err(anyhow!("refusing unsafe skill file path {rel:?}"));
        }
        out.push(part);
        parts += 1;
    }
    if parts == 0 {
        return Err(anyhow!("refusing empty skill file path"));
    }
    Ok(out)
}

/// Remove an installed skill's directory (the whole `<id>` folder).
pub fn uninstall_skill(instance: &InstanceManifest, id: &str) -> Result<()> {
    let dir = skills_dir(instance).join(skill_dir_name(id));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    Ok(())
}

/// On-disk presence and validity of a manifest-declared skill — the closest the
/// launcher can get to "installed" for a plain file, since DSH exposes no
/// skills-list RPC. `valid` means the `SKILL.md` parses with a non-empty `name`
/// in its YAML frontmatter.
#[derive(Debug, Clone)]
pub struct SkillDiskState {
    pub present: bool,
    pub valid: bool,
    pub dir: String,
}

pub fn skill_disk_state(instance: &InstanceManifest, id: &str) -> SkillDiskState {
    let dir_name = skill_dir_name(id);
    let dir = skills_dir(instance).join(&dir_name);
    let path = dir.join("SKILL.md");
    let present = path.is_file();
    let valid = present
        && skill_frontmatter_valid(&std::fs::read_to_string(&path).unwrap_or_default());
    SkillDiskState {
        present,
        valid,
        dir: format!("skills/{dir_name}"),
    }
}

/// Validate a `SKILL.md`'s leading `--- … ---` YAML frontmatter: it must parse
/// and carry a non-empty `name`. Skills without this are not usable by DSH's
/// `skill-filesystem` plugin.
fn skill_frontmatter_valid(text: &str) -> bool {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return false;
    }
    let mut frontmatter = String::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        frontmatter.push_str(line);
        frontmatter.push('\n');
    }
    serde_yaml::from_str::<serde_yaml::Value>(&frontmatter)
        .map(|value| {
            value
                .get("name")
                .and_then(|name| name.as_str())
                .is_some_and(|name| !name.trim().is_empty())
        })
        .unwrap_or(false)
}

/// Heuristic: whether the last known DSH inventory shows the skill loader
/// (`skill-filesystem`) mounted. DSH has no skills-list RPC, so this is the only
/// signal that a running DSH could discover skill files right now.
pub fn skill_loader_active(inventory: &[InstalledPlugin]) -> bool {
    inventory
        .iter()
        .any(|plugin| plugin.name.to_lowercase().contains("skill"))
}

/// A skill as its source ships it: every file at and below the skill's own
/// directory, keyed by path relative to that directory with `/` separators
/// (`SKILL.md`, `references/stage_1.md`, …), plus the URL that stands in for the
/// skill as provenance.
///
/// A skill is a directory, not a file. `references/`, `tools/`, `scripts/` and
/// `templates/` sit beside the `SKILL.md` and are opened by relative path from
/// it, so landing the `SKILL.md` alone produced a skill that loads and then
/// fails the moment it reaches for one of them — which is what this type exists
/// to prevent.
#[derive(Debug, Clone)]
pub struct SkillBundle {
    files: Vec<(String, Vec<u8>)>,
    source: String,
}

impl SkillBundle {
    /// `files` must carry a `SKILL.md`: it is the file DSH's skill loader
    /// discovers, and the one whose content hash is the skill's update signal.
    fn new(files: Vec<(String, Vec<u8>)>, source: String) -> Result<Self> {
        if !files.iter().any(|(path, _)| path == "SKILL.md") {
            return Err(anyhow!("skill has no SKILL.md"));
        }
        Ok(Self { files, source })
    }

    /// Every file, in a deterministic (path-sorted) order.
    fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
    }

    /// The `SKILL.md` body — guaranteed present by [`SkillBundle::new`].
    fn skill_md(&self) -> &[u8] {
        self.files
            .iter()
            .find(|(path, _)| path == "SKILL.md")
            .map(|(_, bytes)| bytes.as_slice())
            .expect("SkillBundle is always built with a SKILL.md")
    }

    /// SHA-256 (hex) of the `SKILL.md`. Deliberately the same content hash the
    /// single-file installer recorded, so hashes stored by older versions stay
    /// comparable and the update check keeps working across the change.
    fn hash(&self) -> String {
        sha256_hex(self.skill_md())
    }

    /// The provenance URL written to the record — the catalog's pre-resolved
    /// `fetch` URL when it has one, else the source repo. Both are resolvable by
    /// the update check, which is what this string is for.
    fn source(&self) -> &str {
        &self.source
    }
}

/// Fetch a skill as a whole directory, plus the URL that should stand in for it
/// going forward (the provenance `source` stored on the record).
///
/// The repo is the source of truth and is therefore tried first: only the repo
/// can reveal whether the skill has files beside its `SKILL.md`, and the
/// catalog's raw `fetch` URL — by construction a URL to one file — never can.
/// The raw URL is the fallback for a source with no clonable repo (a bare
/// gist, a plain file host) or a clone that cannot run at all: a single-file
/// install is worse than a complete one but far better than none.
async fn fetch_skill_bundle(entry: &RegistryPlugin, mirror: bool) -> Result<SkillBundle> {
    let source = skill_source(entry).unwrap_or_default();
    let Some(resolved) = resolve_source(&source) else {
        return fetch_single_file(
            entry,
            source,
            "the skill has no resolvable github repo".to_string(),
            mirror,
        )
        .await;
    };
    match clone_bundle(&resolved.repo, resolved.dir.as_deref(), &entry.name, mirror).await {
        Ok(files) => SkillBundle::new(files, source),
        Err(clone_err) => fetch_single_file(entry, source, clone_err.to_string(), mirror).await,
    }
}

/// The one-file bundle a raw `fetch` URL can produce — the fallback when there
/// is no clonable repo, or the clone failed. A skill with no repo at all is
/// better served by the single file the catalog pinned than by no install.
async fn fetch_single_file(
    entry: &RegistryPlugin,
    source: String,
    why: String,
    mirror: bool,
) -> Result<SkillBundle> {
    let Some(fetch) = entry
        .fetch
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return Err(anyhow!(why));
    };
    let text = fetch_text(fetch, mirror).await.map_err(|fetch_err| {
        anyhow!("{why}; the raw SKILL.md fallback failed too: {fetch_err}")
    })?;
    SkillBundle::new(vec![("SKILL.md".to_string(), text.into_bytes())], source)
}

/// Clone `repo` and read the skill's directory out of it, preferring
/// `dir_hint` — the `/`-separated path the catalog's `fetch` URL pinned — and
/// falling back to locating the `SKILL.md` by `name`.
async fn clone_bundle(
    repo: &str,
    dir_hint: Option<&str>,
    name: &str,
    mirror: bool,
) -> Result<Vec<(String, Vec<u8>)>> {
    let tmp = std::env::temp_dir().join(format!(
        "ahl-skill-{}-{:x}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    std::fs::create_dir_all(&tmp)?;
    let result = async {
        clone_repo(repo, &tmp, mirror).await?;
        let root = skill_root(&tmp, dir_hint, name)
            .ok_or_else(|| anyhow!("no SKILL.md found in {repo}"))?;
        read_bundle(&root)
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The `owner/repo` slug of a github URL — what both the clone URL and the raw
/// `SKILL.md` URL are built from.
fn github_repo(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")?;
    let slug = rest.trim_end_matches(".git").trim_end_matches('/');
    (!slug.is_empty()).then(|| slug.to_string())
}

/// The `git clone` argv for a skill.
///
/// `-c core.autocrlf=false` is not incidental: with the Git-for-Windows default
/// of `autocrlf=true` a checkout rewrites every LF to CRLF, so the `SKILL.md` on
/// disk is no longer the file the repo holds — and its hash would never match
/// the one the update check derives from the raw URL, leaving the skill
/// permanently "update available". `-c` outranks every config file, so a user's
/// global setting cannot reintroduce it.
fn clone_args(url: &str, dest: &Path) -> Vec<String> {
    vec![
        "-c".to_string(),
        "core.autocrlf=false".to_string(),
        "clone".to_string(),
        "--depth".to_string(),
        "1".to_string(),
        "--quiet".to_string(),
        "--".to_string(),
        url.to_string(),
        dest.display().to_string(),
    ]
}

/// Shallow-clone `repo` into `dest` (which must be absent or empty), trying the
/// transports [`github::fetch_candidates`] picks — the mirror first when the
/// Install Center toggle is on, else github.com first — and only failing when
/// both are exhausted.
///
/// A skill install that dies at the clone is the mainland-China failure this
/// exists for: the raw `SKILL.md` path has always fallen back to the relay, but
/// the clone did not, so multi-file skills failed where single-file ones worked.
async fn clone_repo(repo: &str, dest: &Path, mirror: bool) -> Result<()> {
    let candidates = github::fetch_candidates(&format!("https://github.com/{repo}"), mirror);
    let mut errors = Vec::new();
    for (attempt, url) in candidates.iter().enumerate() {
        if attempt > 0 {
            // A failed clone can leave a partial checkout behind, and `git
            // clone` refuses a non-empty destination. The directory is ours
            // (`clone_bundle` just created it), so clearing it is safe.
            let _ = std::fs::remove_dir_all(dest);
        }
        match clone_into(url, dest).await {
            Ok(()) => {
                if attempt > 0 {
                    tracing::debug!(repo, url = %url, "cloned through the gh-proxy relay");
                }
                return Ok(());
            }
            Err(e) => errors.push(e),
        }
    }
    Err(match errors.len() {
        1 => errors.pop().expect("one clone attempt reported one error"),
        _ => anyhow!(
            "{}; the retry failed too: {}",
            errors[0],
            errors[1]
        ),
    })
}

/// One clone attempt against a known URL. The caller owns the retry so the
/// failure text can name both transports.
async fn clone_into(url: &str, dest: &Path) -> Result<()> {
    let args = clone_args(url, dest);
    // Route through the shared timed runner so a stalled transfer is killed
    // after GIT_TIMEOUT (process tree included) instead of hanging forever.
    let code = crate::run_timed(
        "git",
        &args,
        dest.parent().unwrap_or(dest),
        &[],
        crate::silent_log_sink(),
        crate::GIT_TIMEOUT,
    )
    .await
    .map_err(|e| anyhow!("git clone {url} failed: {e}"))?;
    if code != 0 {
        return Err(anyhow!(
            "git clone {url} failed — check the repo exists and is public, and that your \
             network can reach it. If it is public and you are on a throttled link, turn on \
             the GitHub mirror in the Install Center and Retry."
        ));
    }
    Ok(())
}

/// The directory inside a clone that holds the skill — the parent of its
/// `SKILL.md`. `dir_hint` (from the catalog's pinned `fetch` URL, `""` meaning
/// the repo root) wins when it actually holds a `SKILL.md`; a bare repo URL
/// gives only a name, so the `SKILL.md` is located by search instead.
fn skill_root(repo_dir: &Path, dir_hint: Option<&str>, name: &str) -> Option<PathBuf> {
    if let Some(dir) = dir_hint {
        let candidate = if dir.is_empty() {
            repo_dir.to_path_buf()
        } else {
            repo_dir.join(dir)
        };
        if candidate.join("SKILL.md").is_file() {
            return Some(candidate);
        }
    }
    find_skill_md(repo_dir, name).and_then(|md| md.parent().map(Path::to_path_buf))
}

/// The directory a raw `fetch` URL points at, as a `/`-separated path relative
/// to the repo root (`""` for a skill that *is* the repo root, as in
/// `…/<owner>/<repo>/HEAD/SKILL.md`). `None` when the URL is not a raw
/// githubusercontent one or does not end at a `SKILL.md`.
fn fetch_dir_in_repo(fetch: Option<&str>) -> Option<String> {
    let rest = fetch?
        .trim()
        .strip_prefix("https://raw.githubusercontent.com/")?;
    // <owner>/<repo>/<ref>/<path…>
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 {
        return None;
    }
    let path = &parts[3..];
    if !path.last()?.eq_ignore_ascii_case("SKILL.md") {
        return None;
    }
    Some(path[..path.len() - 1].join("/"))
}

/// Read every file at or below `root` as `(path relative to root, bytes)`.
fn read_bundle(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for path in walk_files(root) {
        let Some(rel) = rel_path(root, &path) else {
            continue;
        };
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        files.push((rel, bytes));
    }
    Ok(files)
}

/// `path` relative to `root`, `/`-separated so it is a bundle key rather than a
/// platform path.
fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Every file at or below `root`, path-sorted so a bundle is built in a
/// deterministic order.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(root, &mut out);
    out.sort();
    out
}

/// Directories that are never part of a skill's content: version control
/// metadata, dependency trees, and Python bytecode caches.
fn is_noise_dir(name: &str) -> bool {
    matches!(name, ".git" | ".github" | "node_modules" | "__pycache__")
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(is_noise_dir)
                .unwrap_or(false);
            if !skip {
                collect_files(&path, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Fetch one small file (a `SKILL.md`), trying the transports
/// [`github::fetch_candidates`] picks so a blocked github.com is not the end of
/// the install.
async fn fetch_text(url: &str, mirror: bool) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let urls = github::fetch_candidates(url, mirror);
    let mut last_err = None;
    for u in urls {
        match client.get(&u).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp.text().await.context("read SKILL.md body");
            }
            Ok(resp) => last_err = Some(anyhow!("SKILL.md HTTP {}", resp.status())),
            Err(e) => last_err = Some(anyhow!("SKILL.md fetch: {e}")),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("SKILL.md fetch failed")))
}

/// Locate a `SKILL.md` in a cloned repo, preferring a parent directory whose
/// name matches the skill's short name, then a path containing it, then any.
fn find_skill_md(root: &Path, name: &str) -> Option<PathBuf> {
    let all: Vec<PathBuf> = walk_files(root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case("SKILL.md"))
                .unwrap_or(false)
        })
        .collect();
    let dir_matches = |p: &PathBuf| {
        p.parent()
            .and_then(|d| d.file_name())
            .map(|n| n == name)
            .unwrap_or(false)
    };
    if let Some(p) = all.iter().find(|p| dir_matches(p)) {
        return Some(p.clone());
    }
    if let Some(p) = all.iter().find(|p| p.to_string_lossy().contains(name)) {
        return Some(p.clone());
    }
    all.into_iter().next()
}

/// Canonical install id for an MCP entry (`owner/name`), matching what the
/// manifest stores and the frontend compares — the same `key()` identity skills
/// use, so installed-MCP matching on the card is uniform across content kinds.
pub fn mcp_id(entry: &RegistryPlugin) -> String {
    entry.key()
}

/// The `serverName` an MCP server's tools are published under. Curated entries
/// carry it explicitly; a bare entry falls back to a sanitized name so it still
/// satisfies the mcp-client `[A-Za-z0-9_-]{1,32}` pattern.
fn mcp_server_name(entry: &RegistryPlugin) -> String {
    entry
        .server_name
        .clone()
        .unwrap_or_else(|| sanitize_server_name(&entry.name))
}

/// Force an arbitrary config key (imported `serverName`, or a bare entry name)
/// into the `[A-Za-z0-9_-]{1,32}` pattern the mcp-client server id requires.
pub(crate) fn sanitize_server_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    if s.is_empty() {
        s = "server".into();
    }
    s
}

/// The full connection record for an MCP catalog entry — what the launcher
/// stores in `InstanceManifest.mcp` as the single source of truth, from which
/// `cordis.patch.yml` is compiled by [`sync_mcp_patch`]. `transport` defaults
/// to `stdio` and the record installs enabled.
pub fn mcp_record(entry: &RegistryPlugin) -> McpServerRecord {
    McpServerRecord {
        id: entry.key(),
        server_name: mcp_server_name(entry),
        transport: entry.transport.clone().unwrap_or_else(|| "stdio".into()),
        command: entry.command.clone().unwrap_or_default(),
        args: entry.args.clone().unwrap_or_default(),
        env: entry.env.clone().unwrap_or_default(),
        url: entry.mcp_url.clone().unwrap_or_default(),
        headers: entry.headers.clone().unwrap_or_default(),
        required_env: entry.required_env.clone(),
        enabled: true,
    }
}

/// Whether a stdio server is **directory-gated**: it can only operate on paths
/// a client whitelists (filesystem-family `allowed-directory` args, or MCP
/// roots), and launched without one it idles ("waiting for roots") then rejects
/// every path — observed real-machine on the filesystem server
/// (`list_allowed_directories` empty, "Access denied - path outside allowed
/// directories"). The install flow injects the instance workspace as the
/// default allowed directory for these records.
///
/// v1 recognizes the filesystem family by record id / server name — the
/// catalog's only directory-gated server today. The match is deliberately
/// narrow so github / memory / kanboard and every non-directory server stays
/// out. A future catalog capability field would drive this more precisely;
/// name matching is the honest v1 bound.
pub fn needs_allowed_directory(record: &McpServerRecord) -> bool {
    let id = record.id.to_ascii_lowercase();
    let name = record.server_name.trim().to_ascii_lowercase();
    name == "filesystem"
        || name == "server-filesystem"
        || id.ends_with("/server-filesystem")
        || id.ends_with("/filesystem")
}

/// The mcp-client plugin's config as a `serde_yaml::Value`, from a record:
/// `stdio` carries `command`/`args`/`env`; `streamable-http` carries
/// `url`/`headers`. Both carry `serverName` and `transport`.
fn record_config_yaml(record: &McpServerRecord) -> Result<Yaml> {
    let mut config = serde_yaml::Mapping::new();
    config.insert(
        "serverName".into(),
        Yaml::String(record.server_name.clone()),
    );
    config.insert("transport".into(), Yaml::String(record.transport.clone()));
    if record.transport == "streamable-http" {
        config.insert("url".into(), Yaml::String(record.url.clone()));
        config.insert("headers".into(), serde_yaml::to_value(&record.headers)?);
    } else {
        config.insert("command".into(), Yaml::String(record.command.clone()));
        config.insert("args".into(), serde_yaml::to_value(&record.args)?);
        config.insert("env".into(), serde_yaml::to_value(&record.env)?);
    }
    Ok(Yaml::Mapping(config))
}

/// Serialize one MCP record as a *row* nested under a top-level `- insert:`
/// block (indents 4 / 6 / 8 for the row / its fields / config keys):
/// ```yaml
///     - id: mcp-github
///       name: '@deepseek-ai/dsh-mcp-client'
///       config:
///         serverName: github
///         transport: stdio
///         command: npx
///         args:
///         - -y
///         - '@modelcontextprotocol/server-github'
///         env: {}
/// ```
fn mcp_insert_row(record: &McpServerRecord) -> Result<String> {
    let row_id = format!("mcp-{}", record.server_name);
    let config_yaml = serde_yaml::to_string(&record_config_yaml(record)?)?;
    let mut row = format!("    - id: {row_id}\n");
    row.push_str("      name: '@deepseek-ai/dsh-mcp-client'\n");
    row.push_str("      config:\n");
    for line in config_yaml.lines() {
        row.push_str("        ");
        row.push_str(line);
        row.push('\n');
    }
    Ok(row)
}

/// Compile the instance's MCP records into its profile `cordis.patch.yml` —
/// the launcher writes the whole `@deepseek-ai/dsh-mcp-client` region from the
/// manifest, the single source of truth. Install / uninstall / disable all
/// funnel through this: mutate the record, then regenerate.
///
/// 1. Read the current patch.
/// 2. Drop every launcher-owned MCP insert block (see
///    [`remove_mcp_insert_blocks`](crate::remove_mcp_insert_blocks)) — plugin
///    `- id:`/`disabled:` rows, comments, and user content are left as-is.
/// 3. The enabled records become **one** `- insert:` block (each a row); with
///    none enabled the block is empty.
/// 4. Non-empty block → append via [`append_block_to_text`], so the empty-list
///    `[]` placeholder is commented out first. Empty → [`restore_placeholder`]
///    revives `[]` when the file holds no other content.
pub fn sync_mcp_patch(instance: &InstanceManifest, records: &[McpServerRecord]) -> Result<()> {
    let patch_path = DshAdapter::profile_dir(instance).join("cordis.patch.yml");
    let text = std::fs::read_to_string(&patch_path).unwrap_or_default();
    let stripped = crate::remove_mcp_insert_blocks(&text);

    let enabled: Vec<&McpServerRecord> = records.iter().filter(|r| r.enabled).collect();
    let next = if enabled.is_empty() {
        crate::restore_placeholder(&stripped)
    } else {
        let mut block = String::from("- insert:\n");
        for record in enabled {
            block.push_str(&mcp_insert_row(record)?);
        }
        crate::append_block_to_text(&stripped, &block)
    };
    std::fs::write(&patch_path, next).with_context(|| format!("write {}", patch_path.display()))
}

/// Derive a stable, cordis-safe insert `id` for a skin from its npm package
/// name: strip any scope (`@scope/`), strip a `dsh-`/`dsh-client-` prefix, and
/// ensure a `skin-` prefix. `id` is a *free* unique identifier in the cordis
/// layer — unrelated to any `__ModuleLoader__.load({id})` inside the skin's
/// `client.js` — so only `[A-Za-z0-9_.-]` and uniqueness matter here.
pub(crate) fn skin_id_from_package(package: &str) -> String {
    let base = package.rsplit('/').next().unwrap_or(package);
    let base = base
        .strip_prefix("dsh-client-")
        .or_else(|| base.strip_prefix("dsh-"))
        .unwrap_or(base);
    let base = base.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.');
    if base.starts_with("skin-") || base == "skin" {
        base.to_string()
    } else {
        format!("skin-{base}")
    }
}

/// Serialize one skin as an insert row nested under a top-level `- insert:`
/// block:
/// ```yaml
///     - id: skin-sakura
///       name: dsh-skin-sakura
/// ```
/// The `name` must equal the npm package name (`package.json.name`) so DSH's
/// `require.resolve(name)` locates it; scoped names (`@scope/pkg`) are quoted.
fn skin_insert_row(package: &str, id: &str) -> String {
    let name = if package.contains('/') {
        format!("'{package}'")
    } else {
        package.to_string()
    };
    format!("    - id: {id}\n      name: {name}\n")
}

/// Compile the instance's skin packages into its profile `cordis.patch.yml` —
/// the same single-source-of-truth pattern as [`sync_mcp_patch`], but keyed on
/// package name (the `name:` field DSH resolves) instead of `@deepseek-ai/dsh-
/// mcp-client`. `enabled` gates whether a skin's row survives into the insert
/// block; absent from the insert *is* disabled.
///
/// 1. Read the current patch.
/// 2. Drop every launcher-owned skin insert block (see
///    [`remove_skin_insert_blocks`](crate::remove_skin_insert_blocks)) — MCP
///    rows, plugin rows, comments, and user content are left as-is.
/// 3. The enabled skins become **one** `- insert:` block; none enabled → empty.
/// 4. Non-empty → append via [`append_block_to_text`]; empty →
///    [`restore_placeholder`].
pub fn sync_skin_patch(instance: &InstanceManifest, skins: &[SkinPackage]) -> Result<()> {
    let patch_path = DshAdapter::profile_dir(instance).join("cordis.patch.yml");
    let text = std::fs::read_to_string(&patch_path).unwrap_or_default();
    let stripped = crate::remove_skin_insert_blocks(&text);

    let enabled: Vec<&SkinPackage> = skins
        .iter()
        // A skin declaring `dsh.bundle` is already mounted through the profile
        // bundles (auto-registered by `dsh plugin add`); it must never also get
        // an insert row — that would double-mount the same package.
        .filter(|s| s.enabled && !skin_has_bundle(instance, &s.package))
        .collect();
    let next = if enabled.is_empty() {
        crate::restore_placeholder(&stripped)
    } else {
        let mut block = String::from("- insert:\n");
        let mut used: HashSet<String> = HashSet::new();
        for skin in enabled {
            let base = skin_id_from_package(&skin.package);
            let mut id = base.clone();
            let mut n = 2;
            while used.contains(&id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            used.insert(id.clone());
            block.push_str(&skin_insert_row(&skin.package, &id));
        }
        crate::append_block_to_text(&stripped, &block)
    };
    std::fs::write(&patch_path, next).with_context(|| format!("write {}", patch_path.display()))
}

/// Read the npm package name (`package.json.name`) from an install directory.
/// Returns `None` when the directory has no `package.json` or no `name` — the
/// signal that this is *not* the real skin package (e.g. a github monorepo root
/// shell), and the subdirectory fallback should keep looking.
pub fn skin_package_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("name")?.as_str().map(String::from)
}

/// Does the installed skin declare `dsh.bundle`? When it does, `dsh plugin add`
/// has already registered it into the profile bundles, so the launcher must
/// *not* write an insert row (that would double-mount). Reads the installed
/// package under the profile's `node_modules`.
pub fn skin_has_bundle(instance: &InstanceManifest, package: &str) -> bool {
    let pkg = DshAdapter::profile_dir(instance)
        .join("node_modules")
        .join(package)
        .join("package.json");
    let Ok(text) = std::fs::read_to_string(pkg) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value.pointer("/dsh/bundle").is_some()
}

/// Does the installed package mount a web-app JS client (`dsh.client`)? A
/// client bundle's own patch inserts a loader row that names the package, so at
/// boot the loader imports the package entry — a source-only GitHub checkout
/// (no built artifact) of such a bundle bricks the whole profile on the very
/// next boot. Resource-only bundles (assets/config, no `dsh.client`) never need
/// a built entry and stay exempt.
pub fn package_mounts_client(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value.pointer("/dsh/client").is_some()
}

/// Does the package's declared entry artifact actually exist on disk? A github
/// source checkout of a build-required skin ships no `lib/` — ported from
/// dsh-market's `entryArtifactExists` (profile.ts). `dsh plugin add` exits 0
/// for such a checkout (it only links the source directory), so without this
/// check the insert row gets written and the next boot dies with
/// `ERR_MODULE_NOT_FOUND`. Collects `main`, then `exports["."]` (string or its
/// object's string values), falling back to `index.js` when none are declared.
pub fn entry_artifact_exists(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let mut candidates: Vec<String> = Vec::new();
    if let Some(main) = value.get("main").and_then(|v| v.as_str()) {
        candidates.push(main.to_string());
    }
    match value.get("exports") {
        Some(serde_json::Value::String(s)) => candidates.push(s.clone()),
        Some(obj) => {
            if let Some(root) = obj.get(".") {
                match root {
                    serde_json::Value::String(s) => candidates.push(s.clone()),
                    serde_json::Value::Object(map) => {
                        for v in map.values() {
                            if let Some(s) = v.as_str() {
                                candidates.push(s.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if candidates.is_empty() {
        candidates.push("index.js".to_string());
    }
    candidates.iter().any(|rel| dir.join(rel).is_file())
}

/// Is an installed skin safe to leave enabled for the next boot? A *non-bundle*
/// skin is mounted by an insert row, so the loader must be able to resolve its
/// entry — without a built artifact it would brick the whole profile. A
/// *bundle* skin is auto-registered into the profile bundles by `dsh plugin
/// add`, so it is safe only when it is resource-only (assets/config, no
/// `dsh.client`); a bundle that mounts a web-app client inserts a loader row
/// that imports the package, so it needs its own built entry too. A source-only
/// checkout of such a bundle is the Angelina disease: `dsh plugin add` exits 0
/// and the next boot dies `ERR_MODULE_NOT_FOUND`.
pub fn installed_skin_loadable(instance: &InstanceManifest, package: &str) -> bool {
    let nm = DshAdapter::profile_dir(instance)
        .join("node_modules")
        .join(package);
    if skin_has_bundle(instance, package) && !package_mounts_client(&nm) {
        return true;
    }
    entry_artifact_exists(&nm)
}

/// Pre-boot quarantine for a leftover brick: a package sitting in
/// `dsh.profile.bundles` that mounts a web-app client (`dsh.client`) but whose
/// built entry is missing. That is the signature of an *interrupted* install —
/// `dsh plugin add` linked a source-only checkout and the job died before the
/// install-time gate (or `land_install_disabled`) could run. DSH would die
/// `ERR_MODULE_NOT_FOUND` importing it on the very next boot, and the
/// post-launch reconcile is too late to protect that boot. Disabling its loader
/// rows — exactly what `plugin_toggle(false)` does — makes the boot safe.
/// Returns the package names quarantined so the caller can tell the user to
/// remove them from Library.
///
/// Best-effort: a package we cannot disable (no disableable row, or a write
/// failure) is skipped rather than propagated — the quarantine must never wedge
/// the launch that invoked it.
pub fn quarantine_unloadable_client_bundles(instance: &InstanceManifest) -> Vec<String> {
    let profile_dir = DshAdapter::profile_dir(instance);
    let Ok(text) = std::fs::read_to_string(profile_dir.join("package.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(bundles) = value
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
    else {
        return Vec::new();
    };
    let mut quarantined = Vec::new();
    for bundle in bundles {
        let Some(name) = bundle.as_str() else {
            continue;
        };
        let nm = profile_dir.join("node_modules").join(name);
        if !skin_has_bundle(instance, name) || !package_mounts_client(&nm) {
            continue; // resource-only bundle or non-bundle — nothing auto-imports the package
        }
        if entry_artifact_exists(&nm) {
            continue; // built entry present → loads fine, nothing to quarantine
        }
        let ids = DshAdapter::plugin_row_ids(instance, name);
        if ids.is_empty() || DshAdapter::set_plugin_enabled(instance, name, false).is_err() {
            continue; // no disableable row, or the write failed — skip, don't propagate
        }
        quarantined.push(name.to_string());
    }
    quarantined
}

/// Structured validation of an MCP entry's config, returned as machine-readable
/// issue codes the frontend maps to localized hints. Install is intentionally
/// *not* blocked here: a curated catalog legitimately omits `command`/`url` on
/// some servers (they may need manual auth/env), so Library surfaces the hint
/// instead of refusing the write. Checked: `transport` + the transport-
/// appropriate endpoint, plus any declared auth-token placeholder the catalog
/// cannot itself supply (see [`mcp_needs_token`]).
pub fn mcp_config_issues(entry: &RegistryPlugin) -> Vec<String> {
    let mut issues = Vec::new();
    let transport = entry.transport.clone().unwrap_or_else(|| "stdio".into());
    match transport.as_str() {
        "stdio" => {
            if entry.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                issues.push("mcp.missingCommand".to_string());
            }
        }
        "streamable-http" => {
            let url = entry.mcp_url.as_deref().unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                issues.push("mcp.missingUrl".to_string());
            }
        }
        _ => issues.push("mcp.unknownTransport".to_string()),
    }
    if mcp_needs_token(entry) {
        issues.push("mcp.missingToken".to_string());
    }
    issues
}

/// Live validation of an *installed* MCP record — the record-side mirror of
/// [`mcp_config_issues`]. Library computes issues from the manifest record at
/// read time (like skills rows) instead of caching an install-time snapshot of
/// the catalog entry: a Phase-4 git-source server legitimately has a `null`
/// catalog `command` by design (the record only gains a real local `command`
/// after the local build), so judging the *entry* reports a false
/// "missing command" that persists on an otherwise Healthy record. Issue codes
/// are the same vocabulary as [`mcp_config_issues`]; the transport/endpoint
/// check reads the record's effective launch (`command`/`url`) and any
/// auth-token placeholder in its resolved env/headers.
pub fn mcp_record_config_issues(record: &McpServerRecord) -> Vec<String> {
    let mut issues = Vec::new();
    let transport = if record.transport.trim().is_empty() {
        "stdio"
    } else {
        record.transport.as_str()
    };
    match transport {
        "stdio" => {
            if record.command.trim().is_empty() {
                issues.push("mcp.missingCommand".to_string());
            }
        }
        "streamable-http" => {
            if !(record.url.starts_with("http://") || record.url.starts_with("https://")) {
                issues.push("mcp.missingUrl".to_string());
            }
        }
        _ => issues.push("mcp.unknownTransport".to_string()),
    }
    if mcp_record_needs_token(record) {
        issues.push("mcp.missingToken".to_string());
    }
    issues
}

/// The subset of `required` env vars a record has no non-empty value for. Used
/// by both [`mcp_record_missing_config`] (declarations persisted on the record)
/// and the Library snapshot's catalog fallback for legacy records that predate
/// `required_env` (their declaration lives only in `content-mcp-env.json`).
pub fn mcp_missing_against(
    record: &McpServerRecord,
    required: &[McpEnvRequirement],
) -> Vec<McpEnvRequirement> {
    required
        .iter()
        .filter(|req| record.env.get(&req.key).is_none_or(|v| v.trim().is_empty()))
        .cloned()
        .collect()
}

/// Which of a record's catalog-declared required env vars are still unset — the
/// live Library "needs configuring" signal. A declaration is satisfied when
/// `record.env` holds a non-empty value under that key. This is the *declared*
/// set (`required_env`, from `content-mcp-env.json`), independent of the
/// runtime degraded probe which only fires after launch.
pub fn mcp_record_missing_config(record: &McpServerRecord) -> Vec<McpEnvRequirement> {
    mcp_missing_against(record, &record.required_env)
}

/// Whether an installed MCP record carries an unresolved auth credential in its
/// effective launch config — mirrors [`mcp_needs_token`] over the record's
/// concrete `env`/`headers` (already merged from catalog + build + preferences).
/// Whether an installed MCP record declares an auth credential the launcher
/// cannot supply (a `${VAR}` reference or a token-named env/header). The
/// install rollback gate consults this: a server whose *record itself* declares
/// auth need is config-gated — a failed post-install probe is kept with an
/// honest badge rather than rolled back (the user configures the value, then
/// re-checks). A record that declares nothing and still cannot run is a genuine
/// install failure and is rolled back.
pub fn mcp_record_needs_token(record: &McpServerRecord) -> bool {
    record
        .env
        .values()
        .chain(record.headers.values())
        .any(|v| value_needs_token(v))
}

/// Whether an MCP entry declares an auth credential the catalog can't supply:
/// an `env`/`headers` value that is a `${VAR}` reference, or names a token/key.
/// The curated catalog ships these empty today, but the schema reserves them so
/// a future MCP can declare auth the same way the runtime consumes it.
fn mcp_needs_token(entry: &RegistryPlugin) -> bool {
    entry
        .env
        .iter()
        .flat_map(|m| m.values())
        .chain(entry.headers.iter().flat_map(|m| m.values()))
        .any(|v| value_needs_token(v))
}

/// Whether a single config value looks like a credential the runtime must
/// supply: a `${VAR}` reference, or a value that names a token/key. Shared by
/// [`mcp_needs_token`] and the MCP import warning path (roadmap §10 / P3).
pub(crate) fn value_needs_token(v: &str) -> bool {
    v.contains("${")
        || {
            let upper = v.to_ascii_uppercase();
            upper.contains("TOKEN")
                || upper.contains("API_KEY")
                || upper.contains("SECRET")
                || upper.contains("BEARER")
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_dir_name_flattens_owner_slash() {
        assert_eq!(skill_dir_name("anthropics/docx"), "anthropics-docx");
        assert_eq!(skill_dir_name("bare"), "bare");
    }

    #[test]
    fn sanitize_server_name_keeps_valid_and_truncates() {
        assert_eq!(sanitize_server_name("server-github"), "server-github");
        assert_eq!(sanitize_server_name("My Server/Repo"), "MyServerRepo");
        let long = "x".repeat(64);
        assert_eq!(sanitize_server_name(&long), "x".repeat(32));
        assert_eq!(sanitize_server_name("///"), "server");
    }

    fn rec(id: &str, server_name: &str) -> McpServerRecord {
        McpServerRecord {
            id: id.into(),
            server_name: server_name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn needs_allowed_directory_recognizes_only_filesystem_family() {
        // Directory-gated: the official filesystem server, under any name/id form.
        assert!(needs_allowed_directory(&rec("modelcontextprotocol/server-filesystem", "server-filesystem")));
        assert!(needs_allowed_directory(&rec("modelcontextprotocol/server-filesystem", "filesystem")));
        assert!(needs_allowed_directory(&rec("someone/mcp-filesystem", "filesystem")));
        // Deliberately NOT directory-gated — injecting a workspace into these
        // would corrupt their launch args.
        assert!(!needs_allowed_directory(&rec("modelcontextprotocol/server-github", "github")));
        assert!(!needs_allowed_directory(&rec("modelcontextprotocol/server-memory", "memory")));
        assert!(!needs_allowed_directory(&rec("ErnestoCorona/kanboard-mcp", "kanboard")));
        assert!(!needs_allowed_directory(&rec("mendableai/firecrawl-mcp", "firecrawl-mcp")));
        assert!(!needs_allowed_directory(&rec("modelcontextprotocol/server-puppeteer", "puppeteer")));
        // No false positive on id prefixes that merely contain the word.
        assert!(!needs_allowed_directory(&rec("acme/filesystem-proxy-not-local", "proxy")));
    }

    #[test]
    fn mcp_record_missing_config_reflects_declared_vs_set_env() {
        let req = |key: &str, secret: bool| McpEnvRequirement {
            key: key.into(),
            label: None,
            secret,
        };
        let mut record = rec("ErnestoCorona/kanboard-mcp", "kanboard");
        record.required_env = vec![req("KANBOARD_URL", false), req("KANBOARD_API_TOKEN", true)];

        // Nothing set → every declaration is missing.
        assert_eq!(mcp_record_missing_config(&record).len(), 2);

        // One set → only the unset key survives (with its secret flag).
        record.env.insert("KANBOARD_URL".into(), "https://pm.example.com".into());
        let missing = mcp_record_missing_config(&record);
        assert_eq!(missing, vec![req("KANBOARD_API_TOKEN", true)]);

        // All set → nothing missing.
        record.env.insert("KANBOARD_API_TOKEN".into(), "tok".into());
        assert!(mcp_record_missing_config(&record).is_empty());

        // Whitespace-only still counts as unset.
        record.env.insert("KANBOARD_API_TOKEN".into(), "  ".into());
        assert_eq!(mcp_record_missing_config(&record), vec![req("KANBOARD_API_TOKEN", true)]);

        // No declarations → never missing.
        record.required_env.clear();
        assert!(mcp_record_missing_config(&record).is_empty());
    }

    #[test]
    fn mcp_missing_against_supports_legacy_catalog_fallback() {
        let req = |key: &str, secret: bool| McpEnvRequirement {
            key: key.into(),
            label: None,
            secret,
        };
        // A legacy record predating `required_env`: its persisted declaration set
        // is empty, but the catalog still declares KANBOARD_URL. The Library
        // snapshot falls back to evaluating the declared list against the record.
        let mut record = rec("ErnestoCorona/kanboard-mcp", "kanboard");
        assert!(record.required_env.is_empty(), "legacy record shape");
        let declared = vec![req("KANBOARD_URL", false)];

        assert_eq!(mcp_missing_against(&record, &declared).len(), 1);
        // Record gained a value (future fill phase writes record.env) → hint clears.
        record.env.insert("KANBOARD_URL".into(), "https://pm.example.com".into());
        assert!(mcp_missing_against(&record, &declared).is_empty());
        // No declaration for this record → still nothing missing.
        assert!(mcp_missing_against(&record, &[]).is_empty());
    }

    /// A throwaway instance whose `$DSH_HOME` sits in temp; returns the instance
    /// and the dir (caller cleans up). Unique per tag + pid.
    fn test_instance(tag: &str) -> (InstanceManifest, std::path::PathBuf) {
        let ws = std::env::temp_dir().join(format!("ahl-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join("profiles").join("web")).unwrap();
        let instance = InstanceManifest {
            id: "test".into(),
            name: "Test".into(),
            runtime: launcher_core::RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "default".into(),
            plugins: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            skins: Vec::new(),
            skin_packages: Vec::new(),
            workspace: ws.display().to_string(),
        };
        (instance, ws)
    }

    fn patch_path(instance: &InstanceManifest) -> std::path::PathBuf {
        DshAdapter::profile_dir(instance).join("cordis.patch.yml")
    }

    #[test]
    fn mcp_record_maps_entry_connection_fields() {
        let entry = RegistryPlugin {
            name: "server-github".into(),
            owner: "modelcontextprotocol".into(),
            server_name: Some("github".into()),
            transport: Some("stdio".into()),
            command: Some("npx".into()),
            args: Some(vec!["-y".into(), "@modelcontextprotocol/server-github".into()]),
            env: Some(Default::default()),
            ..Default::default()
        };
        let r = mcp_record(&entry);
        assert_eq!(r.id, "modelcontextprotocol/server-github");
        assert_eq!(r.server_name, "github");
        assert_eq!(r.transport, "stdio");
        assert_eq!(r.command, "npx");
        assert_eq!(r.args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert!(r.enabled);
    }

    #[test]
    fn mcp_insert_row_builds_stdio_row() {
        let record = McpServerRecord {
            id: "modelcontextprotocol/server-github".into(),
            server_name: "github".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            ..Default::default()
        };
        let row = mcp_insert_row(&record).unwrap();
        assert!(row.starts_with("    - id: mcp-github\n"));
        assert!(row.contains("      name: '@deepseek-ai/dsh-mcp-client'\n"));
        assert!(row.contains("        serverName: github\n"));
        assert!(row.contains("        transport: stdio\n"));
        assert!(row.contains("        command: npx\n"));
        assert!(row.contains("        - -y\n"));
        assert!(row.contains("        env: {}\n"));
    }

    #[test]
    fn mcp_insert_row_builds_http_row() {
        let record = McpServerRecord {
            id: "o/web".into(),
            server_name: "web".into(),
            transport: "streamable-http".into(),
            url: "http://localhost:3000/mcp".into(),
            ..Default::default()
        };
        let row = mcp_insert_row(&record).unwrap();
        assert!(row.contains("        transport: streamable-http\n"));
        assert!(row.contains("        url: http://localhost:3000/mcp\n"));
        assert!(!row.contains("command:"));
    }

    #[test]
    fn sync_mcp_patch_compiles_records_into_single_block() {
        let (instance, ws) = test_instance("sync-multi");
        std::fs::write(patch_path(&instance), "[]\n").unwrap();

        let records = vec![rec("modelcontextprotocol/server-github", "github"), rec("o/web", "web")];
        sync_mcp_patch(&instance, &records).unwrap();

        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert_eq!(
            text.lines().filter(|l| l.starts_with("- insert:")).count(),
            1,
            "N servers compile into ONE insert block, got:\n{text}"
        );
        assert!(text.contains("mcp-github"), "{text}");
        assert!(text.contains("mcp-web"), "{text}");
        // The empty-list placeholder was commented out, not duplicated.
        assert!(text.contains("# []"), "{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn sync_mcp_patch_disable_reenable_and_uninstall() {
        let (instance, ws) = test_instance("sync-toggle");
        let github = rec("modelcontextprotocol/server-github", "github");
        let web = rec("o/web", "web");
        let both = |github_enabled: bool| vec![
            McpServerRecord { enabled: github_enabled, ..github.clone() },
            web.clone(),
        ];

        sync_mcp_patch(&instance, &both(true)).unwrap();

        // Disable github → its row disappears from the single block.
        sync_mcp_patch(&instance, &both(false)).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(!text.contains("mcp-github"), "{text}");
        assert!(text.contains("mcp-web"), "{text}");
        assert_eq!(text.lines().filter(|l| l.starts_with("- insert:")).count(), 1);

        // Re-enable → row is back.
        sync_mcp_patch(&instance, &both(true)).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(text.contains("mcp-github"), "{text}");

        // Uninstall both (records gone) → block disappears, `[]` restored.
        sync_mcp_patch(&instance, &[]).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(!text.contains("- insert:"), "{text}");
        assert_eq!(text.trim(), "[]", "placeholder restored, got:\n{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn sync_mcp_patch_preserves_plugin_rows_and_user_blocks() {
        let (instance, ws) = test_instance("sync-preserve");
        std::fs::write(
            patch_path(&instance),
            "# launcher comment\n- insert:\n    - id: user-row\n      name: other-plugin\n      config:\n        x: 1\n- id: timer\n  disabled: true\n",
        )
        .unwrap();

        sync_mcp_patch(&instance, &[rec("modelcontextprotocol/server-github", "github")]).unwrap();

        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(text.contains("# launcher comment"), "{text}");
        assert!(text.contains("user-row"), "{text}");
        assert!(text.contains("- id: timer\n  disabled: true"), "{text}");
        assert!(text.contains("mcp-github"), "{text}");
        // User block kept alongside — the launcher block is appended, never merged.
        assert_eq!(text.lines().filter(|l| l.starts_with("- insert:")).count(), 2, "{text}");
        assert!(text.trim_end().ends_with("env: {}"), "launcher block appended last:\n{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    fn skin(key: &str, package: &str, enabled: bool) -> SkinPackage {
        SkinPackage {
            key: key.into(),
            package: package.into(),
            enabled,
        }
    }

    #[test]
    fn skin_id_from_package_derives_stable_slug() {
        assert_eq!(skin_id_from_package("dsh-skin-sakura"), "skin-sakura");
        assert_eq!(skin_id_from_package("dsh-client-ui-aqua"), "skin-ui-aqua");
        assert_eq!(skin_id_from_package("@deepseek-ai/dsh-skin-dark"), "skin-dark");
        assert_eq!(skin_id_from_package("plain"), "skin-plain");
        assert_eq!(skin_id_from_package("dsh-skin"), "skin");
    }

    #[test]
    fn skin_insert_row_quotes_scoped_names() {
        assert_eq!(
            skin_insert_row("dsh-skin-sakura", "skin-sakura"),
            "    - id: skin-sakura\n      name: dsh-skin-sakura\n"
        );
        assert_eq!(
            skin_insert_row("@scope/pkg", "skin-pkg"),
            "    - id: skin-pkg\n      name: '@scope/pkg'\n"
        );
    }

    #[test]
    fn sync_skin_patch_compiles_enabled_skins_into_single_block() {
        let (instance, ws) = test_instance("skin-sync-multi");
        std::fs::write(patch_path(&instance), "[]\n").unwrap();
        let skins = vec![
            skin("owner/sakura", "dsh-skin-sakura", true),
            skin("owner/dark", "dsh-skin-dark", true),
        ];
        sync_skin_patch(&instance, &skins).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert_eq!(text.lines().filter(|l| l.starts_with("- insert:")).count(), 1, "{text}");
        assert!(text.contains("skin-sakura"), "{text}");
        assert!(text.contains("skin-dark"), "{text}");
        assert!(text.contains("# []"), "{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn sync_skin_patch_disable_reenable_and_uninstall() {
        let (instance, ws) = test_instance("skin-sync-toggle");
        let sakura = skin("owner/sakura", "dsh-skin-sakura", true);
        let dark = skin("owner/dark", "dsh-skin-dark", true);
        let both = |sakura_enabled: bool| vec![
            SkinPackage {
                enabled: sakura_enabled,
                ..sakura.clone()
            },
            dark.clone(),
        ];

        sync_skin_patch(&instance, &both(true)).unwrap();

        sync_skin_patch(&instance, &both(false)).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(!text.contains("skin-sakura"), "{text}");
        assert!(text.contains("skin-dark"), "{text}");
        assert_eq!(text.lines().filter(|l| l.starts_with("- insert:")).count(), 1);

        sync_skin_patch(&instance, &both(true)).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(text.contains("skin-sakura"), "{text}");

        sync_skin_patch(&instance, &[]).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(!text.contains("- insert:"), "{text}");
        assert_eq!(text.trim(), "[]", "placeholder restored, got:\n{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn sync_skin_patch_preserves_mcp_and_plugin_rows() {
        let (instance, ws) = test_instance("skin-sync-preserve");
        std::fs::write(
            patch_path(&instance),
            "# launcher comment\n- insert:\n    - id: mcp-a\n      name: '@deepseek-ai/dsh-mcp-client'\n- id: timer\n  disabled: true\n",
        )
        .unwrap();
        sync_skin_patch(&instance, &[skin("owner/sakura", "dsh-skin-sakura", true)]).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(text.contains("# launcher comment"), "{text}");
        assert!(text.contains("mcp-a"), "{text}");
        assert!(text.contains("- id: timer\n  disabled: true"), "{text}");
        assert!(text.contains("skin-sakura"), "{text}");
        // MCP block kept; skin block appended → two insert blocks.
        assert_eq!(text.lines().filter(|l| l.starts_with("- insert:")).count(), 2, "{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn sync_skin_patch_never_inserts_a_bundle_skin() {
        let (instance, ws) = test_instance("skin-sync-bundle");
        // catppuccin is installed and declares `dsh.bundle`; sakura is a plain
        // client skin. Both are enabled in the launcher model.
        let profile = DshAdapter::profile_dir(&instance);
        let bundle_dir = profile
            .join("node_modules")
            .join("dsh-catppuccin");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(
            bundle_dir.join("package.json"),
            r#"{"name":"dsh-catppuccin","keywords":["skin"],"dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        let skins = vec![
            skin("owner/catppuccin", "dsh-catppuccin", true),
            skin("owner/sakura", "dsh-skin-sakura", true),
        ];
        sync_skin_patch(&instance, &skins).unwrap();
        let text = std::fs::read_to_string(patch_path(&instance)).unwrap();
        assert!(
            !text.contains("dsh-catppuccin"),
            "a dsh.bundle skin must not get an insert row (double mount):\n{text}"
        );
        assert!(text.contains("skin-sakura"), "client skin still inserted:\n{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn skin_package_name_reads_json_name() {
        let root = std::env::temp_dir().join(format!("ahl-skin-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"dsh-skin-sakura"}"#).unwrap();
        assert_eq!(skin_package_name(&root), Some("dsh-skin-sakura".into()));

        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(skin_package_name(&empty), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A scratch directory unique to one test — the pid alone would collide
    /// with a sibling test's tree when the suite runs in parallel.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ahl-skill-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fetch_dir_in_repo_parses_root_and_nested_skills() {
        // Nested: the skill lives in a subdirectory of the repo.
        assert_eq!(
            fetch_dir_in_repo(Some(
                "https://raw.githubusercontent.com/anthropics/skills/HEAD/skills/docx/SKILL.md"
            )),
            Some("skills/docx".to_string())
        );
        // Root: the skill *is* the repo (`…/HEAD/SKILL.md`).
        assert_eq!(
            fetch_dir_in_repo(Some(
                "https://raw.githubusercontent.com/rootkiller6788/mathmodel-skill/HEAD/SKILL.md"
            )),
            Some(String::new())
        );
        // Anything that isn't a raw SKILL.md URL pins no directory.
        assert_eq!(fetch_dir_in_repo(Some("https://github.com/o/r")), None);
        assert_eq!(fetch_dir_in_repo(None), None);
    }

    #[test]
    fn clone_args_disable_line_ending_translation() {
        // Without this a Windows checkout turns LF into CRLF, the installed
        // `SKILL.md` stops matching the raw URL byte for byte, and every skill
        // shows "update available" forever.
        let args = clone_args("https://github.com/o/r", Path::new("dest"));
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "core.autocrlf=false");
        assert_eq!(args[2], "clone");
        assert!(args.iter().any(|arg| arg == "--depth"));
    }

    #[test]
    fn raw_skill_md_candidates_resolves_both_record_shapes() {
        // A record that captured the catalog's raw URL probes exactly there.
        assert_eq!(
            raw_skill_md_candidates(
                "https://raw.githubusercontent.com/o/r/HEAD/skills/docx/SKILL.md"
            ),
            vec!["https://raw.githubusercontent.com/o/r/HEAD/skills/docx/SKILL.md"]
        );
        // A repo URL gets no raw candidate at all: its root `SKILL.md` is not
        // necessarily *this* skill's, so the probe resolves it by cloning.
        assert!(raw_skill_md_candidates("https://github.com/o/r").is_empty());
        // A non-github source is used as-is.
        assert_eq!(
            raw_skill_md_candidates("https://example.test/skill/SKILL.md"),
            vec!["https://example.test/skill/SKILL.md"]
        );
    }

    #[test]
    fn github_repo_normalises_urls() {
        assert_eq!(github_repo("https://github.com/o/r").as_deref(), Some("o/r"));
        assert_eq!(github_repo("https://github.com/o/r/").as_deref(), Some("o/r"));
        assert_eq!(github_repo("https://github.com/o/r.git").as_deref(), Some("o/r"));
        assert_eq!(github_repo("https://gitlab.com/o/r"), None);
        assert_eq!(github_repo("https://github.com/"), None);
    }

    #[test]
    fn skill_root_prefers_the_catalog_fetch_path_over_a_name_search() {
        let root = scratch("root");
        // A repo whose root is itself a skill, which also happens to contain a
        // `docx/SKILL.md` — so the name search and the catalog's pinned path
        // disagree about which one is the skill.
        std::fs::create_dir_all(root.join("docx")).unwrap();
        std::fs::write(root.join("SKILL.md"), "root skill").unwrap();
        std::fs::write(root.join("docx").join("SKILL.md"), "nested skill").unwrap();

        // The pinned URL goes through the same parse the installer uses.
        let pinned = Some("https://raw.githubusercontent.com/o/r/HEAD/SKILL.md");
        let hint = fetch_dir_in_repo(pinned);
        assert_eq!(skill_root(&root, hint.as_deref(), "docx").unwrap(), root);

        // With no pinned path the search takes over and matches on the name.
        assert_eq!(skill_root(&root, None, "docx").unwrap(), root.join("docx"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_bundle_keeps_nested_files_and_skips_caches() {
        let root = scratch("bundle");
        for (rel, body) in [
            ("SKILL.md", "root skill"),
            ("references/stage_1.md", "stage one"),
            ("tools/run.sh", "#!/bin/sh"),
            ("node_modules/dep/index.js", "module.exports = {}"),
            (".git/config", "[core]"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
        }
        let files = read_bundle(&root).unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, vec!["SKILL.md", "references/stage_1.md", "tools/run.sh"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn land_bundle_writes_the_tree_and_prunes_what_the_author_deleted() {
        let dir = scratch("land");
        let first = SkillBundle::new(
            vec![
                ("SKILL.md".to_string(), b"v1".to_vec()),
                ("references/old.md".to_string(), b"gone next version".to_vec()),
                ("scripts/check.py".to_string(), b"print(1)".to_vec()),
            ],
            "https://example.test/skill".to_string(),
        )
        .unwrap();
        land_bundle(&dir, &first).unwrap();
        assert!(dir.join("references/old.md").is_file());
        assert!(dir.join("scripts/check.py").is_file());
        assert!(bundle_matches_disk(&dir, &first));

        // The next version drops `references/` and `scripts/` entirely.
        let second = SkillBundle::new(
            vec![
                ("SKILL.md".to_string(), b"v2".to_vec()),
                ("templates/report.tex".to_string(), b"\\documentclass".to_vec()),
            ],
            "https://example.test/skill".to_string(),
        )
        .unwrap();
        land_bundle(&dir, &second).unwrap();
        assert!(!dir.join("references/old.md").exists());
        assert!(!dir.join("scripts/check.py").exists());
        // …and the directories they lived in go with them, rather than lingering
        // empty for the skill to enumerate.
        assert!(!dir.join("references").exists());
        assert!(!dir.join("scripts").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("templates/report.tex")).unwrap(),
            "\\documentclass"
        );
        assert_eq!(std::fs::read_to_string(dir.join("SKILL.md")).unwrap(), "v2");
        assert!(bundle_matches_disk(&dir, &second));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_matches_disk_spots_a_changed_sibling_file() {
        let dir = scratch("match");
        let bundle = SkillBundle::new(
            vec![
                ("SKILL.md".to_string(), b"body".to_vec()),
                ("references/a.md".to_string(), b"one".to_vec()),
            ],
            String::new(),
        )
        .unwrap();
        land_bundle(&dir, &bundle).unwrap();
        assert!(bundle_matches_disk(&dir, &bundle));

        // Same `SKILL.md`, changed sibling — exactly the case a hash-only
        // comparison declares "already up to date".
        let updated = SkillBundle::new(
            vec![
                ("SKILL.md".to_string(), b"body".to_vec()),
                ("references/a.md".to_string(), b"two".to_vec()),
            ],
            String::new(),
        )
        .unwrap();
        assert_eq!(updated.hash(), bundle.hash());
        assert!(!bundle_matches_disk(&dir, &updated));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bundle_without_a_skill_md_is_refused() {
        let err = SkillBundle::new(vec![("README.md".to_string(), b"x".to_vec())], String::new())
            .unwrap_err();
        assert!(err.to_string().contains("SKILL.md"), "{err}");
    }

    /// The item's real-machine acceptance check: install a genuine multi-file
    /// skill and confirm the whole directory lands, not just its `SKILL.md`.
    /// Opt-in because it clones from github:
    /// `cargo test -p dsh-adapter --lib -- --ignored installs_a_real`
    #[tokio::test]
    #[ignore = "clones rootkiller6788/mathmodel-skill from github — needs network + git"]
    async fn installs_a_real_multi_file_skill_directory() {
        let (instance, ws) = test_instance("real-skill");
        let entry = RegistryPlugin {
            kind: launcher_core::market::ContentKind::Skill,
            name: "mathmodel-skill".into(),
            owner: "rootkiller6788".into(),
            url: "https://github.com/rootkiller6788/mathmodel-skill".into(),
            fetch: Some(
                "https://raw.githubusercontent.com/rootkiller6788/mathmodel-skill/HEAD/SKILL.md"
                    .into(),
            ),
            ..Default::default()
        };
        let record = install_skill(&instance, &entry, false).await.unwrap();
        assert_eq!(record.id, "rootkiller6788/mathmodel-skill");
        assert_eq!(record.hash.len(), 64);

        let dir = ws.join("skills").join("rootkiller6788-mathmodel-skill");
        for rel in [
            "SKILL.md",
            "references",
            "tools",
            "scripts/latex_check",
            "templates/latex",
        ] {
            assert!(dir.join(rel).exists(), "{rel} did not land in the skill");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Item 10's real-machine check. The claim worth verifying is not any
    /// particular hash but *agreement*: the probe must answer with the content an
    /// install would land, or the update check offers updates that do not exist
    /// (or misses ones that do). Opt-in, and slow — it clones several times:
    /// `cargo test -p dsh-adapter --lib -- --ignored probes_agree`
    #[tokio::test]
    #[ignore = "clones from github a few times — needs network + git"]
    async fn probes_agree_with_what_an_install_lands() {
        let entry = |fetch: Option<&str>| RegistryPlugin {
            kind: launcher_core::market::ContentKind::Skill,
            name: "mathmodel-skill".into(),
            owner: "rootkiller6788".into(),
            url: "https://github.com/rootkiller6788/mathmodel-skill".into(),
            fetch: fetch.map(str::to_string),
            ..Default::default()
        };

        // The shape the catalog produces: `fetch` pins the exact `SKILL.md`.
        let pinned = entry(Some(
            "https://raw.githubusercontent.com/rootkiller6788/mathmodel-skill/HEAD/SKILL.md",
        ));
        let source = skill_source(&pinned).unwrap();
        let hash = fetch_skill_hash(&source, &pinned.name, false).await.unwrap();
        let (instance, ws) = test_instance("probe-pinned");
        assert_eq!(hash, install_skill(&instance, &pinned, false).await.unwrap().hash);
        let _ = std::fs::remove_dir_all(&ws);

        // The shape a record captured before the catalog pinned `fetch`, i.e. a
        // bare repo URL. Hashing its HTML landing page — what the probe used to
        // do — could never agree with an install.
        let repo_only = entry(None);
        let source = skill_source(&repo_only).unwrap();
        assert_eq!(source, repo_only.url);
        let hash = fetch_skill_hash(&source, &repo_only.name, false).await.unwrap();
        let (instance, ws) = test_instance("probe-repo");
        assert_eq!(hash, install_skill(&instance, &repo_only, false).await.unwrap().hash);
        let _ = std::fs::remove_dir_all(&ws);

        // A skill nested inside its repo: resolved by the name search, which is
        // the only thing a repo URL offers.
        let nested = fetch_skill_hash("https://github.com/anthropics/skills", "docx", false)
            .await
            .unwrap();
        let nested_raw = fetch_text(
            "https://raw.githubusercontent.com/anthropics/skills/HEAD/skills/docx/SKILL.md",
            false,
        )
        .await
        .unwrap();
        assert_eq!(nested, sha256_hex(nested_raw.as_bytes()));

        // A raw URL whose file moved: the probe answers from the clone instead
        // of failing — the 404 fallback this item adds.
        let moved =
            "https://raw.githubusercontent.com/rootkiller6788/mathmodel-skill/HEAD/nope/SKILL.md";
        assert!(
            fetch_text(moved, false).await.is_err(),
            "the path must 404 for this to prove anything"
        );
        assert_eq!(
            fetch_skill_hash(moved, "mathmodel-skill", false).await.unwrap(),
            nested_hash_of_repo("rootkiller6788/mathmodel-skill", "mathmodel-skill").await
        );
    }

    /// The hash an install would record for a repo with no pinned path — the
    /// probe's answer after a raw URL 404s.
    async fn nested_hash_of_repo(repo: &str, name: &str) -> String {
        let files = clone_bundle(repo, None, name, false).await.unwrap();
        SkillBundle::new(files, String::new()).unwrap().hash()
    }

    #[test]
    fn safe_join_refuses_paths_that_escape_the_skill_directory() {
        let dir = PathBuf::from("/skills/owner-name");
        assert_eq!(
            safe_join(&dir, "references/a.md").unwrap(),
            PathBuf::from("/skills/owner-name/references/a.md")
        );
        for bad in [
            "../outside.md",
            "refs/../../outside.md",
            "/absolute.md",
            "..",
            "",
            "a\\b.md",
        ] {
            assert!(safe_join(&dir, bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn find_skill_md_prefers_matching_dir() {
        let root = std::env::temp_dir().join(format!("ahl-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let deep = root.join("skills").join("docx");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("SKILL.md"), "deep").unwrap();
        let other = root.join("unrelated");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("SKILL.md"), "other").unwrap();

        assert_eq!(find_skill_md(&root, "docx").unwrap(), deep.join("SKILL.md"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_frontmatter_valid_requires_name() {
        assert!(skill_frontmatter_valid("---\nname: docx\ndescription: read docs\n---\nbody"));
        assert!(!skill_frontmatter_valid("---\ndescription: no name\n---\nbody"));
        assert!(!skill_frontmatter_valid("no frontmatter at all"));
        assert!(!skill_frontmatter_valid(""));
    }

    #[test]
    fn mcp_config_issues_flags_missing_endpoint() {
        

        let stdio_missing = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("stdio".into()),
            command: None,
            ..Default::default()
        };
        assert_eq!(mcp_config_issues(&stdio_missing), vec!["mcp.missingCommand"]);

        let http_missing = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("streamable-http".into()),
            mcp_url: None,
            ..Default::default()
        };
        assert_eq!(mcp_config_issues(&http_missing), vec!["mcp.missingUrl"]);

        let unknown = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("sse".into()),
            ..Default::default()
        };
        assert_eq!(mcp_config_issues(&unknown), vec!["mcp.unknownTransport"]);

        let ok = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("stdio".into()),
            command: Some("npx".into()),
            ..Default::default()
        };
        assert!(mcp_config_issues(&ok).is_empty());
    }

    #[test]
    fn mcp_config_issues_flags_declared_token_placeholder() {
        use std::collections::HashMap;

        let bearer = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("streamable-http".into()),
            mcp_url: Some("https://api.example.com/mcp".into()),
            headers: Some(HashMap::from([(
                "Authorization".into(),
                "Bearer ${TOKEN}".into(),
            )])),
            ..Default::default()
        };
        assert_eq!(
            mcp_config_issues(&bearer),
            vec!["mcp.missingToken".to_string()]
        );

        let key_env = RegistryPlugin {
            name: "s".into(),
            owner: "o".into(),
            transport: Some("stdio".into()),
            command: Some("npx".into()),
            env: Some(HashMap::from([(
                "API_KEY".into(),
                "${API_KEY}".into(),
            )])),
            ..Default::default()
        };
        assert_eq!(
            mcp_config_issues(&key_env),
            vec!["mcp.missingToken".to_string()]
        );
    }

    #[test]
    fn mcp_record_config_issues_reads_record_not_entry() {
        use std::collections::HashMap;

        // stdio record with no command → flagged (record genuinely can't launch).
        let stdio_missing = McpServerRecord {
            id: "o/s".into(),
            transport: "stdio".into(),
            ..Default::default()
        };
        assert_eq!(
            mcp_record_config_issues(&stdio_missing),
            vec!["mcp.missingCommand"]
        );

        // http record with no url → flagged.
        let http_missing = McpServerRecord {
            id: "o/s".into(),
            transport: "streamable-http".into(),
            ..Default::default()
        };
        assert_eq!(
            mcp_record_config_issues(&http_missing),
            vec!["mcp.missingUrl"]
        );

        // Unknown transport → flagged.
        let unknown = McpServerRecord {
            id: "o/s".into(),
            transport: "sse".into(),
            ..Default::default()
        };
        assert_eq!(
            mcp_record_config_issues(&unknown),
            vec!["mcp.unknownTransport"]
        );

        // A Phase-4 source-built record: catalog command is null by design but
        // the *record* now carries a real local launch line → clean, no issue.
        let git_built = McpServerRecord {
            id: "o/s".into(),
            transport: "stdio".into(),
            command: "C:\\instances\\mcp\\kanboard\\bin\\kanboard-mcp.exe".into(),
            ..Default::default()
        };
        assert!(mcp_record_config_issues(&git_built).is_empty());

        // Token placeholder declared on the record's own env → flagged.
        let key_env = McpServerRecord {
            id: "o/s".into(),
            transport: "stdio".into(),
            command: "server".into(),
            env: HashMap::from([("API_KEY".into(), "${API_KEY}".into())]),
            ..Default::default()
        };
        assert_eq!(
            mcp_record_config_issues(&key_env),
            vec!["mcp.missingToken"]
        );
    }

    #[test]
    fn skill_loader_active_detects_loader_module() {
        use crate::{InstalledPluginKind, InstalledPluginSource};
        let plugin = |name: &str| InstalledPlugin {
            name: name.to_string(),
            enabled: true,
            toggleable: false,
            kind: InstalledPluginKind::Plugin,
            source: InstalledPluginSource::Inventory,
            entry_id: None,
            fiber_phase: None,
        };
        assert!(skill_loader_active(&[plugin("skill-filesystem"), plugin("timer")]));
        assert!(!skill_loader_active(&[plugin("timer"), plugin("market")]));
        assert!(!skill_loader_active(&[]));
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // RFC 6234 test vector for SHA-256("abc").
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn skill_source_prefers_fetch_then_repo() {
        let fetch = RegistryPlugin {
            name: "docx".into(),
            owner: "anthropics".into(),
            fetch: Some("https://raw.githubusercontent.com/anthropics/skills/HEAD/skills/docx/SKILL.md".into()),
            url: "https://github.com/anthropics/skills".into(),
            ..Default::default()
        };
        assert_eq!(
            skill_source(&fetch).unwrap(),
            "https://raw.githubusercontent.com/anthropics/skills/HEAD/skills/docx/SKILL.md"
        );

        let repo_only = RegistryPlugin {
            name: "docx".into(),
            owner: "anthropics".into(),
            fetch: None,
            url: "https://github.com/anthropics/skills".into(),
            ..Default::default()
        };
        assert_eq!(skill_source(&repo_only).unwrap(), "https://github.com/anthropics/skills");

        let none = RegistryPlugin {
            name: "ghost".into(),
            owner: "o".into(),
            fetch: None,
            url: String::new(),
            ..Default::default()
        };
        assert!(skill_source(&none).is_none());
    }

    #[test]
    fn write_atomic_lands_and_replaces_without_residue() {
        let dir = std::env::temp_dir().join(format!("ahl-skill-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_atomic(&dir, "SKILL.md", b"v1 body").unwrap();
        let path = dir.join("SKILL.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1 body");
        // No temp sibling survives a successful write.
        let residue: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(residue.is_empty(), "no .tmp residue after a clean write");

        // Overwrite (an update) replaces content atomically.
        write_atomic(&dir, "SKILL.md", "v2 body \u{2014} longer".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v2 body \u{2014} longer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_skill_record_shape_matches_provenance() {
        // install_skill hits the network, so exercise the record contract via
        // the same pieces it composes: write_atomic + sha256 over SKILL.md.
        let dir = std::env::temp_dir().join(format!("ahl-skill-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = b"---\nname: docx\ndescription: read docs\n---\n# DOCX\n";
        write_atomic(&dir, "SKILL.md", body).unwrap();
        let installed = std::fs::read(dir.join("SKILL.md")).unwrap();
        assert_eq!(sha256_hex(&installed), sha256_hex(body));
        assert_eq!(sha256_hex(&installed).len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_artifact_exists_matches_declared_entry_files() {
        // A git checkout whose `main` names a file that does not exist on disk
        // — tp7's disease. `dsh plugin add` exits 0 for it.
        // (declared main via the test helper's file param is empty → no file)
        let dir = std::env::temp_dir().join(format!("ahl-tp7-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-client-ui-tp7-skin","main":"lib/index.js"}"#,
        )
        .unwrap();
        assert!(!entry_artifact_exists(&dir), "declared-but-missing main is NOT loadable");
        let _ = std::fs::remove_dir_all(&dir);

        // A working skin: `main` present on disk.
        let dir = std::env::temp_dir().join(format!("ahl-silk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/index.js"), b"export {}").unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"silk-background","main":"lib/index.js"}"#,
        )
        .unwrap();
        assert!(entry_artifact_exists(&dir), "present main is loadable");
        let _ = std::fs::remove_dir_all(&dir);

        // No main/exports but an index.js fallback.
        let dir = std::env::temp_dir().join(format!("ahl-wx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.js"), b"export {}").unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name":"ui-dsh-wx-skin"}"#).unwrap();
        assert!(entry_artifact_exists(&dir), "index.js fallback is loadable");
        let _ = std::fs::remove_dir_all(&dir);

        // No main/exports and no index.js — a bare source shell.
        let dir = std::env::temp_dir().join(format!("ahl-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name":"bare-shell"}"#).unwrap();
        assert!(!entry_artifact_exists(&dir), "no artifact at all is NOT loadable");
        let _ = std::fs::remove_dir_all(&dir);

        // exports["."] as a string form (modern dual-package skins).
        let dir = std::env::temp_dir().join(format!("ahl-exp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(dir.join("dist/client.js"), b"export {}").unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"modern-skin","exports":{".":"./dist/client.js"}}"#,
        )
        .unwrap();
        assert!(entry_artifact_exists(&dir), "string exports['.'] is loadable");
        let _ = std::fs::remove_dir_all(&dir);

        // exports["."] naming a missing file — the same brick through exports.
        let dir = std::env::temp_dir().join(format!("ahl-expmiss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"exp-miss","exports":{".":"./dist/missing.js"}}"#,
        )
        .unwrap();
        assert!(!entry_artifact_exists(&dir), "missing exports['.'] is NOT loadable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_skin_loadable_gates_client_bundles_not_resource_bundles() {
        // The Angelina disease: a `dsh.bundle` skin that ALSO mounts a web-app
        // client (`dsh.client`) from a source-only checkout (no built entry).
        // `dsh plugin add` exits 0 and the old gate blessed any bundle — the
        // next boot died ERR_MODULE_NOT_FOUND. A client bundle with no built
        // artifact must NOT be loadable.
        let (instance, ws) = test_instance("loadable-angelina");
        let pkg = DshAdapter::profile_dir(&instance)
            .join("node_modules")
            .join("@flowerwater1019/angelina-dsh-plugin");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@flowerwater1019/angelina-dsh-plugin","main":"lib/index.js","dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        assert!(
            !installed_skin_loadable(&instance, "@flowerwater1019/angelina-dsh-plugin"),
            "client bundle with no built entry must be rejected"
        );
        let _ = std::fs::remove_dir_all(&ws);

        // Same bundle shape but the repo commits its build (catppuccin / glass):
        // the built entry exists → loadable.
        let (instance, ws) = test_instance("loadable-client-built");
        let pkg = DshAdapter::profile_dir(&instance)
            .join("node_modules")
            .join("dsh-catppuccin");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(pkg.join("lib/index.js"), b"export {}").unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"dsh-catppuccin","main":"lib/index.js","dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        assert!(installed_skin_loadable(&instance, "dsh-catppuccin"));
        let _ = std::fs::remove_dir_all(&ws);

        // Resource-only bundle (no `dsh.client`, no own JS): exempt — a bundle
        // that only ships assets/config never needs a built entry.
        let (instance, ws) = test_instance("loadable-resource");
        let pkg = DshAdapter::profile_dir(&instance)
            .join("node_modules")
            .join("dsh-resource-theme");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"dsh-resource-theme","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        assert!(installed_skin_loadable(&instance, "dsh-resource-theme"));
        let _ = std::fs::remove_dir_all(&ws);

        // A non-bundle client skin with a missing built entry (tp7) stays
        // rejected — unchanged by the client-bundle tightening.
        let (instance, ws) = test_instance("loadable-tp7");
        let pkg = DshAdapter::profile_dir(&instance)
            .join("node_modules")
            .join("@deepseek-ai/dsh-client-ui-tp7-skin");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-client-ui-tp7-skin","main":"lib/index.js"}"#,
        )
        .unwrap();
        assert!(
            !installed_skin_loadable(&instance, "@deepseek-ai/dsh-client-ui-tp7-skin"),
            "tp7 source-only skin stays rejected"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn quarantine_disables_only_unloadable_client_bundles() {
        // A profile whose bundles carry a client bundle with no built entry — the
        // interrupted-install signature. Quarantine must disable its row (so the
        // next boot is safe) and leave resource-only + built bundles alone.
        let (instance, ws) = test_instance("quarantine");
        let profile = DshAdapter::profile_dir(&instance);
        let profile_pkg = serde_json::json!({
            "name": "dsh-profile-web",
            "dsh": { "profile": { "bundles": ["dsh-brick", "dsh-res", "dsh-built"] } }
        });
        std::fs::write(
            profile.join("package.json"),
            serde_json::to_string_pretty(&profile_pkg).unwrap(),
        )
        .unwrap();
        std::fs::write(profile.join("cordis.patch.yml"), "[]\n").unwrap();

        let nm = profile.join("node_modules");
        // The brick: dsh.client bundle, self-named insert row, no built lib/.
        let brick = nm.join("dsh-brick");
        std::fs::create_dir_all(&brick).unwrap();
        std::fs::write(
            brick.join("package.json"),
            r#"{"name":"dsh-brick","main":"lib/index.js","dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        std::fs::write(
            brick.join("cordis.patch.yml"),
            "- insert:\n    - id: brick\n      name: 'dsh-brick'\n",
        )
        .unwrap();
        // A resource-only bundle (no dsh.client): safe without a built entry.
        let res = nm.join("dsh-res");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(
            res.join("package.json"),
            r#"{"name":"dsh-res","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(res.join("cordis.patch.yml"), "[]\n").unwrap();
        // A client bundle that commits its build: loads fine, no quarantine.
        let built = nm.join("dsh-built");
        std::fs::create_dir_all(built.join("lib")).unwrap();
        std::fs::write(built.join("lib/index.js"), b"export {}").unwrap();
        std::fs::write(
            built.join("package.json"),
            r#"{"name":"dsh-built","main":"lib/index.js","dsh":{"bundle":{"patch":"./cordis.patch.yml"},"client":{"platform":"web"}}}"#,
        )
        .unwrap();
        std::fs::write(
            built.join("cordis.patch.yml"),
            "- insert:\n    - id: built\n      name: 'dsh-built'\n",
        )
        .unwrap();

        let quarantined = quarantine_unloadable_client_bundles(&instance);
        assert_eq!(quarantined, vec!["dsh-brick"], "only the brick is quarantined");
        let text = std::fs::read_to_string(profile.join("cordis.patch.yml")).unwrap();
        assert!(
            text.contains("- id: brick\n  disabled: true"),
            "brick row disabled so the next boot skips it:\n{text}"
        );
        assert!(!text.contains("id: built"), "built bundle untouched:\n{text}");
        assert!(!text.contains("id: res"), "resource bundle untouched:\n{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
