// Unit tests for the catalog mirror/stamp logic (no network).
//   node --test scripts/resolver/stamp.test.mjs

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mirrorInstall, stampPlugins, mcpCoverage } from './stamp.mjs'

test('mirrorInstall: npx -y <pkg> mcp mirrors <pkg>, never the trailing subcommand', () => {
  // Regression: base bulk form `npx -y motionlint mcp` used to mirror the last
  // arg "mcp" (the official SDK) — poisoned rows shipped to users.
  const m = mirrorInstall({ command: 'npx', args: ['-y', 'motionlint', 'mcp'] })
  assert.equal(m.package, 'motionlint')
  assert.deepEqual(m.launch, { command: 'npx', args: ['-y', 'motionlint', 'mcp'] })
})

test('mirrorInstall: scoped registry package', () => {
  const m = mirrorInstall({ command: 'npx', args: ['-y', '@narasimhaponnada/mermaid-mcp-server'] })
  assert.equal(m.package, '@narasimhaponnada/mermaid-mcp-server')
})

test('mirrorInstall: git tokens are never mirrored (pseudo)', () => {
  assert.equal(mirrorInstall({ command: 'npx', args: ['-y', 'github:o/r'] }), null)
  assert.equal(mirrorInstall({ command: 'uvx', args: ['git+https://github.com/o/r'] }), null)
})

test('mirrorInstall: SDK/framework/system deps are never mirrored as a server', () => {
  assert.equal(mirrorInstall({ command: 'npx', args: ['-y', 'chromium'] }), null)
  assert.equal(mirrorInstall({ command: 'npx', args: ['-y', 'fastmcp'] }), null)
  assert.equal(mirrorInstall({ command: 'uvx', args: ['fastmcp'] }), null)
})

test('mirrorInstall: uvx --from dep warms the --from dep, not the entrypoint', () => {
  const m = mirrorInstall({ command: 'uvx', args: ['--from', 'liquid-api[mcp]', 'liquid-mcp'] })
  assert.equal(m.package, 'liquid-api[mcp]')
})

test('mirrorInstall: plain uvx name', () => {
  const m = mirrorInstall({ command: 'uvx', args: ['mcp-server-git'] })
  assert.equal(m.package, 'mcp-server-git')
})

test('mirrorInstall: streamable-http and empty are not mirrored', () => {
  assert.equal(mirrorInstall({ command: 'npx', args: ['-y', '@x/y'], transport: 'streamable-http' }), null)
  assert.equal(mirrorInstall({ command: 'npx', args: [] }), null)
  assert.equal(mirrorInstall({ command: null, args: null }), null)
})

test('stampPlugins: resolved row naming a framework/SDK pkg is not stamped (guard on resolved path)', () => {
  // Regression: jlowin/fastmcp & punkpeye/fastmcp were kept resolved in the cache
  // and bypassed the NON_SERVER_DEPS guard via resolved-wins — shipping `uvx
  // fastmcp` / `npx -y fastmcp` as a server plan. Same rule as the mirror path.
  const plugins = [
    { kind: 'mcp', owner: 'jlowin', name: 'fastmcp', command: 'uvx', args: ['fastmcp'] },
    { kind: 'mcp', owner: 'punkpeye', name: 'fastmcp', command: 'npx', args: ['-y', 'fastmcp'] },
  ]
  const resolved = {
    'jlowin/fastmcp': {
      status: 'resolved',
      install: { runtime: 'python', method: 'uv', package: 'fastmcp', launch: { command: 'uvx', args: ['fastmcp'] } },
      launch: { command: 'uvx', args: ['fastmcp'] },
    },
  }
  const { fromResolved } = stampPlugins(plugins, resolved)
  assert.equal(fromResolved, 0)
  assert.equal(plugins[0].mcpInstall, undefined) // not stamped → base form, no bogus plan
  assert.equal(plugins[0].command, 'uvx')
})

test('stampPlugins: resolved canonical overwrite beats mirror', () => {
  const plugins = [
    { kind: 'mcp', owner: 'o', name: 'r', command: 'npx', args: ['-y', 'some-old'] },
    { kind: 'mcp', owner: 'o', name: 's', command: 'npx', args: ['-y', 'realpkg', 'mcp'] },
    { kind: 'mcp', owner: 'o', name: 't', command: 'npx', args: ['-y', 'github:o/t'] },
    { kind: 'skin', owner: 'o', name: 'u' },
  ]
  const resolved = {
    'o/r': {
      status: 'resolved',
      install: { runtime: 'node', method: 'npm', package: 'canon', launch: { command: 'npx', args: ['-y', 'canon'] } },
      launch: { command: 'npx', args: ['-y', 'canon'] },
    },
  }
  const { fromResolved, mirrored } = stampPlugins(plugins, resolved)
  assert.equal(fromResolved, 1)
  assert.equal(mirrored, 1)
  assert.equal(plugins[0].mcpInstall.package, 'canon') // resolved wins
  assert.equal(plugins[1].mcpInstall.package, 'realpkg') // mirror skips trailing "mcp"
  assert.equal(plugins[2].mcpInstall, undefined) // pseudo: never mirrored
  assert.equal(plugins[3].mcpInstall, undefined)
})

test('mcpCoverage counts mcp entries with a plan', () => {
  const plugins = [
    { kind: 'mcp', owner: 'o', name: 'a', mcpInstall: {} },
    { kind: 'mcp', owner: 'o', name: 'b' },
    { kind: 'skin' },
  ]
  assert.deepEqual(mcpCoverage(plugins), { total: 2, withPlan: 1 })
})
