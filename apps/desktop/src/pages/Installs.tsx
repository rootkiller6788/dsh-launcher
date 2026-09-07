import { Download } from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { JobRow } from '../components/InstallCenter'
import { MirrorToggle } from '../components/MirrorToggle'

export function Installs() {
  const t = useT()
  const jobs = useAppStore((s) => s.jobs)
  const active = jobs.filter((j) => j.status === 'waiting' || j.status === 'running')

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
        <div className="flex items-center gap-2 pb-0.5">
          <MirrorToggle />
        </div>
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
