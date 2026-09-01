//! Package discovery + smart search.
//!
//! The launcher's native re-implementation of the two Market projects'
//! thin discovery logic: `dsh-market` (the curated registry catalog) and
//! `smart-plugin-market` (local prefilter → LLM re-rank → name validation).
//! Both are DSH *plugins*, not importable SDKs, so we reproduce their small,
//! dependency-free algorithms here and keep the safety invariant they share:
//! **a recommended plugin name is always one that exists in the registry.**
//!
//! The LLM call goes to the provider the user already configured (key lives in
//! the OS credential vault), so nothing here needs its own key.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{write_json_atomic, AppPaths, ResolvedProvider};

/// Catalog sources, mirroring dsh-market's region routing (src/regions.ts):
/// the official URL lives on GitHub Pages, which is unreliable from mainland
/// China, so the catalog is *also* published as the npm package
/// `dsh-plugin-catalog` and read through an npm mirror there.
const CATALOG_OFFICIAL: &str = "https://awesome-dsh-plugin.com/plugins.json";
const CATALOG_PACKAGE: &str = "dsh-plugin-catalog";
const NPM_GLOBAL: &str = "https://registry.npmjs.org";
const NPM_CHINA: &str = "https://mirrors.cloud.tencent.com/npm";
/// Per-request timeout; the tarball is a few hundred KB.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);

/// The LLM system prompt for recommendation (faithful to smart-plugin-market).
const SYSTEM_PROMPT: &str = "You are the bundle recommendation assistant for DeepSeek Harness.\n\
Given a user need, compose 3 bundle plans drawn ONLY from the candidate list, mixing plugins, skins, skills and MCP servers as the need warrants.\n\
Rules:\n\
- Output ONLY a JSON object, no prose, no markdown fences.\n\
- Each plan: id (\"A\"|\"B\"|\"C\"), title, rationale, and an items array of 2-6 entries.\n\
- Each item: \"name\" must be EXACTLY one candidate name (form \"owner/repo\"), \"kind\" must be EXACTLY one of plugin|theme|skill|mcp, plus a one-line \"reason\".\n\
- The three plans should trade off: minimal vs comprehensive vs focused on one aspect.\n\
- Never cite a name or kind outside the candidate list.";

/// What kind of content a market entry is. The catalog carries plugins plus
/// themes (skins — which are themselves DSH plugins), skills, and MCP servers;
/// `Bundle` is reserved for curated composition packages (import-only today).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    #[default]
    Plugin,
    Theme,
    Skill,
    Mcp,
    Bundle,
}

impl ContentKind {
    /// Lowercase wire form, matching `#[serde(rename_all = "lowercase")]`; used
    /// in recommendation candidate ids (`kind:owner/name`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentKind::Plugin => "plugin",
            ContentKind::Theme => "theme",
            ContentKind::Skill => "skill",
            ContentKind::Mcp => "mcp",
            ContentKind::Bundle => "bundle",
        }
    }
}

/// One content item within a bundle (a curated catalog bundle or an
/// LLM-composed recommendation plan). `name` is the `owner/name` key of the
/// referenced entry, resolved against the merged catalog at install time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub name: String,
    pub kind: ContentKind,
    #[serde(default)]
    pub reason: String,
}

/// One curated plugin entry. `spec` is a computed field (not part of the
/// registry JSON): the ready-to-install pnpm target the launcher hands to
/// `dsh plugin add`, derived npm → tarball → `github:owner/repo`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RegistryPlugin {
    /// Content type discriminator. Existing plugin catalogs omit it and
    /// default to [`ContentKind::Plugin`].
    #[serde(default)]
    pub kind: ContentKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub url: String,
    /// `string` or `string[]` in the wild; normalized to a list.
    #[serde(default, deserialize_with = "de_string_or_vec")]
    pub category: Vec<String>,
    #[serde(default, deserialize_with = "de_string_map")]
    pub description: HashMap<String, String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub tarball: Option<String>,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(default)]
    pub stars: Option<f64>,
    #[serde(default)]
    pub downloads: Option<f64>,
    #[serde(default)]
    pub install: String,
    #[serde(default)]
    pub added: String,
    #[serde(default)]
    pub deprecated: Option<bool>,
    #[serde(default)]
    pub replacement: Option<String>,
    // --- theme (skin) specific ---
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub preview_css: Option<String>,
    /// Monorepo subdirectory the skin lives in (install still targets repo root).
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub gist: Option<String>,
    // --- skill specific ---
    /// Direct URL to the SKILL.md (raw.githubusercontent…), pre-resolved at
    /// catalog-generation time so install is a plain download.
    #[serde(default)]
    pub fetch: Option<String>,
    #[serde(default)]
    pub skill_name: Option<String>,
    // --- mcp specific ---
    #[serde(default)]
    pub server_name: Option<String>,
    /// `"stdio"` or `"streamable-http"`.
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub mcp_url: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    // --- bundle specific ---
    /// Curated bundle's item references (kind + owner/name + reason), resolved
    /// against the merged catalog and installed as a group.
    #[serde(default)]
    pub items: Option<Vec<PlanItem>>,
    /// Computed install target (npm | tarball | `github:owner/repo`).
    #[serde(default)]
    pub spec: String,
}

impl RegistryPlugin {
    /// Stable identity used in the recommendation candidate list and as the
    /// frontend↔backend match key: `owner/name`, else `name`.
    pub fn key(&self) -> String {
        let owner = self.owner.trim();
        if owner.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", owner, self.name)
        }
    }

    /// Kind-qualified identity for cross-kind recommendation (`kind:owner/name`),
    /// unambiguous when the merged catalog carries plugins + themes + skills + MCP.
    pub fn kind_key(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.key())
    }

    /// Install target derivation (npm → tarball → `github:owner/repo`).
    pub fn install_spec(&self) -> String {
        if let Some(npm) = self.npm.as_deref().filter(|s| !s.trim().is_empty()) {
            return npm.to_string();
        }
        if let Some(tb) = self.tarball.as_deref().filter(|s| !s.trim().is_empty()) {
            return tb.to_string();
        }
        self.github_spec().unwrap_or_default()
    }

    fn github_spec(&self) -> Option<String> {
        let rest = self
            .url
            .trim()
            .strip_prefix("https://github.com/")
            .or_else(|| self.url.trim().strip_prefix("http://github.com/"))?;
        let mut path = rest;
        for sep in ["/tree/", "/blob/", "#"] {
            if let Some(idx) = path.find(sep) {
                path = &path[..idx];
            }
        }
        let path = path.trim_end_matches('/').trim_end_matches(".git");
        if path.is_empty() {
            None
        } else {
            Some(format!("github:{path}"))
        }
    }
}

/// The curated catalog, plus a `spec` per plugin computed by [`hydrate`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub categories: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub plugins: Vec<RegistryPlugin>,
}

/// Fill each plugin's computed `spec` after deserialization (fetch or cache).
pub fn hydrate(mut reg: Registry) -> Registry {
    for p in &mut reg.plugins {
        p.spec = p.install_spec();
    }
    reg
}

/// The bundled theme/skin catalog, compiled into the binary via `include_str!`
/// so the skin tab works offline (no hosted endpoint exists for these, unlike
/// the plugin catalog).
const THEMES_CATALOG: &str = include_str!("../data/content-themes.json");
/// The bundled skill catalog (offline snapshot of awesome-agent-skills).
const SKILLS_CATALOG: &str = include_str!("../data/content-skills.json");
/// The bundled MCP server catalog (offline snapshot of awesome-mcp-servers,
/// enriched with transport/command/args/env from the hand-maintained
/// `scripts/data/mcp-overrides.json`).
const MCPS_CATALOG: &str = include_str!("../data/content-mcps.json");
/// The bundled bundle catalog (offline snapshot of awesome-agent-bundles).
const BUNDLES_CATALOG: &str = include_str!("../data/content-bundles.json");

/// Load the bundled theme catalog, hydrated (install spec computed).
pub fn bundled_themes() -> Registry {
    serde_json::from_str::<Registry>(THEMES_CATALOG)
        .map(hydrate)
        .unwrap_or_default()
}

/// Load the bundled skill catalog. Skills have no install spec (they download a
/// SKILL.md via `fetch`), so hydration is a no-op but keeps the shape uniform.
pub fn bundled_skills() -> Registry {
    serde_json::from_str::<Registry>(SKILLS_CATALOG)
        .map(hydrate)
        .unwrap_or_default()
}

/// Load the bundled MCP catalog. MCP servers carry their own launch config
/// (`serverName`/`transport`/`command`/…), so hydration is a no-op here too.
pub fn bundled_mcps() -> Registry {
    serde_json::from_str::<Registry>(MCPS_CATALOG)
        .map(hydrate)
        .unwrap_or_default()
}

/// Load the bundled bundle catalog. Bundles are composites with no install spec
/// of their own (they expand into their items), so hydration is a no-op.
pub fn bundled_bundles() -> Registry {
    serde_json::from_str::<Registry>(BUNDLES_CATALOG)
        .map(hydrate)
        .unwrap_or_default()
}

/// The merged bundled content catalogs (themes + skills + MCP + bundles) — the
/// offline fallback when the hosted content endpoint is unreachable.
pub fn bundled_content() -> Registry {
    let mut reg = Registry::default();
    reg.plugins.extend(bundled_themes().plugins);
    reg.plugins.extend(bundled_skills().plugins);
    reg.plugins.extend(bundled_mcps().plugins);
    reg.plugins.extend(bundled_bundles().plugins);
    reg.categories.extend(bundled_themes().categories);
    reg.categories.extend(bundled_skills().categories);
    reg.categories.extend(bundled_mcps().categories);
    reg.categories.extend(bundled_bundles().categories);
    reg.count = reg.plugins.len();
    reg
}

/// Append a content registry (themes/skills/MCP) to a plugin registry, merging
/// the kind categories and recounting. Applied at the `market_registry`
/// response boundary so the smart-search candidate set stays plugin-only.
pub fn extend_with_content(mut reg: Registry, content: Registry) -> Registry {
    reg.plugins.extend(content.plugins);
    reg.categories.extend(content.categories);
    reg.count = reg.plugins.len();
    reg
}

/// Append the bundled catalogs to a fetched plugin registry — the offline path,
/// equivalent to [`extend_with_content`] fed by [`bundled_content`].
pub fn extend_with_bundled(reg: Registry) -> Registry {
    extend_with_content(reg, bundled_content())
}

/// One LLM-composed bundle combination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendPlan {
    pub id: String,
    pub title: String,
    pub rationale: String,
    pub items: Vec<PlanItem>,
}

/// The smart-search result: plans (already validated ⊆ registry) + the raw
/// candidate names and raw model text for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendResult {
    pub plans: Vec<RecommendPlan>,
    pub candidates: Vec<String>,
    pub raw: String,
}

fn cache_file(paths: &AppPaths) -> std::path::PathBuf {
    paths.cache.join("registry.json")
}

fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

fn load_cached_registry(paths: &AppPaths) -> Option<Registry> {
    let text = std::fs::read_to_string(cache_file(paths)).ok()?;
    let reg: Registry = serde_json::from_str(&text).ok()?;
    Some(hydrate(reg))
}

fn cache_registry(paths: &AppPaths, reg: &Registry) {
    if let Ok(value) = serde_json::to_value(reg) {
        let _ = write_json_atomic(&cache_file(paths), &value);
    }
}

enum CatalogSource {
    Url(String),
    Npm(String),
}

/// Fetch the catalog, trying each source in order — China npm mirror first,
/// then the official URL, then the global npm registry — caching on success and
/// falling back to the last cached copy when every source fails.
pub async fn fetch_registry(paths: &AppPaths) -> Result<Registry> {
    let client = reqwest::Client::builder()
        .timeout(CATALOG_TIMEOUT)
        .build()?;

    // A named catalog REPLACES the chain: someone pointing at their own registry
    // does not want it quietly reverting to ours when theirs is briefly down.
    if let Some(url) = env_override("AHL_REGISTRY_URL") {
        if let Ok(reg) = fetch_url_catalog(&client, &url).await {
            cache_registry(paths, &reg);
            return Ok(reg);
        }
        return load_cached_registry(paths)
            .ok_or_else(|| anyhow!("registry unreachable and no cached copy exists"));
    }

    let mirror = env_override("AHL_NPM_MIRROR").unwrap_or_else(|| NPM_CHINA.to_string());
    let sources = [
        CatalogSource::Npm(mirror),
        CatalogSource::Url(CATALOG_OFFICIAL.to_string()),
        CatalogSource::Npm(NPM_GLOBAL.to_string()),
    ];

    let mut last_err: Option<anyhow::Error> = None;
    for src in sources {
        let result = match src {
            CatalogSource::Url(url) => fetch_url_catalog(&client, &url).await,
            CatalogSource::Npm(registry) => fetch_npm_catalog(&client, &registry).await,
        };
        match result {
            Ok(reg) => {
                cache_registry(paths, &reg);
                return Ok(reg);
            }
            Err(e) => last_err = Some(e),
        }
    }

    if let Some(reg) = load_cached_registry(paths) {
        return Ok(reg);
    }
    Err(last_err.unwrap_or_else(|| anyhow!("registry unreachable")))
}

/// Non-plugin content (themes/skills/MCP/bundles) is also served as structured
/// JSON from a hosted endpoint — one repo, four files — fetched live with the
/// bundled snapshot as the offline fallback. The base URL defaults to the
/// plugin catalog's host so a repo can publish `content-*.json` alongside
/// `plugins.json`; `AHL_CONTENT_URL` overrides it (e.g. a dedicated repo's
/// GitHub Pages root).
const CONTENT_BASE_DEFAULT: &str = "https://awesome-dsh-plugin.com/";

/// Fetch each content kind from the hosted endpoint, falling back to its
/// bundled snapshot when that file is unreachable. Always returns a merged
/// registry of all four kinds (bundled content guarantees non-empty results).
/// The four fetches run concurrently so a slow/hostile endpoint costs at most
/// one timeout, not four.
pub async fn fetch_content() -> Result<Registry> {
    let client = reqwest::Client::builder()
        .timeout(CATALOG_TIMEOUT)
        .build()?;
    let base = env_override("AHL_CONTENT_URL").unwrap_or_else(|| CONTENT_BASE_DEFAULT.to_string());

    let themes_url = format!("{base}content-themes.json");
    let skills_url = format!("{base}content-skills.json");
    let mcps_url = format!("{base}content-mcps.json");
    let bundles_url = format!("{base}content-bundles.json");
    let (themes, skills, mcps, bundles) = tokio::join!(
        fetch_url_catalog(&client, &themes_url),
        fetch_url_catalog(&client, &skills_url),
        fetch_url_catalog(&client, &mcps_url),
        fetch_url_catalog(&client, &bundles_url),
    );

    let mut content = Registry::default();
    for (remote, bundled) in [
        (themes, bundled_themes as fn() -> Registry),
        (skills, bundled_skills as fn() -> Registry),
        (mcps, bundled_mcps as fn() -> Registry),
        (bundles, bundled_bundles as fn() -> Registry),
    ] {
        let kind = remote.unwrap_or_else(|_| bundled());
        content.plugins.extend(kind.plugins);
        content.categories.extend(kind.categories);
    }
    content.count = content.plugins.len();
    Ok(content)
}

async fn fetch_url_catalog(client: &reqwest::Client, url: &str) -> Result<Registry> {
    let resp = client.get(url).send().await.context("catalog request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("catalog HTTP {}", resp.status()));
    }
    let text = resp.text().await.context("read catalog body")?;
    let reg: Registry = serde_json::from_str(&text).context("parse catalog JSON")?;
    Ok(hydrate(reg))
}

/// Read the catalog from the published `dsh-plugin-catalog` npm package: fetch
/// its metadata, follow `dist.tarball`, and pull `package/plugins.json` out of
/// the gzipped tar. This is the China-safe route (mirrors carry the package).
async fn fetch_npm_catalog(client: &reqwest::Client, registry: &str) -> Result<Registry> {
    let base = registry.trim_end_matches('/');
    let meta_url = format!("{base}/{CATALOG_PACKAGE}/latest");
    let resp = client
        .get(&meta_url)
        .send()
        .await
        .context("catalog package metadata")?;
    if !resp.status().is_success() {
        return Err(anyhow!("catalog package HTTP {}", resp.status()));
    }
    let meta: serde_json::Value = resp.json().await.context("parse catalog metadata")?;
    let tarball = meta["dist"]["tarball"]
        .as_str()
        .ok_or_else(|| anyhow!("catalog metadata names no tarball"))?;
    let tar_resp = client.get(tarball).send().await.context("catalog tarball")?;
    if !tar_resp.status().is_success() {
        return Err(anyhow!("catalog tarball HTTP {}", tar_resp.status()));
    }
    let bytes = tar_resp.bytes().await.context("read catalog tarball")?;
    let json_bytes = file_from_tarball(&bytes, "package/plugins.json")
        .ok_or_else(|| anyhow!("catalog tarball carries no plugins.json"))?;
    let reg: Registry = serde_json::from_slice(&json_bytes).context("parse catalog JSON")?;
    Ok(hydrate(reg))
}

/// Extract one file's bytes from a gzipped tar (512-byte headers, npm-style
/// `package/…` entry), mirroring dsh-market's `catalog-npm.ts:fileFromTarball`.
fn file_from_tarball(gz: &[u8], wanted: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(gz);
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).ok()?;

    let mut offset = 0usize;
    while offset + 512 <= buf.len() {
        let name = cstr(&buf[offset..offset + 100]);
        if name.is_empty() {
            break;
        }
        let size_str = cstr(&buf[offset + 124..offset + 136]);
        let size = usize::from_str_radix(size_str.trim(), 8).ok()?;
        let type_byte = buf[offset + 156];
        offset += 512;
        if (type_byte == b'0' || type_byte == 0) && name == wanted {
            return Some(buf[offset..offset + size].to_vec());
        }
        offset += (size + 511) / 512 * 512;
    }
    None
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

/// The npm registry the market and update checks read (mirror-aware).
pub fn npm_registry() -> String {
    env_override("AHL_NPM_MIRROR").unwrap_or_else(|| NPM_CHINA.to_string())
}

/// The `latest` dist-tag version of an npm package (the version update checks
/// compare against). Scoped names get their `/` URL-encoded.
pub async fn npm_latest(registry: &str, pkg: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(CATALOG_TIMEOUT)
        .build()?;
    let base = registry.trim_end_matches('/');
    let url = format!("{base}/{}/latest", pkg.replace('/', "%2F"));
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("npm latest metadata for {pkg}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("npm latest HTTP {} for {pkg}", resp.status()));
    }
    let meta: serde_json::Value = resp.json().await?;
    meta["version"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow!("npm latest has no version for {pkg}"))
}

/// True when `a` is a semantically higher dotted-numeric version than `b`
/// (leading `v`, pre-release and build suffixes ignored — forwards-only, like
/// dsh-market's `isUpgrade`).
pub fn version_newer(a: &str, b: &str) -> bool {
    fn num(s: &str) -> Vec<u64> {
        s.trim()
            .trim_start_matches(|c: char| c == 'v' || c == 'V')
            .split(|c: char| c == '-' || c == '+')
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    }
    let a = num(a);
    let b = num(b);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Local keyword/CJK prefilter: score every plugin against the need and return
/// the top `n` (the candidate set the LLM may cite). Pure and side-effect free.
pub fn prefilter(registry: &Registry, need: &str, n: usize) -> Vec<RegistryPlugin> {
    let tokens = tokenize(need);
    let mut scored: Vec<(usize, i32)> = registry
        .plugins
        .iter()
        .enumerate()
        .map(|(i, p)| (i, score_plugin(p, &tokens)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored
        .into_iter()
        .take(n)
        .map(|(i, _)| registry.plugins[i].clone())
        .collect()
}

/// Bundle recommendation candidate pool: sample the top `per_kind` matches of
/// each content kind so a need can produce mixed plans (plugins + skins + skills
/// + MCP) instead of being swamped by whichever kind is largest.
pub fn prefilter_diverse(registry: &Registry, need: &str, per_kind: usize) -> Vec<RegistryPlugin> {
    let tokens = tokenize(need);
    let mut kinds: Vec<ContentKind> = Vec::new();
    for p in &registry.plugins {
        // Bundles are composites, not leaf installables — never recommended as
        // a plan item.
        if p.kind != ContentKind::Bundle && !kinds.contains(&p.kind) {
            kinds.push(p.kind);
        }
    }
    kinds.sort_by_key(|k| k.as_str());
    let mut out: Vec<RegistryPlugin> = Vec::new();
    for kind in kinds {
        let mut scored: Vec<(usize, i32)> = registry
            .plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == kind)
            .map(|(i, p)| (i, score_plugin(p, &tokens)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.extend(scored.into_iter().take(per_kind).map(|(i, _)| registry.plugins[i].clone()));
    }
    out
}

/// Compose three plans from the model output, dropping any item whose
/// `kind:name` falls outside the candidate set — the `Result ⊆ Registry` invariant.
pub fn validate_plans(raw: &str, allowed: &HashSet<String>) -> Vec<RecommendPlan> {
    let json = extract_json(raw);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Vec::new();
    };
    let Some(arr) = value.get("plans").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for plan in arr {
        let id = plan.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let title = plan.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let rationale = plan
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut items = Vec::new();
        let raw_items = plan
            .get("items")
            .and_then(|v| v.as_array())
            .or_else(|| plan.get("plugins").and_then(|v| v.as_array()));
        if let Some(arr) = raw_items {
            for it in arr {
                let name = it.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let kind_raw = it.get("kind").and_then(|v| v.as_str()).unwrap_or("plugin");
                let kind = parse_kind(kind_raw);
                let key = format!("{}:{}", kind.as_str(), name);
                if allowed.contains(&key) {
                    let reason = it
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    items.push(PlanItem { name, kind, reason });
                }
            }
        }
        if !items.is_empty() {
            out.push(RecommendPlan {
                id,
                title,
                rationale,
                items,
            });
        }
    }
    out
}

/// Map a model-supplied kind string to a [`ContentKind`], tolerating the
/// user-facing "skin" alias for themes.
fn parse_kind(raw: &str) -> ContentKind {
    match raw.trim().to_ascii_lowercase().as_str() {
        "skin" | "theme" => ContentKind::Theme,
        "skill" => ContentKind::Skill,
        "mcp" => ContentKind::Mcp,
        "bundle" => ContentKind::Bundle,
        _ => ContentKind::Plugin,
    }
}

/// Smart search: prefilter → LLM re-rank → validate, against the user's
/// configured provider (key resolved from the vault by the caller).
pub async fn recommend(
    registry: &Registry,
    provider: &ResolvedProvider,
    need: &str,
) -> Result<RecommendResult> {
    let candidates = prefilter_diverse(registry, need, 12);
    let candidate_names: Vec<String> = candidates.iter().map(|p| p.kind_key()).collect();
    let allowed: HashSet<String> = candidate_names.iter().cloned().collect();

    let base = provider
        .profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.deepseek.com")
        .trim_end_matches('/');
    let model = provider
        .profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("deepseek-v4-flash");

    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": build_prompt(need, &candidates) },
        ],
        "max_tokens": 1200,
        "stream": false,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()?;
    let resp = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("LLM request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.context("read LLM response")?;
    if !status.is_success() {
        return Err(anyhow!(
            "LLM error {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| anyhow!("LLM returned non-JSON: {e}"))?;
    let raw = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let plans = validate_plans(&raw, &allowed);
    Ok(RecommendResult {
        plans,
        candidates: candidate_names,
        raw,
    })
}

fn build_prompt(need: &str, candidates: &[RegistryPlugin]) -> String {
    let mut lines = vec![
        format!("Need: {}", need.trim()),
        String::new(),
        "Candidates (kind | name | category | description):".to_string(),
    ];
    for (i, p) in candidates.iter().enumerate() {
        let name = p.key();
        let kind = p.kind.as_str();
        let cat = p.category.first().cloned().unwrap_or_else(|| "other".into());
        let desc = format!(
            "{} {}",
            p.description.get("en").cloned().unwrap_or_default(),
            p.description.get("zh").cloned().unwrap_or_default()
        );
        let desc = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let desc: String = desc.chars().take(180).collect();
        lines.push(format!("{}. {} | {} | {} | {}", i + 1, kind, name, cat, desc));
    }
    lines.push(String::new());
    lines.push("Return 3 plans as JSON:".to_string());
    lines.push(
        "{\"plans\":[{\"id\":\"A\",\"title\":\"...\",\"rationale\":\"...\",\"items\":[{\"name\":\"owner/repo\",\"kind\":\"plugin|theme|skill|mcp\",\"reason\":\"...\"}]}]}"
            .to_string(),
    );
    lines.join("\n")
}

/// Pull the first balanced `{ … }` block out of a possibly-fenced model reply.
fn extract_json(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```");
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => s[a..=b].to_string(),
        _ => s.to_string(),
    }
}

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{3000}'..='\u{303F}'
    )
}

fn flush_cjk(cjk: &mut Vec<char>, out: &mut Vec<String>) {
    if cjk.len() >= 2 {
        for w in cjk.windows(2) {
            out.push(w.iter().collect());
        }
    }
    cjk.clear();
}

/// Tokenize a query into lowercase ASCII words + CJK bigrams.
fn tokenize(s: &str) -> Vec<String> {
    let s = s.to_lowercase();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            word.push(ch);
            flush_cjk(&mut cjk, &mut out);
        } else if is_cjk(ch) {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            cjk.push(ch);
        } else {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            flush_cjk(&mut cjk, &mut out);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    flush_cjk(&mut cjk, &mut out);
    out
}

fn score_plugin(p: &RegistryPlugin, tokens: &[String]) -> i32 {
    let hay_name = p.name.to_lowercase();
    let hay_owner = p.owner.to_lowercase();
    let hay_cat: Vec<String> = p.category.iter().map(|c| c.to_lowercase()).collect();
    let hay_desc = format!(
        "{} {}",
        p.description.get("en").cloned().unwrap_or_default(),
        p.description.get("zh").cloned().unwrap_or_default()
    )
    .to_lowercase();

    let mut score = 0;
    for t in tokens {
        if t.is_empty() {
            continue;
        }
        if hay_name.contains(t) {
            score += 3;
        }
        if hay_owner.contains(t) {
            score += 2;
        }
        for c in &hay_cat {
            if c.contains(t) {
                score += 2;
            }
        }
        if hay_desc.contains(t) {
            score += 1;
        }
    }
    score
}

fn de_string_or_vec<'de, D>(d: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or array of strings")
        }
        fn visit_str<E>(self, v: &str) -> std::result::Result<Vec<String>, E> {
            Ok(vec![v.to_string()])
        }
        fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Vec<String>, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut out = Vec::new();
            while let Some(v) = seq.next_element::<String>()? {
                out.push(v);
            }
            Ok(out)
        }
        fn visit_none<E>(self) -> std::result::Result<Vec<String>, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E>(self) -> std::result::Result<Vec<String>, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_any(V)
}

/// Accept an object of string→string (the `{en, zh}` description), a bare
/// string (folded into `en`), or null — so one malformed entry can't fail the
/// whole catalog parse.
fn de_string_map<'de, D>(d: D) -> std::result::Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = HashMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string map, string, or null")
        }
        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut out = HashMap::new();
            while let Some((k, v)) = map.next_entry::<String, String>()? {
                out.insert(k, v);
            }
            Ok(out)
        }
        fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
            Ok([("en".to_string(), v.to_string())].into_iter().collect())
        }
        fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(HashMap::new())
        }
        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(HashMap::new())
        }
    }
    d.deserialize_any(V)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str, owner: &str, cat: &str, desc_en: &str, url: &str) -> RegistryPlugin {
        RegistryPlugin {
            name: name.into(),
            owner: owner.into(),
            url: url.into(),
            category: vec![cat.into()],
            description: [("en".into(), desc_en.into()), ("zh".into(), String::new())]
                .into_iter()
                .collect(),
            npm: None,
            tarball: None,
            ..Default::default()
        }
    }

    #[test]
    fn tokenize_splits_ascii_and_cjk() {
        let t = tokenize("github 操作 Git");
        assert!(t.iter().any(|x| x == "github"));
        assert!(t.iter().any(|x| x == "git"));
        assert!(t.iter().any(|x| x == "操作"));
    }

    #[test]
    fn prefilter_ranks_name_over_description() {
        let reg = Registry {
            plugins: vec![
                plugin("github-thing", "a", "tools", "does stuff with git", "https://github.com/a/github-thing"),
                plugin("other", "b", "fun", "unrelated", "https://github.com/b/other"),
            ],
            ..Default::default()
        };
        let top = prefilter(&reg, "github", 40);
        assert_eq!(top[0].name, "github-thing");
    }

    #[test]
    fn validate_drops_names_outside_candidates() {
        let allowed: HashSet<String> = ["plugin:a/github-thing".into(), "plugin:b/other".into()].into_iter().collect();
        let raw = r#"{"plans":[{"id":"A","title":"t","rationale":"r","items":[
            {"name":"a/github-thing","kind":"plugin","reason":"real"},
            {"name":"evil/nonexistent","kind":"plugin","reason":"hallucinated"}
        ]}]}"#;
        let plans = validate_plans(raw, &allowed);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].items.len(), 1);
        assert_eq!(plans[0].items[0].name, "a/github-thing");
    }

    #[test]
    fn install_spec_derives_from_npm_then_github() {
        let npm = RegistryPlugin {
            npm: Some("@scope/pkg".into()),
            url: "https://github.com/o/r".into(),
            ..Default::default()
        };
        assert_eq!(npm.install_spec(), "@scope/pkg");

        let gh = RegistryPlugin {
            url: "https://github.com/o/r/tree/main/sub".into(),
            ..Default::default()
        };
        assert_eq!(gh.install_spec(), "github:o/r");
    }

    #[test]
    fn kind_defaults_to_plugin() {
        let json = r#"{"name":"p","owner":"o","url":"https://github.com/o/p"}"#;
        let p: RegistryPlugin = serde_json::from_str(json).expect("kindless entry parses");
        assert_eq!(p.kind, ContentKind::Plugin);
    }

    #[test]
    fn bundled_mcps_load_with_config() {
        let reg = bundled_mcps();
        assert!(!reg.plugins.is_empty(), "bundled MCP catalog is empty");
        assert!(
            reg.plugins.iter().all(|p| p.kind == ContentKind::Mcp),
            "bundled catalog should be all MCP servers"
        );
        assert!(
            reg.plugins.iter().all(|p| p.server_name.is_some()),
            "every MCP entry must carry a serverName"
        );
        // Bulk-parsed entries whose source has no runnable command (Go/Rust/etc.)
        // ship without one and fall back to "open GitHub" in the UI — but the large
        // majority must still be one-click installable.
        let installable = reg
            .plugins
            .iter()
            .filter(|p| p.command.is_some() || p.mcp_url.is_some())
            .count();
        assert!(
            installable * 100 >= reg.plugins.len() * 60,
            "most MCP entries should carry a command (stdio) or mcpUrl (http), got {installable}/{}",
            reg.plugins.len()
        );
    }

    #[test]
    fn bundled_themes_load_and_hydrate() {
        let reg = bundled_themes();
        assert!(!reg.plugins.is_empty(), "bundled theme catalog is empty");
        assert!(
            reg.plugins.iter().all(|p| p.kind == ContentKind::Theme),
            "bundled catalog should be all themes"
        );
        assert!(
            reg.plugins.iter().all(|p| !p.spec.is_empty()),
            "every theme must derive a non-empty install spec"
        );
    }

    #[test]
    fn bundled_content_merges_all_kinds() {
        let content = bundled_content();
        assert!(!content.plugins.is_empty());
        let has = |k: ContentKind| content.plugins.iter().any(|p| p.kind == k);
        assert!(has(ContentKind::Theme));
        assert!(has(ContentKind::Skill));
        assert!(has(ContentKind::Mcp));
        assert!(has(ContentKind::Bundle));
        // Each kind's category label is merged for the filter dropdown.
        for key in ["skin", "skill", "mcp", "bundle"] {
            assert!(content.categories.contains_key(key), "missing category {key}");
        }
        assert_eq!(content.count, content.plugins.len());
    }

    #[test]
    fn bundled_bundles_load_and_reference_items() {
        let reg = bundled_bundles();
        assert!(!reg.plugins.is_empty(), "bundled bundle catalog is empty");
        assert!(
            reg.plugins.iter().all(|p| p.kind == ContentKind::Bundle),
            "bundled catalog should be all bundles"
        );
        // Every bundle expands into at least one leaf item, each with a kind
        // (plugin/theme/skill/mcp — never bundle) and a non-empty reference key.
        for b in &reg.plugins {
            let items = b.items.as_ref().expect("bundle should carry items");
            assert!(!items.is_empty(), "bundle {} has no items", b.name);
            for it in items {
                assert_ne!(it.kind, ContentKind::Bundle, "bundle item must be a leaf");
                assert!(!it.name.is_empty(), "bundle item missing reference key");
            }
        }
    }

    #[test]
    fn version_newer_compares_dotted_versions() {
        assert!(version_newer("1.2.0", "1.1.9"));
        assert!(!version_newer("1.1.9", "1.2.0"));
        assert!(version_newer("2.0.0", "1.9.9"));
        assert!(!version_newer("1.2.0", "1.2.0"));
        assert!(version_newer("v2.1.0", "2.0.5"));
        assert!(version_newer("1.2.0-beta.1", "1.1.0"));
        assert!(!version_newer("0.9.0", "0.10.0"));
    }

    #[test]
    fn file_from_tarball_extracts_named_entry() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let content = b"{\"plugins\":[]}";
        let mut tar = Vec::new();
        let name = b"package/plugins.json";
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name);
        let size_field = format!("{:011o}\0", content.len());
        header[124..136].copy_from_slice(size_field.as_bytes());
        header[156] = b'0'; // regular file
        tar.extend_from_slice(&header);
        tar.extend_from_slice(content);
        let pad = (512 - (content.len() % 512)) % 512;
        tar.extend(std::iter::repeat(0u8).take(pad));
        tar.extend(std::iter::repeat(0u8).take(1024)); // two zero blocks

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        let gz = enc.finish().unwrap();

        assert_eq!(file_from_tarball(&gz, "package/plugins.json").unwrap(), content);
        assert!(file_from_tarball(&gz, "package/nope.json").is_none());
    }
}
