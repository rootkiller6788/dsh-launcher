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
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::download::download_file;
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

/// Resolver-produced install plan for an MCP server (roadmap §2.2 / §8.2).
///
/// `Discovery → Install → Runtime` single-direction derivation: the catalog
/// carries Discovery data; this is the Install manifest — *what* to prefetch and
/// the *canonical launch* that replaces any best-effort `github:` pseudo command.
/// Precomputed at catalog-build time by `scripts/resolver/github-analyzer.mjs` or
/// probed at runtime by the dsh-adapter resolver for a single missing entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpInstallManifest {
    /// Runtime the server needs: `"node"` or `"python"` (go/rust arrive in Phase 4).
    #[serde(default)]
    pub runtime: String,
    /// Package manager used for the install-time prefetch: `"npm"` or `"uv"`.
    #[serde(default)]
    pub method: String,
    /// Prefetch target — a registry package name, or a `github:` / `git+https://…`
    /// spec when the repo isn't published (source-run via npx/uvx).
    #[serde(default)]
    pub package: String,
    /// Canonical launch command (+args) that should be recorded on the MCP row.
    #[serde(default)]
    pub launch: McpLaunchSpec,
}

/// `command` + `args` pair used both for the catalog launch and the recorded MCP row.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpLaunchSpec {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// A single environment variable an MCP server needs configured to actually
/// operate, declared by the catalog (`content-mcp-env.json`, keyed by
/// `owner/name`). The launcher only *declares* in this phase — values are never
/// stored here; Library surfaces unset requirements and a future stage wires
/// value entry (secrets → the OS credential vault).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct McpEnvRequirement {
    /// Env var name the server reads, e.g. `KANBOARD_URL`.
    pub key: String,
    /// Human-readable label for the value the user must supply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `true` = a credential (token/key) — the UI must never echo or log it.
    #[serde(default)]
    pub secret: bool,
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
    /// Resolver-produced install plan (MCP). Absent for entries whose launch is a
    /// plain best-effort command, or ones the resolver couldn't fingerprint.
    #[serde(default)]
    pub mcp_install: Option<McpInstallManifest>,
    // --- bundle specific ---
    /// Curated bundle's item references (kind + owner/name + reason), resolved
    /// against the merged catalog and installed as a group.
    #[serde(default)]
    pub items: Option<Vec<PlanItem>>,
    /// Computed install target (npm | tarball | `github:owner/repo`).
    #[serde(default)]
    pub spec: String,
    /// Catalog-declared env the server needs configured (merged from
    /// `content-mcp-env.json` by [`hydrate`], keyed by `owner/name`). Never
    /// carries values — only declares *what* is required.
    #[serde(default)]
    pub required_env: Vec<McpEnvRequirement>,
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

    /// The `github:owner/repo` install spec derived from the record's URL, if
    /// it is a github.com link. Public so install paths can fall back to the
    /// GitHub source when a catalog `npm` name turns out to be unpublished.
    pub fn github_spec(&self) -> Option<String> {
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

/// Fill each plugin's computed `spec` after deserialization (fetch or cache),
/// and merge the per-server required-env declarations (`content-mcp-env.json`)
/// onto each MCP by catalog key (`owner/name`).
pub fn hydrate(mut reg: Registry) -> Registry {
    let envs = required_env_map();
    for p in &mut reg.plugins {
        p.spec = p.install_spec();
        p.required_env = envs.get(&p.key()).cloned().unwrap_or_default();
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
/// Per-server required-env declarations (`what` each MCP needs configured) —
/// embedded like the catalogs so it works offline. Never stores values.
const MCP_ENV_CATALOG: &str = include_str!("../data/content-mcp-env.json");

/// The parsed `owner/name → requirements` map from [`MCP_ENV_CATALOG`]. The
/// file's `comment` key is ignored by serde (unknown fields are skipped).
pub fn required_env_map() -> HashMap<String, Vec<McpEnvRequirement>> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        servers: HashMap<String, Vec<McpEnvRequirement>>,
    }
    serde_json::from_str::<File>(MCP_ENV_CATALOG)
        .map(|f| f.servers)
        .unwrap_or_default()
}
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
            CatalogSource::Npm(registry) => fetch_npm_catalog(&client, &registry, &paths.cache).await,
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

/// One hosted content file: `(kind, file name, bundled snapshot)`.
type ContentKindSpec = (&'static str, &'static str, fn() -> Registry);

/// The four hosted content files. Report order, so a failure list reads
/// themes → skills → mcps → bundles.
const CONTENT_KINDS: [ContentKindSpec; 4] = [
    ("themes", "content-themes.json", bundled_themes),
    ("skills", "content-skills.json", bundled_skills),
    ("mcps", "content-mcps.json", bundled_mcps),
    ("bundles", "content-bundles.json", bundled_bundles),
];

/// One content file the hosted endpoint did not answer, so its bundled snapshot
/// was used instead.
///
/// The fallback is invisible by design — the Market renders the same either way —
/// which is exactly why the miss has to be carried back to the caller: a host
/// that 404s all four files looks identical to one serving a catalog nobody
/// updates, and the only trace used to be a `404` in a log file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFetchFailure {
    /// `themes` / `skills` / `mcps` / `bundles`.
    pub kind: &'static str,
    /// The URL that did not answer.
    pub url: String,
    /// Why, in one line (`HTTP 404 Not Found`, a transport error, bad JSON).
    pub reason: String,
    /// The HTTP status when the host answered at all; `None` = never reached.
    pub status: Option<u16>,
}

impl ContentFetchFailure {
    /// The host answered "this file is not here": the snapshot is the only
    /// source by design, not a transient failure worth retrying.
    pub fn is_not_published(&self) -> bool {
        self.status == Some(404)
    }
}

/// One actionable line describing a content-fetch fallback, shared by the log
/// and the Activity entry so the two cannot drift.
pub fn content_failure_summary(failures: &[ContentFetchFailure]) -> String {
    let detail = failures
        .iter()
        .map(|f| format!("{}: {}", f.kind, f.reason))
        .collect::<Vec<_>>()
        .join("; ");
    if !failures.is_empty() && failures.iter().all(ContentFetchFailure::is_not_published) {
        format!(
            "live content catalogs are not published on the host ({detail}) — \
             using the snapshots bundled with this build. Point AHL_CONTENT_URL at a \
             base URL serving content-*.json to use a live catalog."
        )
    } else {
        format!("live content catalogs unreachable ({detail}) — using the bundled snapshots")
    }
}

/// The reason one content file could not be used, and the status if the host
/// answered. Kept apart from `anyhow` so a caller can tell "the endpoint does
/// not publish this file" from "we could not reach the endpoint" — they call for
/// different diagnostics.
#[derive(Debug, Clone)]
struct ContentMiss {
    reason: String,
    status: Option<u16>,
}

/// Fetch each content kind from the hosted endpoint, falling back to its
/// bundled snapshot when that file is unreachable. Always returns a merged
/// registry of all four kinds (bundled content guarantees non-empty results).
/// The four fetches run concurrently so a slow/hostile endpoint costs at most
/// one timeout, not four.
pub async fn fetch_content() -> Result<Registry> {
    Ok(fetch_content_report().await.0)
}

/// [`fetch_content`] plus the failures behind it, so a caller can tell the user
/// that a snapshot answered instead of the live catalog — see
/// [`ContentFetchFailure`]. Not an error: the registry is always usable.
pub async fn fetch_content_report() -> (Registry, Vec<ContentFetchFailure>) {
    let base = env_override("AHL_CONTENT_URL").unwrap_or_else(|| CONTENT_BASE_DEFAULT.to_string());
    fetch_content_from(&base).await
}

/// [`fetch_content_report`] with the base URL passed in, so a test can point it
/// at a local server rather than mutate the process environment.
async fn fetch_content_from(base: &str) -> (Registry, Vec<ContentFetchFailure>) {
    let client = match reqwest::Client::builder().timeout(CATALOG_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(error = %e, "catalog HTTP client unavailable — serving the bundled snapshots");
            let failures = CONTENT_KINDS
                .iter()
                .map(|(kind, file, _)| ContentFetchFailure {
                    kind,
                    url: format!("{base}{file}"),
                    reason: format!("HTTP client unavailable: {e}"),
                    status: None,
                })
                .collect();
            return (bundled_content(), failures);
        }
    };

    // One URL per kind, built here so the report and the request cannot disagree
    // about which file was asked for.
    let urls: Vec<String> = CONTENT_KINDS
        .iter()
        .map(|(_, file, _)| format!("{base}{file}"))
        .collect();
    let (themes, skills, mcps, bundles) = tokio::join!(
        fetch_content_catalog(&client, &urls[0]),
        fetch_content_catalog(&client, &urls[1]),
        fetch_content_catalog(&client, &urls[2]),
        fetch_content_catalog(&client, &urls[3]),
    );

    let mut content = Registry::default();
    let mut failures = Vec::new();
    let results = [themes, skills, mcps, bundles];
    for (i, remote) in results.into_iter().enumerate() {
        let (kind, _, bundled) = CONTENT_KINDS[i];
        match remote {
            Ok(reg) => {
                content.plugins.extend(reg.plugins);
                content.categories.extend(reg.categories);
            }
            Err(miss) => {
                let url = urls[i].clone();
                tracing::debug!(kind, url = %url, reason = %miss.reason, "content catalog unavailable — serving the bundled snapshot");
                failures.push(ContentFetchFailure {
                    kind,
                    url,
                    reason: miss.reason,
                    status: miss.status,
                });
                let snapshot = bundled();
                content.plugins.extend(snapshot.plugins);
                content.categories.extend(snapshot.categories);
            }
        }
    }
    content.count = content.plugins.len();
    // One warn for the whole batch, after the per-kind debug lines: a run where
    // the endpoint is dead should not bury the rest of the log in four copies of
    // the same complaint.
    if !failures.is_empty() {
        tracing::warn!("{}", content_failure_summary(&failures));
    }
    (content, failures)
}

/// Fetch one hosted content file, reporting the status separately from the
/// transport error so the caller can word the diagnostic correctly.
async fn fetch_content_catalog(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<Registry, ContentMiss> {
    let miss = |reason: String, status: Option<u16>| ContentMiss { reason, status };
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| miss(format!("{e:#}"), None))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(miss(format!("HTTP {status}"), Some(status.as_u16())));
    }
    let text = resp.text().await.map_err(|e| miss(format!("{e:#}"), None))?;
    serde_json::from_str(&text)
        .map(hydrate)
        .map_err(|e| miss(format!("unreadable JSON: {e:#}"), None))
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
///
/// The tarball goes through [`download_file`] into the download cache rather
/// than a one-shot `bytes()`, so a mid-body drop from a flaky mirror resumes
/// from the partial instead of restarting, and an unchanged catalog version is
/// served from the cached file on later refreshes (the URL is version-pinned,
/// so a cached artifact can never go stale).
async fn fetch_npm_catalog(
    client: &reqwest::Client,
    registry: &str,
    cache: &Path,
) -> Result<Registry> {
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

    let cached = npm_tarball_cache_path(cache, tarball);
    if !cached.is_file() {
        // `download_file` verifies nothing here — npm's own metadata is the
        // authority, and only a fully-received file is promoted out of `.part`.
        // Callers that know a trusted hash pass it to the same function.
        download_file(client, tarball, &cached, None)
            .await
            .map_err(|e| anyhow!("catalog tarball: {e:#}"))?;
    }
    let bytes = std::fs::read(&cached).context("read cached catalog tarball")?;
    let json_bytes = file_from_tarball(&bytes, "package/plugins.json")
        .ok_or_else(|| anyhow!("catalog tarball carries no plugins.json"))?;
    let reg: Registry = serde_json::from_slice(&json_bytes).context("parse catalog JSON")?;
    Ok(hydrate(reg))
}

/// The download-cache path for a version-pinned npm tarball URL. Sanitizing the
/// URL keeps one artifact per registry × version and leaves the file name
/// readable (`…/downloads/registry.npmjs.org_npm_dsh-plugin-catalog_-_…-1.2.3.tgz`).
fn npm_tarball_cache_path(cache: &Path, tarball: &str) -> PathBuf {
    let sanitized: String = tarball
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache.join("downloads").join(sanitized)
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
        offset += size.div_ceil(512) * 512;
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
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
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

    let raw = crate::llm::chat(provider, SYSTEM_PROMPT, &build_prompt(need, &candidates)).await?;
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
    fn hydrate_merges_env_requirements_by_owner_slash_name() {
        let reg = bundled_mcps();
        // kanboard lives in the 3452-row bulk catalog (not the curated 25), so a
        // hit proves the merge keys off `owner/name` across bulk rows too.
        let kb = reg
            .plugins
            .iter()
            .find(|p| p.owner == "ErnestoCorona" && p.name == "kanboard-mcp")
            .expect("kanboard-mcp present in bundled MCP catalog");
        assert_eq!(
            kb.required_env,
            vec![McpEnvRequirement {
                key: "KANBOARD_URL".into(),
                label: Some("Kanboard base URL".into()),
                secret: false,
            }],
            "kanboard must inherit its catalog-declared required env after hydrate"
        );
        // Every declared key that exists in the catalog must be hydrated; entries
        // without a declaration must stay clean — no phantom "needs config" hints.
        let declared = required_env_map();
        let mut declared_hits = 0usize;
        for p in &reg.plugins {
            match declared.get(&p.key()) {
                Some(reqs) => {
                    declared_hits += 1;
                    assert_eq!(p.required_env, *reqs, "{} must inherit its declaration", p.key());
                }
                None => assert!(
                    p.required_env.is_empty(),
                    "{} picked up an undeclared requirement",
                    p.key()
                ),
            }
        }
        assert!(
            declared_hits > 0,
            "content-mcp-env.json declarations must reach at least one catalog entry"
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
        // Each kind's category label is merged for the filter dropdown. Themes
        // and bundles keep their fixed labels; skills and MCP servers now carry
        // their README sections as categories (not a flat "skill"/"mcp" bucket),
        // so assert every such entry has a labeled category.
        for key in ["skin", "bundle"] {
            assert!(content.categories.contains_key(key), "missing category {key}");
        }
        for kind in [ContentKind::Skill, ContentKind::Mcp] {
            for p in content.plugins.iter().filter(|p| p.kind == kind) {
                assert!(!p.category.is_empty(), "{} missing category", p.name);
                assert!(
                    content.categories.contains_key(p.category[0].as_str()),
                    "{} has unlabeled category {:?}",
                    p.name,
                    p.category
                );
            }
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
        tar.extend(std::iter::repeat_n(0u8, pad));
        tar.extend(std::iter::repeat_n(0u8, 1024)); // two zero blocks

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        let gz = enc.finish().unwrap();

        assert_eq!(file_from_tarball(&gz, "package/plugins.json").unwrap(), content);
        assert!(file_from_tarball(&gz, "package/nope.json").is_none());
    }

    // ---- hosted content catalog: the fallback must be reported, not silent ----

    /// Read one HTTP/1.1 request head off the socket. (The download tests keep
    /// their own copy — this one only needs the request line.)
    async fn read_request_line(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// Answer `requests` connections, picking the route whose file name appears
    /// in the request line (404 when none matches), then closing each socket —
    /// so the four concurrent fetches get four connections.
    async fn serve_content(
        listener: tokio::net::TcpListener,
        routes: Vec<(&'static str, u16, &'static str)>,
    ) {
        use tokio::io::AsyncWriteExt;
        for _ in 0..4 {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let request_line = read_request_line(&mut sock).await;
                let (status, body) = routes
                    .iter()
                    .find(|(file, _, _)| request_line.contains(file))
                    .map(|(_, status, body)| (*status, *body))
                    .unwrap_or((404, ""));
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
            });
        }
    }

    async fn content_server(
        routes: Vec<(&'static str, u16, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (
            format!("http://{addr}/"),
            tokio::spawn(serve_content(listener, routes)),
        )
    }

    /// Wait for the server task, so a panic inside a handler is not swallowed.
    async fn join(server: tokio::task::JoinHandle<()>) {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), server).await;
    }

    #[tokio::test]
    async fn content_fetch_falls_back_to_the_snapshots_and_says_which_files_are_missing() {
        // The live host publishes no content-*.json at all (the state this
        // launcher was in). Every kind must fall back AND be reported.
        let (base, server) = content_server(vec![]).await;
        let (content, failures) = fetch_content_from(&base).await;
        join(server).await;

        assert_eq!(
            failures.iter().map(|f| f.kind).collect::<Vec<_>>(),
            ["themes", "skills", "mcps", "bundles"],
            "one failure per kind, in report order"
        );
        for f in &failures {
            assert_eq!(f.status, Some(404), "{}", f.kind);
            assert!(f.is_not_published(), "{} reads as not published", f.kind);
            assert!(f.url.ends_with(&format!("content-{}.json", f.kind)));
        }
        let summary = content_failure_summary(&failures);
        assert!(summary.contains("HTTP 404 Not Found"), "{summary}");
        assert!(summary.contains("AHL_CONTENT_URL"), "actionable: {summary}");

        let bundled = bundled_content();
        assert_eq!(
            content.plugins.len(),
            bundled.plugins.len(),
            "the four snapshots still make a full catalog"
        );
        assert_eq!(content.count, content.plugins.len());
    }

    #[tokio::test]
    async fn content_fetch_keeps_the_remote_copy_and_reports_only_the_rest() {
        let remote_theme = r#"{"count":1,"categories":{"remote":{"en":"Remote"}},
            "plugins":[{"name":"remote-theme","owner":"o","kind":"theme",
            "url":"https://github.com/o/remote-theme"}]}"#;
        let (base, server) = content_server(vec![("content-themes.json", 200, remote_theme)]).await;
        let (content, failures) = fetch_content_from(&base).await;
        join(server).await;

        assert!(
            content.plugins.iter().any(|p| p.name == "remote-theme"),
            "the served copy is used, not the snapshot"
        );
        assert_eq!(
            failures.iter().map(|f| f.kind).collect::<Vec<_>>(),
            ["skills", "mcps", "bundles"]
        );
        assert!(content.categories.contains_key("remote"));
        // The snapshot themes are gone — the served file replaced that kind.
        // (Parsed once: the snapshot is ~1 MB and the catalog is thousands of rows.)
        let snapshot_keys: HashSet<String> =
            bundled_themes().plugins.iter().map(|p| p.key()).collect();
        assert!(!content
            .plugins
            .iter()
            .any(|p| snapshot_keys.contains(&p.key())));
    }

    #[tokio::test]
    async fn content_fetch_calls_an_unreachable_host_unreachable_not_missing() {
        // A bound-then-dropped port: nothing is listening, so the fetches fail
        // without ever getting a status — a different diagnostic from a 404.
        let addr = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let (content, failures) = fetch_content_from(&format!("http://{addr}/")).await;

        assert_eq!(failures.len(), 4);
        for f in &failures {
            assert_eq!(f.status, None, "{}: no HTTP status was received", f.kind);
            assert!(!f.is_not_published());
            assert!(!f.reason.is_empty());
        }
        let summary = content_failure_summary(&failures);
        assert!(summary.contains("unreachable"), "{summary}");
        assert!(
            !summary.contains("AHL_CONTENT_URL"),
            "a dead host is not a publishing problem: {summary}"
        );
        assert_eq!(content.plugins.len(), bundled_content().plugins.len());
    }

    #[test]
    fn content_failure_summary_separates_missing_from_unreachable() {
        let missing = ContentFetchFailure {
            kind: "skills",
            url: "https://example.com/content-skills.json".into(),
            reason: "HTTP 404 Not Found".into(),
            status: Some(404),
        };
        let refused = ContentFetchFailure {
            status: None,
            reason: "error sending request".into(),
            ..missing.clone()
        };
        // All-404: the host does not publish the files — name the way out.
        let summary = content_failure_summary(&[missing.clone(), missing.clone()]);
        assert!(summary.contains("not published"), "{summary}");
        assert!(summary.contains("AHL_CONTENT_URL"), "{summary}");
        // One of them unreachable: do not claim the host does not publish them.
        let summary = content_failure_summary(&[missing, refused]);
        assert!(summary.contains("unreachable"), "{summary}");
        assert!(!summary.contains("AHL_CONTENT_URL"), "{summary}");
    }
}
