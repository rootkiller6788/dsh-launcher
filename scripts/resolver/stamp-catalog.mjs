// Stamp resolver results into the bundled MCP catalog (roadmap §8.1 → §8.2).
//
// Reads scripts/data/mcp-resolved.json (github-analyzer.mjs) and rewrites
// crates/launcher-core/data/content-mcps.json in place. Idempotent — safe to
// re-run as the batch grows, and run once more after it finishes:
//   node scripts/resolver/stamp-catalog.mjs

import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { mcpCoverage, stampPlugins } from './stamp.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const CATALOG = resolve(__dirname, '../..', 'crates/launcher-core/data/content-mcps.json')
const RESOLVED = resolve(__dirname, '..', 'data/mcp-resolved.json')

const catalog = JSON.parse(readFileSync(CATALOG, 'utf8'))
let resolved = {}
try {
  resolved = JSON.parse(readFileSync(RESOLVED, 'utf8')).entries || {}
} catch {
  console.warn('no mcp-resolved.json yet — run github-analyzer.mjs first')
}

const { fromResolved, mirrored } = stampPlugins(catalog.plugins, resolved)
const { total, withPlan } = mcpCoverage(catalog.plugins)
catalog.count = total
catalog.updated = new Date().toISOString().slice(0, 10)
writeFileSync(CATALOG, JSON.stringify(catalog, null, 2) + '\n')

console.log(`content-mcps.json: ${total} MCP entries`)
console.log(`  from resolver: ${fromResolved}   mirrored real: ${mirrored}`)
console.log(`  now carry mcpInstall: ${withPlan}/${total} (${((withPlan / total) * 100).toFixed(1)}%)`)
