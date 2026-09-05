import { useEffect, useMemo, useRef, useState, type ImgHTMLAttributes } from 'react'
import {
  Boxes,
  Download,
  Layers3,
  Package,
  RefreshCw,
  Search,
  Shuffle,
  SlidersHorizontal,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { Select } from '../components/Select'
import type {
  BundleManifest,
  ContentKind,
  InstalledPlugin,
  Job,
  PluginUpdate,
  RegistryPlugin,
} from '../lib/types'

/** Stable identity: `owner/name` when an owner exists, else the bare name. */
function pluginKey(p: RegistryPlugin) {
  return p.owner ? `${p.owner}/${p.name}` : p.name
}

function repoPackageName(entry: RegistryPlugin) {
  const repoPath = entry.url
    ?.replace(/^https?:\/\/github\.com\//i, '')
    .replace(/\/(?:tree|blob)\/.*$/i, '')
    .replace(/#.*$/i, '')
    .replace(/\/$/i, '')
    .replace(/\.git$/i, '')
    .toLowerCase()
  return repoPath?.replace(/[/-]/g, '__')
}

type SortKey = 'stars' | 'new' | 'name'
const KINDS: { value: ContentKind; label: string }[] = [
  { value: 'plugin', label: 'market.tabPlugins' },
  { value: 'theme', label: 'market.tabThemes' },
  { value: 'skill', label: 'market.tabSkills' },
  { value: 'mcp', label: 'market.tabMcp' },
  // 整合包 tab 暂时下线：bundle 生态尚未成熟，等 awesome-agent-bundles 火起来再开。
  // { value: 'bundle', label: 'market.tabBundles' },
]

const KIND_LABEL: Record<ContentKind, string> = {
  plugin: 'market.tabPlugins',
  theme: 'market.tabThemes',
  skill: 'market.tabSkills',
  mcp: 'market.tabMcp',
  bundle: 'market.tabBundles',
}

const KIND_NOTES: Record<ContentKind, string[]> = {
  plugin: [
    'market.notePlugins1',
    'market.notePlugins2',
    'market.notePlugins3',
    'market.notePlugins4',
    'market.notePlugins5',
  ],
  theme: [
    'market.noteSkins1',
    'market.noteSkins2',
    'market.noteSkins3',
    'market.noteSkins4',
    'market.noteSkins5',
  ],
  skill: [
    'market.noteSkills1',
    'market.noteSkills2',
    'market.noteSkills3',
    'market.noteSkills4',
    'market.noteSkills5',
  ],
  mcp: [
    'market.noteMcp1',
    'market.noteMcp2',
    'market.noteMcp3',
    'market.noteMcp4',
    'market.noteMcp5',
  ],
  bundle: [
    'market.noteBundles1',
    'market.noteBundles2',
    'market.noteBundles3',
    'market.noteBundles4',
    'market.noteBundles5',
  ],
}

function MarketStat({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof Package
  label: string
  value: string | number
}) {
  return (
    <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="flex items-center gap-2 text-[11px] text-zinc-500">
        <Icon className="h-3.5 w-3.5" strokeWidth={1.75} />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-1 truncate text-xl font-semibold tabular-nums text-zinc-100">{value}</div>
    </div>
  )
}

/// Prefix proxy for `raw.githubusercontent.com` screenshots (the same gh-proxy
/// dsh-market uses in its China route), so image loads aren't hostage to the
/// direct GitHub link being slow from mainland China.
const IMAGE_PROXY = 'https://gh-proxy.com'

/// The proxied twin of a screenshot URL, or null when it isn't GitHub-hosted.
function proxied(url: string): string | null {
  return url.includes('raw.githubusercontent.com') ? `${IMAGE_PROXY}/${url}` : null
}

/**
 * An `<img>` that races the direct URL against the gh-proxy URL and shows
 * whichever finishes first — direct for a fast/overseas link, the proxy for a
 * slow mainland-China link. Falls back gracefully to direct when there is no
 * proxy twin.
 */
function SmartImg(props: ImgHTMLAttributes<HTMLImageElement>) {
  const { src, onLoad, ...rest } = props
  const [cur, setCur] = useState(src)
  const settledRef = useRef(false)

  useEffect(() => {
    settledRef.current = false
    setCur(src)
    if (!src) return
    const alt = proxied(src)
    if (!alt) return
    const img = new Image()
    img.onload = () => {
      if (!settledRef.current) {
        settledRef.current = true
        setCur(alt)
      }
    }
    img.src = alt
  }, [src])

  return (
    <img
      src={cur}
      {...rest}
      onLoad={(e) => {
        settledRef.current = true
        onLoad?.(e)
      }}
    />
  )
}

export function Market() {
  const t = useT()
  const language = useAppStore((s) => s.language)
  const registry = useAppStore((s) => s.registry)
  const registryError = useAppStore((s) => s.registryError)
  const installedPlugins = useAppStore((s) => s.installedPlugins)
  const installedSkills = useAppStore((s) => s.installedSkills)
  const updates = useAppStore((s) => s.updates)
  const busy = useAppStore((s) => s.busy)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const loadRegistry = useAppStore((s) => s.loadRegistry)
  const refreshInstalledPlugins = useAppStore((s) => s.refreshInstalledPlugins)
  const installMarketEntry = useAppStore((s) => s.installMarketEntry)
  const uninstallPlugin = useAppStore((s) => s.uninstallPlugin)
  const togglePlugin = useAppStore((s) => s.togglePlugin)
  const updatePlugin = useAppStore((s) => s.updatePlugin)
  const uninstallSkill = useAppStore((s) => s.uninstallSkill)
  const refreshInstalledSkills = useAppStore((s) => s.refreshInstalledSkills)
  const installedMcps = useAppStore((s) => s.installedMcps)
  const jobs = useAppStore((s) => s.jobs)
  const uninstallMcp = useAppStore((s) => s.uninstallMcp)
  const refreshInstalledMcps = useAppStore((s) => s.refreshInstalledMcps)
  const importBundle = useAppStore((s) => s.importBundle)

  const [query, setQuery] = useState('')
  const [category, setCategory] = useState('')
  const [catOpen, setCatOpen] = useState(false)
  const [sort, setSort] = useState<SortKey>('stars')
  const [visible, setVisible] = useState(60)
  const [shuffleKey, setShuffleKey] = useState(0)
  const [noteIndex, setNoteIndex] = useState(() => Math.floor(Math.random() * 5))
  const [preview, setPreview] = useState<{ urls: string[]; index: number } | null>(null)
  const [activeKind, setActiveKind] = useState<ContentKind>('plugin')

  useEffect(() => {
    void loadRegistry()
    void refreshInstalledPlugins()
    void refreshInstalledSkills()
    void refreshInstalledMcps()
  }, [loadRegistry, refreshInstalledPlugins, refreshInstalledSkills, refreshInstalledMcps])

  useEffect(() => {
    void refreshInstalledPlugins()
    void refreshInstalledSkills()
    void refreshInstalledMcps()
  }, [activeId, refreshInstalledPlugins, refreshInstalledSkills, refreshInstalledMcps])

  useEffect(() => {
    setVisible(60)
  }, [query, category])

  const match = (p: RegistryPlugin): InstalledPlugin | undefined =>
    installedPlugins.find((ip) =>
      p.npm ? ip.name === p.npm : ip.name.toLowerCase().includes(p.name.toLowerCase()),
    )
  const installedSkinKeys = new Set(activeInstance?.skins ?? [])
  const skinInstalled = (p: RegistryPlugin): InstalledPlugin | undefined => {
    if (!installedSkinKeys.has(pluginKey(p))) return undefined
    const found = match(p) ?? installedPlugins.find((ip) => ip.name.toLowerCase().includes(p.name.toLowerCase()))
    if (found && p.path && found.name.toLowerCase() === repoPackageName(p)) return undefined
    return found
  }

  const filtered = useMemo(() => {
    if (!registry) return []
    let list = registry.plugins.filter((p) => (p.kind ?? 'plugin') === activeKind)
    const q = query.trim().toLowerCase()
    if (q) {
      list = list.filter((p) => {
        const hay = `${pluginKey(p)} ${p.category.join(' ')} ${p.description.en} ${p.description.zh}`
        return hay.toLowerCase().includes(q)
      })
    }
    if (category) list = list.filter((p) => p.category.includes(category))
    const result = [...list]
    if (shuffleKey > 0) {
      for (let i = result.length - 1; i > 0; i--) {
        const j = Math.floor(Math.random() * (i + 1))
        ;[result[i], result[j]] = [result[j], result[i]]
      }
      return result
    }
    return result.sort((a, b) => {
      if (sort === 'stars') return (b.stars ?? -1) - (a.stars ?? -1)
      if (sort === 'new') return b.added.localeCompare(a.added)
      return a.name.localeCompare(b.name)
    })
  }, [registry, query, category, sort, shuffleKey, activeKind])

  const shown = filtered.slice(0, visible)
  const findItem = (kind: ContentKind, name: string) =>
    registry?.plugins.find((p) => (p.kind ?? 'plugin') === kind && pluginKey(p) === name)
  const updateFor = (name: string) => updates.find((u) => u.name === name)
  const catLabel = (id: string) =>
    registry?.categories?.[id]?.[language] || registry?.categories?.[id]?.en || id
  // The category dropdown is per-kind: collect only the categories that appear
  // on items of the active type (plugins / skins / skills / MCP each have their
  // own), never the global union.
  const kindCategories = useMemo(() => {
    if (!registry) return []
    const set = new Set<string>()
    for (const p of registry.plugins) {
      if ((p.kind ?? 'plugin') !== activeKind) continue
      for (const c of p.category) set.add(c)
    }
    return [...set].sort((a, b) => a.localeCompare(b))
  }, [registry, activeKind])

  const installedCount = installedPlugins.length + installedSkills.length + installedMcps.length
  const activeKindCount =
    registry?.plugins.filter((p) => (p.kind ?? 'plugin') === activeKind).length ?? 0
  const updatableCount = updates.filter((u) => u.updatable).length
  const noteKeys = KIND_NOTES[activeKind]
  const noteKey = noteKeys[noteIndex % noteKeys.length]
  const switchKind = (kind: ContentKind) => {
    setActiveKind(kind)
    setVisible(60)
    setQuery('')
    setCategory('')
    setNoteIndex(Math.floor(Math.random() * KIND_NOTES[kind].length))
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('market.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">
            {t('market.installsInto')}{' '}
            <span className="font-medium text-zinc-200">{activeInstance?.name ?? '—'}</span>
          </p>
        </div>
        {registry && (
          <span className="rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 py-2 text-xs text-zinc-500">
            {t('market.updated')} {registry.updated}
          </span>
        )}
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-12 gap-5">
        <aside className="col-span-4 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="grid grid-cols-3 gap-3">
            <MarketStat icon={Package} label={t('market.catalog')} value={registry?.count ?? '-'} />
            <MarketStat icon={Download} label={t('market.installed')} value={installedCount} />
            <MarketStat icon={RefreshCw} label={t('market.updates')} value={updatableCount} />
          </div>

          <div className="mt-5">
            <h2 className="text-sm font-semibold text-zinc-200">{t('market.contentTypes')}</h2>
            <div className="mt-3 grid grid-cols-2 gap-2">
              {KINDS.map((k) => (
                <button
                  key={k.value}
                  onClick={() => switchKind(k.value)}
                  className={`rounded-lg border px-3 py-3 text-left text-sm font-medium transition-colors ${
                    activeKind === k.value
                      ? 'border-blue-500/50 bg-blue-500/10 text-blue-300'
                      : 'border-zinc-800/80 bg-zinc-950/20 text-zinc-400 hover:border-zinc-700 hover:bg-zinc-800/30 hover:text-zinc-200'
                  }`}
                >
                  {t(k.label)}
                </button>
              ))}
            </div>
          </div>

          <div className="mt-5 rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
            <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-zinc-500">
              <Boxes className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('market.activeCatalog')}
            </div>
            <div className="mt-3 flex items-end justify-between gap-4">
              <div className="text-3xl font-semibold leading-tight tabular-nums text-zinc-100">
                {activeKindCount}
              </div>
              <div className="text-right text-xs text-zinc-500">
                <div>{t(KIND_LABEL[activeKind])}</div>
                <div>{filtered.length} {t('market.visible')}</div>
              </div>
            </div>
          </div>

          <div className="mt-auto rounded-lg border border-blue-500/20 bg-blue-500/10 p-4">
            <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-blue-300">
              <Layers3 className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t(KIND_LABEL[activeKind])} · {t('market.guidance')}
            </div>
            <p className="mt-3 text-xs leading-5 text-blue-100/80">
              {t(noteKey)}
            </p>
          </div>
        </aside>

        <section className="col-span-8 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="flex shrink-0 items-center gap-3">
            <div className="flex min-w-0 flex-1 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950/35 px-3 py-2">
              <Search className="h-4 w-4 shrink-0 text-zinc-500" strokeWidth={1.75} />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t('market.filter')}
                className="min-w-0 flex-1 bg-transparent text-sm text-zinc-100 outline-none placeholder:text-zinc-600"
              />
            </div>
            <div className="relative">
              <button
                type="button"
                onClick={() => setCatOpen((o) => !o)}
                className="flex min-h-10 min-w-36 items-center justify-between gap-2 rounded-lg border border-zinc-800 bg-zinc-950/35 px-3 text-sm text-zinc-200 outline-none hover:border-zinc-700 focus:border-blue-500"
              >
                <span className="truncate">{category ? catLabel(category) : t('market.allCategories')}</span>
                <SlidersHorizontal className="h-4 w-4 shrink-0 text-zinc-500" strokeWidth={1.75} />
              </button>
              {catOpen && (
                <>
                  <div className="fixed inset-0 z-40" onClick={() => setCatOpen(false)} />
                  <div className="no-scrollbar absolute right-0 z-50 mt-1 max-h-64 overflow-y-auto rounded-lg border border-zinc-700 bg-zinc-900 p-1 shadow-xl">
                    <button
                      type="button"
                      onClick={() => {
                        setCategory('')
                        setCatOpen(false)
                      }}
                      className={`block w-full whitespace-nowrap rounded px-3 py-1 text-left text-sm ${
                        category === '' ? 'bg-blue-600/20 text-blue-300' : 'text-zinc-300 hover:bg-zinc-800'
                      }`}
                    >
                      {t('market.allCategories')}
                    </button>
                    {kindCategories.map((id) => (
                      <button
                        key={id}
                        type="button"
                        onClick={() => {
                          setCategory(id)
                          setCatOpen(false)
                        }}
                        className={`block w-full whitespace-nowrap rounded px-3 py-1 text-left text-sm ${
                          category === id
                            ? 'bg-blue-600/20 text-blue-300'
                            : 'text-zinc-300 hover:bg-zinc-800'
                        }`}
                      >
                        {catLabel(id)}
                      </button>
                    ))}
                  </div>
                </>
              )}
            </div>
            <Select
              value={sort}
              onChange={(next: SortKey) => {
                setSort(next)
                setShuffleKey(0)
              }}
              options={[
                { value: 'stars', label: t('market.sortStars') },
                { value: 'new', label: t('market.sortNew') },
                { value: 'name', label: t('market.sortName') },
              ]}
              triggerClassName="h-10 w-44 shrink-0"
            />
            <button
              onClick={() => setShuffleKey((k) => k + 1)}
              title="Shuffle (random order)"
              className="flex h-10 w-10 items-center justify-center rounded-lg border border-zinc-800 bg-zinc-950/35 text-zinc-300 hover:border-blue-500/50 hover:text-blue-300"
            >
              <Shuffle className="h-4 w-4" strokeWidth={1.75} />
            </button>
          </div>

          <div className="mt-4 flex shrink-0 items-center justify-between text-xs text-zinc-500">
            <span className="font-medium text-zinc-300">{t(KIND_LABEL[activeKind])}</span>
            <span>
              {query.trim() || category
                ? t('market.matches', { n: filtered.length })
                : t('market.total', { n: filtered.length })}
              {shown.length < filtered.length ? t('market.showing', { n: shown.length }) : ''}
            </span>
          </div>

          {registryError && (
            <div className="mt-4 rounded-lg border border-blue-500/25 bg-blue-500/10 px-4 py-3 text-sm text-blue-200">
              {t('market.loadFailed')}{' '}
              <button
                className="font-semibold underline underline-offset-2"
                onClick={() => void loadRegistry()}
              >
                {t('market.retry')}
              </button>
            </div>
          )}

          {!registry && !registryError && (
            <p className="flex flex-1 items-center justify-center text-sm text-zinc-500">
              {t('market.loading')}
            </p>
          )}

          {registry && (
            <div className="mt-4 min-h-0 flex-1 overflow-y-auto pr-1">
              <div className="grid grid-cols-1 gap-3 xl:grid-cols-2">
                {shown.map((p) => {
                  const key = pluginKey(p)
                  // Only an in-flight job (waiting/running) blocks re-install; a
                  // terminal job leaves the card clickable to enqueue a fresh one.
                  const job = jobs.find(
                    (j) => j.key === key && (j.status === 'waiting' || j.status === 'running'),
                  )
                  if ((p.kind ?? 'plugin') === 'skill') {
                    return (
                      <SkillCard
                        key={key}
                        plugin={p}
                        installed={installedSkills.some((r) => r.id === key)}
                        job={job}
                        busy={busy}
                        install={installMarketEntry}
                        remove={uninstallSkill}
                      />
                    )
                  }
                  if ((p.kind ?? 'plugin') === 'mcp') {
                    return (
                      <McpCard
                        key={key}
                        plugin={p}
                        installed={installedMcps.some((r) => r.id === key)}
                        job={job}
                        busy={busy}
                        install={installMarketEntry}
                        remove={uninstallMcp}
                      />
                    )
                  }
                  if ((p.kind ?? 'plugin') === 'bundle') {
                    return <BundleCard key={key} plugin={p} findItem={findItem} importBundle={importBundle} />
                  }
                  if ((p.kind ?? 'plugin') === 'theme') {
                    return (
                      <SkinCard
                        key={key}
                        plugin={p}
                        installed={skinInstalled(p)}
                        job={job}
                        busy={busy}
                        install={installMarketEntry}
                        remove={uninstallPlugin}
                        onPreview={(urls) => setPreview({ urls, index: 0 })}
                      />
                    )
                  }
                  return (
                    <PluginCard
                      key={key}
                      plugin={p}
                      installed={match(p)}
                      update={match(p) ? updateFor(match(p)!.name) : undefined}
                      job={job}
                      busy={busy}
                      install={installMarketEntry}
                      remove={uninstallPlugin}
                      toggle={togglePlugin}
                      updateAction={updatePlugin}
                      onPreview={(urls) => setPreview({ urls, index: 0 })}
                    />
                  )
                })}
              </div>
              {shown.length < filtered.length && (
                <div className="mt-4 text-center">
                  <button
                    onClick={() => setVisible((v) => v + 60)}
                    className="rounded-lg border border-zinc-700 px-4 py-2 text-sm text-zinc-300 hover:bg-zinc-800"
                  >
                    {t('market.loadMore', { n: filtered.length - shown.length })}
                  </button>
                </div>
              )}
            </div>
          )}
        </section>
      </div>

      {preview && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/85 p-8"
          onClick={() => setPreview(null)}
        >
          {preview.urls.length > 1 && (
            <button
              onClick={(e) => {
                e.stopPropagation()
                setPreview((p) =>
                  p ? { ...p, index: (p.index - 1 + p.urls.length) % p.urls.length } : p,
                )
              }}
              className="absolute left-4 rounded-full bg-zinc-800 px-3 py-2 text-xl text-zinc-200 hover:bg-zinc-700"
            >
              ‹
            </button>
          )}
          <SmartImg
            src={preview.urls[preview.index]}
            className="max-h-[85vh] max-w-full rounded-lg"
            onClick={(e) => e.stopPropagation()}
          />
          {preview.urls.length > 1 && (
            <button
              onClick={(e) => {
                e.stopPropagation()
                setPreview((p) =>
                  p ? { ...p, index: (p.index + 1) % p.urls.length } : p,
                )
              }}
              className="absolute right-4 rounded-full bg-zinc-800 px-3 py-2 text-xl text-zinc-200 hover:bg-zinc-700"
            >
              ›
            </button>
          )}
          <button
            onClick={() => setPreview(null)}
            className="absolute right-4 top-4 rounded-full bg-zinc-800 px-3 py-1.5 text-lg text-zinc-200 hover:bg-zinc-700"
          >
            ×
          </button>
        </div>
      )}
    </div>
  )
}

function BundleCard({
  plugin: p,
  findItem,
  importBundle,
}: {
  plugin: RegistryPlugin
  findItem: (kind: ContentKind, name: string) => RegistryPlugin | undefined
  importBundle: (manifest: BundleManifest) => Promise<Job | null>
}) {
  const t = useT()
  const busy = useAppStore((s) => s.busy)
  const desc = p.description.en || p.description.zh || ''
  const installAll = async () => {
    const items = (p.items ?? [])
      .map((it) => findItem(it.kind, it.name))
      .filter((x): x is RegistryPlugin => !!x)
    if (items.length === 0) return
    await importBundle({ name: p.name, version: '1.0.0', description: desc, items })
  }
  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-950/50 p-4">
      <div className="mb-1 flex items-center gap-2">
        <span className="rounded bg-blue-600/20 px-2 py-0.5 text-xs font-bold text-blue-300">
          {t(KIND_LABEL.bundle)}
        </span>
        <span className="font-semibold text-zinc-100">{p.name}</span>
      </div>
      <p className="mb-3 text-xs text-zinc-400">{desc}</p>
      <div className="space-y-2">
        {(p.items ?? []).map((it) => (
          <div
            key={`${it.kind}:${it.name}`}
            className="flex items-center gap-3 rounded-lg bg-zinc-900/60 px-3 py-2"
          >
            <span className="shrink-0 rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] uppercase text-zinc-400">
              {t(KIND_LABEL[it.kind])}
            </span>
            <div className="min-w-0 flex-1">
              <div className="truncate font-mono text-xs text-zinc-200">{it.name}</div>
              <div className="truncate text-xs text-zinc-500">{it.reason}</div>
            </div>
          </div>
        ))}
      </div>
      <button
        onClick={() => void installAll()}
        disabled={busy}
        className="mt-3 w-full rounded-lg bg-blue-500 px-3 py-1.5 text-xs font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
      >
        {t('market.installAll')}
      </button>
    </div>
  )
}

function SkinCard({
  plugin: p,
  installed,
  job,
  busy,
  install,
  remove,
  onPreview,
}: {
  plugin: RegistryPlugin
  installed: InstalledPlugin | undefined
  job?: Job
  busy: boolean
  install: (entry: RegistryPlugin) => Promise<boolean>
  remove: (name: string) => Promise<boolean>
  onPreview: (urls: string[]) => void
}) {
  const t = useT()
  const desc = p.description.en || p.description.zh || ''
  const shots = p.screenshots.length > 0 ? p.screenshots : p.preview ? [p.preview] : []
  return (
    <div className="flex flex-col overflow-hidden rounded-xl border border-zinc-800 bg-zinc-900/40">
      {/* Skins are visual — the preview IS the product. Full-bleed hero. */}
      {shots.length > 0 && (
        <button onClick={() => onPreview(shots)} className="relative block aspect-video overflow-hidden">
          <SmartImg src={shots[0]} className="h-full w-full object-cover transition-transform hover:scale-105" loading="lazy" />
          <span className="absolute bottom-2 right-2 rounded-full bg-blue-500 px-2 py-0.5 text-[10px] font-semibold text-white">
            {t('market.preview')}
          </span>
        </button>
      )}
      <div className="flex flex-1 flex-col p-4">
        <div className="mb-1 flex items-start justify-between gap-2">
          <div className="min-w-0">
            <div className="truncate font-semibold text-zinc-100">{p.name}</div>
            <div className="truncate font-mono text-[11px] text-zinc-500">{pluginKey(p)}</div>
          </div>
          <span className="shrink-0 rounded-full bg-blue-500/10 px-2 py-0.5 text-[10px] font-medium text-blue-300">
            SKIN
          </span>
        </div>
        <p className="mb-3 line-clamp-2 text-xs text-zinc-400">{desc}</p>
        <div className="mt-auto flex items-center gap-2">
          {installed ? (
            <button
              onClick={() => void remove(installed.name)}
              disabled={busy}
              className="ml-auto rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
            >
              {t('market.remove')}
            </button>
          ) : (
            <button
              onClick={() => void install(p)}
              disabled={!!job || busy || !p.spec}
              className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
            >
              {job ? t(`market.install.${job.status}`) : t('market.install')}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

function SkillCard({
  plugin: p,
  installed,
  job,
  busy,
  install,
  remove,
}: {
  plugin: RegistryPlugin
  installed: boolean
  job?: Job
  busy: boolean
  install: (entry: RegistryPlugin) => Promise<boolean>
  remove: (id: string) => Promise<boolean>
}) {
  const t = useT()
  const desc = p.description.en || p.description.zh || ''
  return (
    <div className="flex flex-col rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
      <div className="mb-1 flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-semibold text-zinc-100">{p.name}</div>
          <div className="truncate font-mono text-[11px] text-zinc-500">{pluginKey(p)}</div>
        </div>
        <span className="shrink-0 rounded-full bg-zinc-800 px-2 py-0.5 text-[10px] text-zinc-400">
          SKILL
        </span>
      </div>
      <p className="mb-3 line-clamp-2 text-xs text-zinc-400">{desc}</p>
      <div className="mt-auto flex items-center gap-2">
        {installed ? (
          <button
            onClick={() => void remove(pluginKey(p))}
            disabled={busy}
            className="ml-auto rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
          >
            {t('market.remove')}
          </button>
        ) : (
          <button
            onClick={() => void install(p)}
            disabled={!!job || busy || !p.fetch}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {job ? t(`market.install.${job.status}`) : t('market.install')}
          </button>
        )}
      </div>
    </div>
  )
}

function McpCard({
  plugin: p,
  installed,
  job,
  busy,
  install,
  remove,
}: {
  plugin: RegistryPlugin
  installed: boolean
  job?: Job
  busy: boolean
  install: (entry: RegistryPlugin) => Promise<boolean>
  remove: (id: string) => Promise<boolean>
}) {
  const t = useT()
  const desc = p.description.en || p.description.zh || ''
  const transport = p.transport ?? 'stdio'
  const launch =
    transport === 'streamable-http' ? p.mcpUrl || '' : p.command || ''
  const installable = !!(p.command || p.mcpUrl)
  return (
    <div className="flex flex-col rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
      <div className="mb-1 flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-semibold text-zinc-100">{p.name}</div>
          <div className="truncate font-mono text-[11px] text-zinc-500">{pluginKey(p)}</div>
        </div>
        <span className="shrink-0 rounded-full bg-zinc-800 px-2 py-0.5 text-[10px] text-zinc-400">
          MCP
        </span>
      </div>
      <div className="mb-1 truncate font-mono text-[11px] text-zinc-500">
        {installable ? transport : 'manual'}
        {installable && launch ? ` · ${launch}` : ''}
      </div>
      <p className="mb-3 line-clamp-2 text-xs text-zinc-400">{desc}</p>
      <div className="mt-auto flex items-center gap-2">
        {installed ? (
          <button
            onClick={() => void remove(pluginKey(p))}
            disabled={busy}
            className="ml-auto rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
          >
            {t('market.remove')}
          </button>
        ) : installable ? (
          <button
            onClick={() => void install(p)}
            disabled={!!job || busy}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {job ? t(`market.install.${job.status}`) : t('market.install')}
          </button>
        ) : (
          <a
            href={p.url}
            target="_blank"
            rel="noreferrer"
            className="ml-auto rounded-lg border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:border-blue-500/50 hover:text-blue-300"
          >
            {t('market.openGithub')}
          </a>
        )}
      </div>
    </div>
  )
}

function PluginCard({
  plugin: p,
  installed,
  update,
  job,
  busy,
  install,
  remove,
  toggle,
  updateAction,
  onPreview,
}: {
  plugin: RegistryPlugin
  installed: InstalledPlugin | undefined
  update: PluginUpdate | undefined
  job?: Job
  busy: boolean
  install: (entry: RegistryPlugin) => Promise<boolean>
  remove: (name: string) => Promise<boolean>
  toggle: (name: string, enabled: boolean) => Promise<boolean>
  updateAction: (name: string) => Promise<boolean>
  onPreview: (urls: string[]) => void
}) {
  const t = useT()
  const desc = p.description.en || p.description.zh || ''
  const cat = p.category[0]
  // Themes carry a single `preview` image; plugins carry `screenshots`.
  const shots = p.screenshots.length > 0 ? p.screenshots : p.preview ? [p.preview] : []
  return (
    <div className="flex flex-col rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
      {shots.length > 0 && (
        <button onClick={() => onPreview(shots)} className="relative mb-3 overflow-hidden rounded-lg">
          <SmartImg src={shots[0]} className="h-32 w-full object-cover" loading="lazy" />
          {shots.length > 1 && (
            <span className="absolute bottom-2 right-2 rounded-full bg-black/70 px-2 py-0.5 text-[10px] text-zinc-200">
              {t('market.shots', { n: shots.length })}
            </span>
          )}
        </button>
      )}
      <div className="mb-1 flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-semibold text-zinc-100">{p.name}</div>
          <div className="truncate font-mono text-[11px] text-zinc-500">{pluginKey(p)}</div>
        </div>
        {cat && (
          <span className="shrink-0 rounded-full bg-zinc-800 px-2 py-0.5 text-[10px] text-zinc-400">
            {cat}
          </span>
        )}
      </div>
      <p className="mb-3 line-clamp-2 text-xs text-zinc-400">{desc}</p>
      <div className="mb-3 flex items-center gap-3 text-[11px] text-zinc-500">
        {p.stars != null && <span>⭐ {p.stars}</span>}
        {p.downloads != null && <span>↓ {p.downloads}</span>}
      </div>
      <div className="mt-auto flex flex-wrap items-center gap-2">
        {installed ? (
          <>
            {installed.toggleable && (
              <button
                onClick={() => void toggle(installed.name, !installed.enabled)}
                disabled={busy}
                className="rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-300 hover:text-zinc-100 disabled:opacity-40"
              >
                {installed.enabled ? t('market.disable') : t('market.enable')}
              </button>
            )}
            {update && update.updatable && (
              <button
                onClick={() => void updateAction(installed.name)}
                disabled={busy}
                className="rounded-lg border border-blue-500/40 px-2.5 py-1 text-xs text-blue-300 hover:bg-blue-500/10 disabled:opacity-40"
              >
                {t('market.update', { from: update.installed, to: update.latest })}
              </button>
            )}
            <button
              onClick={() => void remove(installed.name)}
              disabled={busy}
              className="ml-auto rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
            >
              {t('market.remove')}
            </button>
          </>
        ) : (
          <button
            onClick={() => void install(p)}
            disabled={!!job || busy || !p.spec}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {job ? t(`market.install.${job.status}`) : t('market.install')}
          </button>
        )}
      </div>
    </div>
  )
}
