// Generates the bundled content catalogs that ship inside the launcher binary:
//   crates/launcher-core/data/content-themes.json  <- awesome-dsh-themes/data/themes.json
//   crates/launcher-core/data/content-skills.json  <- awesome-agent-skills/README.md
//                                                    + scripts/data/skill-overrides.json (hand-added)
//   crates/launcher-core/data/content-mcps.json    <- scripts/data/mcp-overrides.json + mcp-bulk.json,
//                                                    then resolver-stamped from scripts/data/mcp-resolved.json
//   crates/launcher-core/data/content-bundles.json <- awesome-agent-bundles/data/bundles.json
//
// The awesome-* clones live OUTSIDE this repo (D:/Opencode/dsh-plugin/…); this is
// a dev-time tool — run it, commit the resulting JSON, and the launcher embeds
// the JSON via include_str! (offline, no hosted endpoint needed for these kinds).
//
//   node scripts/resolver/github-analyzer.mjs   # optional: resolve → mcp-resolved.json
//   node scripts/gen-content-catalog.mjs        # regenerate all catalogs (mcps get stamped)
//
// When mcp-resolved.json is absent the MCP catalog is emitted un-stamped (base
// form, still installable for its already-real entries) — run the analyzer first
// to lift coverage of the pseudo/no-command entries.

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { mcpCoverage, stampPlugins } from './resolver/stamp.mjs'
import {
  mergeSkillOverrides,
  parseSkillsMd,
  skillId,
  skillIdDelta,
} from './lib/skills.mjs'
import { stdioFlagGap, withStdioFlag } from './lib/mcp-stdio.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, '..')
const dataDir = resolve(repoRoot, 'crates/launcher-core/data')
const CLONES = 'D:/Opencode/dsh-plugin'

const CJK = /[\u4e00-\u9fff]/

// RegistryPlugin.description is `{en, zh}`; theme descriptions are plain text.
// CJK text lands in `zh`, everything else in `en`.
function toDescription(desc) {
  if (!desc) return {}
  const d = String(desc).trim()
  return CJK.test(d) ? { zh: d } : { en: d }
}

// Extract a human category name from a `### …` section heading. Skill/MCP
// READMEs group entries under headings like `### Python Skills` or
// `### <a name="databases"></a>Databases`; this keeps that grouping so the
// Market's per-kind category filter has a real axis.
function sectionName(line) {
  let s = line.replace(/^#+\s*/, '') // strip leading #'s
  s = s.replace(/<a[^>]*>.*?<\/a>/g, ' ') // drop inline <a name=…></a> anchor
  s = s.replace(/^[^\x20-\x7E]+/, '') // drop leading emoji/pictograph
  s = s.trim()
  return s || ''
}

// Category label → 中文. Functional domains and language groups get a real
// translation; vendor names stay in latin. Anything not covered falls back to
// a rule or the english name.
const CATEGORY_ZH = {
  // skill groups
  '.NET Skills': '.NET 技能',
  'Core Skills': '核心技能',
  'Java Skills': 'Java 技能',
  'Python Skills': 'Python 技能',
  'Rust Skills': 'Rust 技能',
  'TypeScript Skills': 'TypeScript 技能',
  'Context Engineering': '上下文工程',
  'Development and Testing': '开发与测试',
  Advertising: '广告',
  Marketing: '营销',
  'Product Manager': '产品经理',
  'Productivity and Collaboration': '效率与协作',
  'Specialized Domains': '专业领域',
  'Vector Databases': '向量数据库',
  'n8n Automation': 'n8n 自动化',
  'video-search-and-summarization': '视频搜索与总结',
  'Official Claude Skills': 'Claude 官方 Skills',
  // mcp functional domains
  Accessibility: '无障碍',
  'Aerospace & Astrodynamics': '航空航天',
  Aggregators: '聚合器',
  'Agreements & Coordination': '协议与协作',
  'Architecture & Design': '架构与设计',
  'Art & Culture': '艺术与文化',
  'Biology, Medicine and Bioinformatics': '生物医药',
  'Browser Automation': '浏览器自动化',
  'Cloud Platforms': '云平台',
  'Code Execution': '代码执行',
  'Coding Agents': '编码代理',
  'Command Line': '命令行',
  Communication: '通信',
  'Conversational AI': '对话式 AI',
  Cryptography: '密码学',
  Curated: '精选',
  'Customer Data Platforms': '客户数据平台',
  'Data Platforms': '数据平台',
  'Data Science Tools': '数据科学工具',
  'Data Visualization': '数据可视化',
  Databases: '数据库',
  Delivery: '交付',
  'Developer Tools': '开发工具',
  'E-Commerce': '电商',
  Education: '教育',
  'Embedded System': '嵌入式系统',
  'Environment & Nature': '环境与自然',
  'File Systems': '文件系统',
  'Finance & Fintech': '金融科技',
  Gaming: '游戏',
  'Health & Wellness': '健康',
  'Home Automation': '智能家居',
  'Industrial & IoT': '工业物联网',
  'Knowledge & Memory': '知识记忆',
  Legal: '法律',
  'Location Services': '位置服务',
  Monitoring: '监控',
  'Multimedia Process': '多媒体处理',
  'OS Automation': '系统自动化',
  'Other Tools and Integrations': '其他工具与集成',
  Podcasts: '播客',
  'Product Management': '产品管理',
  'Real Estate': '房地产',
  Research: '研究',
  'Search & Data Extraction': '搜索与数据提取',
  Security: '安全',
  'Social Media': '社交媒体',
  'Speech-to-Text': '语音转文字',
  'Spirituality & Esoterica': '灵性',
  Sports: '运动',
  'Support & Service Management': '支持与服务管理',
  'Text-to-Speech': '文字转语音',
  'Translation Services': '翻译服务',
  'Travel & Transportation': '出行交通',
  'Version Control': '版本控制',
  'Workplace & Productivity': '办公与效率',
  'end to end RAG platforms': '端到端 RAG 平台',
}

// Localize a category id. Vendor groups ("Skills by X") keep the vendor name
// but get a 团队 suffix; "X Skills by Y" flips to "Y 的 X".
function zhLabel(en) {
  if (CATEGORY_ZH[en]) return CATEGORY_ZH[en]
  let m = en.match(/^Skills by (.+)$/)
  if (m) {
    const v = m[1]
      .replace(/\bTeam\b/gi, '')
      .replace(/[—-]/g, ' ')
      .replace(/\s{2,}/g, ' ')
      .trim()
    return v ? `${v} 团队` : en
  }
  m = en.match(/^(.+?) Skills by (.+)$/)
  if (m) return `${m[2]} 的 ${zhLabel(m[1])}`
  return en
}

function genThemes() {
  const raw = JSON.parse(
    readFileSync(resolve(CLONES, 'awesome-dsh-themes/data/themes.json'), 'utf8'),
  )
  const skins = (raw.themes || [])
    .filter((t) => t.kind === 'skin' && t.status === 'verified')
    .map((t) => {
      const owner = (t.repo || '').split('/')[0] || ''
      return {
        name: t.name,
        owner,
        url: t.repo ? `https://github.com/${t.repo}` : '',
        category: [t.category || 'skin'],
        description: toDescription(t.description),
        npm: t.npm ?? null,
        tarball: null,
        screenshots: [],
        stars: null,
        downloads: null,
        install: t.install || '',
        added: t.added || '',
        deprecated: null,
        replacement: null,
        kind: 'theme',
        preview: t.preview ?? null,
        previewCss: t.previewCss ?? null,
        path: t.path ?? null,
        gist: t.gist ?? null,
      }
    })
  // Skins ship a handful of real `category` values; label them all instead of
  // leaving three of the four to fall back to a bare english token.
  const SKIN_LABEL = {
    skin: { en: 'Skins', zh: '皮肤' },
    tokens: { en: 'Tokens', zh: '配色' },
    fun: { en: 'Fun', zh: '趣味' },
    companion: { en: 'Companion', zh: '陪伴' },
  }
  const catSet = new Set(skins.map((t) => t.category[0]))
  const categories = {}
  for (const c of catSet) categories[c] = SKIN_LABEL[c] || { en: c, zh: c }
  const out = {
    updated: raw.updated || '',
    count: skins.length,
    categories,
    plugins: skins,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-themes.json'), JSON.stringify(out, null, 2))
  console.log(`content-themes.json: ${skins.length} verified skins`)
}

// The README walk, the override merge and the delta report live in lib/skills.mjs
// so they can be unit-tested without the upstream clone (scripts/lib/skills.test.mjs).
//
// `content-skills.json` is regenerated from upstream's README every run, so a
// skill appended straight to the JSON (or one the README drops) disappears
// silently. Two guards, both needed:
//   1. scripts/data/skill-overrides.json — the protected zone for hand-added
//      entries. They are appended after the README-derived set, and an id that
//      is in both places is emitted once with the override's fields.
//   2. the dropped-id report below — new/removed ids are always logged, so a
//      README change that would erase catalog entries is visible in the run
//      output instead of only in the JSON diff.
function genSkills() {
  const overrides = JSON.parse(
    readFileSync(resolve(__dirname, 'data/skill-overrides.json'), 'utf8'),
  )
  const md = readFileSync(resolve(CLONES, 'awesome-agent-skills/README.md'), 'utf8')
  const { skills, cats } = parseSkillsMd(md, { toDescription, sectionName })
  const fromReadme = new Set(skills.map(skillId))
  mergeSkillOverrides(skills, cats, overrides)
  const categories = {}
  for (const c of [...cats].sort()) categories[c] = { en: c, zh: zhLabel(c) }
  const out = {
    updated: '',
    count: skills.length,
    categories,
    plugins: skills,
  }
  mkdirSync(dataDir, { recursive: true })
  const target = resolve(dataDir, 'content-skills.json')
  // Report the delta against the committed catalog: a README that dropped or
  // renamed a line shows up here as a removal, and an override that stopped
  // being picked up shows up as an addition the next run would repeat.
  let delta = ''
  try {
    const before = JSON.parse(readFileSync(target, 'utf8')).plugins || []
    // `erased` = hand-added to the JSON and in neither source, so this run just
    // dropped it. Name the fix, not only the casualty.
    const { added, dropped, erased } = skillIdDelta(before, skills, {
      readmeIds: fromReadme,
      overrideIds: new Set(overrides.map(skillId)),
    })
    if (added.length || dropped.length) {
      delta =
        `\n  +${added.length} new: ${added.slice(0, 10).join(', ') || '-'}` +
        `\n  -${dropped.length} dropped: ${dropped.slice(0, 10).join(', ') || '-'}`
      if (erased.length) {
        delta +=
          `\n  !! ${erased.length} of them were neither in the README nor in skill-overrides.json` +
          ` — move them there or they are gone for good: ${erased.slice(0, 10).join(', ')}`
      }
    }
  } catch {
    // No previous catalog to compare against (first run) — nothing to report.
  }
  writeFileSync(target, JSON.stringify(out, null, 2))
  console.log(
    `content-skills.json: ${skills.length} skills in ${Object.keys(categories).length} categories` +
      ` (${overrides.length} hand-added)${delta}`,
  )
}

// MCP servers ship from two lists: a hand-maintained curated set (verified npm
// packages, good descriptions, notes on required env vars) plus a bulk list
// auto-parsed from awesome-mcp-servers/README.md (scripts/parse-awesome-mcp.mjs).
// Curated first, then bulk. Each entry is self-contained: name/owner/url for the
// card, plus serverName/transport/command/args/env (stdio) or mcpUrl/headers
// (streamable-http) for the mcp-client insert row.
function genMcps() {
  const overrides = JSON.parse(
    readFileSync(resolve(__dirname, 'data/mcp-overrides.json'), 'utf8'),
  )
  const bulk = JSON.parse(
    readFileSync(resolve(__dirname, 'data/mcp-bulk.json'), 'utf8'),
  )
  const cats = new Set()
  // Curated overrides are hand-picked; bulk entries carry their README section
  // (assigned by parse-awesome-mcp.mjs). Anything else falls back to 'mcp'.
  const curated = overrides.map((o) => ({ ...o, category: ['Curated'] }))
  const mcps = [...curated, ...bulk].map((o) => {
    const cat = o.category && o.category.length ? o.category : ['mcp']
    cat.forEach((c) => cats.add(c))
    return {
      name: o.name,
      owner: o.owner,
      url: o.url,
      category: cat,
      description: toDescription(o.description),
      npm: null,
      tarball: null,
      screenshots: [],
      stars: null,
      downloads: null,
      install: '',
      added: '',
      deprecated: null,
      replacement: null,
      kind: 'mcp',
      serverName: o.serverName ?? null,
      transport: o.transport ?? null,
      command: o.command ?? null,
      args: o.args ?? null,
      env: o.env ?? null,
      mcpUrl: o.mcpUrl ?? null,
      headers: o.headers ?? null,
    }
  })
  const categories = {}
  for (const c of [...cats].sort()) categories[c] = { en: c, zh: zhLabel(c) }

  // Resolver stamp: overlay mcpInstall from scripts/data/mcp-resolved.json when the
  // batch has run (see stamp.mjs). Best-effort — missing cache keeps base form.
  let stampNote = ''
  try {
    const resolved =
      JSON.parse(readFileSync(resolve(__dirname, 'data/mcp-resolved.json'), 'utf8')).entries || {}
    const { fromResolved, mirrored } = stampPlugins(mcps, resolved)
    if (fromResolved + mirrored > 0) {
      stampNote = ` (+${fromResolved} resolved, ${mirrored} mirrored)`
    }
  } catch {
    stampNote = ' (no mcp-resolved.json — un-stamped)'
  }

  // One place decides the app-server flag, after stamping, so the base record
  // and the resolver's launch args can never disagree about it.
  for (const m of mcps) {
    m.args = withStdioFlag(m.args)
    if (m.mcpInstall?.launch) m.mcpInstall.launch.args = withStdioFlag(m.mcpInstall.launch.args)
  }
  const stdioGaps = mcps.map(stdioFlagGap).filter(Boolean)

  const out = {
    updated: stampNote ? new Date().toISOString().slice(0, 10) : '',
    count: mcps.length,
    categories,
    plugins: mcps,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-mcps.json'), JSON.stringify(out, null, 2))
  const cov = mcpCoverage(mcps)
  const gapNote = stdioGaps.length
    ? `\n  !! ${stdioGaps.length} app server(s) carry no --stdio and are not in STDIO_APP_SERVERS` +
      ` — they will boot HTTP and never answer a stdio probe: ${stdioGaps.join(', ')}`
    : ''
  console.log(
    `content-mcps.json: ${cov.total} MCP servers in ${Object.keys(categories).length} categories, ` +
      `${cov.withPlan} with mcpInstall${stampNote}${gapNote}`,
  )
}

// Bundles are curated cross-kind combinations. Each entry is a composite that
// references existing content by `kind` + `owner/name`; the launcher resolves
// those references against the merged catalog at install time. The card shows
// the bundle title + rationale, and a one-click install expands into its items.
function genBundles() {
  const raw = JSON.parse(
    readFileSync(resolve(CLONES, 'awesome-agent-bundles/data/bundles.json'), 'utf8'),
  )
  const bundles = (raw.bundles || []).map((b) => ({
    name: b.name,
    owner: '',
    url: '',
    category: ['bundle'],
    description: toDescription(b.description),
    npm: null,
    tarball: null,
    screenshots: [],
    stars: null,
    downloads: null,
    install: '',
    added: b.added || '',
    deprecated: null,
    replacement: null,
    kind: 'bundle',
    items: (b.items || []).map((it) => ({
      name: it.name,
      kind: it.kind,
      reason: it.reason || '',
    })),
  }))
  const out = {
    updated: raw.updated || '',
    count: bundles.length,
    categories: { bundle: { en: 'Bundles', zh: '整合包' } },
    plugins: bundles,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-bundles.json'), JSON.stringify(out, null, 2))
  console.log(`content-bundles.json: ${bundles.length} bundles`)
}

genThemes()
genSkills()
genMcps()
genBundles()
