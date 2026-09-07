//! Content installers for the Market's non-plugin kinds.
//!
//! Skills are plain files under `$DSH_HOME/skills/` — a `SKILL.md` (or a
//! directory-bundle `<name>/SKILL.md`) discovered by DSH's `skill-filesystem`
//! plugin. There is no npm install and no enable toggle; the launcher lands a
//! `SKILL.md` atomically (write to a `.tmp` sibling, then rename) and records
//! `{source, hash, installed}` provenance in `InstanceManifest.skills` — the
//! SHA-256 of the content is the only update signal DSH exposes.
//!
//! Install tries a pre-resolved raw `fetch` URL first (fast path), then falls
//! back to a shallow `git clone` of the source repo + a `SKILL.md` search —
//! the awesome-* markdown gives no reliable path, so the clone is what makes
//! install work across the many repo layouts.
//!
//! (MCP connection records live in `InstanceManifest.mcp` — the single source
//! of truth — and are compiled into `cordis.patch.yml` as a whole by
//! [`sync_mcp_patch`]; install/uninstall/disable mutate the record then
//! regenerate.)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
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

/// Download a skill's `SKILL.md` and land it atomically at
/// `$DSH_HOME/skills/<id>/SKILL.md`: the body is written to a same-directory
/// `.tmp` sibling and then renamed over the target, so a crash can never leave
/// a half-written file at the final path. Returns the provenance record (source
/// + content SHA-256); the command layer stamps `installed`.
pub async fn install_skill(
    instance: &InstanceManifest,
    entry: &RegistryPlugin,
) -> Result<SkillRecord> {
    let (text, source) = fetch_skill_md(entry).await?;
    let id = skill_id(entry);
    let dir = skills_dir(instance).join(skill_dir_name(&id));
    write_atomic(&dir, "SKILL.md", text.as_bytes())?;
    Ok(SkillRecord {
        id,
        source,
        hash: sha256_hex(text.as_bytes()),
        installed: 0,
    })
}

/// SHA-256 (hex) of the `SKILL.md` currently served at `url` — the update
/// check's "has the author changed it upstream?" probe. Reuses the same
/// mirror-aware fetch as install, so it agrees with what an install would land.
pub async fn fetch_skill_hash(url: &str) -> Result<String> {
    let text = fetch_text(url).await?;
    Ok(sha256_hex(text.as_bytes()))
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

/// Bring a skill up to the version its source currently serves. Returns
/// `Ok(None)` when upstream content already matches `current_hash` (nothing to
/// do — stays a no-op, so "update" is idempotent); `Ok(Some(record))` after
/// writing the newer `SKILL.md` atomically and hashing it (the command layer
/// stamps `installed` and persists the record).
pub async fn update_skill(
    instance: &InstanceManifest,
    entry: &RegistryPlugin,
    current_hash: &str,
) -> Result<Option<SkillRecord>> {
    let (text, source) = fetch_skill_md(entry).await?;
    let hash = sha256_hex(text.as_bytes());
    if !current_hash.is_empty() && hash == current_hash {
        return Ok(None);
    }
    let id = skill_id(entry);
    let dir = skills_dir(instance).join(skill_dir_name(&id));
    write_atomic(&dir, "SKILL.md", text.as_bytes())?;
    Ok(Some(SkillRecord {
        id,
        source,
        hash,
        installed: 0,
    }))
}

/// Atomically land `bytes` as `dir/file_name` by writing a same-directory
/// `.tmp` sibling first, then renaming over the target (same-volume rename is
/// atomic). Mirrors the `.part`-then-rename discipline of
/// `launcher_core::download_file` for the buffered-text skill path.
fn write_atomic(dir: &Path, file_name: &str, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        "{file_name}.tmp-{}-{:x}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    let dest = dir.join(file_name);
    if dest.exists() {
        std::fs::remove_file(&dest).with_context(|| format!("replace {}", dest.display()))?;
    }
    std::fs::rename(&tmp, &dest).with_context(|| format!("finalize {}", dest.display()))?;
    Ok(())
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

/// Fetch a skill's SKILL.md plus the URL that should stand in for it going
/// forward (the provenance `source` stored on the record): the pre-resolved raw
/// URL when present, else the source repo. The raw URL is tried first; a
/// shallow clone of the repo with a `SKILL.md` search is the fallback.
async fn fetch_skill_md(entry: &RegistryPlugin) -> Result<(String, String)> {
    if let Some(fetch) = entry.fetch.as_deref() {
        if let Ok(text) = fetch_text(fetch).await {
            return Ok((text, fetch.to_string()));
        }
    }
    let source = entry.url.trim().to_string();
    let text = fetch_from_repo(entry).await?;
    Ok((text, source))
}

async fn fetch_text(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut urls = vec![url.to_string()];
    if url.starts_with("https://raw.githubusercontent.com/") {
        urls.push(format!("https://gh-proxy.com/{url}"));
    }
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

async fn fetch_from_repo(entry: &RegistryPlugin) -> Result<String> {
    let repo = entry
        .url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .ok_or_else(|| anyhow!("skill has no resolvable github repo"))?;
    let tmp = std::env::temp_dir().join(format!(
        "ahl-skill-{}-{:x}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    std::fs::create_dir_all(&tmp)?;
    let result = clone_and_read(repo, &tmp, &entry.name).await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn clone_and_read(repo: &str, tmp: &Path, name: &str) -> Result<String> {
    let url = format!("https://github.com/{repo}");
    let args = vec![
        "clone".to_string(),
        "--depth".to_string(),
        "1".to_string(),
        "--quiet".to_string(),
        "--".to_string(),
        url.clone(),
        tmp.display().to_string(),
    ];
    // Route through the shared timed runner so a stalled transfer is killed
    // after GIT_TIMEOUT (process tree included) instead of hanging forever.
    let code = crate::run_timed(
        "git",
        &args,
        tmp.parent().unwrap_or(tmp),
        &[],
        crate::silent_log_sink(),
        crate::GIT_TIMEOUT,
    )
    .await
    .map_err(|e| anyhow!("git clone {url} failed: {e}"))?;
    if code != 0 {
        return Err(anyhow!(
            "git clone {url} failed — check the repo exists and is public, and that your network can reach github.com"
        ));
    }
    let skill_md = find_skill_md(tmp, name).ok_or_else(|| anyhow!("no SKILL.md found in {url}"))?;
    std::fs::read_to_string(&skill_md).with_context(|| format!("read {}", skill_md.display()))
}

/// Locate a `SKILL.md` in a cloned repo, preferring a parent directory whose
/// name matches the skill's short name, then a path containing it, then any.
fn find_skill_md(root: &Path, name: &str) -> Option<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    collect_skill_md(root, &mut all);
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

fn collect_skill_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| matches!(n, ".git" | "node_modules"))
                .unwrap_or(false);
            if !skip {
                collect_skill_md(&path, out);
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("SKILL.md"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
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
}
