// Dev-time Resolver batch (roadmap Phase 1 §8.1): probe every catalog MCP entry
// that currently has a *pseudo* command (`npx github:o/r` / `uvx git+…`) or no
// command at all, read its repo fingerprint files, verify against the package
// registries, and record a canonical InstallManifest.
//
// Results cache to scripts/data/mcp-resolved.json keyed by "owner/repo" so reruns
// only touch entries that errored (or --fresh). The content generator
// (gen-content-catalog.mjs genMcps) consumes this cache to stamp entries.
//
//   node scripts/resolver/github-analyzer.mjs                # full batch (resumable)
//   node scripts/resolver/github-analyzer.mjs --limit 60 --seed 1   # sample
//   node scripts/resolver/github-analyzer.mjs --only foo/bar
//
// Env: AHL_NPM_MIRROR (default registry.npmmirror.com), AHL_PYPI_MIRROR (default pypi.org).

import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { classify, decide, hintOf, ownerRepoOf, parsePackageJson, parsePyproject, parseSetupPy, decompileDockerfile } from './analyze-repo.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const ROOT = resolve(__dirname, '..', '..')
const CATALOG = resolve(ROOT, 'crates/launcher-core/data/content-mcps.json')
const CACHE = resolve(__dirname, '..', 'data/mcp-resolved.json')

const NPM_MIRROR = process.env.AHL_NPM_MIRROR || 'https://registry.npmmirror.com'
const PYPI_MIRROR = process.env.AHL_PYPI_MIRROR || 'https://pypi.org/pypi'

// --- tiny CLI parser --------------------------------------------------------

function parseArgs(argv) {
  const o = { only: [], class: null, limit: 0, concurrency: 8, fresh: false }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    const next = () => argv[++i]
    if (a === '--limit') o.limit = Number(next())
    else if (a === '--seed') o.seed = Number(next())
    else if (a === '--class') o.class = next()
    else if (a === '--concurrency') o.concurrency = Number(next())
    else if (a === '--fresh') o.fresh = true
    else if (a === '--only') o.only.push(next())
  }
  return o
}

// --- HTTP helpers -----------------------------------------------------------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function http(url, timeoutMs) {
  const c = new AbortController()
  const t = setTimeout(() => c.abort(), timeoutMs)
  try {
    const res = await fetch(url, { signal: c.signal, headers: { 'user-agent': 'dshl-resolver/0.1' } })
    const body = await res.text()
    return { ok: res.ok, status: res.status, body }
  } catch {
    return { ok: false, status: 0, body: '' }
  } finally {
    clearTimeout(t)
  }
}

// File fetch: try raw HEAD → jsdelivr default-branch → raw main/master.
const RAW = (o, r, p) => `https://raw.githubusercontent.com/${o}/${r}/HEAD/${p}`
const JSD = (o, r, p, br) => `https://cdn.jsdelivr.net/gh/${o}/${r}@${br}/${p}`
const RAWB = (o, r, p, br) => `https://raw.githubusercontent.com/${o}/${r}/${br}/${p}`

/** @returns {string|null|'ERR'} text on 200, null if repo/file absent, 'ERR' on net fail */
async function fetchFile(owner, repo, path) {
  const attempts = [
    [RAW(owner, repo, path), 12000],
    [JSD(owner, repo, path, 'main'), 10000],
    [JSD(owner, repo, path, 'master'), 10000],
    [RAWB(owner, repo, path, 'main'), 10000],
    [RAWB(owner, repo, path, 'master'), 10000],
  ]
  for (const [url, ms] of attempts) {
    const res = await http(url, ms)
    if (res.ok && res.body.length) return res.body
    if (res.status === 404) return null // authoritative absence (raw reflects default branch)
    if (res.status === 0) continue // network error → try next mirror
    continue // 429/5xx — try next mirror
  }
  return 'ERR'
}

// Registry existence checks with cross-entry in-flight dedupe.
const registryInflight = new Map()
const verified = { npm: {}, pypi: {} }

async function verifyName(name, kind) {
  const key = `${kind}:${name}`
  if (kind === 'npm' && name in verified.npm) return verified.npm[name]
  if (kind === 'pypi' && name in verified.pypi) return verified.pypi[name]
  if (registryInflight.has(key)) return registryInflight.get(key)
  const p = (async () => {
    const url = kind === 'npm'
      ? `${NPM_MIRROR}/${encodeURIComponent(name)}`
      : `${PYPI_MIRROR}/${name}/json`
    const res = await http(url, 12000)
    if (res.status === 0 || res.status === 429) return 'ERR' // retry next run
    const ok = res.status === 200
    verified[kind][name] = ok
    return ok
  })()
  registryInflight.set(key, p)
  try {
    return await p
  } finally {
    registryInflight.delete(key)
  }
}

// --- fingerprinting ---------------------------------------------------------

const TIER1 = {
  node: ['package.json'],
  python: ['pyproject.toml', 'setup.py', 'requirements.txt'],
  none: ['package.json', 'pyproject.toml'],
}
const TIER2 = ['Dockerfile', 'Cargo.toml', 'go.mod']

async function probeRepo(owner, repo, hint) {
  const files = {}
  const wanted = new Set([...TIER1[hint || 'none']])
  const fetchAll = async (list) => {
    for (const p of list) {
      if (files[p] !== undefined) continue
      const r = await fetchFile(owner, repo, p)
      files[p] = r === 'ERR' ? null : r
      if (r === 'ERR') return false // network trouble — stop, retry next run
      if (p === 'requirements.txt' && r === null) files[p] = null
    }
    return true
  }
  const clean = await fetchAll([...wanted])
  if (!clean) return { error: true }

  const pkgText = files['package.json']
  const pyText = files['pyproject.toml']
  const setupText = files['setup.py']
  const reqText = files['requirements.txt']
  const identityFound = pkgText || pyText || setupText

  let dockerText = null
  if (!identityFound || hint === null) {
    const more = TIER2.filter((p) => files[p] === undefined)
    const clean2 = await fetchAll(more)
    if (!clean2) return { error: true }
    dockerText = files['Dockerfile']
  }

  const pkg = pkgText ? parsePackageJson(pkgText) : null
  const py = pyText ? parsePyproject(pyText) : null
  const setupPy = setupText ? parseSetupPy(setupText) : null
  const docker = dockerText ? decompileDockerfile(dockerText) : null
  let unsupported = null
  if (!pkg && !py && !setupPy && !docker) {
    if (files['Cargo.toml']) unsupported = 'rust'
    else if (files['go.mod']) unsupported = 'go'
  }
  return {
    error: false,
    pkg,
    py,
    setupPy,
    req: !!(reqText && reqText.trim()),
    docker,
    unsupported,
    files: {
      packageJson: pkg ? { name: pkg.name, bin: pkg.hasBin, ws: pkg.workspaces } : null,
      pyproject: py ? { name: py.projectName, scripts: py.scripts } : null,
      setupPy: setupPy || null,
      requirements: !!(reqText && reqText.trim()),
      dockerfile: docker ? { runtime: docker.runtime, npm: docker.npmPkgs, pip: docker.pipPkgs } : null,
      unsupported,
    },
  }
}

// --- main -------------------------------------------------------------------

function loadCache() {
  try {
    const o = JSON.parse(readFileSync(CACHE, 'utf8'))
    for (const k of Object.keys(o.verified?.npm || {})) verified.npm[k] = o.verified.npm[k]
    for (const k of Object.keys(o.verified?.pypi || {})) verified.pypi[k] = o.verified.pypi[k]
    return o.entries || {}
  } catch {
    return {}
  }
}

function shuffle(arr, seed) {
  let s = seed >>> 0
  const rnd = () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296)
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(rnd() * (i + 1))
    ;[arr[i], arr[j]] = [arr[j], arr[i]]
  }
  return arr
}

const [node, script, ...argv] = process.argv
const opt = parseArgs(argv)

const catalog = JSON.parse(readFileSync(opt.input ? resolve(opt.input) : CATALOG, 'utf8'))
const entriesCache = loadCache()

// Target set: pseudo + none classes, non-curated, github urls, dedupe by owner/repo.
const targets = []
const seenKeys = new Set()
for (const e of catalog.plugins) {
  if (!e || !e.command && e.transport) continue
  const cls = classify(e)
  if (!e.url || (e.category || []).includes('Curated')) continue
  if (opt.class && opt.class !== 'all' && cls !== opt.class) continue
  if (cls === 'real') continue // already installable — no probing needed
  const pr = ownerRepoOf(e.url)
  if (!pr) continue
  const key = `${pr.owner}/${pr.repo}`
  if (seenKeys.has(key)) continue
  seenKeys.add(key)
  targets.push({ ...pr, cls, hint: hintOf(e) })
}
const onlyKeys = opt.only.map((k) => k.toLowerCase())
const filtered = onlyKeys.length
  ? targets.filter((t) => onlyKeys.includes(`${t.owner}/${t.repo}`.toLowerCase()))
  : targets
const pool = opt.seed ? shuffle(filtered, opt.seed) : filtered
const toProcess = (opt.limit ? pool.slice(0, opt.limit) : pool).filter(
  (t) => opt.fresh || !entriesCache[`${t.owner}/${t.repo}`]?.status || entriesCache[`${t.owner}/${t.repo}`].status === 'error',
)

const stats = { kept: filtered.length - toProcess.length, resolved: 0, unresolved: 0, error: 0 }
const reasons = {}
let done = 0

async function workOne(target) {
  const key = `${target.owner}/${target.repo}`
  const probe = await probeRepo(target.owner, target.repo, target.hint)
  if (probe.error) {
    entriesCache[key] = { status: 'error', cls: target.cls, hint: target.hint, at: new Date().toISOString() }
    stats.error += 1
    return
  }
  const { pkg, py, setupPy, req, docker, unsupported } = probe
  const res = await decide({
    owner: target.owner,
    repo: target.repo,
    hint: target.hint,
    pkg,
    py,
    setupPy,
    req,
    docker,
    unsupported,
    verify: verifyName,
  })
  const status = res.install ? 'resolved' : 'unresolved'
  entriesCache[key] = {
    status,
    cls: target.cls,
    hint: target.hint,
    install: res.install,
    launch: res.launch,
    reason: res.reason,
    files: probe.files,
    at: new Date().toISOString(),
  }
  stats[status] += 1
  if (res.reason) reasons[res.reason] = (reasons[res.reason] || 0) + 1
}

async function run() {
  console.log(
    `targets=${filtered.length} toProcess=${toProcess.length} (${toProcess.length ? '' : 'cache hits only'}) class=${opt.class || 'pseudo+none'}`,
  )
  let cursor = 0
  const workers = Array.from({ length: Math.min(opt.concurrency, Math.max(1, toProcess.length)) }, async () => {
    while (cursor < toProcess.length) {
      const t = toProcess[cursor++]
      await workOne(t)
      done += 1
      if (done % 50 === 0) {
        writeCache()
        console.log(`  … ${done}/${toProcess.length} resolved=${stats.resolved} unresolved=${stats.unresolved} error=${stats.error}`)
      }
    }
  })
  await Promise.all(workers)
  writeCache()
  console.log('\n== summary ==')
  console.log(`kept(cache)=${stats.kept}  resolved=${stats.resolved}  unresolved=${stats.unresolved}  error=${stats.error}`)
  const reasonOrder = Object.entries(reasons).sort((a, b) => b[1] - a[1])
  for (const [k, v] of reasonOrder) console.log(`  unresolved: ${String(v).padStart(4)}  ${k}`)
  // spot-check
  const resolved = Object.entries(entriesCache).filter(([, v]) => v.status === 'resolved')
  console.log(`\nresolved sample (${Math.min(5, resolved.length)}):`)
  for (const [k, v] of resolved.slice(0, 5)) {
    console.log(`  ${k}: ${v.install?.method} ${v.install?.package} → ${v.launch?.command} ${(v.launch?.args || []).join(' ')}`)
  }
}

function writeCache() {
  const out = {
    generatedAt: new Date().toISOString(),
    stats,
    verified,
    entries: entriesCache,
  }
  writeFileSync(CACHE, JSON.stringify(out, null, 2))
}

run().catch((err) => {
  console.error(err)
  writeCache()
  process.exit(1)
})
