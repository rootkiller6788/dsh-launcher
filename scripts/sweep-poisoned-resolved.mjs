// One-shot maintenance: purge docker-fallback / SDK-collision mis-resolutions
// from scripts/data/mcp-resolved.json (discovered 2026-09-06 — see roadmap §13).
//
// A registry-backed MCP entry must point at the *server's own published package*.
// The resolver's docker fallback used to pick any RUN-installed dep that verifies
// on npm/pypi — so an apt-installed browser (`chromium`), a system tool
// (`ffmpeg`, `graphviz`, `bash`, `pkg-config`) or an SDK/framework layer
// (`mcp`, `fastmcp`, `requests`, `aiosqlite`) could become the "package", and
// `npx -y chromium` shipped as the install. analyze-repo.mjs now guards this, but
// already-poisoned rows persist in the cache.
//
// Policy: rows whose chosen package is demonstrably NOT the server are flipped to
// `unresolved` (base source-run form preserved → install routes to git-source /
// install-time AI resolve). Rows that may legitimately be their own package
// (framework repos, plausible real names) are left untouched for later per-repo
// verification. `Narasimhaponnada/mermaid-mcp` is corrected to its real package.
//
//   node scripts/sweep-poisoned-resolved.mjs   # then: node scripts/gen-content-catalog.mjs

import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const PATH = resolve(__dirname, 'data/mcp-resolved.json')
const CORRECTION = {
  'Narasimhaponnada/mermaid-mcp': {
    // Real published server: monorepo `server/` subdir package (bin mermaid-mcp).
    install: {
      runtime: 'node',
      method: 'npm',
      package: '@narasimhaponnada/mermaid-mcp-server',
      launch: { command: 'npx', args: ['-y', '@narasimhaponnada/mermaid-mcp-server'] },
    },
    launch: { command: 'npx', args: ['-y', '@narasimhaponnada/mermaid-mcp-server'] },
    reason:
      'corrected 2026-09-06: docker apt dep mis-resolution (chromium); real pkg ' +
      '@narasimhaponnada/mermaid-mcp-server (server/ subdir, bin mermaid-mcp)',
  },
}

// Every shipped row found poisoned by scanning content-mcps.json (pkg in the
// non-server dep set) — the flip target set, minus keys kept for verification.
const SHIPPED_POISONED = [
  'Octodamus/octodamus-core', 'bobaba99/motionlint', 'Narasimhaponnada/mermaid-mcp',
  'segentic-lab/periscope-mcp', 'LvcidPsyche/auto-browser', 'bright8192/esxi-mcp-server',
  'x7even/cloudcostsmcp', 'anypost/emailmd', 'grzgrzgrz3/pingwa-client',
  'mohitbadwal/ringback', 'croc100/Litescope', 'scarletkc/vexor', 'raccioly/docguard',
  'saiffmirza/kiyas', 'graphpilot-oss/graphpilot', 'JannLeo/telinksdk-builder-mcp',
  'alejooroncoy/campus-cli', 'ebbfijsf/agent-reader', 'microsoft/markitdown',
  'bakyang2/kr-crypto-intelligence', 'abcxz/conviction-fm', 'giskard09/argentum-core',
  'yolfinance/yolfi-agent', 'bevanding/signaldaemon', 'lumayapartners/memini',
  'aidesignblueprint/integrations', 'ZengLiangYi/ChatCrystal', 'giuliohome-org/doc-manager',
  'giskard09/giskard-oasis', 'Schubeler-Consulting/knowmind', 'whynowlab/jarvis-orb',
  'SVerITG/Metis', 'ohad6k/emulo', 'hermoso-ai/hermoso', 'giskard09/giskard-search',
  'eliottreich/taskbounty-check', 'securityfortech/secops-mcp', 'BelleKou/mcp-viral-transformer',
  'taisly/agent', 'johnanleitner1-Coder/lastminutedeals-api', '2niuhe/plantuml_web',
  'jlowin/fastmcp', 'punkpeye/fastmcp',
]
// Repos that ARE the package/framework themselves or carry a plausible real name —
// verify individually later rather than flipping on a guess.
const KEEP_RESOLVED = new Set(['jlowin/fastmcp', 'punkpeye/fastmcp', 'abcxz/conviction-fm', 'x7even/cloudcostsmcp'])

const j = JSON.parse(readFileSync(PATH, 'utf8'))
const entries = j.entries
const flipped = []
const corrected = []
const skipped = []
const missing = []
for (const key of SHIPPED_POISONED) {
  const e = entries[key]
  if (!e || e.status !== 'resolved') {
    missing.push(key)
    continue
  }
  const oldPkg = e.install?.package
  if (CORRECTION[key]) {
    Object.assign(e, { status: 'resolved', install: CORRECTION[key].install, launch: CORRECTION[key].launch, reason: CORRECTION[key].reason })
    corrected.push(`${key}: ${oldPkg} → ${CORRECTION[key].install.package}`)
    continue
  }
  if (KEEP_RESOLVED.has(key)) {
    skipped.push(`${key}: ${oldPkg} (left for verification)`)
    continue
  }
  const why = `mis-resolution 2026-09-06: '${oldPkg}' was picked by the docker/verify fallback but is a ` +
    `system dep or SDK layer, not the server — analyze-repo NON_SERVER_DEPS guard now refuses it; ` +
    `install routes to git-source/install-time AI resolve`
  Object.assign(e, { status: 'unresolved', install: null, launch: null, reason: why })
  flipped.push(`${key}: '${oldPkg}'`)
}

writeFileSync(PATH, JSON.stringify(j, null, 2) + '\n')
console.log(`mcp-resolved.json: ${flipped.length} flipped to unresolved, ${corrected.length} corrected, ${skipped.length} kept, ${missing.length} missing`)
for (const c of corrected) console.log('  CORRECTED ', c)
for (const s of skipped) console.log('  KEPT      ', s)
for (const m of missing) console.log('  MISSING?  ', m)
