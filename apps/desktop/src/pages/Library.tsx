import { useEffect, useState } from 'react'
import {
  Boxes,
  CheckCircle2,
  Layers3,
  PackageCheck,
  Puzzle,
  RefreshCw,
  Shield,
  Sparkles,
  Trash2,
  Waypoints,
  type LucideIcon,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { InstallCenter } from '../components/InstallCenter'
import type { ContentKind, LibraryInventoryItem, RegistryPlugin } from '../lib/types'

type LibraryKind = 'plugin' | 'skill' | 'mcp' | 'theme'

const TABS: { id: LibraryKind; icon: LucideIcon; label: string }[] = [
  { id: 'plugin', icon: Puzzle, label: 'library.plugins' },
  { id: 'skill', icon: Sparkles, label: 'library.skills' },
  { id: 'mcp', icon: Waypoints, label: 'library.mcp' },
  { id: 'theme', icon: Layers3, label: 'library.skins' },
]

const TONES: Record<
  LibraryKind,
  {
    icon: string
    chip: string
    active: string
  }
> = {
  plugin: {
    icon: 'bg-blue-500/10 text-blue-300',
    chip: 'text-blue-300',
    active: 'border-blue-500/40 bg-blue-500/10 text-blue-200',
  },
  skill: {
    icon: 'bg-violet-500/10 text-violet-300',
    chip: 'text-violet-300',
    active: 'border-violet-500/40 bg-violet-500/10 text-violet-200',
  },
  mcp: {
    icon: 'bg-cyan-500/10 text-cyan-300',
    chip: 'text-cyan-300',
    active: 'border-cyan-500/40 bg-cyan-500/10 text-cyan-200',
  },
  theme: {
    icon: 'bg-fuchsia-500/10 text-fuchsia-300',
    chip: 'text-fuchsia-300',
    active: 'border-fuchsia-500/40 bg-fuchsia-500/10 text-fuchsia-200',
  },
}

function pluginKey(p: RegistryPlugin) {
  return p.owner ? `${p.owner}/${p.name}` : p.name
}

function findCatalogEntry(
  registry: RegistryPlugin[],
  kind: ContentKind,
  id: string,
) {
  return registry.find((entry) => (entry.kind ?? 'plugin') === kind && pluginKey(entry) === id)
}

function itemKind(kind: ContentKind): LibraryKind {
  return kind === 'theme' ? 'theme' : kind === 'skill' ? 'skill' : kind === 'mcp' ? 'mcp' : 'plugin'
}

function sourceLabelKey(source: LibraryInventoryItem['source']) {
  return `library.source.${source}`
}

function stateSourceLabelKey(source: LibraryInventoryItem['stateSource']) {
  return `library.stateSource.${source}`
}

function SectionStat({
  label,
  value,
}: {
  label: string
  value: string | number
}) {
  return (
    <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="text-[11px] uppercase tracking-wide text-zinc-500">{label}</div>
      <div className="mt-1 truncate text-2xl font-semibold tabular-nums text-zinc-100">{value}</div>
    </div>
  )
}

function AssetRow({
  kind,
  icon: Icon,
  title,
  meta,
  status,
  source,
  detail,
  enabled,
  busy,
  onToggle,
  onUpdate,
  onRemove,
}: {
  kind: LibraryKind
  icon: LucideIcon
  title: string
  meta: string
  status: string
  source: string
  detail?: string
  enabled?: boolean
  busy?: boolean
  onToggle?: () => void
  onUpdate?: () => void
  onRemove?: () => void
}) {
  const t = useT()
  const tone = TONES[kind]
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="flex min-w-0 items-center gap-3">
        <div className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg ${tone.icon}`}>
          <Icon className="h-4 w-4" strokeWidth={1.75} />
        </div>
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold text-zinc-100">{title}</div>
          <div className="mt-0.5 truncate text-xs text-zinc-500">
            {meta}
            {detail ? ` · ${detail}` : ''}
          </div>
        </div>
      </div>

      <div className="flex items-center gap-2">
        <span className="rounded-full border border-zinc-800 bg-zinc-950/50 px-2 py-1 text-[10px] font-medium text-zinc-400">
          {source}
        </span>
        <span
          className={`rounded-full px-2 py-1 text-[10px] font-medium uppercase tracking-wide ${
            enabled === false
              ? 'bg-zinc-500/15 text-zinc-400'
              : 'bg-emerald-500/10 text-emerald-400'
          }`}
        >
          {status}
        </span>
        {onToggle && (
          <button
            onClick={onToggle}
            disabled={busy}
            className="h-8 rounded-lg border border-zinc-800 px-3 text-xs font-medium text-zinc-300 hover:border-blue-500/40 hover:text-blue-200 disabled:cursor-not-allowed disabled:opacity-45"
          >
            {enabled ? t('library.disable') : t('library.enable')}
          </button>
        )}
        {onUpdate && (
          <button
            onClick={onUpdate}
            disabled={busy}
            className="flex h-8 items-center gap-1.5 rounded-lg border border-zinc-800 px-3 text-xs font-medium text-zinc-300 hover:border-blue-500/40 hover:text-blue-200 disabled:cursor-not-allowed disabled:opacity-45"
          >
            <RefreshCw className="h-3.5 w-3.5" strokeWidth={1.75} />
            {t('library.update')}
          </button>
        )}
        {onRemove && (
          <button
            onClick={onRemove}
            disabled={busy}
            title={t('library.remove')}
            className="flex h-8 w-8 items-center justify-center rounded-lg border border-zinc-800 text-zinc-500 hover:border-red-500/40 hover:text-red-300 disabled:cursor-not-allowed disabled:opacity-45"
          >
            <Trash2 className="h-3.5 w-3.5" strokeWidth={1.75} />
          </button>
        )}
      </div>
    </div>
  )
}

export function Library() {
  const t = useT()
  const registry = useAppStore((s) => s.registry)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const libraryDetail = useAppStore((s) => s.libraryDetail)
  const installJobs = useAppStore((s) => s.installJobs)
  const updates = useAppStore((s) => s.updates)
  const busy = useAppStore((s) => s.busy)
  const loadRegistry = useAppStore((s) => s.loadRegistry)
  const refreshLibraryDetail = useAppStore((s) => s.refreshLibraryDetail)
  const reconcileLibraryInventory = useAppStore((s) => s.reconcileLibraryInventory)
  const refreshUpdates = useAppStore((s) => s.refreshUpdates)
  const togglePlugin = useAppStore((s) => s.togglePlugin)
  const updatePlugin = useAppStore((s) => s.updatePlugin)
  const uninstallPlugin = useAppStore((s) => s.uninstallPlugin)
  const uninstallSkill = useAppStore((s) => s.uninstallSkill)
  const uninstallMcp = useAppStore((s) => s.uninstallMcp)
  const [kind, setKind] = useState<LibraryKind>('plugin')

  useEffect(() => {
    void loadRegistry()
    void refreshLibraryDetail()
  }, [activeId, loadRegistry, refreshLibraryDetail])

  const catalog = registry?.plugins ?? []
  const inventoryItems =
    libraryDetail?.instanceId === activeId
      ? libraryDetail.items
      : []
  const plugins = inventoryItems.filter((item) => item.kind === 'plugin')
  const skins = inventoryItems.filter((item) => item.kind === 'theme')
  const skills = inventoryItems.filter((item) => item.kind === 'skill')
  const mcps = inventoryItems.filter((item) => item.kind === 'mcp')
  const total = inventoryItems.length
  const inventoryCount = inventoryItems.filter((item) => item.stateSource === 'dshInventory').length
  const jobCount = Object.keys(installJobs).length

  const refreshAll = () => {
    void (async () => {
      await reconcileLibraryInventory()
      await refreshUpdates()
    })()
  }

  const renderRows = () => {
    const visible =
      kind === 'plugin' ? plugins : kind === 'theme' ? skins : kind === 'skill' ? skills : mcps
    if (visible.length === 0) return null
    return visible.map((item) => {
      const rowKind = itemKind(item.kind)
      const Icon = rowKind === 'plugin' ? Puzzle : rowKind === 'theme' ? Layers3 : rowKind === 'skill' ? Sparkles : Waypoints
      const catalogEntry = item.market?.key ? findCatalogEntry(catalog, item.kind, item.market.key) : undefined
      const update = item.packageName
        ? updates.find((u) => u.name === item.packageName && u.updatable)
        : undefined
      const removablePlugin =
        item.packageName && item.source !== 'dshNative'
          ? () => void uninstallPlugin(item.packageName!)
          : undefined
      const removableSkill = item.kind === 'skill' ? () => void uninstallSkill(item.id) : undefined
      const mcpEntry = item.kind === 'mcp' ? findCatalogEntry(catalog, 'mcp', item.id) : undefined
      return (
        <AssetRow
          key={`${item.kind}:${item.id}`}
          kind={rowKind}
          icon={Icon}
          title={item.title}
          meta={
            catalogEntry?.category.join(' / ') ||
            item.packageName ||
            (item.kind === 'skill'
              ? t('library.filesystem')
              : item.kind === 'mcp'
                ? mcpEntry?.transport ?? t('library.mcpPatch')
                : item.kind === 'theme'
                  ? t('library.skinPlugin')
                  : t('library.package'))
          }
          status={
            item.enabled === false
              ? t('library.disabled')
              : item.kind === 'theme' && item.enabled
                ? t('library.active')
                : item.enabled === true
                  ? t('library.enabled')
                  : t('library.installed')
          }
          source={t(sourceLabelKey(item.source))}
          detail={`${t(stateSourceLabelKey(item.stateSource))}${item.detail ? ` · ${item.detail}` : ''}`}
          enabled={item.enabled ?? undefined}
          busy={busy}
          onToggle={item.toggleable && item.packageName ? () => void togglePlugin(item.packageName!, !item.enabled) : undefined}
          onUpdate={update && item.packageName ? () => void updatePlugin(item.packageName!) : undefined}
          onRemove={
            item.kind === 'skill'
              ? removableSkill
              : item.kind === 'mcp'
                ? mcpEntry ? () => void uninstallMcp(mcpEntry) : undefined
                : removablePlugin
          }
        />
      )
    })
  }

  const rows = renderRows()

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('library.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">
            {t('library.subtitle')}{' '}
            <span className="font-medium text-zinc-200">{activeInstance?.name ?? '-'}</span>
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={refreshAll}
            className="flex min-h-10 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 text-sm font-medium text-zinc-300 hover:border-blue-500/40 hover:text-blue-200"
          >
            <RefreshCw className="h-4 w-4" strokeWidth={1.75} />
            {t('overview.refreshChecks')}
          </button>
        </div>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-12 gap-5">
        <aside className="col-span-4 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="grid grid-cols-3 gap-3">
            <SectionStat label={t('library.total')} value={total} />
            <SectionStat label={t('library.enabled')} value={inventoryItems.filter((item) => item.enabled).length} />
            <SectionStat label="DSH Inventory" value={inventoryCount} />
          </div>

            <div className="mt-5 space-y-2">
            {TABS.map(({ id, icon: Icon, label }) => {
              const tone = TONES[id]
              const count =
                id === 'plugin'
                  ? plugins.length
                  : id === 'theme'
                    ? skins.length
                    : id === 'skill'
                      ? skills.length
                      : mcps.length
              return (
                <button
                  key={id}
                  onClick={() => setKind(id)}
                  className={`grid w-full grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3 rounded-lg border px-3 py-3 text-left transition-colors ${
                    kind === id
                      ? tone.active
                      : 'border-zinc-800/70 bg-zinc-950/20 text-zinc-400 hover:border-zinc-700 hover:text-zinc-200'
                  }`}
                >
                  <Icon className={`h-4 w-4 ${kind === id ? '' : tone.chip}`} strokeWidth={1.75} />
                  <span className="truncate text-sm font-medium">{t(label)}</span>
                  <span className="font-mono text-xs tabular-nums text-zinc-500">{count}</span>
                </button>
              )
            })}
          </div>

          <div className="mt-5 rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
            <InstallCenter embedded />
          </div>

          <div className="mt-auto rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
            <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-zinc-500">
              <Shield className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('library.boundary')}
            </div>
            <p className="mt-3 text-xs leading-5 text-zinc-400">
              {t('library.boundaryText')}
            </p>
          </div>
        </aside>

        <section className="col-span-8 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="flex shrink-0 items-center justify-between gap-4 border-b border-zinc-800 pb-4">
            <div className="flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-blue-500/10 text-blue-300">
                <PackageCheck className="h-5 w-5" strokeWidth={1.75} />
              </div>
              <div>
                <h2 className="text-sm font-semibold text-zinc-100">
                  {t(TABS.find((tab) => tab.id === kind)?.label ?? 'library.title')}
                </h2>
                <p className="mt-0.5 text-xs text-zinc-500">{t(`library.${kind}Hint`)}</p>
              </div>
            </div>
            <div className="flex items-center gap-2 text-xs text-zinc-500">
              <CheckCircle2 className="h-4 w-4 text-emerald-400" strokeWidth={1.75} />
              {activeId ? t('library.instanceScoped') : t('library.noInstance')}
            </div>
          </div>

          <div className="mt-4 min-h-0 flex-1 overflow-y-auto pr-1">
            {rows ? (
              <div className="space-y-2">{rows}</div>
            ) : (
              <div className="flex h-full items-center justify-center rounded-lg border border-dashed border-zinc-800 bg-zinc-950/20 text-center">
                <div>
                  <Boxes className="mx-auto h-8 w-8 text-zinc-700" strokeWidth={1.5} />
                  <p className="mt-3 text-sm font-medium text-zinc-400">{t('library.empty')}</p>
                  <p className="mt-1 text-xs text-zinc-600">{t('library.emptyHint')}</p>
                </div>
              </div>
            )}
          </div>
        </section>
      </div>
    </div>
  )
}
