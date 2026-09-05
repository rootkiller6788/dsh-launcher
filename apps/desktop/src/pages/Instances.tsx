import { useEffect, useMemo, useRef, useState } from 'react'
import {
  Box,
  Boxes,
  Copy,
  FileUp,
  Folder,
  Layers3,
  Pencil,
  Play,
  Plus,
  Save,
  ShieldCheck,
  Trash2,
  X,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { ipc } from '../lib/ipc'
import { StatusDot } from '../components/StatusDot'
import type { ContentKind, EnvironmentPreviewResult, InstanceManifest, Job } from '../lib/types'

const KIND_LABEL: Record<ContentKind, string> = {
  plugin: 'market.tabPlugins',
  theme: 'market.tabThemes',
  skill: 'market.tabSkills',
  mcp: 'market.tabMcp',
  bundle: 'market.tabBundles',
}

function StatTile({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof Boxes
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

function DetailRow({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="min-w-0 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="text-[11px] uppercase tracking-wide text-zinc-500">{label}</div>
      <div className="mt-1 truncate text-sm font-medium text-zinc-200">{value}</div>
    </div>
  )
}

function ActionButton({
  icon: Icon,
  label,
  onClick,
  disabled,
  primary,
  danger,
}: {
  icon: typeof Play
  label: string
  onClick: () => void
  disabled?: boolean
  primary?: boolean
  danger?: boolean
}) {
  const tone = primary
    ? 'border-blue-400/30 bg-blue-500 text-white hover:bg-blue-400'
    : danger
      ? 'border-red-500/30 bg-red-500/10 text-red-300 hover:bg-red-500/15'
      : 'border-zinc-700 bg-zinc-800/50 text-zinc-200 hover:bg-zinc-800'
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`flex min-h-11 items-center justify-center gap-2 rounded-lg border px-3 py-2 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${tone}`}
    >
      <Icon className="h-4 w-4 shrink-0" strokeWidth={1.8} />
      <span className="truncate">{label}</span>
    </button>
  )
}

function InstanceRow({
  instance,
  selected,
  active,
  running,
  status,
  pluginCount,
  mcpCount,
  onSelect,
}: {
  instance: InstanceManifest
  selected: boolean
  active: boolean
  running: boolean
  status: string
  pluginCount: number
  mcpCount: number
  onSelect: () => void
}) {
  const t = useT()
  return (
    <button
      onClick={onSelect}
      className={`group w-full rounded-lg border p-4 text-left transition-colors ${
        selected
          ? 'border-blue-500/50 bg-blue-500/10'
          : 'border-zinc-800/80 bg-zinc-950/20 hover:border-zinc-700 hover:bg-zinc-800/30'
      }`}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <StatusDot status={running ? status : 'stopped'} />
            <span className="truncate text-sm font-semibold text-zinc-100">{instance.name}</span>
          </div>
          <div className="mt-1 truncate font-mono text-[11px] text-zinc-600">{instance.id}</div>
        </div>
        {active && (
          <span className="shrink-0 rounded-full bg-blue-500/10 px-2 py-1 text-[10px] font-medium uppercase tracking-wide text-blue-300">
            {t('instances.active')}
          </span>
        )}
      </div>
      <div className="mt-4 grid grid-cols-3 gap-2">
        <div className="rounded bg-zinc-900/60 px-2 py-1.5">
          <div className="text-[10px] text-zinc-600">{t('instances.runtime')}</div>
          <div className="truncate text-xs text-zinc-300">{instance.runtime.version || '-'}</div>
        </div>
        <div className="rounded bg-zinc-900/60 px-2 py-1.5">
          <div className="text-[10px] text-zinc-600">MCP</div>
          <div className="text-xs tabular-nums text-zinc-300">{mcpCount}</div>
        </div>
        <div className="rounded bg-zinc-900/60 px-2 py-1.5">
          <div className="text-[10px] text-zinc-600">{t('instances.plugins')}</div>
          <div className="text-xs tabular-nums text-zinc-300">{pluginCount}</div>
        </div>
      </div>
    </button>
  )
}

export function Instances() {
  const t = useT()
  const instances = useAppStore((s) => s.instances)
  const activeId = useAppStore((s) => s.activeId)
  const installedPlugins = useAppStore((s) => s.installedPlugins)
  const libraryInventory = useAppStore((s) => s.libraryInventory)
  const runningId = useAppStore((s) => s.runningId)
  const processState = useAppStore((s) => s.processState)
  const busy = useAppStore((s) => s.busy)
  const setError = useAppStore((s) => s.setError)
  const createInstance = useAppStore((s) => s.createInstance)
  const renameInstance = useAppStore((s) => s.renameInstance)
  const cloneInstance = useAppStore((s) => s.cloneInstance)
  const switchInstance = useAppStore((s) => s.switchInstance)
  const deleteInstance = useAppStore((s) => s.deleteInstance)
  const importEnvironment = useAppStore((s) => s.importEnvironment)
  const importEnvironmentPackage = useAppStore((s) => s.importEnvironmentPackage)
  const openJobInInstalls = useAppStore((s) => s.openJobInInstalls)

  const [newName, setNewName] = useState('')
  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(activeId)
  const [renaming, setRenaming] = useState(false)
  const [renameDraft, setRenameDraft] = useState('')
  const [cloning, setCloning] = useState(false)
  const [cloneDraft, setCloneDraft] = useState('')
  const [deleteTarget, setDeleteTarget] = useState<InstanceManifest | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [importPath, setImportPath] = useState('')
  const [importName, setImportName] = useState('')
  const [importResult, setImportResult] = useState<Job | null>(null)
  const [importPreview, setImportPreview] = useState<EnvironmentPreviewResult | null>(null)
  const [importBytes, setImportBytes] = useState<number[] | null>(null)

  useEffect(() => {
    if (!selectedId || !instances.some((i) => i.id === selectedId)) {
      setSelectedId(activeId ?? instances[0]?.id ?? null)
    }
  }, [activeId, instances, selectedId])

  const selected = useMemo(
    () => instances.find((i) => i.id === selectedId) ?? instances.find((i) => i.id === activeId) ?? instances[0] ?? null,
    [activeId, instances, selectedId],
  )
  const selectedRunning = !!selected && selected.id === runningId
  const selectedActive = !!selected && selected.id === activeId
  const runningCount = runningId ? 1 : 0
  const pluginCountFor = (instance: InstanceManifest) =>
    libraryInventory[instance.id]?.plugins ??
    (instance.id === activeId ? installedPlugins.length || instance.plugins.length : instance.plugins.length)
  const mcpCountFor = (instance: InstanceManifest) => libraryInventory[instance.id]?.mcp ?? instance.mcp.length
  const skillCountFor = (instance: InstanceManifest) => libraryInventory[instance.id]?.skills ?? instance.skills.length
  const selectedPluginCount = selected ? pluginCountFor(selected) : 0
  const selectedMcpCount = selected ? mcpCountFor(selected) : 0
  const selectedSkillCount = selected ? skillCountFor(selected) : 0
  const totalPlugins = instances.reduce((sum, i) => sum + pluginCountFor(i), 0)
  const totalMcps = instances.reduce((sum, i) => sum + mcpCountFor(i), 0)

  const create = () => {
    const name = newName.trim()
    if (!name) return
    void createInstance(name).then((ok) => {
      if (ok) setNewName('')
    })
  }

  const saveRename = () => {
    if (!selected || !renameDraft.trim()) return
    void renameInstance(selected.id, renameDraft.trim()).then((ok) => {
      if (ok) setRenaming(false)
    })
  }

  const saveClone = () => {
    if (!selected) return
    const name = cloneDraft.trim() || t('instances.copyDefault', { name: selected.name })
    void cloneInstance(selected.id, name).then((ok) => {
      if (ok) {
        setCloning(false)
        setCloneDraft('')
      }
    })
  }

  const runImport = () => {
    const name = importName.trim() || null
    const task = importBytes
      ? importEnvironmentPackage(importBytes, name)
      : importPath.trim()
        ? importEnvironment(importPath.trim(), name)
        : Promise.resolve(null)
    void task.then((job) => {
      if (!job) return
      setImportResult(job)
      setSelectedId(job.instanceId)
      setImportPath('')
      setImportName('')
      setImportBytes(null)
      setImportPreview(null)
    })
  }

  const choosePackage = () => fileInputRef.current?.click()

  const onPackageFile = async (file: File | null) => {
    setImportResult(null)
    setImportPreview(null)
    setImportBytes(null)
    if (!file) return
    try {
      const bytes = Array.from(new Uint8Array(await file.arrayBuffer()))
      const preview = await ipc.environmentPreview(bytes)
      setImportBytes(bytes)
      setImportPath(file.name)
      setImportPreview(preview)
      if (!importName.trim()) setImportName(preview.name)
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('instances.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">{t('instances.subtitle')}</p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => {
              setImportOpen(true)
              setImportResult(null)
              setImportPreview(null)
              setImportBytes(null)
            }}
            className="flex min-h-11 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-4 text-sm font-semibold text-zinc-200 hover:border-blue-500/40 hover:text-blue-300"
          >
            <FileUp className="h-4 w-4" strokeWidth={1.8} />
            {t('instances.import')}
          </button>
        <div className="flex w-[420px] max-w-[42vw] items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 p-2">
          <input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && create()}
            placeholder={t('instances.newPlaceholder')}
            className="min-w-0 flex-1 rounded-md border border-transparent bg-zinc-950/50 px-3 py-2 text-sm text-zinc-100 outline-none focus:border-blue-500"
          />
          <button
            onClick={create}
            disabled={busy || !newName.trim()}
            className="flex min-h-9 items-center gap-2 rounded-md bg-blue-500 px-3 text-sm font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
          >
            <Plus className="h-4 w-4" strokeWidth={2} />
            {t('instances.new')}
          </button>
        </div>
        </div>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-12 gap-5">
        <section className="col-span-5 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="grid grid-cols-3 gap-3">
            <StatTile icon={Boxes} label={t('instances.total')} value={instances.length} />
            <StatTile icon={Play} label={t('instances.running')} value={runningCount} />
            <StatTile icon={Layers3} label={t('instances.extensions')} value={totalPlugins + totalMcps} />
          </div>

          <div className="mt-5 flex items-center justify-between">
            <h2 className="text-sm font-semibold text-zinc-200">{t('instances.fleet')}</h2>
            <span className="text-xs text-zinc-600">{t('instances.manifests')}</span>
          </div>

          {instances.length === 0 ? (
            <p className="flex flex-1 items-center justify-center text-center text-sm text-zinc-500">
              {t('instances.empty')}
            </p>
          ) : (
            <div className="mt-3 min-h-0 flex-1 space-y-3 overflow-y-auto pr-1">
              {instances.map((instance) => {
                const isActive = instance.id === activeId
                const isRunning = instance.id === runningId
                return (
                  <InstanceRow
                    key={instance.id}
                    instance={instance}
                    selected={instance.id === selected?.id}
                    active={isActive}
                    running={isRunning}
                    status={processState?.status ?? 'running'}
                    pluginCount={pluginCountFor(instance)}
                    mcpCount={mcpCountFor(instance)}
                    onSelect={() => {
                      setSelectedId(instance.id)
                      setRenaming(false)
                      setCloning(false)
                    }}
                  />
                )
              })}
            </div>
          )}
        </section>

        <section className="col-span-7 flex min-h-0 flex-col overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60">
          {selected ? (
            <>
              <div className="flex min-h-0 flex-1">
                <div className={`w-1.5 shrink-0 ${selectedRunning ? 'bg-amber-400' : selectedActive ? 'bg-blue-400' : 'bg-zinc-700'}`} />
                <div className="flex min-w-0 flex-1 flex-col p-5">
                  <div className="flex items-start justify-between gap-5">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-zinc-500">
                        <Box className="h-3.5 w-3.5" strokeWidth={1.75} />
                        {t('instances.selected')}
                      </div>
                      {renaming ? (
                        <div className="mt-2 flex max-w-xl items-center gap-2">
                          <input
                            autoFocus
                            value={renameDraft}
                            onChange={(e) => setRenameDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter') saveRename()
                              if (e.key === 'Escape') setRenaming(false)
                            }}
                            className="min-w-0 flex-1 rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-xl font-semibold text-zinc-100 outline-none focus:border-blue-500"
                          />
                          <button
                            onClick={saveRename}
                            disabled={busy || !renameDraft.trim()}
                            className="flex h-11 w-11 items-center justify-center rounded-lg bg-blue-500 text-white hover:bg-blue-400 disabled:opacity-40"
                            title={t('instances.save')}
                          >
                            <Save className="h-4 w-4" strokeWidth={1.8} />
                          </button>
                          <button
                            onClick={() => setRenaming(false)}
                            className="flex h-11 w-11 items-center justify-center rounded-lg border border-zinc-700 bg-zinc-800/50 text-zinc-300 hover:bg-zinc-800"
                            title="Cancel"
                          >
                            <X className="h-4 w-4" strokeWidth={1.8} />
                          </button>
                        </div>
                      ) : (
                        <h2 className="mt-2 truncate text-3xl font-semibold leading-tight text-zinc-50">
                          {selected.name}
                        </h2>
                      )}
                      <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                        <span className="rounded bg-zinc-800/60 px-2 py-1 font-mono">{selected.id}</span>
                        <span>{selectedActive ? t('instances.active') : t('instances.standby')}</span>
                        <span className="text-zinc-700">/</span>
                        <span>{selectedRunning ? t(`status.${processState?.status ?? 'running'}`) : t('status.stopped')}</span>
                      </div>
                    </div>
                    <div className="shrink-0 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3 text-right">
                      <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('instances.provider')}</div>
                      <div className="mt-1 text-sm font-medium text-zinc-200">{selected.providerRef}</div>
                    </div>
                  </div>

                  <div className="mt-6 grid grid-cols-4 gap-3">
                    <DetailRow label={t('instances.runtime')} value={`DSH ${selected.runtime.version || '-'}`} />
                    <DetailRow label={t('instances.plugins')} value={selectedPluginCount} />
                    <DetailRow label="MCP" value={selectedMcpCount} />
                    <DetailRow label="Skills" value={selectedSkillCount} />
                  </div>

                  <div className="mt-4 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
                    <div className="flex items-center gap-2 text-[11px] uppercase tracking-wide text-zinc-500">
                      <Folder className="h-3.5 w-3.5" strokeWidth={1.75} />
                      {t('instances.workspace')}
                    </div>
                    <div className="mt-2 truncate font-mono text-xs text-zinc-400">{selected.workspace}</div>
                  </div>

                  {cloning && (
                    <div className="mt-4 flex items-center gap-2 rounded-lg border border-blue-500/30 bg-blue-500/10 p-3">
                      <Copy className="h-4 w-4 shrink-0 text-blue-300" strokeWidth={1.75} />
                      <input
                        autoFocus
                        value={cloneDraft}
                        onChange={(e) => setCloneDraft(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') saveClone()
                          if (e.key === 'Escape') setCloning(false)
                        }}
                        placeholder={t('instances.copyDefault', { name: selected.name })}
                        className="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus:border-blue-500"
                      />
                      <button
                        onClick={saveClone}
                        disabled={busy}
                        className="rounded-md bg-blue-500 px-3 py-2 text-sm font-medium text-white hover:bg-blue-400 disabled:opacity-40"
                      >
                        {t('instances.clone')}
                      </button>
                    </div>
                  )}

                  <div className="mt-auto grid grid-cols-4 gap-2">
                    <ActionButton
                      icon={ShieldCheck}
                      label={t('instances.switch')}
                      onClick={() => void switchInstance(selected.id)}
                      disabled={busy || selectedActive}
                      primary={!selectedActive}
                    />
                    <ActionButton
                      icon={Pencil}
                      label={t('instances.rename')}
                      onClick={() => {
                        setRenaming(true)
                        setCloning(false)
                        setRenameDraft(selected.name)
                      }}
                      disabled={busy}
                    />
                    <ActionButton
                      icon={Copy}
                      label={t('instances.clone')}
                      onClick={() => {
                        setCloning((open) => !open)
                        setRenaming(false)
                      }}
                      disabled={busy}
                    />
                    <ActionButton
                      icon={Trash2}
                      label={t('instances.delete')}
                      onClick={() => setDeleteTarget(selected)}
                      disabled={busy || selectedRunning}
                      danger
                    />
                  </div>
                </div>
              </div>
            </>
          ) : (
            <p className="flex flex-1 items-center justify-center text-center text-sm text-zinc-500">
              {t('instances.empty')}
            </p>
          )}
        </section>
      </div>

      {deleteTarget && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-zinc-950/70 px-6">
          <div className="w-full max-w-md rounded-lg border border-zinc-800 bg-zinc-900 p-5 shadow-2xl shadow-black/40">
            <div className="flex items-start gap-4">
              <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg border border-red-500/30 bg-red-500/10 text-red-300">
                <Trash2 className="h-5 w-5" strokeWidth={1.8} />
              </div>
              <div className="min-w-0 flex-1">
                <h2 className="text-base font-semibold text-zinc-100">
                  {t('instances.deleteTitle')}
                </h2>
                <p className="mt-2 text-sm leading-6 text-zinc-400">
                  {t('instances.deleteConfirm', { name: deleteTarget.name })}
                </p>
              </div>
            </div>

            <div className="mt-4 rounded-lg border border-zinc-800/70 bg-zinc-950/35 px-3 py-2">
              <div className="text-[10px] uppercase tracking-wide text-zinc-600">
                {t('instances.workspace')}
              </div>
              <div className="mt-1 truncate font-mono text-xs text-zinc-400">
                {deleteTarget.workspace}
              </div>
            </div>

            <div className="mt-5 flex justify-end gap-2">
              <button
                onClick={() => setDeleteTarget(null)}
                className="rounded-lg border border-zinc-700 bg-zinc-800/50 px-4 py-2 text-sm font-medium text-zinc-200 hover:bg-zinc-800"
              >
                {t('instances.cancel')}
              </button>
              <button
                onClick={() => {
                  const id = deleteTarget.id
                  setDeleteTarget(null)
                  void deleteInstance(id)
                }}
                disabled={busy}
                className="rounded-lg border border-red-500/30 bg-red-500/10 px-4 py-2 text-sm font-semibold text-red-300 hover:bg-red-500/15 disabled:opacity-40"
              >
                {t('instances.delete')}
              </button>
            </div>
          </div>
        </div>
      )}

      {importOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-zinc-950/70 px-6">
          <div className="w-full max-w-lg rounded-lg border border-zinc-800 bg-zinc-900 p-5 shadow-2xl shadow-black/40">
            <div className="flex items-start gap-4">
              <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg border border-blue-500/30 bg-blue-500/10 text-blue-300">
                <FileUp className="h-5 w-5" strokeWidth={1.8} />
              </div>
              <div className="min-w-0 flex-1">
                <h2 className="text-base font-semibold text-zinc-100">
                  {t('instances.importTitle')}
                </h2>
                <p className="mt-2 text-sm leading-6 text-zinc-400">
                  {t('instances.importHint')}
                </p>
              </div>
            </div>

            <div className="mt-5 space-y-3">
              <input
                ref={fileInputRef}
                type="file"
                accept=".dshenv"
                className="hidden"
                onChange={(e) => void onPackageFile(e.target.files?.[0] ?? null)}
              />
              <button
                onClick={choosePackage}
                className="flex w-full items-center justify-center gap-2 rounded-lg border border-blue-500/30 bg-blue-500/10 px-4 py-3 text-sm font-semibold text-blue-200 hover:bg-blue-500/15"
              >
                <FileUp className="h-4 w-4" strokeWidth={1.8} />
                {t('instances.choosePackage')}
              </button>
              <input
                value={importPath}
                onChange={(e) => setImportPath(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && runImport()}
                placeholder={t('instances.importPathPlaceholder')}
                className="w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500"
              />
              <input
                value={importName}
                onChange={(e) => setImportName(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && runImport()}
                placeholder={t('instances.importNamePlaceholder')}
                className="w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus:border-blue-500"
              />
            </div>

            {importPreview && (
              <div className="mt-4 rounded-lg border border-zinc-800/70 bg-zinc-950/35 p-4">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-semibold text-zinc-100">{importPreview.name}</div>
                    <div className="mt-1 line-clamp-2 text-xs leading-5 text-zinc-500">{importPreview.description}</div>
                  </div>
                  <div className="shrink-0 rounded-full bg-blue-500/10 px-2 py-1 text-[11px] font-medium text-blue-300">
                    {importPreview.itemCount} items
                  </div>
                </div>
                <div className="mt-4 grid grid-cols-4 gap-2">
                  <DetailRow label="Plugins" value={importPreview.plugins} />
                  <DetailRow label="Skins" value={importPreview.skins} />
                  <DetailRow label="Skills" value={importPreview.skills} />
                  <DetailRow label="MCP" value={importPreview.mcps} />
                </div>

                {importPreview.items.length > 0 && (
                  <div className="mt-3 max-h-40 space-y-1 overflow-y-auto pr-1">
                    {importPreview.items.map((it, i) => (
                      <div key={`${it.kind}:${it.name}:${i}`} className="flex items-center gap-2 text-xs">
                        <span className="shrink-0 rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] uppercase text-zinc-400">
                          {t(KIND_LABEL[it.kind])}
                        </span>
                        <span className="truncate font-mono text-zinc-200">{it.name}</span>
                        <span className="ml-auto shrink-0 truncate font-mono text-[10px] text-zinc-500">
                          {it.version ? `${it.version} · ` : ''}{it.source || '—'}
                        </span>
                      </div>
                    ))}
                  </div>
                )}

                {importPreview.compatibleWith && (
                  <div className="mt-3 text-[11px] text-zinc-500">
                    {t('instances.previewCompatible', { version: importPreview.compatibleWith })}
                  </div>
                )}

                {importPreview.conflicts.length > 0 && (
                  <div className="mt-3 rounded-md border border-amber-500/30 bg-amber-500/10 p-2">
                    <div className="text-[11px] font-semibold text-amber-300">
                      {t('instances.previewConflicts')}
                    </div>
                    <ul className="mt-1 space-y-0.5 text-[11px] text-amber-200/80">
                      {importPreview.conflicts.map((c) => (
                        <li key={c} className="truncate">{c}</li>
                      ))}
                    </ul>
                  </div>
                )}

                {importPreview.missingTokens.length > 0 && (
                  <div className="mt-2 rounded-md border border-red-500/30 bg-red-500/10 p-2">
                    <div className="text-[11px] font-semibold text-red-300">
                      {t('instances.previewMissingTokens')}
                    </div>
                    <ul className="mt-1 space-y-0.5 text-[11px] text-red-200/80">
                      {importPreview.missingTokens.map((m) => (
                        <li key={m} className="truncate font-mono">{m}</li>
                      ))}
                    </ul>
                  </div>
                )}

                <div className="mt-3 truncate font-mono text-[10px] text-zinc-600">
                  sha256:{importPreview.checksum}
                </div>
              </div>
            )}

            {importResult && (
              <div className="mt-4 rounded-lg border border-blue-500/20 bg-blue-500/10 p-3">
                <div className="text-xs text-blue-300">
                  {t('instances.importQueued', { name: importResult.label })}
                </div>
                <button
                  onClick={() => openJobInInstalls()}
                  className="mt-3 flex h-9 items-center gap-2 rounded-md bg-blue-500 px-3 text-xs font-semibold text-zinc-950 hover:bg-blue-400"
                >
                  {t('instances.viewInstallCenter')}
                </button>
              </div>
            )}

            <div className="mt-5 flex justify-end gap-2">
              <button
                onClick={() => setImportOpen(false)}
                className="rounded-lg border border-zinc-700 bg-zinc-800/50 px-4 py-2 text-sm font-medium text-zinc-200 hover:bg-zinc-800"
              >
                {t('instances.cancel')}
              </button>
              <button
                onClick={runImport}
                disabled={busy || (!importBytes && !importPath.trim())}
                className="rounded-lg bg-blue-500 px-4 py-2 text-sm font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
              >
                {busy ? t('settings.rtBusy') : t('instances.import')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
