// Skill-catalog assembly, split out of gen-content-catalog.mjs so the rules
// that protect hand-added entries can be unit-tested without the upstream clone.
//   node --test scripts/lib/skills.test.mjs
//
// The catalog is regenerated from awesome-agent-skills/README.md wholesale on
// every run, which is why the merge step below exists: anything a human added by
// hand goes in scripts/data/skill-overrides.json, and `skillIdDelta` reports ids
// that left the catalog so a README edit that erases entries is visible.

export const skillId = (p) => `${p.owner}/${p.name}`

// One catalog plugin, with the README-derived defaults filled in. Hand-added
// overrides go through the same shape so both paths land identically.
export function skillEntry(o) {
  return {
    name: o.name,
    owner: o.owner,
    url: o.url,
    category: o.category && o.category.length ? o.category : ['General'],
    description: o.description ?? {},
    npm: null,
    tarball: null,
    screenshots: [],
    stars: null,
    downloads: null,
    install: o.install || '',
    added: o.added || '',
    deprecated: null,
    replacement: null,
    kind: 'skill',
    fetch: o.fetch || '',
  }
}

// Map a README link to the catalog's owner/name plus the two URLs we need:
// `repo` (the install clone target) and `fetch` (a best-effort raw SKILL.md fast
// path — wrong for repos with an unconventional tree, where the install falls
// back to a shallow clone + SKILL.md search).
export function resolveSkill(url) {
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
export function lastSkillSegment(path) {
  const segs = path.split('/').filter(Boolean)
  if (segs.length && segs[segs.length - 1].toLowerCase() === 'skill.md') segs.pop()
  return segs[segs.length - 1] || ''
}

// Walk the README's entry lines. `toDescription` (text → {en}|{zh}) and
// `sectionName` (heading → category) are injected: both are shared with the
// theme/MCP/bundle generators, which live in the script that calls this.
// Returns the entries plus the raw set of category names they used.
export function parseSkillsMd(md, { toDescription, sectionName }) {
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
    skills.push(
      skillEntry({
        name: resolved.name,
        owner: resolved.owner,
        url: resolved.repo,
        category: [section],
        description: toDescription(desc),
        fetch: resolved.fetch,
      }),
    )
  }
  return { skills, cats }
}

// Append the hand-added entries, after the README-derived set so the diff stays
// small. An id the README has caught up with is overlaid field-wise and stays
// where it is — the override wins on the keys it actually sets, and the catalog
// still lists the skill exactly once. `cats` is extended in place with any
// category the overrides introduce.
export function mergeSkillOverrides(skills, cats, overrides) {
  for (const o of overrides) {
    const id = skillId(o)
    const at = skills.findIndex((s) => skillId(s) === id)
    // Field-wise so an override that only fixes the wording keeps the
    // README-derived fetch URL instead of blanking it.
    const entry = skillEntry(at === -1 ? o : { ...skills[at], ...o })
    if (at === -1) skills.push(entry)
    else skills[at] = entry
    entry.category.forEach((c) => cats.add(c))
  }
  return skills
}

// Which ids entered/left the catalog relative to the committed file. Pass the
// id sets of both sources to also get `erased`: the dropped ids that were in
// neither, i.e. hand-added straight to the JSON, which this regenerate has just
// thrown away — the failure mode skill-overrides.json exists to stop. Without
// the sets `erased` stays empty (there is nothing to classify against).
export function skillIdDelta(previousPlugins, skills, { readmeIds, overrideIds } = {}) {
  const old = new Set(previousPlugins.map(skillId))
  const now = new Set(skills.map(skillId))
  const dropped = [...old].filter((id) => !now.has(id))
  const known = readmeIds && overrideIds
  return {
    added: [...now].filter((id) => !old.has(id)),
    dropped,
    erased: known ? dropped.filter((id) => !readmeIds.has(id) && !overrideIds.has(id)) : [],
  }
}
