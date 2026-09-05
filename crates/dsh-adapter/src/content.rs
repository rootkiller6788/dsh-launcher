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

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use launcher_core::{
    sha256_hex, InstanceManifest, McpServerRecord, RegistryPlugin, SkillRecord,
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
    let status = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", "--"])
        .arg(&url)
        .arg(tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|e| anyhow!(
            "git clone failed — is git installed and on PATH? run `git --version` to check: {e}"
        ))?;
    if !status.success() {
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

fn sanitize_server_name(name: &str) -> String {
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
        enabled: true,
    }
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
        .any(|v| {
            v.contains("${")
                || {
                    let upper = v.to_ascii_uppercase();
                    upper.contains("TOKEN")
                        || upper.contains("API_KEY")
                        || upper.contains("SECRET")
                        || upper.contains("BEARER")
                }
        })
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
