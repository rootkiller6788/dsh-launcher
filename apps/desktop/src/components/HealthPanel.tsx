import { useState } from 'react'
import { Activity, CircleCheck, CircleX, TriangleAlert } from 'lucide-react'

import { useT } from '../lib/i18n'
import type { HealthCheck, HealthFix, HealthStatus } from '../lib/types'
import { useAppStore } from '../stores/appStore'

/**
 * What the instance measures *right now* — the counterpart to {@link BootRecovery},
 * which only speaks after a boot has already failed.
 *
 * Renders nothing when every check is `ok`: a healthy instance should not spend
 * Overview space saying so. Warnings are included in what it shows, so a
 * half-configured MCP server is visible before it becomes a launch failure.
 */
export function HealthPanel({ running }: { running: boolean }) {
  const t = useT()
  const health = useAppStore((s) => s.health)
  const refreshHealth = useAppStore((s) => s.refreshHealth)
  const applyHealthFix = useAppStore((s) => s.applyHealthFix)
  const [applying, setApplying] = useState<string | null>(null)

  const findings = (health?.checks ?? []).filter((c) => c.status !== 'ok')
  if (findings.length === 0) return null

  const onApply = async (check: HealthCheck, fix: HealthFix) => {
    setApplying(`${check.id}:${fix}`)
    try {
      await applyHealthFix(check, fix)
    } finally {
      setApplying(null)
    }
  }

  return (
    <section className="shrink-0 space-y-2">
      <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Activity className="h-4 w-4 shrink-0 text-zinc-400" strokeWidth={1.75} />
            <h2 className="text-sm font-semibold text-zinc-200">{t('health.title')}</h2>
            <span className="text-[11px] text-zinc-500">
              {t('health.summary', { n: findings.length })}
            </span>
          </div>
          <button
            onClick={() => void refreshHealth()}
            className="shrink-0 rounded-md border border-zinc-800 px-2 py-1 text-[11px] text-zinc-400 transition-colors hover:border-zinc-700 hover:text-zinc-200"
          >
            {t('health.recheck')}
          </button>
        </div>
        <ul className="mt-2.5 space-y-2">
          {findings.map((check) => (
            <li key={check.id} className="flex items-start gap-2.5">
              <StatusIcon status={check.status} />
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-baseline gap-x-2">
                  <span className="text-xs font-medium text-zinc-200">
                    {t(`health.check.${check.id}`)}
                  </span>
                  <span className="text-[10px] uppercase tracking-wide text-zinc-600">
                    {t(`health.group.${check.group}`)}
                  </span>
                </div>
                {/* The measured specifics — names, paths, counts. Backend English,
                    like a crash message: it says what was measured, and no
                    translation should soften that. */}
                <div className="mt-0.5 break-words text-[11px] text-zinc-500">{check.detail}</div>
                {check.fixes.length > 0 && (
                  <div className="mt-1 flex flex-wrap gap-1.5">
                    {check.fixes.map((fix) => (
                      <button
                        key={fix}
                        onClick={() => void onApply(check, fix)}
                        disabled={
                          applying !== null ||
                          // Everything but taking a snapshot writes files a live
                          // harness owns, and the backend refuses those while it
                          // runs (`ensure_not_running`) — so does this.
                          (running && fix !== 'create-rescue')
                        }
                        title={running && fix !== 'create-rescue' ? t('rescue.stopFirst') : undefined}
                        className="shrink-0 rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-200 transition-colors hover:bg-amber-500/20 disabled:cursor-not-allowed disabled:border-zinc-800 disabled:bg-transparent disabled:text-zinc-600"
                      >
                        {applying === `${check.id}:${fix}` ? t('health.applying') : t(`health.fix.${fix}`)}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </li>
          ))}
        </ul>
      </div>
    </section>
  )
}

/**
 * The severity marker. `fail` and `warn` differ in colour and icon rather than
 * in wording, so the eye can rank the list without reading it.
 */
function StatusIcon({ status }: { status: HealthStatus }) {
  if (status === 'fail') {
    return <CircleX className="mt-0.5 h-3.5 w-3.5 shrink-0 text-red-400" strokeWidth={1.75} />
  }
  if (status === 'warn') {
    return (
      <TriangleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-300" strokeWidth={1.75} />
    )
  }
  return <CircleCheck className="mt-0.5 h-3.5 w-3.5 shrink-0 text-emerald-400" strokeWidth={1.75} />
}
