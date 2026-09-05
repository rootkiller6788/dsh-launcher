import { useState } from 'react'
import {
  Archive,
  CheckCircle2,
  CircleSlash2,
  Download,
  FolderOpen,
  Library,
  Loader2,
  RotateCcw,
  Settings2,
  Trash2,
  X,
  XCircle,
} from 'lucide-react'
import { useT } from '../lib/i18n'
import { useAppStore } from '../stores/appStore'
import type { Job, JobStatus } from '../lib/types'

const ACTIVE: JobStatus[] = ['waiting', 'running']
const TERMINAL: JobStatus[] = ['done', 'failed', 'cancelled']

function toneFor(status: JobStatus) {
  switch (status) {
    case 'failed':
      return 'text-red-300'
    case 'done':
      return 'text-emerald-300'
    case 'cancelled':
      return 'text-zinc-400'
    default:
      return 'text-blue-300'
  }
}

function barFor(status: JobStatus) {
  switch (status) {
    case 'failed':
      return 'bg-red-400'
    case 'done':
      return 'bg-emerald-400'
    case 'cancelled':
      return 'bg-zinc-500'
    default:
      return 'bg-blue-400'
  }
}

function iconFor(status: JobStatus) {
  switch (status) {
    case 'failed':
      return XCircle
    case 'done':
      return CheckCircle2
    case 'cancelled':
      return CircleSlash2
    case 'running':
      return Loader2
    default:
      return Archive
  }
}

export function InstallCenter({ embedded = false }: { embedded?: boolean }) {
  const t = useT()
  const jobs = useAppStore((s) => s.jobs)
  const clearFinishedJobs = useAppStore((s) => s.clearFinishedJobs)
  const openJobInInstalls = useAppStore((s) => s.openJobInInstalls)
  const [open, setOpen] = useState(embedded)
  const active = jobs.filter((j) => ACTIVE.includes(j.status))
  const finished = jobs.filter((j) => TERMINAL.includes(j.status))

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
        <div className="flex items-center gap-1.5">
          {finished.length > 0 && (
            <button
              onClick={() => void clearFinishedJobs()}
              className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-zinc-600 hover:text-zinc-200"
            >
              <Trash2 className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('installCenter.clearFinished')}
            </button>
          )}
          {!embedded && (
            <button onClick={() => setOpen(false)} className="flex h-8 w-8 items-center justify-center rounded-lg text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200">
              <X className="h-4 w-4" strokeWidth={1.75} />
            </button>
          )}
        </div>
      </div>
      <div className="max-h-72 space-y-2 overflow-y-auto pr-1">
        {jobs.length === 0 ? (
          <div className="rounded-lg border border-dashed border-zinc-800 bg-zinc-950/25 px-3 py-6 text-center text-xs text-zinc-600">
            {t('installCenter.empty')}
          </div>
        ) : (
          jobs.map((job) => <JobRow key={job.id} job={job} />)
        )}
      </div>
    </div>
  )

  if (embedded) return content

  return (
    <div className="relative" onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(false)}>
      <button
        onClick={() => openJobInInstalls()}
        title={t('installCenter.title')}
        className="flex h-10 w-10 items-center justify-center rounded-lg border border-zinc-800 bg-zinc-900/60 text-zinc-300 hover:border-blue-500/40 hover:text-blue-200"
      >
        <Download className="h-4 w-4 text-blue-300" strokeWidth={1.75} />
        {active.length > 0 && (
          <span className="absolute -right-1.5 -top-1.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-blue-500 px-1 font-mono text-[10px] leading-none text-white">
            {active.length}
          </span>
        )}
      </button>
      {open && content}
    </div>
  )
}

export function JobRow({ job }: { job: Job }) {
  const t = useT()
  const retryJob = useAppStore((s) => s.retryJob)
  const cancelJob = useAppStore((s) => s.cancelJob)
  const deleteJob = useAppStore((s) => s.deleteJob)
  const openJobInLibrary = useAppStore((s) => s.openJobInLibrary)
  const revealJobWorkspace = useAppStore((s) => s.revealJobWorkspace)
  const revealJobConfig = useAppStore((s) => s.revealJobConfig)
  const tone = toneFor(job.status)
  const Icon = iconFor(job.status)
  const spinning = job.status === 'running'

  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-900/70 p-3">
      <div className="flex items-start gap-3">
        <div className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-zinc-950/70 ${tone}`}>
          <Icon className={`h-4 w-4 ${spinning ? 'animate-spin' : ''}`} strokeWidth={1.75} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-3">
            <div className="truncate text-xs font-semibold text-zinc-200">{job.label}</div>
            <div className={`shrink-0 text-[10px] font-medium uppercase tracking-wide ${tone}`}>
              {t(`market.install.${job.status}`)}
            </div>
          </div>
          {job.stage && job.status === 'running' && (
            <div className="mt-1 text-[10px] text-zinc-500">
              {t(`installCenter.stage.${job.stage}`)}
            </div>
          )}
          <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-zinc-800">
            <div
              className={`h-full rounded-full transition-all duration-300 ${barFor(job.status)}`}
              style={{ width: `${job.progress}%` }}
            />
          </div>
          {job.stderrTail && (
            <div className="mt-2 rounded-md border border-zinc-800 bg-zinc-950/50 px-2 py-1.5">
              {job.stderrTail
                .split('\n')
                .slice(-3)
                .map((line, index) => (
                  <div
                    key={index}
                    className={`truncate text-[10px] ${job.status === 'failed' ? 'text-red-300/80' : 'text-zinc-500'}`}
                  >
                    {line}
                  </div>
                ))}
            </div>
          )}
          {job.error && job.status === 'failed' && (
            <div className="mt-2 truncate text-[10px] text-red-300/80">{job.error}</div>
          )}
          <div className="mt-3 flex flex-wrap gap-1.5">
            <button onClick={() => openJobInLibrary()} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
              <Library className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('installCenter.openLibrary')}
            </button>
            <button onClick={() => void revealJobWorkspace(job.id)} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
              <FolderOpen className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('installCenter.workspace')}
            </button>
            <button onClick={() => void revealJobConfig(job.id)} className="flex h-7 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-blue-500/40 hover:text-blue-200">
              <Settings2 className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('installCenter.config')}
            </button>
            {job.status === 'failed' && (
              <button onClick={() => void retryJob(job.id)} className="flex h-7 items-center gap-1.5 rounded-md border border-red-500/30 px-2 text-[11px] text-red-200 hover:bg-red-500/10">
                <RotateCcw className="h-3.5 w-3.5" strokeWidth={1.75} />
                {t('installCenter.retry')}
              </button>
            )}
          </div>
        </div>
        {job.status === 'waiting' && (
          <button onClick={() => void cancelJob(job.id)} className="flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-zinc-800 px-2 text-[11px] text-zinc-400 hover:border-zinc-600 hover:text-zinc-200">
            {t('installCenter.cancel')}
          </button>
        )}
        {TERMINAL.includes(job.status) && (
          <button onClick={() => void deleteJob(job.id)} className="shrink-0 rounded px-1.5 py-0.5 text-xs text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200" title={t('installCenter.clear')}>
            <X className="h-3.5 w-3.5" strokeWidth={1.75} />
          </button>
        )}
      </div>
    </div>
  )
}
