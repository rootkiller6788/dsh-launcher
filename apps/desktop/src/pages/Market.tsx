import { useEffect, useMemo, useRef, useState, type ImgHTMLAttributes } from 'react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import type {
  BundleManifest,
  BundleSummary,
  ContentKind,
  InstalledPlugin,
  PluginUpdate,
  RecommendPlan,
  RegistryPlugin,
} from '../lib/types'

/** Stable identity: `owner/name` when an owner exists, else the bare name. */
function pluginKey(p: RegistryPlugin) {
  return p.owner ? `${p.owner}/${p.name}` : p.name
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
  const recommendations = useAppStore((s) => s.recommendations)
  const searching = useAppStore((s) => s.searching)
  const updates = useAppStore((s) => s.updates)
  const busy = useAppStore((s) => s.busy)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const loadRegistry = useAppStore((s) => s.loadRegistry)
  const refreshInstalledPlugins = useAppStore((s) => s.refreshInstalledPlugins)
  const refreshUpdates = useAppStore((s) => s.refreshUpdates)
  const recommend = useAppStore((s) => s.recommend)
  const installPlugin = useAppStore((s) => s.installPlugin)
  const uninstallPlugin = useAppStore((s) => s.uninstallPlugin)
  const togglePlugin = useAppStore((s) => s.togglePlugin)
  const updatePlugin = useAppStore((s) => s.updatePlugin)
  const installSkill = useAppStore((s) => s.installSkill)
  const uninstallSkill = useAppStore((s) => s.uninstallSkill)
  const refreshInstalledSkills = useAppStore((s) => s.refreshInstalledSkills)
  const installedMcps = useAppStore((s) => s.installedMcps)
  const installMcp = useAppStore((s) => s.installMcp)
  const uninstallMcp = useAppStore((s) => s.uninstallMcp)
  const refreshInstalledMcps = useAppStore((s) => s.refreshInstalledMcps)
  const importBundle = useAppStore((s) => s.importBundle)

  const [need, setNeed] = useState('')
  const [query, setQuery] = useState('')
  const [category, setCategory] = useState('')
  const [catOpen, setCatOpen] = useState(false)
  const [sort, setSort] = useState<SortKey>('stars')
  const [visible, setVisible] = useState(60)
  const [shuffleKey, setShuffleKey] = useState(0)
  const [preview, setPreview] = useState<{ urls: string[]; index: number } | null>(null)
  const [activeKind, setActiveKind] = useState<ContentKind>('plugin')

  useEffect(() => {
    void loadRegistry()
    void refreshInstalledPlugins()
    void refreshUpdates()
    void refreshInstalledSkills()
    void refreshInstalledMcps()
  }, [loadRegistry, refreshInstalledPlugins, refreshUpdates, refreshInstalledSkills, refreshInstalledMcps])

  useEffect(() => {
    void refreshInstalledPlugins()
    void refreshUpdates()
    void refreshInstalledSkills()
    void refreshInstalledMcps()
  }, [activeId, refreshInstalledPlugins, refreshUpdates, refreshInstalledSkills, refreshInstalledMcps])

  useEffect(() => {
    setVisible(60)
  }, [query, category])

  const match = (p: RegistryPlugin): InstalledPlugin | undefined =>
    installedPlugins.find((ip) =>
      p.npm ? ip.name === p.npm : ip.name.toLowerCase().includes(p.name.toLowerCase()),
    )

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

  return (
    <div className="mx-auto max-w-4xl p-8">
      <div className="mb-4 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{t('market.title')}</h1>
          <p className="text-sm text-zinc-400">
            {t('market.installsInto')}{' '}
            <span className="font-medium text-zinc-200">{activeInstance?.name ?? '—'}</span>
          </p>
        </div>
        {registry && (
          <span className="text-xs text-zinc-500">
            {registry.count} items · updated {registry.updated}
          </span>
        )}
      </div>

      {/* Content-type tabs */}
      <div className="mb-6 flex rounded-lg border border-zinc-800 bg-zinc-900/60 p-1">
        {KINDS.map((k) => (
          <button
            key={k.value}
            onClick={() => {
              setActiveKind(k.value)
              setVisible(60)
              setQuery('')
              setCategory('')
            }}
            className={`flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
              activeKind === k.value ? 'bg-blue-500 text-white' : 'text-zinc-400 hover:text-zinc-200'
            }`}
          >
            {t(k.label)}
          </button>
        ))}
      </div>

      {activeKind === 'skill' && (
        <p className="mb-6 rounded-lg border border-zinc-800 bg-zinc-900/40 px-4 py-2 text-xs text-zinc-500">
          {t('market.skillNote')}
        </p>
      )}

      {activeKind === 'mcp' && (
        <p className="mb-6 rounded-lg border border-zinc-800 bg-zinc-900/40 px-4 py-2 text-xs text-zinc-500">
          {t('market.mcpNote')}
        </p>
      )}

      {/* Smart search — bundle assistant: composes cross-kind plans over the
          full catalog (plugins + skins + skills + MCP) */}
      {activeKind === 'bundle' && (
      <div className="mb-6 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-5">
        <h2 className="mb-1 text-xs font-semibold uppercase tracking-wide text-zinc-500">
          {t('market.smartSearch')}
        </h2>
        <p className="mb-3 text-xs text-zinc-500">{t('market.smartHint')}</p>
        <div className="flex gap-2">
          <input
            value={need}
            onChange={(e) => setNeed(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && need.trim() && void recommend(need.trim())}
            placeholder={t('market.smartPlaceholder')}
            className="flex-1 rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-zinc-100 outline-none focus:border-blue-500"
          />
          <button
            onClick={() => need.trim() && void recommend(need.trim())}
            disabled={searching || !need.trim()}
            className="rounded-lg bg-blue-500 px-4 py-2 text-sm font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {searching ? t('market.thinking') : t('market.recommend')}
          </button>
        </div>

        {recommendations &&
          (recommendations.plans.length === 0 ? (
            <p className="mt-4 text-sm text-zinc-500">{t('market.noPlans')}</p>
          ) : (
            <div className="mt-4 space-y-3">
              {recommendations.plans.map((plan) => (
                <PlanCard key={plan.id} plan={plan} findItem={findItem} importBundle={importBundle} />
              ))}
            </div>
          ))}
        </div>
      )}

      {/* Browse */}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <h2 className="mr-auto text-xs font-semibold uppercase tracking-wide text-zinc-500">
          {t('market.browse')}
        </h2>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('market.filter')}
          className="w-52 rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-100 outline-none focus:border-blue-500"
        />
        <div className="relative">
          <button
            type="button"
            onClick={() => setCatOpen((o) => !o)}
            className="flex items-center gap-2 rounded-lg border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 outline-none focus:border-blue-500"
          >
            {category ? catLabel(category) : t('market.allCategories')}
            <span className="text-[10px] text-zinc-500">▾</span>
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
                {registry &&
                  Object.entries(registry.categories).map(([id]) => (
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
        <select
          value={sort}
          onChange={(e) => {
            setSort(e.target.value as SortKey)
            setShuffleKey(0)
          }}
          className="rounded-lg border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 outline-none focus:border-blue-500"
        >
          <option value="stars">{t('market.sortStars')}</option>
          <option value="new">{t('market.sortNew')}</option>
          <option value="name">{t('market.sortName')}</option>
        </select>
        <button
          onClick={() => setShuffleKey((k) => k + 1)}
          title="Shuffle (random order)"
          className="rounded-lg border border-blue-600 px-2.5 py-1.5 text-sm font-medium text-blue-300 hover:bg-blue-600/10"
        >
          {t('market.shuffle')}
        </button>
      </div>

      {registryError && (
        <div className="mb-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-300">
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
        <p className="text-sm text-zinc-500">{t('market.loading')}</p>
      )}

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        {shown.map((p) => {
          const key = pluginKey(p)
          if ((p.kind ?? 'plugin') === 'skill') {
            return (
              <SkillCard
                key={key}
                plugin={p}
                installed={installedSkills.includes(key)}
                busy={busy}
                install={installSkill}
                remove={uninstallSkill}
              />
            )
          }
          if ((p.kind ?? 'plugin') === 'mcp') {
            return (
              <McpCard
                key={key}
                plugin={p}
                installed={installedMcps.includes(key)}
                busy={busy}
                install={installMcp}
                remove={uninstallMcp}
              />
            )
          }
          if ((p.kind ?? 'plugin') === 'bundle') {
            return <BundleCard key={key} plugin={p} findItem={findItem} importBundle={importBundle} />
          }
          return (
            <PluginCard
              key={key}
              plugin={p}
              installed={match(p)}
              update={match(p) ? updateFor(match(p)!.name) : undefined}
              busy={busy}
              install={installPlugin}
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
      {registry && (
        <p className="mt-3 text-center text-xs text-zinc-600">
          {query.trim() || category
            ? t('market.matches', { n: filtered.length })
            : t('market.total', { n: filtered.length })}
          {shown.length < filtered.length ? t('market.showing', { n: shown.length }) : ''}
        </p>
      )}

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

function PlanCard({
  plan,
  findItem,
  importBundle,
}: {
  plan: RecommendPlan
  findItem: (kind: ContentKind, name: string) => RegistryPlugin | undefined
  importBundle: (manifest: BundleManifest) => Promise<BundleSummary | null>
}) {
  const t = useT()
  const busy = useAppStore((s) => s.busy)
  const installAll = async () => {
    const items = plan.items
      .map((it) => findItem(it.kind, it.name))
      .filter((p): p is RegistryPlugin => !!p)
    if (items.length === 0) return
    await importBundle({
      name: plan.title,
      version: '1.0.0',
      description: plan.rationale,
      items,
    })
  }
  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-950/50 p-4">
      <div className="mb-1 flex items-center gap-2">
        <span className="rounded bg-blue-600/20 px-2 py-0.5 text-xs font-bold text-blue-300">
          {plan.id}
        </span>
        <span className="font-semibold text-zinc-100">{plan.title}</span>
      </div>
      <p className="mb-3 text-xs text-zinc-400">{plan.rationale}</p>
      <div className="space-y-2">
        {plan.items.map((it) => (
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

function BundleCard({
  plugin: p,
  findItem,
  importBundle,
}: {
  plugin: RegistryPlugin
  findItem: (kind: ContentKind, name: string) => RegistryPlugin | undefined
  importBundle: (manifest: BundleManifest) => Promise<BundleSummary | null>
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
        <span className="rounded bg-purple-600/20 px-2 py-0.5 text-xs font-bold text-purple-300">
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

function SkillCard({
  plugin: p,
  installed,
  busy,
  install,
  remove,
}: {
  plugin: RegistryPlugin
  installed: boolean
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
            disabled={busy || !p.fetch}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {t('market.install')}
          </button>
        )}
      </div>
    </div>
  )
}

function McpCard({
  plugin: p,
  installed,
  busy,
  install,
  remove,
}: {
  plugin: RegistryPlugin
  installed: boolean
  busy: boolean
  install: (entry: RegistryPlugin) => Promise<boolean>
  remove: (entry: RegistryPlugin) => Promise<boolean>
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
            onClick={() => void remove(p)}
            disabled={busy}
            className="ml-auto rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
          >
            {t('market.remove')}
          </button>
        ) : installable ? (
          <button
            onClick={() => void install(p)}
            disabled={busy}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {t('market.install')}
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
  busy: boolean
  install: (spec: string) => Promise<boolean>
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
            <button
              onClick={() => void toggle(installed.name, !installed.enabled)}
              disabled={busy}
              className="rounded-lg border border-zinc-700 px-2.5 py-1 text-xs text-zinc-300 hover:text-zinc-100 disabled:opacity-40"
            >
              {installed.enabled ? t('market.disable') : t('market.enable')}
            </button>
            {update && update.updatable && (
              <button
                onClick={() => void updateAction(installed.name)}
                disabled={busy}
                className="rounded-lg border border-amber-600/60 px-2.5 py-1 text-xs text-amber-300 hover:bg-amber-600/10 disabled:opacity-40"
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
            onClick={() => p.spec && void install(p.spec)}
            disabled={busy || !p.spec}
            className="ml-auto rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            {t('market.install')}
          </button>
        )}
      </div>
    </div>
  )
}
