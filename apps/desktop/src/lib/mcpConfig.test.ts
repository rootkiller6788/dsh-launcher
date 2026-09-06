import { describe, expect, it } from 'vitest'
import { runtimeNeedsConfig, runtimeNoTools } from './mcpConfig'
import type { McpRuntimeState } from './types'

function degraded(error: string): McpRuntimeState {
  return {
    state: 'degraded',
    transport: 'stdio',
    failCount: 1,
    tools: [],
    error,
  }
}

describe('runtimeNeedsConfig', () => {
  it('flags config-shaped degraded verdicts from real servers', () => {
    expect(
      runtimeNeedsConfig(
        degraded(
          'server answered initialize but self-reports a degraded/misconfigured state. Cause: KANBOARD_URL is required but was not set',
        ),
      ),
    ).toBe(true)
    expect(
      runtimeNeedsConfig(
        degraded('started stdio transport in DEGRADED mode — tools are listable but every call will fail until credentials are fixed'),
      ),
    ).toBe(true)
    expect(runtimeNeedsConfig(degraded('Missing Authorization header'))).toBe(true)
    expect(runtimeNeedsConfig(degraded('invalid api key — check your FIRECRAWL_API_KEY'))).toBe(true)
  })

  it('never flags ok/error verdicts or absent runtime', () => {
    expect(runtimeNeedsConfig(null)).toBe(false)
    expect(runtimeNeedsConfig(undefined)).toBe(false)
    const ok: McpRuntimeState = { state: 'ok', transport: 'stdio', failCount: 0, tools: [] }
    expect(runtimeNeedsConfig(ok)).toBe(false)
    const hard: McpRuntimeState = {
      state: 'error',
      transport: 'stdio',
      failCount: 2,
      tools: [],
      error: 'spawn failed: npx not found',
    }
    expect(runtimeNeedsConfig(hard)).toBe(false)
  })

  it('does not over-flag non-config degraded noise', () => {
    expect(
      runtimeNeedsConfig(degraded('initialize timed out after 15s — server never answered')),
    ).toBe(false)
    expect(
      runtimeNeedsConfig(degraded('unsupported protocol version 2024-11-05')),
    ).toBe(false)
  })
})

describe('runtimeNoTools', () => {
  it('flags ok-but-empty tool servers (firecrawl-no-key shape)', () => {
    expect(
      runtimeNoTools({ state: 'ok', transport: 'stdio', failCount: 0, tools: [] }),
    ).toBe(true)
  })
  it('never flags servers that actually list tools, or non-ok states', () => {
    const healthy: McpRuntimeState = { state: 'ok', transport: 'stdio', failCount: 0, tools: [{ name: 'x' } as never] }
    expect(runtimeNoTools(healthy)).toBe(false)
    expect(runtimeNoTools(degraded('KANBOARD_URL is required but was not set'))).toBe(false)
    expect(runtimeNoTools(null)).toBe(false)
    expect(runtimeNoTools(undefined)).toBe(false)
  })
})
