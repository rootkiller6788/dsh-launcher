// Generates the bundled content catalogs that ship inside the launcher binary:
//   crates/launcher-core/data/content-themes.json  <- awesome-dsh-themes/data/themes.json
//   crates/launcher-core/data/content-skills.json  <- awesome-agent-skills/README.md
//   crates/launcher-core/data/content-mcps.json    <- scripts/data/mcp-overrides.json
//   crates/launcher-core/data/content-bundles.json <- awesome-agent-bundles/data/bundles.json
//
// The awesome-* clones live OUTSIDE this repo (D:/Opencode/dsh-plugin/…); this is
// a dev-time tool — run it, commit the resulting JSON, and the launcher embeds
// the JSON via include_str! (offline, no hosted endpoint needed for these kinds).
//
//   node scripts/gen-content-catalog.mjs

import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'

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

// Skill README line shape:  `- **[owner/name](URL)** - description`
// The URL is an officialskills.sh deep-link or a github.com blob/tree/repo link.
// `repo` is the canonical github.com repo (the clone/install target); `fetch` is
// a best-effort raw SKILL.md URL — a fast path that works for the well-formed
// layouts but is wrong for repos with an unconventional tree (install falls back
// to a shallow clone + SKILL.md search when it 404s).
function resolveSkill(url) {
  let m
  // officialskills.sh/<owner>/<repo>/<name>  →  github.com/<owner>/<repo>
  m = url.match(/^https?:\/\/officialskills\.sh\/([^/]+)\/([^/]+)\/([^/]+)\/?$/)
  if (m) {
    const [, o, r, name] = m
    return {
      owner: o,
      name,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/HEAD/skills/${name}/SKILL.md`,
    }
  }
  // github blob: keep the full path (which ends in SKILL.md)
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/blob\/([^/]+)\/(.+)$/)
  if (m) {
    const [, o, r, branch, path] = m
    return {
      owner: o,
      name: lastSkillSegment(path) || r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/${branch}/${path}`,
    }
  }
  // github tree: append SKILL.md to the subdir
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/tree\/([^/]+)\/(.+)$/)
  if (m) {
    const [, o, r, branch, path] = m
    return {
      owner: o,
      name: lastSkillSegment(path) || r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/${branch}/${path}/SKILL.md`,
    }
  }
  // github repo root: SKILL.md at the root of the default branch
  m = url.match(/^https?:\/\/github\.com\/([^/]+)\/([^/]+)\/?$/)
  if (m) {
    const [, o, r] = m
    return {
      owner: o,
      name: r,
      repo: `https://github.com/${o}/${r}`,
      fetch: `https://raw.githubusercontent.com/${o}/${r}/HEAD/SKILL.md`,
    }
  }
  return null
}

// The skill folder name = the path segment holding SKILL.md (drop a trailing
// `SKILL.md` segment first for blob URLs).
function lastSkillSegment(path) {
  const segs = path.split('/').filter(Boolean)
  if (segs.length && segs[segs.length - 1].toLowerCase() === 'skill.md') segs.pop()
  return segs[segs.length - 1] || ''
}

function genSkills() {
  const md = readFileSync(resolve(CLONES, 'awesome-agent-skills/README.md'), 'utf8')
  const seen = new Set()
  const skills = []
  const cats = new Set()
  let section = 'General'
  // Vendor blocks are `<details><summary><h3>Skills by X</h3></summary>…</details>`.
  // Their nested `### Product` subheadings (NVIDIA's 17 products, Microsoft's
  // sub-docs) are sub-groupings, not catalog categories — fold them under the
  // block title instead of promoting each to a top-level category.
  let inDetails = false
  for (const raw of md.split('\n')) {
    if (/^<\/details>/.test(raw)) {
      inDetails = false
      continue
    }
    if (/^<details/.test(raw)) {
      inDetails = true
      continue
    }
    // Vendor groups use an HTML heading: `<summary><h3 …>Title</h3></summary>`.
    const h3 = raw.match(/<h3[^>]*>([^<]+)<\/h3>/)
    if (h3) {
      section = h3[1].trim()
      continue
    }
    if (/^###\s/.test(raw)) {
      if (!inDetails) {
        const name = sectionName(raw)
        if (name) section = name
      }
      continue
    }
    const m = raw.match(/^\s*-\s*\*\*\[([^\]]+)\]\(([^)]+)\)\*\*([\s\S]*)$/)
    if (!m) continue
    const [, , url, rest] = m
    const resolved = resolveSkill(url)
    if (!resolved) continue
    const id = `${resolved.owner}/${resolved.name}`
    if (seen.has(id)) continue
    seen.add(id)
    cats.add(section)
    const dashIdx = rest.search(/[-–—]/)
    const desc = (dashIdx === -1 ? rest : rest.slice(dashIdx + 1)).trim().slice(0, 200)
    skills.push({
      name: resolved.name,
      owner: resolved.owner,
      url: resolved.repo,
      category: [section],
      description: toDescription(desc),
      npm: null,
      tarball: null,
      screenshots: [],
      stars: null,
      downloads: null,
      install: '',
      added: '',
      deprecated: null,
      replacement: null,
      kind: 'skill',
      fetch: resolved.fetch,
    })
  }
  const categories = {}
  for (const c of [...cats].sort()) categories[c] = { en: c, zh: zhLabel(c) }
  const out = {
    updated: '',
    count: skills.length,
    categories,
    plugins: skills,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-skills.json'), JSON.stringify(out, null, 2))
  console.log(`content-skills.json: ${skills.length} skills in ${Object.keys(categories).length} categories`)
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
  const out = {
    updated: '',
    count: mcps.length,
    categories,
    plugins: mcps,
  }
  mkdirSync(dataDir, { recursive: true })
  writeFileSync(resolve(dataDir, 'content-mcps.json'), JSON.stringify(out, null, 2))
  console.log(`content-mcps.json: ${mcps.length} MCP servers in ${Object.keys(categories).length} categories`)
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
