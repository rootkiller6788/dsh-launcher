// The app-server `--stdio` rule, kept out of gen-content-catalog.mjs so it can be
// tested without regenerating every catalog (that module generates on import).
//
// The MCP "App Server" packages boot a streamable-HTTP listener on :3001 unless
// told to speak stdio, so an entry for one of them whose args lack `--stdio`
// looks installable, launches, and then never answers a single frame. That flag
// used to be written into each override by hand (wiki-explorer, system-monitor),
// which meant the next app server added would silently miss it — and the failure
// only surfaces as a probe timeout on a user's machine, not in CI.

export const STDIO_APP_SERVERS = new Set([
  '@modelcontextprotocol/server-wiki-explorer',
  '@modelcontextprotocol/server-system-monitor',
])

// Prose that gives away an app server even when its package is not listed above:
// both known ones describe themselves as an "MCP App Server".
const APP_SERVER_PROSE = 'app server'

/// Append `--stdio` when these args launch a known app server and do not already
/// carry the flag. Idempotent: a hand-written flag is left exactly as it is, and
/// any non-array (an entry with no args at all) is passed straight through.
export function withStdioFlag(args) {
  if (!Array.isArray(args) || args.includes('--stdio')) return args
  if (!args.some((a) => STDIO_APP_SERVERS.has(a))) return args
  return [...args, '--stdio']
}

/// The entry name when its prose calls it an App Server but neither its args nor
/// its resolver-stamped launch args carry the flag — the next one to be added,
/// caught at generation time instead of at probe time. `null` when there is no
/// gap.
export function stdioFlagGap(mcp) {
  const text = `${mcp.description?.en ?? ''} ${mcp.description?.zh ?? ''}`.toLowerCase()
  if (!text.includes(APP_SERVER_PROSE)) return null
  const flagged =
    withStdioFlag(mcp.args)?.includes('--stdio') ||
    withStdioFlag(mcp.mcpInstall?.launch?.args)?.includes('--stdio')
  return flagged ? null : mcp.name
}
