// Unit tests for the skill-catalog merge (no network, no upstream clone).
//   node --test scripts/lib/skills.test.mjs
//
// The regression these cover: content-skills.json is regenerated from
// awesome-agent-skills/README.md wholesale, so before the protected zone a
// hand-added skill (rootkiller6788/mathmodel-skill) was erased by the next run.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import {
  mergeSkillOverrides,
  parseSkillsMd,
  resolveSkill,
  skillId,
  skillIdDelta,
} from './skills.mjs'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, '../..')

// Stand-ins for the two helpers the script injects (shared with the theme/MCP
// generators, so they stay in gen-content-catalog.mjs).
const toDescription = (d) => (d ? { en: d } : {})
const sectionName = (line) => line.replace(/^#+\s*/, '').trim()

const line = (owner, name, url, desc) => `- **[${owner}/${name}](${url})** - ${desc}`

test('resolveSkill: officialskills.sh deep-link points at the repo, fetches skills/<name>/SKILL.md', () => {
  const s = resolveSkill('https://officialskills.sh/anthropics/skills/docx')
  assert.equal(s.owner, 'anthropics')
  assert.equal(s.name, 'docx')
  assert.equal(s.repo, 'https://github.com/anthropics/skills')
  assert.equal(s.fetch, 'https://raw.githubusercontent.com/anthropics/skills/HEAD/skills/docx/SKILL.md')
})

test('resolveSkill: blob URL keeps its path, tree URL gains SKILL.md', () => {
  const blob = resolveSkill('https://github.com/o/r/blob/main/skills/pdf/SKILL.md')
  assert.equal(blob.name, 'pdf')
  assert.equal(blob.fetch, 'https://raw.githubusercontent.com/o/r/main/skills/pdf/SKILL.md')
  const tree = resolveSkill('https://github.com/o/r/tree/main/skills/pdf')
  assert.equal(tree.name, 'pdf')
  assert.equal(tree.fetch, 'https://raw.githubusercontent.com/o/r/main/skills/pdf/SKILL.md')
})

test('resolveSkill: a non-github link is not a catalog entry', () => {
  assert.equal(resolveSkill('https://example.com/skill'), null)
})

test('parseSkillsMd: groups by section, dedupes ids, ignores non-entry lines', () => {
  const md = [
    '# Skills',
    '### Core Skills',
    line('o', 'a', 'https://github.com/o/a', 'Does A.'),
    line('o', 'a', 'https://github.com/o/a', 'Duplicate.'),
    'some prose that is not an entry',
    '### Python Skills',
    line('o', 'b', 'https://github.com/o/b', 'Does B.'),
  ].join('\n')
  const { skills, cats } = parseSkillsMd(md, { toDescription, sectionName })
  assert.deepEqual(skills.map(skillId), ['o/a', 'o/b'])
  assert.deepEqual(skills[0].category, ['Core Skills'])
  assert.deepEqual([...cats].sort(), ['Core Skills', 'Python Skills'])
})

test('parseSkillsMd: a vendor block folds its nested product headings into the block title', () => {
  const md = [
    '### Core Skills',
    line('o', 'a', 'https://github.com/o/a', 'Does A.'),
    '<details>',
    '<summary><h3>Skills by NVIDIA</h3></summary>',
    '### TensorRT',
    line('nvidia', 'tensorrt', 'https://github.com/nvidia/tensorrt', 'Inference.'),
    '</details>',
  ].join('\n')
  const { skills } = parseSkillsMd(md, { toDescription, sectionName })
  assert.deepEqual(skills[1].category, ['Skills by NVIDIA'])
})

test('mergeSkillOverrides: a hand-added skill survives a regenerate', () => {
  // The shipped case: mathmodel-skill is not in upstream's README, so only this
  // merge keeps it in the catalog.
  const { skills, cats } = parseSkillsMd(
    line('o', 'a', 'https://github.com/o/a', 'Does A.'),
    { toDescription, sectionName },
  )
  mergeSkillOverrides(skills, cats, [
    {
      name: 'mathmodel-skill',
      owner: 'rootkiller6788',
      url: 'https://github.com/rootkiller6788/mathmodel-skill',
      category: ['Specialized Domains'],
      description: { en: 'Math modeling.', zh: '数学建模。' },
      fetch: 'https://raw.githubusercontent.com/rootkiller6788/mathmodel-skill/HEAD/SKILL.md',
    },
  ])
  assert.deepEqual(skills.map(skillId), ['o/a', 'rootkiller6788/mathmodel-skill'])
  const added = skills[1]
  assert.equal(added.kind, 'skill')
  assert.deepEqual(added.description, { en: 'Math modeling.', zh: '数学建模。' })
  assert.equal(added.install, '')
  assert.equal(added.added, '')
  // Its category has to reach the catalog-wide map or the Market filter drops it.
  assert.ok(cats.has('Specialized Domains'))
})

test('mergeSkillOverrides: an id the README caught up with is listed once, override wins', () => {
  const { skills, cats } = parseSkillsMd(
    line('o', 'a', 'https://github.com/o/a', 'Upstream wording.'),
    { toDescription, sectionName },
  )
  mergeSkillOverrides(skills, cats, [
    {
      name: 'a',
      owner: 'o',
      url: 'https://github.com/o/a',
      category: ['Core Skills'],
      description: { en: 'Our wording.' },
    },
  ])
  assert.equal(skills.length, 1)
  assert.deepEqual(skills[0].description, { en: 'Our wording.' })
  assert.equal(skills[0].fetch, 'https://raw.githubusercontent.com/o/a/HEAD/SKILL.md')
})

test('skillIdDelta: names what entered and what left the catalog', () => {
  const before = [{ owner: 'o', name: 'gone' }, { owner: 'o', name: 'kept' }]
  const { added, dropped } = skillIdDelta(before, [{ owner: 'o', name: 'kept' }, { owner: 'o', name: 'new' }])
  assert.deepEqual(added, ['o/new'])
  assert.deepEqual(dropped, ['o/gone'])
})

test('skillIdDelta: a dropped id from neither source is reported as erased', () => {
  // The pre-fix failure: mathmodel-skill was hand-appended to the JSON, so a
  // regenerate dropped it with nothing to explain where it went.
  const before = [{ owner: 'rootkiller6788', name: 'mathmodel-skill' }, { owner: 'o', name: 'upstream' }]
  const kept = [{ owner: 'o', name: 'renamed' }]
  const noSources = skillIdDelta(before, kept)
  assert.deepEqual(noSources.erased, [], 'unclassified without the source id sets')
  const { dropped, erased } = skillIdDelta(before, kept, {
    readmeIds: new Set(['o/upstream']),
    overrideIds: new Set(),
  })
  assert.deepEqual(dropped.sort(), ['o/upstream', 'rootkiller6788/mathmodel-skill'])
  assert.deepEqual(erased, ['rootkiller6788/mathmodel-skill'])
})

test('the shipped catalog reflects the shipped overrides', () => {
  // Guards the generator pipeline end to end without the upstream clone: every
  // hand-added entry must be in the committed catalog, and `count` must match.
  // Fails if someone hand-edits the JSON without adding the override, or edits
  // the overrides without re-running the generator.
  const overrides = JSON.parse(
    readFileSync(resolve(repoRoot, 'scripts/data/skill-overrides.json'), 'utf8'),
  )
  const catalog = JSON.parse(
    readFileSync(resolve(repoRoot, 'crates/launcher-core/data/content-skills.json'), 'utf8'),
  )
  assert.ok(overrides.length > 0, 'the protected zone should not be empty')
  const ids = new Set(catalog.plugins.map(skillId))
  for (const o of overrides) {
    assert.ok(ids.has(skillId(o)), `${skillId(o)} is in skill-overrides.json but not in the catalog`)
  }
  assert.equal(catalog.count, catalog.plugins.length)
  for (const override of overrides) {
    const entry = catalog.plugins.find((p) => skillId(p) === skillId(override))
    assert.equal(entry.kind, 'skill')
    assert.deepEqual(entry.description, override.description)
    assert.ok(catalog.categories[entry.category[0]], `${skillId(override)}'s category is missing from the map`)
  }
})
