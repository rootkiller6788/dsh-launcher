import { useState } from 'react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { StatusDot } from '../components/StatusDot'
import type { InstanceManifest } from '../lib/types'

export function Instances() {
  const t = useT()
  const instances = useAppStore((s) => s.instances)
  const activeId = useAppStore((s) => s.activeId)
  const runningId = useAppStore((s) => s.runningId)
  const processState = useAppStore((s) => s.processState)
  const busy = useAppStore((s) => s.busy)
  const createInstance = useAppStore((s) => s.createInstance)
  const switchInstance = useAppStore((s) => s.switchInstance)
  const deleteInstance = useAppStore((s) => s.deleteInstance)

  const [newName, setNewName] = useState('')
  const [renaming, setRenaming] = useState<string | null>(null)
  const [renameDraft, setRenameDraft] = useState('')

  const create = () => {
    const name = newName.trim()
    if (!name) return
    void createInstance(name)
    setNewName('')
  }

  return (
    <div className="mx-auto max-w-3xl p-8">
      <h1 className="mb-6 text-2xl font-bold">{t('instances.title')}</h1>

      {/* New instance */}
      <div className="mb-6 flex gap-2 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-4">
        <input
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && create()}
          placeholder={t('instances.newPlaceholder')}
          className="flex-1 rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-zinc-100 outline-none focus:border-blue-500"
        />
        <button
          onClick={create}
          disabled={busy || !newName.trim()}
          className="rounded-lg bg-blue-500 px-4 py-2 text-sm font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
        >
          {t('instances.new')}
        </button>
      </div>

      {instances.length === 0 && (
        <p className="text-sm text-zinc-500">{t('instances.empty')}</p>
      )}

      <div className="space-y-3">
        {instances.map((instance) => {
          const isActive = instance.id === activeId
          const isRunning = instance.id === runningId
          return (
            <div
              key={instance.id}
              className={`rounded-2xl border p-5 ${
                isActive
                  ? 'border-blue-600/50 bg-zinc-900/70'
                  : 'border-zinc-800 bg-zinc-900/40'
              }`}
            >
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <StatusDot
                    status={isRunning ? (processState?.status ?? 'running') : 'stopped'}
                  />
                  <span className="text-lg font-semibold text-zinc-100">{instance.name}</span>
                  {isActive && (
                    <span className="rounded-full bg-blue-600/20 px-2 py-0.5 text-[11px] font-medium text-blue-300">
                      {t('instances.active')}
                    </span>
                  )}
                </div>
                <span className="font-mono text-xs text-zinc-500">{instance.id}</span>
              </div>

              <div className="mt-3 grid grid-cols-3 gap-3 text-xs text-zinc-400">
                <div>
                  <div className="uppercase tracking-wide text-zinc-500">{t('instances.runtime')}</div>
                  <div className="mt-0.5 text-zinc-300">
                    DSH {instance.runtime.version || '—'}
                  </div>
                </div>
                <div>
                  <div className="uppercase tracking-wide text-zinc-500">{t('instances.provider')}</div>
                  <div className="mt-0.5 truncate text-zinc-300">{instance.providerRef}</div>
                </div>
                <div>
                  <div className="uppercase tracking-wide text-zinc-500">{t('instances.plugins')}</div>
                  <div className="mt-0.5 text-zinc-300">{instance.plugins.length}</div>
                </div>
              </div>

              <div className="mt-2 truncate font-mono text-[11px] text-zinc-600">
                {instance.workspace}
              </div>

              <div className="mt-4 flex flex-wrap items-center gap-2">
                {renaming === instance.id ? (
                  <>
                    <input
                      autoFocus
                      value={renameDraft}
                      onChange={(e) => setRenameDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') {
                          if (renameDraft.trim()) void useAppStore.getState().renameInstance(instance.id, renameDraft.trim())
                          setRenaming(null)
                        }
                        if (e.key === 'Escape') setRenaming(null)
                      }}
                      className="w-48 rounded-lg border border-zinc-700 bg-zinc-950 px-2 py-1 text-sm text-zinc-100 outline-none focus:border-blue-500"
                    />
                    <button
                      onClick={() => {
                        if (renameDraft.trim()) void useAppStore.getState().renameInstance(instance.id, renameDraft.trim())
                        setRenaming(null)
                      }}
                      disabled={busy}
                      className="rounded-lg bg-blue-500 px-3 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
                    >
                      {t('instances.save')}
                    </button>
                  </>
                ) : (
                  <>
                    {!isActive && (
                      <button
                        onClick={() => void switchInstance(instance.id)}
                        disabled={busy}
                        className="rounded-lg border border-blue-600 px-3 py-1 text-xs font-medium text-blue-300 hover:bg-blue-600/10 disabled:opacity-40"
                      >
                        {t('instances.switch')}
                      </button>
                    )}
                    <button
                      onClick={() => {
                        setRenaming(instance.id)
                        setRenameDraft(instance.name)
                      }}
                      disabled={busy}
                      className="rounded-lg border border-zinc-700 px-3 py-1 text-xs text-zinc-400 hover:text-zinc-200 disabled:opacity-40"
                    >
                      {t('instances.rename')}
                    </button>
                    <CloneButton instance={instance} busy={busy} />
                    <button
                      onClick={() => {
                        if (window.confirm(t('instances.deleteConfirm', { name: instance.name }))) {
                          void deleteInstance(instance.id)
                        }
                      }}
                      disabled={busy || isRunning}
                      title={isRunning ? t('instances.stopFirst') : undefined}
                      className="ml-auto rounded-lg border border-zinc-700 px-3 py-1 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-40"
                    >
                      {t('instances.delete')}
                    </button>
                  </>
                )}
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

function CloneButton({ instance, busy }: { instance: InstanceManifest; busy: boolean }) {
  const t = useT()
  const cloneInstance = useAppStore((s) => s.cloneInstance)
  const [name, setName] = useState('')
  const [open, setOpen] = useState(false)
  const commit = () => {
    const n = name.trim() || t('instances.copyDefault', { name: instance.name })
    void cloneInstance(instance.id, n)
    setOpen(false)
    setName('')
  }
  return (
    <>
      <button
        onClick={() => setOpen((o) => !o)}
        disabled={busy}
        className="rounded-lg border border-zinc-700 px-3 py-1 text-xs text-zinc-400 hover:text-zinc-200 disabled:opacity-40"
      >
        {t('instances.clone')}
      </button>
      {open && (
        <span className="flex items-center gap-1">
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && commit()}
            placeholder={t('instances.copyDefault', { name: instance.name })}
            className="w-40 rounded-lg border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-100 outline-none focus:border-blue-500"
          />
          <button
            onClick={commit}
            disabled={busy}
            className="rounded-lg bg-blue-500 px-2 py-1 text-xs font-medium text-white hover:bg-blue-400 disabled:opacity-40"
          >
            Go
          </button>
        </span>
      )}
    </>
  )
}
