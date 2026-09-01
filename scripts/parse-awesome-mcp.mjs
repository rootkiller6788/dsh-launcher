// Parses awesome-mcp-servers/README.md (a local clone) into a bulk MCP catalog:
// scripts/data/mcp-bulk.json. Every listed server is kept — the goal is to show
// them ALL, so nothing is filtered out by hand.
//
// Install command resolution, in order:
//   1. an inline `npx …` / `uvx …` command in the description (exact — 480 servers)
//   2. the language flag in the entry: 📇 → `npx -y github:owner/repo`,
//      🐍 → `uvx git+https://github.com/owner/repo` (best-effort default)
//   3. otherwise no command (Go/Rust/C#/Java/etc.) → the card falls back to
//      "open GitHub" since there's no universal one-liner for those runtimes.
//
//   node scripts/parse-awesome-mcp.mjs   # then: node scripts/gen-content-catalog.mjs
//
// This is bulk/auto-parsed: descriptions come straight from the repo README and
// no env vars are inferred, so quality is uneven (many entries need API keys or
// are cloud/paid). The curated scripts/data/mcp-overrides.json ships first.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const BULK = resolve(__dirname, 'data/mcp-bulk.json')
const CLONES = 'D:/Opencode/dsh-plugin'

const md = readFileSync(resolve(CLONES, 'awesome-mcp-servers/README.md'), 'utf8')
const lines = md.split('\n').filter((l) => /^-\s*\[/.test(l))

function sanitize(s) {
  const out = String(s)
    .trim()
    .replace(/[^A-Za-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 32)
  return (out || 'server').toLowerCase()
}

// Split a command snippet on whitespace, honouring single/double quotes.
function tokenize(s) {
  const out = []
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g
  let m
  while ((m = re.exec(s))) out.push(m[1] ?? m[2] ?? m[3])
  return out
}

function parseEntry(line) {
  const m = line.match(/^-\s*\[[^\]]+\]\(https:\/\/github\.com\/([^/]+)\/([^)/]+)/)
  if (!m) return null
  const owner = m[1]
  const name = m[2]
  const url = `https://github.com/${owner}/${name}`

  const di = line.indexOf(' - ')
  const rawDesc = di >= 0 ? line.slice(di + 3).trim() : ''

  const snips = [...line.matchAll(/`([^`]+)`/g)].map((x) => x[1])
  const snippet = snips.find((s) => /^(npx|uvx)\b/.test(s))

  let command = null
  let args = null

  if (snippet) {
    const tokens = tokenize(snippet)
    const head = tokens[0]
    let a = tokens.slice(1)
    // Skip CLI-usage examples (`npx foo search <query>`) — fall back to the
    // language-flag default below rather than shipping a bogus command.
    if (a.length > 0 && !a.some((x) => /[<>]/.test(x))) {
      if (head === 'npx' && !a.includes('-y') && !a.includes('--yes')) a = ['-y', ...a]
      command = head
      args = a
    }
  }

  if (!command) {
    // No explicit command: derive a best-effort default from the language flag.
    if (line.includes('📇')) {
      command = 'npx'
      args = ['-y', `github:${owner}/${name}`]
    } else if (line.includes('🐍')) {
      command = 'uvx'
      args = [`git+https://github.com/${owner}/${name}`]
    }
  }

  let desc = rawDesc.replace(snippet || '', '').replace(/install\s*[:：]\s*/i, '').trim()
  desc = desc.replace(/`/g, '').replace(/\s{2,}/g, ' ').replace(/^[-–—]\s*/, '').trim().slice(0, 200)

  return {
    name,
    owner,
    url,
    description: desc || `${owner}/${name}`,
    serverName: sanitize(`${owner}-${name}`),
    transport: command ? 'stdio' : null,
    command,
    args,
    env: {},
  }
}

const curated = JSON.parse(readFileSync(resolve(__dirname, 'data/mcp-overrides.json'), 'utf8'))
const curatedPkgs = new Set(curated.map((c) => (c.args || []).join(' ')))

const entries = []
const seen = new Set()
for (const line of lines) {
  const e = parseEntry(line)
  if (!e) continue
  if (seen.has(e.serverName)) continue
  if (e.args && curatedPkgs.has(e.args.join(' '))) continue
  seen.add(e.serverName)
  entries.push(e)
}

const installable = entries.filter((e) => e.command).length
const byCmd = entries.reduce((acc, e) => {
  acc[e.command || 'none'] = (acc[e.command || 'none'] || 0) + 1
  return acc
}, {})

mkdirSync(resolve(__dirname, 'data'), { recursive: true })
writeFileSync(BULK, JSON.stringify(entries, null, 2) + '\n')
console.log(`mcp-bulk.json: ${entries.length} servers (${installable} installable)`)
console.log('  by command:', JSON.stringify(byCmd))
