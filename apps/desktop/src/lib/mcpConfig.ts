import type { McpRuntimeState } from './types'

// Text signals a probe-degraded server uses to report that it is missing or
// can't reach a required credential/URL — "this needs configuring", regardless
// of whether the catalog declared the keys. Covers kanboard's
// "KANBOARD_URL is required but was not set", github/other auth-gated servers'
// "missing/invalid credentials", etc. Kept deliberately conservative: a degraded
// line that says none of these (e.g. a protocol version mismatch) is not
// presented as a configuration gap.
const CONFIG_SIGNALS =
  /is required|required but was not set|without valid credentials|missing (?:authorization|api|auth|token|secret|api[ _-]?key)|invalid (?:api key|token|credentials?)|(?:not set|not configured)|authentication required|\bcredentials?\b/i

/** Whether a probe-degraded server's verdict reads as a config gap the user can
 * close by filling credentials/URLs (the row-level "需配置" for undeclared
 * servers). Never true for `ok`/`error` verdicts or a healthy-but-listed state. */
export function runtimeNeedsConfig(runtime: McpRuntimeState | null | undefined): boolean {
  if (!runtime || runtime.state !== 'degraded') return false
  return CONFIG_SIGNALS.test(runtime.error ?? '')
}

/** Whether a tool server initialized "ok" but listed ZERO tools. Many
 * credential-gated servers (firecrawl: needs FIRECRAWL_API_KEY; gitlab/atlassian
 * style key-gated servers) initialize cleanly — they do NOT self-report a
 * degraded state — but expose no tools until a key/URL is present. A tool server
 * that lists nothing after a successful initialize is not "healthy"; surfacing
 * it as a likely-config-gap is a universal detector that needs no catalog
 * declaration. Conservative: never fires on degraded/error verdicts or when the
 * runtime is absent, and does not fire when the probe never listed tools yet
 * (no tools array means "not probed", not "empty"). */
export function runtimeNoTools(runtime: McpRuntimeState | null | undefined): boolean {
  if (!runtime || runtime.state !== 'ok') return false
  return Array.isArray(runtime.tools) && runtime.tools.length === 0
}
