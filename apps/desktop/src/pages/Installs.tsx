import { Download, Trash2 } from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { JobRow } from '../components/InstallCenter'

export function Installs() {
  const t = useT()
  const jobs = useAppStore((s) => s.jobs)
  const clearFinishedJobs = useAppStore((s) => s.clearFinishedJobs)
  const active = jobs.filter((j) => j.status === 'waiting' || j.status === 'running')
  const finished = jobs.filter(
    (j) => j.status === 'done' || j.status === 'failed' || j.status === 'cancelled',
  )

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('installs.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">
            {active.length > 0
              ? t('installCenter.active', { n: active.length, s: active.length === 1 ? '' : 's' })
              : t('installs.subtitle')}
          </p>
        </div>
        {finished.length > 0 && (
          <button
            onClick={() => void clearFinishedJobs()}
            className="flex h-10 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 text-sm font-medium text-zinc-300 hover:border-red-500/40 hover:text-red-200"
          >
            <Trash2 className="h-4 w-4" strokeWidth={1.75} />
            {t('installCenter.clearFinished')}
          </button>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
        {jobs.length === 0 ? (
          <div className="flex h-full items-center justify-center rounded-lg border border-dashed border-zinc-800 bg-zinc-950/20 text-center">
            <div>
              <Download className="mx-auto h-8 w-8 text-zinc-700" strokeWidth={1.5} />
              <p className="mt-3 text-sm font-medium text-zinc-400">{t('installs.empty')}</p>
            </div>
          </div>
        ) : (
          <div className="space-y-2">
            {jobs.map((job) => (
              <JobRow key={job.id} job={job} />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
