//! Content installers for the Market's non-plugin kinds.
//!
//! Skills are plain files under `$DSH_HOME/skills/` — a `SKILL.md` (or a
//! directory-bundle `<name>/SKILL.md`) discovered by DSH's `skill-filesystem`
//! plugin. There is no npm install and no enable toggle; the launcher just
//! downloads a `SKILL.md` and drops it in place, and the manifest
//! (`InstanceManifest.skills`) is the only index of what's installed.
//!
//! Install tries a pre-resolved raw `fetch` URL first (fast path), then falls
//! back to a shallow `git clone` of the source repo + a `SKILL.md` search —
//! the awesome-* markdown gives no reliable path, so the clone is what makes
//! install work across the many repo layouts.
//!
//! (MCP installs append a `mcp-client` insert row to `cordis.patch.yml` and are
//! tracked in `InstanceManifest.mcp` — see [`install_mcp`] / [`uninstall_mcp`].)

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use launcher_core::{InstanceManifest, RegistryPlugin};
use serde_yaml::Value as Yaml;

use crate::{append_patch_entry, remove_insert_block, DshAdapter};

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

/// Installed skills, straight from the manifest (the only index — skills are
/// plain files, not npm packages).
pub fn installed_skills(instance: &InstanceManifest) -> Vec<String> {
    instance.skills.clone()
}

/// Download a skill's `SKILL.md` and write it to
/// `$DSH_HOME/skills/<id>/SKILL.md`.
pub async fn install_skill(instance: &InstanceManifest, entry: &RegistryPlugin) -> Result<()> {
    let text = fetch_skill_md(entry).await?;
    let id = skill_id(entry);
    let dir = skills_dir(instance).join(skill_dir_name(&id));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
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

/// Fetch a skill's SKILL.md: the pre-resolved raw URL first, then a shallow
/// clone of the source repo with a `SKILL.md` search.
async fn fetch_skill_md(entry: &RegistryPlugin) -> Result<String> {
    if let Some(fetch) = entry.fetch.as_deref() {
        if let Ok(text) = fetch_text(fetch).await {
            return Ok(text);
        }
    }
    fetch_from_repo(entry).await
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
        .map_err(|e| anyhow!("git clone failed (is git installed?): {e}"))?;
    if !status.success() {
        return Err(anyhow!("git clone {url} failed"));
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

/// The cordis row id for an MCP entry (`mcp-<serverName>`), the id the insert
/// block registers and uninstall targets.
fn mcp_row_id(entry: &RegistryPlugin) -> String {
    format!("mcp-{}", mcp_server_name(entry))
}

/// Installed MCP servers, straight from the manifest (the only index — MCP is a
/// cordis patch row, not an npm package).
pub fn installed_mcp(instance: &InstanceManifest) -> Vec<String> {
    instance.mcp.clone()
}

/// The mcp-client plugin's config as a `serde_yaml::Value`: `stdio` carries
/// `command`/`args`/`env`; `streamable-http` carries `url`/`headers`. Both carry
/// `serverName` and `transport`.
fn mcp_config_yaml(entry: &RegistryPlugin) -> Result<Yaml> {
    let transport = entry.transport.clone().unwrap_or_else(|| "stdio".into());
    let mut config = serde_yaml::Mapping::new();
    config.insert("serverName".into(), Yaml::String(mcp_server_name(entry)));
    config.insert("transport".into(), Yaml::String(transport.clone()));
    if transport == "streamable-http" {
        config.insert(
            "url".into(),
            Yaml::String(entry.mcp_url.clone().unwrap_or_default()),
        );
        config.insert(
            "headers".into(),
            serde_yaml::to_value(entry.headers.clone().unwrap_or_default())?,
        );
    } else {
        config.insert(
            "command".into(),
            Yaml::String(entry.command.clone().unwrap_or_default()),
        );
        config.insert(
            "args".into(),
            serde_yaml::to_value(entry.args.clone().unwrap_or_default())?,
        );
        config.insert(
            "env".into(),
            serde_yaml::to_value(entry.env.clone().unwrap_or_default())?,
        );
    }
    Ok(Yaml::Mapping(config))
}

/// Serialize one MCP server as a top-level `- insert:` patch block ready for
/// [`append_patch_entry`]:
/// ```yaml
/// - insert:
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
fn mcp_insert_block(entry: &RegistryPlugin) -> Result<String> {
    let row_id = mcp_row_id(entry);
    let config_yaml = serde_yaml::to_string(&mcp_config_yaml(entry)?)?;
    let mut block = String::new();
    block.push_str("- insert:\n");
    block.push_str(&format!("    - id: {row_id}\n"));
    block.push_str("      name: '@deepseek-ai/dsh-mcp-client'\n");
    block.push_str("      config:\n");
    for line in config_yaml.lines() {
        block.push_str("        ");
        block.push_str(line);
        block.push('\n');
    }
    Ok(block)
}

/// Install an MCP server: append its `mcp-client` insert row to the instance's
/// profile `cordis.patch.yml`. The manifest index is updated by the caller
/// (like skills); this only writes the patch row.
pub fn install_mcp(instance: &InstanceManifest, entry: &RegistryPlugin) -> Result<()> {
    let patch_path = DshAdapter::profile_dir(instance).join("cordis.patch.yml");
    let block = mcp_insert_block(entry)?;
    append_patch_entry(&patch_path, &block)?;
    Ok(())
}

/// Uninstall an MCP server: remove the `- insert:` block that holds its row id.
pub fn uninstall_mcp(instance: &InstanceManifest, entry: &RegistryPlugin) -> Result<()> {
    let patch_path = DshAdapter::profile_dir(instance).join("cordis.patch.yml");
    let row_id = mcp_row_id(entry);
    remove_insert_block(&patch_path, &row_id)?;
    Ok(())
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

    #[test]
    fn mcp_insert_block_builds_stdio_row() {
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
        let block = mcp_insert_block(&entry).unwrap();
        assert!(block.starts_with("- insert:\n    - id: mcp-github\n"));
        assert!(block.contains("      name: '@deepseek-ai/dsh-mcp-client'\n"));
        assert!(block.contains("        serverName: github\n"));
        assert!(block.contains("        transport: stdio\n"));
        assert!(block.contains("        command: npx\n"));
        assert!(block.contains("        - -y\n"));
        assert!(block.contains("        env: {}\n"));
    }

    #[test]
    fn mcp_insert_block_builds_http_row() {
        let entry = RegistryPlugin {
            name: "web".into(),
            owner: "o".into(),
            server_name: Some("web".into()),
            transport: Some("streamable-http".into()),
            mcp_url: Some("http://localhost:3000/mcp".into()),
            headers: Some(Default::default()),
            ..Default::default()
        };
        let block = mcp_insert_block(&entry).unwrap();
        assert!(block.contains("        transport: streamable-http\n"));
        assert!(block.contains("        url: http://localhost:3000/mcp\n"));
        assert!(!block.contains("command:"));
    }

    #[test]
    fn remove_insert_block_drops_matching_block_only() {
        let dir = std::env::temp_dir().join(format!("ahl-mcp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cordis.patch.yml");
        std::fs::write(
            &path,
            "- insert:\n    - id: mcp-a\n      name: '@deepseek-ai/dsh-mcp-client'\n- insert:\n    - id: mcp-b\n      name: '@deepseek-ai/dsh-mcp-client'\n- id: timer\n  disabled: true\n",
        )
        .unwrap();

        remove_insert_block(&path, "mcp-a").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("mcp-a"));
        assert!(text.contains("mcp-b"));
        assert!(text.contains("- id: timer"));

        let _ = std::fs::remove_dir_all(&dir);
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
}
