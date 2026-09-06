// Pure MCP repo → install-manifest resolution (roadmap Phase 1, §8.1/§8.3).
//
// This file has NO network. It parses repository fingerprint files and turns
// them into a canonical InstallManifest by asking an injected `verify(name, kind)`
// whether a package actually exists on a registry. The CLI (github-analyzer.mjs)
// supplies the real async verifier + the raw-file fetcher; unit tests stub both.
//
// InstallManifest shape (mirrors the future Rust `McpInstallManifest`, camelCase):
//   { runtime: "node"|"python",
//     method:  "npm"|"uv",
//     package: <registry name | github:/git+ spec used for prefetch>,
//     launch:  { command: "npx"|"uvx", args: [...] } }

const GITHUB_RE = /github\.com\/([^/\s]+)\/([^/\s#]+)/
const GIT_TOKEN = /github:|git\+https|git@|\.git\b/

/** Extract { owner, repo } from a github.com URL (first two segments). */
export function ownerRepoOf(url) {
  const m = String(url || '').match(GITHUB_RE)
  if (!m) return null
  return { owner: m[1], repo: m[2].replace(/\.git$/, '') }
}

/** Classify a catalog entry: real package command / pseudo (git) command / none. */
export function classify(entry) {
  if (!entry.command) return 'none'
  return GIT_TOKEN.test((entry.args || []).join(' ')) ? 'pseudo' : 'real'
}

/** Runtime hint encoded in an existing command (`npx` → node, `uvx` → python). */
export function hintOf(entry) {
  if (entry.command === 'npx') return 'node'
  if (entry.command === 'uvx') return 'python'
  return null
}

// --- package.json ----------------------------------------------------------

export function parsePackageJson(text) {
  let o
  try {
    o = JSON.parse(String(text))
  } catch {
    return null
  }
  if (!o || typeof o !== 'object') return null
  const name = typeof o.name === 'string' && o.name.trim() ? o.name.trim() : null
  if (!name) return null
  const workspaces = !!(
    o.workspaces &&
    (Array.isArray(o.workspaces) ? o.workspaces.length : Object.keys(o.workspaces).length)
  )
  let binName = null
  if (typeof o.bin === 'string') {
    binName = name // single string bin → command named after the package
  } else if (o.bin && typeof o.bin === 'object' && !Array.isArray(o.bin)) {
    const keys = Object.keys(o.bin)
    if (keys.length === 1) binName = keys[0]
    else if (name && keys.includes(name)) binName = name // multiple bins, name matches
  }
  return {
    name,
    private: !!o.private,
    workspaces,
    binName,
    hasBin: !!binName,
  }
}

// --- setup.py (lightweight) ------------------------------------------------

export function parseSetupPy(text) {
  const m = String(text || '').match(/name\s*=\s*["']([^"']+)["']/)
  return m ? m[1] : null
}

// --- pyproject.toml (subset) ------------------------------------------------

const unquote = (s) => s.trim().replace(/^(['"])(.*)\1$/s, (_q, _a, inner) => inner).trim()

/** Parse just enough TOML for resolver decisions: [project] name + [project.scripts]. */
export function parsePyproject(text) {
  let table = null
  let projectName = null
  const scripts = []
  for (const raw of String(text || '').split('\n')) {
    const line = raw.trim()
    if (!line || line.startsWith('#')) continue
    const tbl = line.match(/^\[([^\]]+)\]\s*$/)
    if (tbl) {
      table = tbl[1].trim()
      continue
    }
    const eq = line.indexOf('=')
    if (eq < 0) continue
    const key = line.slice(0, eq).trim()
    let val = line.slice(eq + 1).trim()
    if (key.startsWith('-')) continue // array element of [project.dependencies] etc.
    if (table === 'project') {
      if (key === 'name') projectName = unquote(val)
    } else if (table === 'project.scripts') {
      const name = unquote(key)
      if (name) scripts.push(name)
    }
  }
  if (!projectName) return null
  return { projectName, scripts }
}

// --- Dockerfile decompiler --------------------------------------------------

function parseDockerJsonOrShell(rest) {
  const s = rest.trim()
  if (s.startsWith('[')) {
    try {
      const arr = JSON.parse(s)
      return Array.isArray(arr) ? arr.map(String) : []
    } catch {
      return []
    }
  }
  // shell form: tokenize on whitespace, honour quotes, drop exec-form quoting shell words
  const out = []
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g
  let m
  while ((m = re.exec(s))) out.push(m[1] ?? m[2] ?? m[3])
  return out
}

// Known verbs that end an install arg list (a new shell command after &&/; etc.).
const STOP_TOKENS = new Set([
  '&&', '||', ';', 'apt-get', 'apk', 'curl', 'wget', 'git', 'node', 'npm', 'npx',
  'pip', 'pip3', 'python', 'python3', 'mkdir', 'chmod', 'chown', 'ln', 'cd', 'cp',
  'mv', 'rm', 'tar', 'go', 'cargo', 'pnpm', 'yarn', 'poetry', 'export', 'echo',
])
// Options whose NEXT token is a value (path/url), not a package.
const VALUE_OPTIONS = new Set([
  '-r', '--requirement', '-c', '--constraint', '-f', '--find-links',
  '--index-url', '--extra-index-url', '--target', '--prefix',
])

/** Parse `RUN <pkgmgr> install …` args into concrete package names (skip flags/-r/ci). */
function installedPkgs(body) {
  const tokens = body.split(/\s+/).map((t) => t.trim()).filter(Boolean)
  const pkgs = []
  let capturing = false
  let skipNext = false
  let count = 0
  for (const raw of tokens) {
    const t = raw.replace(/,$/, '')
    if (!capturing) {
      if (t === 'install' || t === 'add' || t === 'i') capturing = true
      continue
    }
    if (STOP_TOKENS.has(t) || count >= 6) break
    if (skipNext) {
      skipNext = false
      continue
    }
    if (VALUE_OPTIONS.has(t)) {
      skipNext = true
      continue
    }
    if (t.startsWith('-')) continue // -g --global --no-cache-dir --save-dev …
    if (/^[.][.]?(\/|$)/.test(t)) continue // . ./
    if (/\.txt$/.test(t)) continue // requirements.txt
    if (/\.git(\/|$)/.test(t)) continue // git urls
    if (/\//.test(t) && !t.startsWith('@')) continue // paths / scoped-npm lookalikes w/o @
    if (t === 'ci') continue
    pkgs.push(t.replace(/[=<>~^].*$/, ''))
    count += 1
  }
  return pkgs
}

// System-package managers: what they install are OS/browser/toolchain deps
// (chromium, ffmpeg, fonts-*, lib*, build tools) — never the MCP entry. A RUN
// under a node/python image that apt/apk-installs a browser must NOT feed
// npmPkgs/pipPkgs; that is exactly how `mermaid-mcp` got mis-resolved to the
// npm `chromium` package (a system dep picked as the runnable server).
const SYSTEM_MANAGERS = new Set([
  'apt-get', 'apt', 'apk', 'yum', 'dnf', 'zypper', 'pacman', 'brew', 'port', 'pkg', 'emerge',
])
const NPM_MANAGERS = new Set(['npm', 'pnpm', 'yarn'])
const PY_MANAGERS = new Set(['pip', 'poetry', 'uv', 'uvx'])

// Package names that are never a standalone MCP entry: official language SDKs /
// generic frameworks / toolchain binaries. A repo that installs one of these as
// a dependency must not be resolved to *it* as the runnable server (only the
// published server itself, or git-source + install-time AI resolve, is honest).
export const NON_SERVER_DEPS = new Set([
  // OS / toolchain / rendering / automation deps
  'chromium', 'firefox', 'playwright', 'puppeteer', 'ffmpeg', 'graphviz',
  'bash', 'dash', 'sh', 'tesseract-ocr', 'poppler-utils', 'swig', 'pkg-config',
  'pkgconf', 'make', 'cmake', 'gcc', 'g++', 'build-essential', 'nodejs', 'npm',
  'python', 'python3', 'pip', 'uv', 'uvx', 'conda', 'wget', 'curl', 'git',
  // python data / web SDKs and MCP framework layers
  'mcp', 'fastmcp', 'pyserial', 'pyyaml', 'aiosqlite', 'requests', 'httpx',
  'urllib3', 'numpy', 'pandas', 'pillow', 'flask', 'fastapi', 'django',
  'openai', 'anthropic', 'chromadb', 'sentence-transformers',
])

/**
 * Capture only *real language-manager* installs from a RUN body, split into
 * shell chains so an apt line never pollutes (or hides) a later npm/pip line.
 * `installedPkgs` still does the flag/token filtering per segment.
 */
function runInstalledPackages(body, runtime) {
  const out = []
  const chains = String(body || '').split(/\s*(?:&&|\|\||;)\s*/).filter(Boolean)
  for (const chain of chains) {
    const seg = chain.trim().replace(/^sudo\s+/, '')
    let toks = seg.split(/\s+/).filter(Boolean)
    if (!toks.length) continue
    // `python[3] -m pip install …` — step past the interpreter preamble.
    if (/^python[23]?$/.test(toks[0]) && toks[1] === '-m') toks = toks.slice(2)
    const mgr = (toks[0] || '').toLowerCase().replace(/\d+$/, '')
    if (SYSTEM_MANAGERS.has(mgr)) continue
    const installing = toks[1] === 'install' || toks[1] === 'add' || toks[1] === 'i'
    if (!installing) continue
    if (runtime === 'node' && NPM_MANAGERS.has(mgr)) out.push(...installedPkgs(seg))
    else if (runtime === 'python' && (PY_MANAGERS.has(mgr) || /^python[23]?$/.test(mgr))) {
      out.push(...installedPkgs(seg))
    }
  }
  return out
}

export function decompileDockerfile(text) {
  // join `\` continuations
  const assembled = []
  for (const raw of String(text || '').split('\n')) {
    let line = raw.replace(/\r$/, '').trim()
    if (assembled.length && /\\$/.test(assembled[assembled.length - 1])) {
      assembled[assembled.length - 1] =
        assembled[assembled.length - 1].replace(/\\$/, '') + ' ' + line
    } else {
      assembled.push(line)
    }
  }
  let runtime = null
  const npmPkgs = []
  const pipPkgs = []
  let cmd = []
  let entrypoint = []
  for (const line of assembled) {
    if (!line || line.startsWith('#')) continue
    const m = line.match(/^([A-Za-z]+)\s+(.*)$/)
    if (!m) continue
    const ins = m[1].toUpperCase()
    const rest = m[2].trim()
    if (ins === 'FROM') {
      const img = rest
        .replace(/\s+as\s+\w+$/i, '')
        .trim()
        .split(/\s+/)[0]
      if (/^(node|node:)/i.test(img)) runtime = 'node'
      else if (/^(python|python:|conda)/i.test(img)) runtime = 'python'
    } else if (ins === 'RUN') {
      for (const pkg of runInstalledPackages(rest, runtime)) {
        if (runtime === 'node') npmPkgs.push(pkg)
        else if (runtime === 'python') pipPkgs.push(pkg)
      }
    } else if (ins === 'CMD') {
      cmd = parseDockerJsonOrShell(rest)
    } else if (ins === 'ENTRYPOINT') {
      entrypoint = parseDockerJsonOrShell(rest)
    }
  }
  return {
    runtime,
    npmPkgs: [...new Set(npmPkgs)],
    pipPkgs: [...new Set(pipPkgs)],
    cmd,
    entrypoint,
  }
}

// --- decision ---------------------------------------------------------------

function nodeManifest(packageName, launchArgs) {
  return { runtime: 'node', method: 'npm', package: packageName, launch: { command: 'npx', args: launchArgs } }
}
function pyManifest(packageName, launchArgs) {
  return { runtime: 'python', method: 'uv', package: packageName, launch: { command: 'uvx', args: launchArgs } }
}

const done = (install, launch) => ({ install, launch, reason: null })
const fail = (reason) => ({ install: null, launch: null, reason })

/**
 * Decide an InstallManifest for one repo.
 * opts: { owner, repo, hint, pkg, py, setupPy, req, docker, unsupported, verify }
 *   verify(name, 'npm'|'pypi') -> Promise<boolean> (registry existence check).
 * Returns { install, launch, reason } — launch may equal install.launch and is the
 * {command,args} pair that should be recorded/overwritten on the catalog entry.
 */
export async function decide(opts) {
  const { owner, repo, hint, pkg, py, setupPy, req, docker, unsupported, verify } = opts
  const gitRepo = `github:${owner}/${repo}`
  const gitUrl = `git+https://github.com/${owner}/${repo}`

  // node path: package.json present (or a node hint with docker decompile)
  const tryNode = async (preferName) => {
    // A repo whose *own* package.json name collides with an SDK/framework on the
    // registry (e.g. name "mcp" / "fastmcp") is not its own published server —
    // resolving to the colliding package would install the SDK, not the repo.
    const selfName = pkg && !pkg.private && pkg.name && !NON_SERVER_DEPS.has(pkg.name) ? pkg.name : null
    const candidate = preferName || selfName
    if (candidate) {
      const published = pkg?.private ? false : await verify(candidate, 'npm')
      if (published) {
        const im = nodeManifest(candidate, ['-y', candidate])
        return done(im, im.launch)
      }
    }
    if (pkg && pkg.binName && !pkg.workspaces) {
      const im = nodeManifest(gitRepo, ['-y', gitRepo])
      return done(im, im.launch) // confirmed runnable via npx github:…
    }
    return null
  }

  // python path: pyproject/setup.py present (or python hint w/ docker decompile)
  const tryPy = async (preferName) => {
    const pyName =
      py?.projectName && !NON_SERVER_DEPS.has(py.projectName) ? py.projectName : setupPy
    if (preferName || pyName) {
      const candidate = preferName || pyName
      const published = await verify(candidate, 'pypi')
      if (published) {
        const im = pyManifest(candidate, [candidate])
        return done(im, im.launch)
      }
    }
    if (py && py.scripts.length) {
      const im = pyManifest(gitUrl, ['--from', gitUrl, py.scripts[0]])
      return done(im, im.launch)
    }
    return null
  }

  if (hint === 'node') {
    const r = await tryNode()
    if (r) return r
    if (docker && docker.runtime === 'node') {
      for (const p of docker.npmPkgs) {
        if (NON_SERVER_DEPS.has(p)) continue
        if (await verify(p, 'npm')) {
          const im = nodeManifest(p, ['-y', p])
          return done(im, im.launch)
        }
      }
    }
    if (py || setupPy) return (await tryPy()) || fail('node-hint-python-no-publish')
    return fail(pkg ? (pkg.workspaces ? 'node-workspace-unpublished' : 'node-no-bin-unpublished') : 'no-node-fingerprint')
  }

  if (hint === 'python') {
    const r = await tryPy()
    if (r) return r
    if (docker && docker.runtime === 'python') {
      for (const p of docker.pipPkgs) {
        if (NON_SERVER_DEPS.has(p)) continue
        if (await verify(p, 'pypi')) {
          const im = pyManifest(p, [p])
          return done(im, im.launch)
        }
      }
    }
    if (pkg) return (await tryNode()) || fail('python-hint-node-no-publish')
    return fail(req ? 'py-requirements-only-no-entry' : py ? 'py-no-publish-no-scripts' : 'no-python-fingerprint')
  }

  // no hint (③ none-class)
  if (pkg) return (await tryNode()) || fail(pkg.workspaces ? 'node-workspace-unpublished' : 'node-no-bin-unpublished')
  if (py || setupPy) return (await tryPy()) || fail('py-no-publish-no-scripts')
  if (docker && docker.runtime === 'node') {
    for (const p of docker.npmPkgs) {
      if (await verify(p, 'npm')) {
        const im = nodeManifest(p, ['-y', p])
        return done(im, im.launch)
      }
    }
    return fail('docker-node-source-no-package')
  }
  if (docker && docker.runtime === 'python') {
    for (const p of docker.pipPkgs) {
      if (await verify(p, 'pypi')) {
        const im = pyManifest(p, [p])
        return done(im, im.launch)
      }
    }
    return fail('docker-python-source-no-package')
  }
  if (unsupported) return fail(`unsupported:${unsupported}`)
  return fail('no-fingerprint')
}
