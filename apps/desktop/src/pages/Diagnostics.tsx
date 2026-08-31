import { useEffect } from 'react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-2xl border border-zinc-800 bg-zinc-900/50 p-5">
      <h2 className="mb-3 text-xs font-semibold uppercase tracking-wide text-zinc-500">{title}</h2>
      {children}
    </div>
  )
}

function Empty({ text }: { text: string }) {
  return <p className="text-sm text-zinc-600">{text}</p>
}

export function Diagnostics() {
  const t = useT()
  const report = useAppStore((s) => s.diagnostics)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const refreshDiagnostics = useAppStore((s) => s.refreshDiagnostics)

  useEffect(() => {
    void refreshDiagnostics()
  }, [activeId, refreshDiagnostics])

  const problems =
    (report?.duplicates.length ?? 0) +
    (report?.orphans.length ?? 0) +
    (report?.orderViolations.length ?? 0)

  return (
    <div className="mx-auto max-w-3xl p-8">
      <div className="mb-4 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">{t('diagnostics.title')}</h1>
          <p className="text-sm text-zinc-400">
            {t('diagnostics.profile')} <span className="font-mono">{report?.profile ?? '…'}</span> ·{' '}
            {t('diagnostics.instance')} <span className="font-mono">{activeInstance?.id ?? '—'}</span>
          </p>
        </div>
        <div className="flex items-center gap-3">
          {report && (
            <span
              className={`rounded-full px-3 py-1 text-xs font-medium ${
                problems > 0 ? 'bg-red-500/15 text-red-400' : 'bg-emerald-500/15 text-emerald-400'
              }`}
            >
              {problems > 0
                ? t('diagnostics.problems', { n: problems, s: problems > 1 ? 's' : '' })
                : t('diagnostics.healthy')}
            </span>
          )}
          <button
            onClick={() => void refreshDiagnostics()}
            className="rounded-lg border border-zinc-700 px-3 py-1 text-sm text-zinc-400 hover:text-zinc-200"
          >
            {t('diagnostics.rescan')}
          </button>
        </div>
      </div>

      {!report ? (
        <p className="text-sm text-zinc-500">{t('diagnostics.scanning')}</p>
      ) : (
        <div className="space-y-4">
          <Section title={t('diagnostics.loadOrder')}>
            <div className="space-y-2">
              {report.bundles.map((b, i) => (
                <div key={b.name} className="rounded-lg bg-zinc-950/50 px-3 py-2">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-[11px] text-zinc-600">{i + 1}</span>
                    <span className="font-mono text-sm text-zinc-200">{b.name}</span>
                    {!b.resolved && b.error && (
                      <span className="rounded-full bg-red-500/15 px-2 py-0.5 text-[10px] text-red-400">
                        {b.error}
                      </span>
                    )}
                  </div>
                  {b.entryIds.length > 0 && (
                    <div className="mt-1 flex flex-wrap gap-1 pl-5">
                      {b.entryIds.map((id) => (
                        <span
                          key={id}
                          className="rounded bg-zinc-800 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400"
                        >
                          {id}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
            {report.suggestedOrder.length > 0 && (
              <p className="mt-3 text-xs text-zinc-500">
                {t('diagnostics.suggested')}{' '}
                <span className="font-mono">{report.suggestedOrder.join(' → ')}</span>
              </p>
            )}
          </Section>

          <Section title={t('diagnostics.conflicts')}>
            {report.duplicates.length === 0 && report.orphans.length === 0 ? (
              <Empty text={t('diagnostics.noConflicts')} />
            ) : (
              <div className="space-y-2">
                {report.duplicates.map((d) => (
                  <div
                    key={d}
                    className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300"
                  >
                    {t('diagnostics.duplicate')} <span className="font-mono">{d}</span>
                  </div>
                ))}
                {report.orphans.map((o) => (
                  <div
                    key={o}
                    className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-300"
                  >
                    {t('diagnostics.orphan')} <span className="font-mono">{o}</span>
                  </div>
                ))}
              </div>
            )}
          </Section>

          <Section title={t('diagnostics.constraints')}>
            {report.orderViolations.length === 0 ? (
              <Empty text={t('diagnostics.noViolations')} />
            ) : (
              <div className="space-y-2">
                {report.orderViolations.map((v) => (
                  <div
                    key={`${v.name}-${v.message}`}
                    className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300"
                  >
                    <span className="font-mono">{v.name}</span> {v.message}
                  </div>
                ))}
              </div>
            )}
          </Section>
        </div>
      )}
    </div>
  )
}
