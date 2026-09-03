import { useState } from 'react'
import {
  Archive,
  CheckCircle2,
  Download,
  FolderOpen,
  Library,
  RotateCcw,
  Settings2,
  X,
  XCircle,
} from 'lucide-react'
import { useT } from '../lib/i18n'
import { useAppStore } from '../stores/appStore'

export function InstallCenter({ embedded = false }: { embedded?: boolean }) {
  const t = useT()
  const jobs = useAppStore((s) => s.installJobs)
  const clearInstallJob = useAppStore((s) => s.clearInstallJob)
  const retryInstallJob = useAppStore((s) => s.retryInstallJob)
  const openInstallJobInLibrary = useAppStore((s) => s.openInstallJobInLibrary)
  const revealInstallWorkspace = useAppStore((s) => s.revealInstallWorkspace)
  const revealInstallConfig = useAppStore((s) => s.revealInstallConfig)
  const [open, setOpen] = useState(embedded)
  const entries = Object.entries(jobs)
  const active = entries.filter(([, job]) => !['done', 'failed'].includes(job.status))
  const content = (
    <div className={embedded ? '' : 'absolute right-0 top-full z-50 mt-2 w-[520px] rounded-lg border border-zinc-800 bg-zinc-950 p-3 shadow-2xl shadow-black/40'}>
      <div className="mb-3 flex items-center justify-between">
        <div>
          <div className="text-sm font-semibold text-zinc-100">{t('installCenter.title')}</div>
          <div className="text-xs text-zinc-500">
            {active.length > 0
              ? t('installCenter.active', { n: active.length, s: active.length === 1 ? '' : 's' })
              : t('installCenter.subtitle')}
          </div>
        </div>
        {!embedded && (
          <button onClick={() => setOpen(false)} className="flex h-8 w-8 items-center justify-center rounded-lg text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200">
            <X className="h-4 w-4" strokeWidth={1.75} />
          </button>
        )}
      </div>
      <div className="max-h-72 space-y-2 overflow-y-auto pr-1">
        {entries.length === 0 ? (
          <div className="rounded-lg border border-dashed border-zinc-800 bg-zinc-950/25 px-3 py-6 text-center text-xs text-zinc-600">
            {t('installCenter.empty')}
          </div>
        ) : (
          entries.map(([key, job]) => {
            const running = !['done', 'failed'].includes(job.status)
            const tone =
              job.status === 'failed'
                ? 'text-red-300'
                : job.status === 'done'
                  ? 'text-emerald-300'
                  : 'text-blue-300'
            const Icon = job.status === 'failed' ? XCircle : job.status === 'done' ? CheckCircle2 : Archive
            return (
              <div key={key} className="rounded-lg border border-zinc-800 bg-zinc-900/70 p-3">
                <div className="flex items-start gap-3">
                  <div className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-zinc-950/70 ${tone}`}>
                    <Icon className="h-4 w-4" strokeWidth={1.75} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-3">
                      <div className="truncate text-xs font-semibold text-zinc-200">{job.label}</div>
                      <div className={`shrink-0 text-[10px] font-medium uppercase tracking-wide ${tone}`}>
                        {t(`market.install.${job.status}`)}
                      </div>
                    </div>
                    <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-800">
                      <div className={`h-full rounded-full transition-all duration-300 ${job.status === 'failed' ? 'bg-red-400' : job.status === 'done' ? 'bg-emerald-400' : 'bg-blue-400'}`} style={{ width: `${job.progress}%` }} />
                    </div>
                    <div className="mt-2 grid grid-cols-4 gap-1.5">
                      {['downloading', 'dshInstalling', 'inventorySync', 'classifying'].map((step) => (
                        <div
                          key={step}
                          className={`h-1 rounded-full ${
                            step === job.status || job.progress >= stepProgress(step)
                              ? job.status === 'failed'
                                ? 'bg-red-400/70'
                                : 'bg-blue-400/80'
                              : 'bg-zinc-800'
                          }`}
                        />
                      ))}
                    </div>
                    {job.logs.length > 0 && (
                      <div className="mt-2 rounded-md border border-zinc-800 bg-zinc-950/50 px-2 py-1.5">
                        {job.logs.slice(-3).map((line, index) => (
                          <div key={index} className={`truncate text-[10px] ${job.status === 'failed' ? 'text-red-300/80' : 'text-zinc-500'}`}>
                            {line}
                          </div>
                        ))}
                      </div>
                    )}
                    <div className="mt-3 flex flex-wrap gap-1.5">
                      <button onClick={() => openInstallJobInLibrary(key)} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
                        <Library className="h-3.5 w-3.5" strokeWidth={1.75} />
                        {t('installCenter.openLibrary')}
                      </button>
                      <button onClick={() => void revealInstallWorkspace(key)} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
                        <FolderOpen className="h-3.5 w-3.5" strokeWidth={1.75} />
                        {t('installCenter.workspace')}
                      </button>
                      <button onClick={() => void revealInstallConfig(key)} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
                        <Settings2 className="h-3.5 w-3.5" strokeWidth={1.75} />
                        {t('installCenter.config')}
                      </button>
                      {job.status === 'failed' && (
                        <button onClick={() => void retryInstallJob(key)} className="flex h-7 items-center gap-1.5 rounded-md border border-red-500/30 px-2 text-[11px] text-red-200 hover:bg-red-500/10">
                          <RotateCcw className="h-3.5 w-3.5" strokeWidth={1.75} />
                          {t('installCenter.retry')}
                        </button>
                      )}
                    </div>
                  </div>
                  {!running && (
                    <button onClick={() => clearInstallJob(key)} className="rounded px-1.5 py-0.5 text-xs text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200">
                      <X className="h-3.5 w-3.5" strokeWidth={1.75} />
                    </button>
                  )}
                </div>
              </div>
            )
          })
        )}
      </div>
    </div>
  )

  if (embedded) return content

  return (
    <div className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex h-10 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 text-sm font-medium text-zinc-300 hover:border-blue-500/40 hover:text-blue-200"
      >
        <Download className="h-4 w-4 text-blue-300" strokeWidth={1.75} />
        {t('installCenter.title')}
        <span className="rounded-full bg-blue-500/15 px-2 py-0.5 font-mono text-[10px] text-blue-300">
          {active.length || entries.length}
        </span>
      </button>
      {open && content}
    </div>
  )
}

function stepProgress(step: string) {
  if (step === 'downloading') return 18
  if (step === 'dshInstalling') return 45
  if (step === 'inventorySync') return 74
  return 88
}
