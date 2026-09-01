// Regenerates scripts/data/mcp-overrides.json from a curated candidate list,
// VERIFYING each npm package against the registry so a name is never shipped
// unless `npx -y <name>` will actually resolve. Existing hand-maintained
// entries are preserved; candidates already present are skipped.
//
//   node scripts/gen-mcp-overrides.mjs   # then: node scripts/gen-content-catalog.mjs

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const OVERRIDES = resolve(__dirname, 'data/mcp-overrides.json')

// pkg         = npm package name (verified against registry.npmjs.org)
// serverName  = clean MCP server id (becomes `mcp-<serverName>` cordis row id)
// repo        = canonical GitHub URL (owner is derived from it)
// note        = appended to the description (e.g. required env vars)
const CANDIDATES = [
  // official reference servers (modelcontextprotocol/servers)
  { pkg: '@modelcontextprotocol/server-everything', serverName: 'everything', repo: 'https://github.com/modelcontextprotocol/servers' },
  { pkg: '@modelcontextprotocol/server-pdf', serverName: 'pdf', repo: 'https://github.com/modelcontextprotocol/servers' },
  { pkg: '@modelcontextprotocol/server-wiki-explorer', serverName: 'wiki-explorer', repo: 'https://github.com/modelcontextprotocol/servers' },
  { pkg: '@modelcontextprotocol/server-system-monitor', serverName: 'system-monitor', repo: 'https://github.com/modelcontextprotocol/servers' },
  { pkg: '@modelcontextprotocol/server-google-maps', serverName: 'google-maps', repo: 'https://github.com/modelcontextprotocol/servers', note: 'requires GOOGLE_MAPS_API_KEY' },
  // popular third-party
  { pkg: '@sentry/mcp-server', serverName: 'sentry', repo: 'https://github.com/getsentry/sentry-mcp', note: 'requires SENTRY_AUTH_TOKEN' },
  { pkg: '@e2b/mcp-server', serverName: 'e2b', repo: 'https://github.com/e2b-dev/mcp-server', note: 'requires E2B_API_KEY' },
  { pkg: '@neondatabase/mcp-server-neon', serverName: 'neon', repo: 'https://github.com/neondatabase/mcp-server-neon', note: 'requires NEON_API_KEY' },
  { pkg: 'chrome-devtools-mcp', serverName: 'chrome-devtools', repo: 'https://github.com/ChromeDevTools/chrome-devtools-mcp' },
  { pkg: '@upstash/context7-mcp', serverName: 'context7', repo: 'https://github.com/upstash/context7' },
  { pkg: '@bytebase/dbhub', serverName: 'dbhub', repo: 'https://github.com/bytebase/dbhub', note: 'requires DATABASE_URL' },
  { pkg: '@browserbasehq/mcp', serverName: 'browserbase', repo: 'https://github.com/browserbase/mcp-server-browserbase', note: 'requires BROWSERBASE_API_KEY' },
  { pkg: 'mcp-server-commands', serverName: 'commands', repo: 'https://github.com/g0t4/mcp-server-commands' },
]

async function npmMeta(pkg) {
  const url = `https://registry.npmjs.org/${encodeURIComponent(pkg).replace('%40', '@')}`
  const res = await fetch(url)
  if (!res.ok) return null
  return res.json()
}

function sanitize(s) {
  const out = String(s).trim().replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 32)
  return out || 'server'
}

const existing = JSON.parse(readFileSync(OVERRIDES, 'utf8'))
const seen = new Set(existing.map((e) => e.serverName))

const added = []
for (const c of CANDIDATES) {
  const meta = await npmMeta(c.pkg)
  if (!meta) {
    console.log(`SKIP (not on npm): ${c.pkg}`)
    continue
  }
  if (seen.has(c.serverName)) {
    console.log(`SKIP (already present): ${c.serverName}`)
    continue
  }
  const owner = (c.repo.match(/github\.com\/([^/]+)/) || [])[1] || ''
  const name = c.pkg.replace(/^@[^/]+\//, '')
  const desc = [meta.description || '', c.note || ''].filter(Boolean).join(' — ')
  added.push({
    name,
    owner,
    url: c.repo,
    description: desc || name,
    serverName: c.serverName,
    transport: 'stdio',
    command: 'npx',
    args: ['-y', c.pkg],
    env: {},
  })
  seen.add(c.serverName)
  console.log(`ADD ${c.pkg}  ->  ${c.serverName}`)
}

if (added.length === 0) {
  console.log('No new entries; overrides unchanged.')
  process.exit(0)
}

const merged = [...existing, ...added]
mkdirSync(resolve(__dirname, 'data'), { recursive: true })
writeFileSync(OVERRIDES, JSON.stringify(merged, null, 2) + '\n')
console.log(`\nWrote ${merged.length} MCP entries (${existing.length} existing + ${added.length} new) to data/mcp-overrides.json`)
