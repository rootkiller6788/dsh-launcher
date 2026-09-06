// Shared stamping logic for the MCP catalog (roadmap §8.1 → §8.2).
//
// One source of truth for "resolver result → catalog mcpInstall", used by BOTH:
//   - gen-content-catalog.mjs (so a full catalog rebuild re-stamps from the cache)
//   - resolver/stamp-catalog.mjs  (CLI: apply the cache to the committed file)
//
// The batch (`github-analyzer.mjs`) writes scripts/data/mcp-resolved.json keyed by
// `owner/repo`. When it is absent, stamping is a no-op — the catalog stays at its
// base form (already-installable entries are still usable).

import { classify, NON_SERVER_DEPS } from './analyze-repo.mjs'

const GIT_TOKEN = /github:|git\+https|git@|\.git\b/

const keyOf = (e) => (e.owner && e.name ? `${e.owner}/${e.name}` : e.name)

/// Canonical `npx -y <pkg>` plan for an already-installable registry command.
function nodePlan(e, pkg) {
  return { runtime: 'node', method: 'npm', package: pkg, launch: { command: 'npx', args: e.args } }
}

/// Canonical `uvx <pkg>` plan. `uvx --from <dep> <entry>` warms the *--from* dep
/// (e.g. `--from "liquid-api[mcp]" liquid-mcp`), not the trailing entrypoint name.
function uvPlan(e, pkg) {
  const fromDep = e.args[0] === '--from' && e.args[1] && !GIT_TOKEN.test(e.args[1]) ? e.args[1] : pkg
  return { runtime: 'python', method: 'uv', package: fromDep, launch: { command: 'uvx', args: e.args } }
}

/// The runnable registry package for a launch command: the token after `-y`
/// (npx) / the `--from` dep handled by uvPlan, or the first positional —
/// *never* the trailing subcommand: `npx -y motionlint mcp` serves the
/// `motionlint` package with an `mcp` subcommand, so mirroring package
/// "mcp" (the SDK) is wrong.
function registryPackage(args) {
  for (const a of args) {
    if (a === '-y' || a === '--yes' || a === '--from' || a.startsWith('-')) continue
    if (GIT_TOKEN.test(a)) continue
    if (/^(@[^/]+\/)?[A-Za-z0-9._~-]+$/.test(a)) return a
  }
  return null
}

/// A mirror `mcpInstall` for a real registry launch already present in the catalog
/// (`npx -y <pkg>` / `uvx <pkg>`) — no probing was needed. Null when not applicable.
export function mirrorInstall(e) {
  if (!e.command || !Array.isArray(e.args) || !e.args.length) return null
  if (e.transport && e.transport !== 'stdio') return null // streamable-http
  const joined = e.args.join(' ')
  if (GIT_TOKEN.test(joined)) return null // pseudo command — never mirrored
  const pkg = registryPackage(e.args)
  if (!pkg) return null // registry-ish name only
  if (NON_SERVER_DEPS.has(pkg)) return null // SDK/framework/system dep — not the server
  if (e.command === 'npx') return nodePlan(e, pkg)
  if (e.command === 'uvx') return uvPlan(e, pkg)
  return null
}

/** Default `serverName` derivation, kept identical to catalog build-time (owner-name). */
function defaultServerName(e) {
  return `${e.owner}-${e.name}`
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 32) || 'server'
}

/// Mutate `plugins` (mcp entries) in place from the resolved cache.
/// Returns { fromResolved, mirrored } counts.
export function stampPlugins(plugins, resolvedEntries = {}) {
  let fromResolved = 0
  let mirrored = 0
  for (const e of plugins) {
    if (e.kind !== 'mcp') continue
    const res = resolvedEntries[keyOf(e)]

    if (res && res.status === 'resolved' && res.install && res.launch && res.launch.command) {
      // A "resolved" row whose chosen package is an SDK/framework/system dep is a
      // mis-resolution — the guard must apply to the resolved path exactly as it
      // does to the mirror path, or fastmcp-class rows bypass it via resolved-wins.
      if (res.install.package && NON_SERVER_DEPS.has(res.install.package)) {
        // not stamped: base (source-run) form preserved → install routes to
        // git-source / install-time AI resolve
        continue
      }
      // Resolver canonical launch wins over any pseudo command.
      e.command = res.launch.command
      e.args = res.launch.args
      e.transport = 'stdio'
      e.mcpInstall = res.install
      if (!e.serverName) e.serverName = defaultServerName(e)
      fromResolved += 1
      continue
    }

    if (classify(e) === 'real' && e.command) {
      const mirror = mirrorInstall(e)
      if (mirror) {
        e.mcpInstall = mirror
        mirrored += 1
      }
    }
  }
  return { fromResolved, mirrored }
}

export function mcpCoverage(plugins) {
  const total = plugins.filter((e) => e.kind === 'mcp').length
  const withPlan = plugins.filter((e) => e.kind === 'mcp' && e.mcpInstall).length
  return { total, withPlan }
}
