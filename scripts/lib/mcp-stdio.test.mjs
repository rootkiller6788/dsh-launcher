// node --test scripts/lib/mcp-stdio.test.mjs
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

import { STDIO_APP_SERVERS, stdioFlagGap, withStdioFlag } from './mcp-stdio.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const catalogPath = resolve(__dirname, '../../crates/launcher-core/data/content-mcps.json')

const appServer = (name) => ({ name, description: { en: 'Foo MCP App Server' } })

test('withStdioFlag appends the flag to a known app server', () => {
  assert.deepEqual(
    withStdioFlag(['-y', '@modelcontextprotocol/server-wiki-explorer']),
    ['-y', '@modelcontextprotocol/server-wiki-explorer', '--stdio'],
  )
  assert.deepEqual(
    withStdioFlag(['-y', '@modelcontextprotocol/server-system-monitor']),
    ['-y', '@modelcontextprotocol/server-system-monitor', '--stdio'],
  )
})

test('withStdioFlag is idempotent and leaves everything else alone', () => {
  // A hand-written flag survives byte-for-byte — no doubled `--stdio`.
  assert.deepEqual(
    withStdioFlag(['-y', '@modelcontextprotocol/server-wiki-explorer', '--stdio']),
    ['-y', '@modelcontextprotocol/server-wiki-explorer', '--stdio'],
  )
  // A normal stdio server gains nothing: the flag would be an unknown argument.
  assert.deepEqual(withStdioFlag(['-y', '@modelcontextprotocol/server-github']), [
    '-y',
    '@modelcontextprotocol/server-github',
  ])
  // An entry with no args (a remote/streamable entry) is passed through.
  assert.equal(withStdioFlag(null), null)
  assert.equal(withStdioFlag(undefined), undefined)
  assert.deepEqual(withStdioFlag([]), [])
})

test('stdioFlagGap reports an app server the list does not know', () => {
  // The point of the guard: a future app server described as one but missing
  // from STDIO_APP_SERVERS is named at generation time, not discovered by a
  // user whose probe times out.
  assert.equal(stdioFlagGap(appServer('brand-new-app-server')), 'brand-new-app-server')
  // A listed one is covered by the rule, so it is not a gap.
  assert.equal(
    stdioFlagGap({
      ...appServer('server-wiki-explorer'),
      args: ['-y', '@modelcontextprotocol/server-wiki-explorer'],
    }),
    null,
  )
  // The flag counts if only the resolver-stamped launch args carry it.
  assert.equal(
    stdioFlagGap({
      ...appServer('server-system-monitor'),
      mcpInstall: { launch: { args: ['-y', '@modelcontextprotocol/server-system-monitor'] } },
    }),
    null,
  )
  // Not an app server at all — a missing flag is normal, not a gap.
  assert.equal(
    stdioFlagGap({ name: 'github', description: { en: 'GitHub MCP server' } }),
    null,
  )
  // Nothing to read: no prose, no verdict.
  assert.equal(stdioFlagGap({ name: 'x' }), null)
})

test('every app-server-like entry in the shipped catalog carries the flag', () => {
  // Guards the catalog itself: if the rule is ever dropped from the generator,
  // the next regeneration puts a flagless app server back in the bundle and this
  // fails rather than a probe timing out on a real install.
  const catalog = JSON.parse(readFileSync(catalogPath, 'utf8'))
  const gaps = catalog.plugins.map(stdioFlagGap).filter(Boolean)
  assert.deepEqual(gaps, [], `app servers with no --stdio: ${gaps.join(', ')}`)

  // And the two the rule exists for really are in there, flagged, twice over
  // (base args + stamped launch args) — a catalog that quietly lost them would
  // otherwise still pass the check above.
  for (const pkg of STDIO_APP_SERVERS) {
    const entry = catalog.plugins.find((p) => p.args?.includes(pkg))
    assert.ok(entry, `${pkg} missing from the catalog`)
    assert.ok(entry.args.includes('--stdio'), `${entry.name}: base args carry --stdio`)
    assert.ok(
      entry.mcpInstall?.launch?.args?.includes('--stdio'),
      `${entry.name}: stamped launch args carry --stdio`,
    )
  }
})
