use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::AppPaths;

/// Which runtime an instance pins (id + detected version).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRef {
    pub id: String,
    pub version: String,
}

/// One installed MCP server — the launcher-side *source of truth* for the full
/// connection definition, which `cordis.patch.yml` is compiled from (see
/// `dsh_adapter::sync_mcp_patch`). `id` is the catalog key (`owner/name`);
/// `server_name` is the DSH-facing `serverName` that becomes the patch row id
/// `mcp-<serverName>`. A record whose `server_name` is empty is a *legacy*
/// bare id migrated from the old `mcp: Vec<String>` manifest — awaiting a
/// catalog backfill (from the merged registry, where the adapter runs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerRecord {
    /// Catalog identity (`owner/name`) — the manifest/library key.
    pub id: String,
    /// DSH-facing server name; the patch row is `mcp-<server_name>`.
    #[serde(default)]
    pub server_name: String,
    /// `"stdio"` or `"streamable-http"`.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// stdio launch: command (e.g. `npx`) …
    #[serde(default)]
    pub command: String,
    /// … its args …
    #[serde(default)]
    pub args: Vec<String>,
    /// … and env.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// streamable-http launch: endpoint URL (catalog `mcpUrl`).
    #[serde(default)]
    pub url: String,
    /// streamable-http auth headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// `false` = compiled out of the patch (DSH no longer loads it). No MCP
    /// `disabled:` toggle row exists — absent from the patch *is* disabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for McpServerRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            server_name: String::new(),
            transport: default_transport(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            url: String::new(),
            headers: HashMap::new(),
            enabled: true,
        }
    }
}

impl McpServerRecord {
    /// A legacy bare `owner/name` string from the pre-refactor manifest. The
    /// empty `server_name` is the marker that triggers the catalog backfill.
    fn from_legacy(id: String) -> Self {
        Self {
            id,
            enabled: true,
            ..Self::default()
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_transport() -> String {
    "stdio".to_string()
}

/// Element wrapper for the `mcp` array: legacy manifests store bare
/// `owner/name` strings; new ones store full connection objects. Untagged so
/// either parses, a string winning when both would fit.
#[derive(Deserialize)]
#[serde(untagged)]
enum McpElement {
    Id(String),
    Record(Box<McpServerRecord>),
}

impl From<McpElement> for McpServerRecord {
    fn from(el: McpElement) -> Self {
        match el {
            McpElement::Id(id) => McpServerRecord::from_legacy(id),
            McpElement::Record(rec) => *rec,
        }
    }
}

/// Deserialize `mcp` from an array of bare ids (legacy), connection records,
/// or a mix of both. Missing/null collapse to an empty list.
fn de_mcp_records<'de, D>(d: D) -> std::result::Result<Vec<McpServerRecord>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<McpServerRecord>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of MCP server ids or connection records")
        }
        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Vec<McpServerRecord>, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(el) = seq.next_element::<McpElement>()? {
                out.push(el.into());
            }
            Ok(out)
        }
        fn visit_none<E>(self) -> std::result::Result<Vec<McpServerRecord>, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E>(self) -> std::result::Result<Vec<McpServerRecord>, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_seq(V)
}

/// One installed skill — a plain `SKILL.md` under `workspace/skills/<dir>/` —
/// plus the provenance needed to detect an update. `hash` is the SHA-256 of the
/// installed `SKILL.md` bytes: DSH exposes no skill version, so content hash is
/// the only "has this changed upstream?" signal. An empty `source`/`hash` with
/// `installed: 0` marks a *legacy* bare id migrated from the old
/// `skills: Vec<String>` manifest.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    /// Canonical install id (`owner/name`) — the manifest/library key.
    pub id: String,
    /// Where this skill's `SKILL.md` came from — a raw `fetch` URL, or the
    /// source repo when install fell back to a shallow clone.
    #[serde(default)]
    pub source: String,
    /// SHA-256 (hex, lowercase) of the installed `SKILL.md` bytes.
    #[serde(default)]
    pub hash: String,
    /// Epoch ms the skill was (last) installed; `0` when unknown (legacy).
    #[serde(default)]
    pub installed: u64,
}

impl SkillRecord {
    /// A legacy bare `owner/name` string from the pre-refactor manifest. The
    /// empty `source`/`hash` + `installed: 0` is the marker for "awaiting an
    /// install to backfill provenance".
    fn from_legacy(id: String) -> Self {
        Self {
            id,
            ..Self::default()
        }
    }
}

/// Element wrapper for the `skills` array: legacy manifests store bare
/// `owner/name` strings; new ones store provenance records. Untagged so either
/// parses, a string winning when both would fit.
#[derive(Deserialize)]
#[serde(untagged)]
enum SkillElement {
    Id(String),
    Record(Box<SkillRecord>),
}

impl From<SkillElement> for SkillRecord {
    fn from(el: SkillElement) -> Self {
        match el {
            SkillElement::Id(id) => SkillRecord::from_legacy(id),
            SkillElement::Record(rec) => *rec,
        }
    }
}

/// Deserialize `skills` from an array of bare ids (legacy), provenance records,
/// or a mix of both. Missing/null collapse to an empty list.
fn de_skill_records<'de, D>(d: D) -> std::result::Result<Vec<SkillRecord>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<SkillRecord>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of skill ids or provenance records")
        }
        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Vec<SkillRecord>, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(el) = seq.next_element::<SkillElement>()? {
                out.push(el.into());
            }
            Ok(out)
        }
        fn visit_none<E>(self) -> std::result::Result<Vec<SkillRecord>, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E>(self) -> std::result::Result<Vec<SkillRecord>, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_seq(V)
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
    /// Installed skills as `{source, hash, installed}` records (the update
    /// signal). Legacy manifests hold bare `owner/name` ids here; those
    /// deserialize into [`SkillRecord`]s with empty `source`/`hash` and
    /// `installed: 0`, backfilled on the next install/update.
    #[serde(default, deserialize_with = "de_skill_records")]
    pub skills: Vec<SkillRecord>,
    /// Installed MCP servers — the launcher-side source of truth for each
    /// connection. Legacy manifests hold bare `owner/name` ids here; those
    /// deserialize into [`McpServerRecord`]s with an empty `server_name` and
    /// get backfilled from the merged catalog on next reconcile.
    #[serde(default, deserialize_with = "de_mcp_records")]
    pub mcp: Vec<McpServerRecord>,
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
        std::fs::write(path, text)
            .with_context(|| format!("write instance manifest {}", path.display()))?;
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

    /// Upsert an installed skill's provenance record, then persist. `record.id`
    /// is the dedup key: a re-install or update overwrites `{source, hash,
    /// installed}` rather than stacking. Skills are plain files under
    /// `workspace/skills/`, so the manifest is the only index of what's
    /// installed.
    pub fn add_skill(paths: &AppPaths, id: &str, record: &SkillRecord) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        if let Some(existing) = m.skills.iter_mut().find(|s| s.id == record.id) {
            *existing = record.clone();
        } else {
            m.skills.push(record.clone());
        }
        m.save(&paths.instance_file(id))?;
        Ok(m)
    }

    /// Remove an installed skill by its id, then persist.
    pub fn remove_skill(paths: &AppPaths, id: &str, skill: &str) -> Result<Self> {
        let mut m = Self::get(paths, id)?;
        m.skills.retain(|s| s.id != skill);
        m.save(&paths.instance_file(id))?;
        Ok(m)
    }

    /// Upsert an installed MCP server's full connection record, then persist.
    /// `record.id` is the dedup key: a re-install overwrites the connection
    /// definition (fresh from the catalog, enabled) rather than stacking
    /// duplicates. The caller regenerates `cordis.patch.yml` from the manifest
    /// afterwards (see `dsh_adapter::sync_mcp_patch`).
    pub fn add_mcp(paths: &AppPaths, instance_id: &str, record: &McpServerRecord) -> Result<Self> {
        let mut m = Self::get(paths, instance_id)?;
        if let Some(existing) = m.mcp.iter_mut().find(|r| r.id == record.id) {
            *existing = record.clone();
        } else {
            m.mcp.push(record.clone());
        }
        m.save(&paths.instance_file(instance_id))?;
        Ok(m)
    }

    /// Remove an installed MCP server by its record id, then persist.
    pub fn remove_mcp(paths: &AppPaths, instance_id: &str, record_id: &str) -> Result<Self> {
        let mut m = Self::get(paths, instance_id)?;
        m.mcp.retain(|r| r.id != record_id);
        m.save(&paths.instance_file(instance_id))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway AppPaths rooted in temp, cleaned via `std::fs::remove_dir_all`
    /// (never shell tools). Unique per tag + pid so parallel tests don't collide.
    fn tmp_paths(tag: &str) -> AppPaths {
        let root = std::env::temp_dir().join(format!("ahl-instance-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        AppPaths {
            root: root.clone(),
            portable: false,
            settings: root.join("settings.json"),
            providers: root.join("providers.json"),
            runtimes: root.join("runtimes"),
            instances: root.join("instances"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            launcher_log: root.join("logs").join("launcher.log"),
        }
    }

    /// Write a minimal instance.json whose `mcp` field is exactly `mcp_json`.
    /// Built through serde_json so Windows `\` path separators stay escaped.
    fn write_manifest(paths: &AppPaths, id: &str, mcp_json: &str) {
        let dir = paths.instances.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "id": id,
            "name": "Test",
            "runtime": { "id": "dsh", "version": "1.0" },
            "profile": "web",
            "providerRef": "default",
            "workspace": dir.join("workspace").display().to_string(),
            "mcp": serde_json::from_str::<serde_json::Value>(mcp_json).expect("valid mcp json"),
        });
        std::fs::write(
            paths.instance_file(id),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn record(id: &str) -> McpServerRecord {
        McpServerRecord {
            id: id.to_string(),
            ..Default::default()
        }
    }

    /// Like [`write_manifest`] but also sets the `skills` field verbatim.
    fn write_manifest_with_skills(paths: &AppPaths, id: &str, skills_json: &str) {
        let dir = paths.instances.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "id": id,
            "name": "Test",
            "runtime": { "id": "dsh", "version": "1.0" },
            "profile": "web",
            "providerRef": "default",
            "workspace": dir.join("workspace").display().to_string(),
            "mcp": [],
            "skills": serde_json::from_str::<serde_json::Value>(skills_json).expect("valid skills json"),
        });
        std::fs::write(
            paths.instance_file(id),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn skill(id: &str) -> SkillRecord {
        SkillRecord {
            id: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn mcp_legacy_string_array_deserializes_to_records() {
        let paths = tmp_paths("legacy-mcp");
        write_manifest(&paths, "default", r#"["a/github","b/other"]"#);
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.mcp.len(), 2);
        assert_eq!(m.mcp[0].id, "a/github");
        assert_eq!(m.mcp[0].server_name, "", "legacy id has no server name");
        assert!(m.mcp[0].enabled, "legacy installs default to enabled");
        assert_eq!(m.mcp[0].transport, "stdio");
        assert_eq!(m.mcp[1].id, "b/other");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn mcp_object_and_mixed_arrays_parse() {
        let paths = tmp_paths("record-mcp");
        write_manifest(
            &paths,
            "default",
            r#"[
                {"id":"http/x","serverName":"x","transport":"streamable-http","url":"http://localhost:8080/sse","headers":{"Authorization":"Bearer t"},"enabled":false},
                {"id":"stdio/y","serverName":"y","command":"npx","args":["-y","pkg"]},
                "legacy/only"
            ]"#,
        );
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.mcp.len(), 3);

        let http = &m.mcp[0];
        assert_eq!(http.transport, "streamable-http");
        assert_eq!(http.url, "http://localhost:8080/sse");
        assert_eq!(http.headers["Authorization"], "Bearer t");
        assert!(!http.enabled);

        let stdio = &m.mcp[1];
        assert_eq!(stdio.command, "npx");
        assert_eq!(stdio.args, vec!["-y", "pkg"]);
        assert!(stdio.enabled, "missing enabled defaults to true");
        assert_eq!(stdio.transport, "stdio", "missing transport defaults to stdio");

        assert_eq!(m.mcp[2].id, "legacy/only");
        assert!(m.mcp[2].server_name.is_empty());
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn mcp_add_is_upsert_and_remove_drops_by_id() {
        let paths = tmp_paths("mcp-mut");
        write_manifest(&paths, "default", "[]");

        InstanceManifest::add_mcp(&paths, "default", &record("owner/srv-a")).unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.mcp.len(), 1);
        assert_eq!(m.mcp[0].id, "owner/srv-a");

        // Re-adding the same id overwrites the connection definition — no dup.
        let mut updated = record("owner/srv-a");
        updated.command = "npx".into();
        InstanceManifest::add_mcp(&paths, "default", &updated).unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.mcp.len(), 1, "re-add must upsert, not stack");
        assert_eq!(m.mcp[0].command, "npx");

        InstanceManifest::add_mcp(&paths, "default", &record("owner/srv-b")).unwrap();
        InstanceManifest::remove_mcp(&paths, "default", "owner/srv-a").unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.mcp.len(), 1);
        assert_eq!(m.mcp[0].id, "owner/srv-b");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn mcp_serializes_as_records_in_camel_case() {
        let paths = tmp_paths("serialize-mcp");
        write_manifest(&paths, "default", r#"["owner/srv-a"]"#);
        let m = InstanceManifest::get(&paths, "default").unwrap();
        let v = serde_json::to_value(&m).unwrap();
        let arr = v["mcp"].as_array().expect("mcp is an array");
        assert!(arr[0].is_object(), "manifest must persist records, not bare ids");
        assert_eq!(arr[0]["serverName"], "", "legacy-derived record round-trips");
        assert_eq!(arr[0]["enabled"], true);
        assert_eq!(arr[0]["transport"], "stdio");
        assert_eq!(arr[0]["id"], "owner/srv-a");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn skills_legacy_string_array_deserializes_to_records() {
        let paths = tmp_paths("legacy-skills");
        write_manifest_with_skills(&paths, "default", r#"["anthropics/docx","acme/plain"]"#);
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.skills.len(), 2);
        assert_eq!(m.skills[0].id, "anthropics/docx");
        assert!(m.skills[0].source.is_empty(), "legacy id has no source");
        assert!(m.skills[0].hash.is_empty(), "legacy id has no hash");
        assert_eq!(m.skills[0].installed, 0, "legacy id has no install time");
        assert_eq!(m.skills[1].id, "acme/plain");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn skills_object_and_mixed_arrays_parse() {
        let paths = tmp_paths("record-skills");
        write_manifest_with_skills(
            &paths,
            "default",
            r#"[
                {"id":"a/docx","source":"https://raw.githubusercontent.com/a/skills/HEAD/skills/docx/SKILL.md","hash":"abc123","installed":1700000000000},
                {"id":"b/plain","source":"https://github.com/b/skills"},
                "legacy/only"
            ]"#,
        );
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.skills.len(), 3);

        let full = &m.skills[0];
        assert_eq!(full.source, "https://raw.githubusercontent.com/a/skills/HEAD/skills/docx/SKILL.md");
        assert_eq!(full.hash, "abc123");
        assert_eq!(full.installed, 1700000000000);

        let partial = &m.skills[1];
        assert_eq!(partial.source, "https://github.com/b/skills");
        assert!(partial.hash.is_empty(), "missing hash defaults empty");
        assert_eq!(partial.installed, 0);

        assert_eq!(m.skills[2].id, "legacy/only");
        assert!(m.skills[2].source.is_empty());
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn skills_add_is_upsert_and_remove_drops_by_id() {
        let paths = tmp_paths("skills-mut");
        write_manifest_with_skills(&paths, "default", "[]");

        let mut rec = skill("owner/docx");
        rec.source = "https://example.com/one".into();
        rec.hash = "aaaa".into();
        rec.installed = 1000;
        InstanceManifest::add_skill(&paths, "default", &rec).unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.skills.len(), 1);
        assert_eq!(m.skills[0].id, "owner/docx");

        // Re-adding the same id overwrites provenance (an update) — no dup.
        let mut updated = skill("owner/docx");
        updated.hash = "bbbb".into();
        updated.installed = 2000;
        InstanceManifest::add_skill(&paths, "default", &updated).unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.skills.len(), 1, "re-add must upsert, not stack");
        assert_eq!(m.skills[0].hash, "bbbb");
        assert_eq!(m.skills[0].source, "", "overwrite replaces the whole record");

        InstanceManifest::add_skill(&paths, "default", &skill("owner/other")).unwrap();
        InstanceManifest::remove_skill(&paths, "default", "owner/docx").unwrap();
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert_eq!(m.skills.len(), 1);
        assert_eq!(m.skills[0].id, "owner/other");
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn skills_serialize_as_records_in_camel_case() {
        let paths = tmp_paths("serialize-skills");
        write_manifest_with_skills(&paths, "default", r#"["owner/docx"]"#);
        let m = InstanceManifest::get(&paths, "default").unwrap();
        let v = serde_json::to_value(&m).unwrap();
        let arr = v["skills"].as_array().expect("skills is an array");
        assert!(arr[0].is_object(), "manifest must persist records, not bare ids");
        assert_eq!(arr[0]["id"], "owner/docx");
        assert_eq!(arr[0]["source"], "");
        assert_eq!(arr[0]["hash"], "");
        assert_eq!(arr[0]["installed"], 0);
        let _ = std::fs::remove_dir_all(&paths.root);
    }

    #[test]
    fn skills_missing_field_defaults_to_empty() {
        let paths = tmp_paths("skills-absent");
        write_manifest(&paths, "default", "[]"); // no skills key at all
        let m = InstanceManifest::get(&paths, "default").unwrap();
        assert!(m.skills.is_empty(), "absent skills field must deserialize to []");
        let _ = std::fs::remove_dir_all(&paths.root);
    }
}
