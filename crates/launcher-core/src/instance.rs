use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::AppPaths;

/// Which runtime an instance pins (id + detected version).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRef {
    pub id: String,
    pub version: String,
}

/// The single source of truth for one AI instance — a portable JSON manifest
/// on disk (`instances/<id>/instance.json`). The DB is only ever an index on
/// top of this file (Phase 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceManifest {
    pub id: String,
    pub name: String,
    pub runtime: RuntimeRef,
    /// Launcher surface profile the harness boots with (e.g. `web`).
    pub profile: String,
    /// Reference into the provider vault, never an inline key.
    pub provider_ref: String,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
    #[serde(default)]
    pub skins: Vec<String>,
    /// The instance's isolated `$DSH_HOME` (profiles/config live here).
    pub workspace: String,
}

impl InstanceManifest {
    pub fn new(id: String, name: String, instances_root: &Path) -> Self {
        Self {
            id: id.clone(),
            name,
            runtime: RuntimeRef {
                id: "dsh".into(),
                version: String::new(),
            },
            profile: "web".into(),
            provider_ref: "default".into(),
            plugins: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            skins: Vec::new(),
            workspace: instances_root
                .join(&id)
                .join("workspace")
                .display()
                .to_string(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read instance manifest {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("write instance manifest {}", path.display()))?;
        Ok(())
    }

    /// Load the default instance, materializing it (and its workspace) if the
    /// file does not exist yet. Kept for first-boot; multi-instance flows use
    /// [`list`](Self::list) / [`create`](Self::create).
    pub fn load_or_create(paths: &AppPaths) -> Result<Self> {
        let file = paths.default_instance_file();
        if file.exists() {
            if let Ok(manifest) = Self::load(&file) {
                return Ok(manifest);
            }
        }
        let manifest = Self::new("default".into(), "Default".into(), &paths.instances);
        let workspace = PathBuf::from(&manifest.workspace);
        std::fs::create_dir_all(&workspace)?;
        manifest.save(&file)?;
        Ok(manifest)
    }

    /// All instances, read from the filesystem (instance.json is the source of
    /// truth), sorted by name.
    pub fn list(paths: &AppPaths) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        if !paths.instances.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&paths.instances)? {
            let entry = entry?;
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest = dir.join("instance.json");
            if manifest.exists() {
                if let Ok(m) = Self::load(&manifest) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Look up one instance by id.
    pub fn get(paths: &AppPaths, id: &str) -> Result<Self> {
        let file = paths.instance_file(id);
        if !file.exists() {
            return Err(anyhow!("instance '{id}' not found"));
        }
        Self::load(&file)
    }

    /// Append an installed skill id (canonical `owner/name`) if not present,
    /// then persist. Skills are plain files under `workspace/skills/`, so the
    /// manifest is the only index of what's installed.
    pub fn add_skill(paths: &AppPaths, id: &str, skill: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        if !m.skills.iter().any(|s| s == skill) {
            m.skills.push(skill.to_string());
            m.save(&paths.instance_file(id))?;
        }
        Ok(m)
    }

    /// Remove an installed skill id, then persist.
    pub fn remove_skill(paths: &AppPaths, id: &str, skill: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        m.skills.retain(|s| s != skill);
        m.save(&paths.instance_file(id))?;
        Ok(m)
    }

    /// Append an installed MCP server id if not present, then persist.
    pub fn add_mcp(paths: &AppPaths, id: &str, server: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        if !m.mcp.iter().any(|s| s == server) {
            m.mcp.push(server.to_string());
            m.save(&paths.instance_file(id))?;
        }
        Ok(m)
    }

    /// Remove an installed MCP server id, then persist.
    pub fn remove_mcp(paths: &AppPaths, id: &str, server: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        m.mcp.retain(|s| s != server);
        m.save(&paths.instance_file(id))?;
        Ok(m)
    }

    /// Append an installed skin id (canonical `owner/name`) if not present.
    /// Skins install through DSH's plugin mechanism, but Library presents them
    /// as visual assets owned by the Market skin catalog.
    pub fn add_skin(paths: &AppPaths, id: &str, skin: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        if !m.skins.iter().any(|s| s == skin) {
            m.skins.push(skin.to_string());
            m.save(&paths.instance_file(id))?;
        }
        Ok(m)
    }

    /// Remove an installed skin id, then persist.
    pub fn remove_skin(paths: &AppPaths, id: &str, skin: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        m.skins.retain(|s| s != skin);
        m.save(&paths.instance_file(id))?;
        Ok(m)
    }

    /// Create a fresh instance: slug id derived from the name, isolated
    /// workspace dir, empty plugin/skill/mcp lists, persisted to disk.
    pub fn create(paths: &AppPaths, name: &str) -> Result<Self> {
        let id = unique_id(&slugify(name), &existing_ids(paths)?);
        let manifest = Self::new(id.clone(), name.trim().to_string(), &paths.instances);
        std::fs::create_dir_all(&manifest.workspace)?;
        manifest.save(&paths.instance_file(&id))?;
        Ok(manifest)
    }

    /// Rename an instance. The id — and therefore its workspace path — never
    /// changes; the id is the stable identity an instance is keyed by.
    pub fn rename(paths: &AppPaths, id: &str, new_name: &str) -> Result<Self> {
        let mut manifest = Self::get(paths, id)?;
        manifest.name = new_name.trim().to_string();
        if manifest.name.is_empty() {
            return Err(anyhow!("name must not be empty"));
        }
        manifest.save(&paths.instance_file(id))?;
        Ok(manifest)
    }

    /// Clone an instance: new id (from the new name) + deep-copy of the source
    /// workspace, everything else copied.
    pub fn clone(paths: &AppPaths, id: &str, new_name: &str) -> Result<Self> {
        let src = Self::get(paths, id)?;
        let new_id = unique_id(&slugify(new_name), &existing_ids(paths)?);
        let mut copy = src.clone();
        copy.id = new_id.clone();
        copy.name = new_name.trim().to_string();
        if copy.name.is_empty() {
            return Err(anyhow!("name must not be empty"));
        }
        copy.workspace = paths
            .instances
            .join(&new_id)
            .join("workspace")
            .display()
            .to_string();
        copy_dir_recursive(Path::new(&src.workspace), Path::new(&copy.workspace))?;
        copy.save(&paths.instance_file(&new_id))?;
        Ok(copy)
    }

    /// Delete an instance's whole directory. Refuses to remove the last
    /// remaining instance (the launcher always needs at least one).
    pub fn delete(paths: &AppPaths, id: &str) -> Result<()> {
        let count = Self::list(paths)?.len();
        if count <= 1 {
            return Err(anyhow!("cannot delete the last instance"));
        }
        let dir = paths.instance_dir(id);
        if !dir.exists() {
            return Err(anyhow!("instance '{id}' not found"));
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        Ok(())
    }
}

/// Lowercase name, spaces → `-`, drop non-alphanumerics; fallback `instance`.
fn slugify(name: &str) -> String {
    let base: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let base = base.trim_matches('-').to_string();
    if base.is_empty() {
        "instance".into()
    } else {
        base
    }
}

fn existing_ids(paths: &AppPaths) -> Result<Vec<String>> {
    Ok(InstanceManifest::list(paths)?
        .into_iter()
        .map(|m| m.id)
        .collect())
}

/// Append `-2`, `-3`, … until the id is not taken.
fn unique_id(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|e| e.as_str() == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let cand = format!("{base}-{n}");
        if !existing.iter().any(|e| e.as_str() == cand) {
            return cand;
        }
        n += 1;
    }
}

/// Recursive directory copy used by clone. Ignores symlinks/junctions.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to).with_context(|| format!("copy {}", from.display()))?;
        }
    }
    Ok(())
}
