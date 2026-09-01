// Generates the bundled content catalogs that ship inside the launcher binary:
//   crates/launcher-core/data/content-themes.json  <- awesome-dsh-themes/data/themes.json
//   crates/launcher-core/data/content-skills.json  <- awesome-agent-skills/README.md
//   crates/launcher-core/data/content-mcps.json    <- scripts/data/mcp-overrides.json
//   crates/launcher-core/data/content-bundles.json <- awesome-agent-bundles/data/bundles.json
//
// The awesome-* clones live OUTSIDE this repo (D:/Opencode/dsh-plugin/…); this is
// a dev-time tool — run it, commit the resulting JSON, and the launcher embeds
// the JSON via include_str! (offline, no hosted endpoint needed for these kinds).
//
//   node scripts/gen-content-catalog.mjs

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, '..')
const dataDir = resolve(repoRoot, 'crates/launcher-core/data')
const CLONES = 'D:/Opencode/dsh-plugin'

const CJK = /[\u4e00-\u9fff]/

// RegistryPlugin.description is `{en, zh}`; theme descriptions are plain text.
// CJK text lands in `zh`, everything else in `en`.
function toDescription(desc) {
  if (!desc) return {}
  const d = String(desc).trim()
  return CJK.test(d) ? { zh: d } : { en: d }
}

function genThemes() {
  const raw = JSON.parse(
    readFileSync(resolve(CLONES, 'awesome-dsh-themes/data/themes.json'), 'utf8'),
  )
  const skins = (raw.themes || [])
    .filter((t) => t.kind === 'skin' && t.status === 'verified')
    .map((t) => {
      const owner = (t.repo || '').split('/')[0] || ''
      return {
        name: t.name,
        owner,
        url: t.repo ? `https://github.com/${t.repo}` : '',
        category: [t.category || 'skin'],
        description: toDescription(t.description),
        npm: t.npm ?? null,
        tarball: null,
        screenshots: [],
        stars: null,
        downloads: null,
        install: t.install || '',
        added: t.added || '',
        deprecated: null,
        replacement: null,
        kind: 'theme',
        preview: t.preview ?? null,
        previewCss: t.previewCss ?? null,
        path: t.path ?? null,
        gist: t.gist ?? null,
      }
    })
  const out = {
    updated: raw.updated || '',
    count: skins.length,
    categories: { skin: { en: 'Skins', zh: '皮肤' } },
    plugins: skins,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-themes.json'), JSON.stringify(out, null, 2))
  console.log(`content-themes.json: ${skins.length} verified skins`)
}

// Skill README line shape:  `- **[owner/name](URL)** - description`
// The URL is an officialskills.sh deep-link or a github.com blob/tree/repo link.
// `repo` is the canonical github.com repo (the clone/install target); `fetch` is
// a best-effort raw SKILL.md URL — a fast path that works for the well-formed
// layouts but is wrong for repos with an unconventional tree (install falls back
// to a shallow clone + SKILL.md search when it 404s).
function resolveSkill(url) {
  let m
  // officialskills.sh/<owner>/<repo>/<name>  →  github.com/<owner>/<repo>
  m = url.match(/^https?:\/\/officialskills\.sh\/([^/]+)\/([^/]+)\/([^/]+)\/?$/)
  if (m) {
    const [, o, r, name] = m
    return {
      owner: o,
      name,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/HEAD/skills/${name}/SKILL.md`,
    }
  }
  // github blob: keep the full path (which ends in SKILL.md)
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/blob\/([^/]+)\/(.+)$/)
  if (m) {
    const [, o, r, branch, path] = m
    return {
      owner: o,
      name: lastSkillSegment(path) || r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/${branch}/${path}`,
    }
  }
  // github tree: append SKILL.md to the subdir
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/tree\/([^/]+)\/(.+)$/)
  if (m) {
    const [, o, r, branch, path] = m
    return {
      owner: o,
      name: lastSkillSegment(path) || r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/${branch}/${path}/SKILL.md`,
    }
  }
  // github repo root: SKILL.md at the root of the default branch
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/?$/)
  if (m) {
    const [, o, r] = m
    return {
      owner: o,
      name: r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/HEAD/SKILL.md`,
    }
  }
  return null
}

// The skill folder name = the path segment holding SKILL.md (drop a trailing
// `SKILL.md` segment first for blob URLs).
function lastSkillSegment(path) {
  const segs = path.split('/').filter(Boolean)
  if (segs.length && segs[segs.length - 1].toLowerCase() === 'skill.md') segs.pop()
  return segs[segs.length - 1] || ''
}

function genSkills() {
  const md = readFileSync(resolve(CLONES, 'awesome-agent-skills/README.md'), 'utf8')
  const seen = new Set()
  const skills = []
  for (const raw of md.split('\n')) {
    const m = raw.match(/^\s*-\s*\*\*\[([^\]]+)\]\(([^)]+)\)\*\*([\s\S]*)$/)
    if (!m) continue
    const [, , url, rest] = m
    const resolved = resolveSkill(url)
    if (!resolved) continue
    const id = `${resolved.owner}/${resolved.name}`
    if (seen.has(id)) continue
    seen.add(id)
    const dashIdx = rest.search(/[-–—]/)
    const desc = (dashIdx === -1 ? rest : rest.slice(dashIdx + 1)).trim().slice(0, 200)
    skills.push({
      name: resolved.name,
      owner: resolved.owner,
      url: resolved.repo,
      category: ['skill'],
      description: toDescription(desc),
      npm: null,
      tarball: null,
      screenshots: [],
      stars: null,
      downloads: null,
      install: '',
      added: '',
      deprecated: null,
      replacement: null,
      kind: 'skill',
      fetch: resolved.fetch,
    })
  }
  const out = {
    updated: '',
    count: skills.length,
    categories: { skill: { en: 'Skills', zh: 'Skill' } },
    plugins: skills,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-skills.json'), JSON.stringify(out, null, 2))
  console.log(`content-skills.json: ${skills.length} skills`)
}

// MCP servers ship from two lists: a hand-maintained curated set (verified npm
// packages, good descriptions, notes on required env vars) plus a bulk list
// auto-parsed from awesome-mcp-servers/README.md (scripts/parse-awesome-mcp.mjs).
// Curated first, then bulk. Each entry is self-contained: name/owner/url for the
// card, plus serverName/transport/command/args/env (stdio) or mcpUrl/headers
// (streamable-http) for the mcp-client insert row.
function genMcps() {
  const overrides = JSON.parse(
    readFileSync(resolve(__dirname, 'data/mcp-overrides.json'), 'utf8'),
  )
  const bulk = JSON.parse(
    readFileSync(resolve(__dirname, 'data/mcp-bulk.json'), 'utf8'),
  )
  const mcps = [...overrides, ...bulk].map((o) => ({
    name: o.name,
    owner: o.owner,
    url: o.url,
    category: ['mcp'],
    description: toDescription(o.description),
    npm: null,
    tarball: null,
    screenshots: [],
    stars: null,
    downloads: null,
    install: '',
    added: '',
    deprecated: null,
    replacement: null,
    kind: 'mcp',
    serverName: o.serverName ?? null,
    transport: o.transport ?? null,
    command: o.command ?? null,
    args: o.args ?? null,
    env: o.env ?? null,
    mcpUrl: o.mcpUrl ?? null,
    headers: o.headers ?? null,
  }))
  const out = {
    updated: '',
    count: mcps.length,
    categories: { mcp: { en: 'MCP Servers', zh: 'MCP' } },
    plugins: mcps,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-mcps.json'), JSON.stringify(out, null, 2))
  console.log(`content-mcps.json: ${mcps.length} MCP servers`)
}

// Bundles are curated cross-kind combinations. Each entry is a composite that
// references existing content by `kind` + `owner/name`; the launcher resolves
// those references against the merged catalog at install time. The card shows
// the bundle title + rationale, and a one-click install expands into its items.
function genBundles() {
  const raw = JSON.parse(
    readFileSync(resolve(CLONES, 'awesome-agent-bundles/data/bundles.json'), 'utf8'),
  )
  const bundles = (raw.bundles || []).map((b) => ({
    name: b.name,
    owner: '',
    url: '',
    category: ['bundle'],
    description: toDescription(b.description),
    npm: null,
    tarball: null,
    screenshots: [],
    stars: null,
    downloads: null,
    install: '',
    added: b.added || '',
    deprecated: null,
    replacement: null,
    kind: 'bundle',
    items: (b.items || []).map((it) => ({
      name: it.name,
      kind: it.kind,
      reason: it.reason || '',
    })),
  }))
  const out = {
    updated: raw.updated || '',
    count: bundles.length,
    categories: { bundle: { en: 'Bundles', zh: '整合包' } },
    plugins: bundles,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-bundles.json'), JSON.stringify(out, null, 2))
  console.log(`content-bundles.json: ${bundles.length} bundles`)
}

genThemes()
genSkills()
genMcps()
genBundles()
