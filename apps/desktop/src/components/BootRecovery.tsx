import { useState } from 'react'
import { LifeBuoy, RotateCcw, TriangleAlert, X } from 'lucide-react'

import { useT } from '../lib/i18n'
import type { CrashIssue, FixAction, LaunchDiagnosis } from '../lib/types'
import { useAppStore } from '../stores/appStore'

/**
 * What a failed boot looked like, and the way back.
 *
 * Two panels that belong together: the diagnosis (why the boot failed, read off
 * its own log) and the rescue point (the profile files as they were at the last
 * good boot). A `restore` fix is only actionable because the rescue point
 * exists, so they share a card rather than sitting on separate pages.
 *
 * Renders nothing when there is neither a diagnosis nor a rescue point — the
 * common, healthy case takes no space on the Overview.
 */
export function BootRecovery({ running }: { running: boolean }) {
  const t = useT()
  const diagnosis = useAppStore((s) => s.launchDiagnosis)
  const rescue = useAppStore((s) => s.rescue)
  const clearLaunchDiagnosis = useAppStore((s) => s.clearLaunchDiagnosis)
  const createRescue = useAppStore((s) => s.createRescue)
  const restoreRescue = useAppStore((s) => s.restoreRescue)
  const applyCrashFix = useAppStore((s) => s.applyCrashFix)
  const [restoring, setRestoring] = useState(false)
  const [applying, setApplying] = useState(false)

  const hasRescue = rescue?.exists ?? false
  if (!diagnosis && !hasRescue) return null

  const onRestore = async () => {
    setRestoring(true)
    try {
      await restoreRescue()
    } finally {
      setRestoring(false)
    }
  }

  const onApply = async (issue: CrashIssue) => {
    setApplying(true)
    try {
      await applyCrashFix(issue)
    } finally {
      setApplying(false)
    }
  }

  return (
    // A full-width banner above the Overview grid rather than a grid cell: the
    // grid switches column counts at breakpoints, and a failed boot should read
    // the same at every width.
    <section className="shrink-0 space-y-2">
      {diagnosis && (
        <div className={STAGES[diagnosis.stage].panel}>
          <div className="flex items-start justify-between gap-3">
            <div className="flex items-center gap-2">
              <TriangleAlert
                className={`h-4 w-4 shrink-0 ${STAGES[diagnosis.stage].icon}`}
                strokeWidth={1.75}
              />
              <h2 className={`text-sm font-semibold ${STAGES[diagnosis.stage].title}`}>
                {t(STAGES[diagnosis.stage].heading)}
              </h2>
            </div>
            <button
              onClick={clearLaunchDiagnosis}
              title={t('crash.dismiss')}
              className="rounded-md p-1 text-zinc-500 transition-colors hover:bg-zinc-800/60 hover:text-zinc-300"
            >
              <X className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>
          </div>
          <p className="mt-1.5 text-[11px] text-zinc-500">{t(STAGES[diagnosis.stage].body)}</p>
          {diagnosis.issues.length === 0 ? (
            <p className="mt-2 text-xs text-zinc-400">{t('crash.unrecognised')}</p>
          ) : (
            <ul className="mt-2 space-y-1.5">
              {diagnosis.issues.map((issue, i) => (
                <li key={`${issue.kind}-${issue.plugin}-${i}`}>
                  <div className="text-xs text-zinc-300">{issue.message}</div>
                  <div className="mt-0.5 flex items-center gap-2">
                    <span className="text-[11px] text-amber-200/80">
                      {t('crash.nextStep')} {fixText(t, issue)}
                    </span>
                    {ACTIONABLE.includes(issue.fix) && (
                      <button
                        onClick={() => void onApply(issue)}
                        // Every fix here writes files a live harness owns (a
                        // toggle, a restore) or re-launches it, and the backend
                        // refuses the writes while it runs — so does this.
                        disabled={applying || running}
                        title={running ? t('rescue.stopFirst') : undefined}
                        className="shrink-0 rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-200 transition-colors hover:bg-amber-500/20 disabled:cursor-not-allowed disabled:border-zinc-800 disabled:bg-transparent disabled:text-zinc-600"
                      >
                        {t(`crash.apply.${FIX_KEYS[issue.fix]}`)}
                      </button>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <div className="flex items-center gap-3 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-2.5">
        <LifeBuoy className="h-4 w-4 shrink-0 text-zinc-500" strokeWidth={1.75} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium text-zinc-300">{t('rescue.title')}</span>
            <span className="truncate text-[11px] text-zinc-500">
              {hasRescue
                ? t('rescue.saved', {
                    when: new Date(rescue?.at ?? 0).toLocaleString(),
                    n: rescue?.files.length ?? 0,
                    s: (rescue?.files.length ?? 0) === 1 ? '' : 's',
                  })
                : t('rescue.none')}
            </span>
          </div>
          {hasRescue && <div className="mt-0.5 truncate text-[11px] text-zinc-600">{t('rescue.hint')}</div>}
        </div>
        <button
          onClick={() => void createRescue()}
          className="shrink-0 rounded-md border border-zinc-800 px-2 py-1 text-[11px] text-zinc-400 transition-colors hover:border-zinc-700 hover:text-zinc-200"
        >
          {t('rescue.save')}
        </button>
        {hasRescue && (
          <button
            onClick={() => void onRestore()}
            disabled={running || restoring}
            // Restoring under a live harness would be undone the moment it next
            // writes its config, so the backend refuses it too — this just makes
            // the refusal visible before the click.
            title={running ? t('rescue.stopFirst') : t('rescue.hint')}
            className="flex shrink-0 items-center gap-1.5 rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-200 transition-colors hover:bg-amber-500/20 disabled:cursor-not-allowed disabled:border-zinc-800 disabled:bg-transparent disabled:text-zinc-600"
          >
            <RotateCcw className="h-3 w-3" strokeWidth={1.75} />
            {restoring ? t('rescue.restoring') : t('rescue.restore')}
          </button>
        )}
      </div>
    </section>
  )
}

/**
 * The action a diagnosis recommends, in the user's language.
 *
 * `exclude-bundle` names the plugin it is about, so it takes a variable; the
 * rest are one-liners. An action with no copy falls back to its own token rather
 * than an empty line — a new `FixAction` variant should be obvious, not silent.
 */
function fixText(t: (key: string, vars?: Record<string, string | number>) => string, issue: CrashIssue): string {
  const key = `crash.fix.${FIX_KEYS[issue.fix] ?? issue.fix}`
  return issue.fix === 'exclude-bundle'
    ? t(key, { plugin: issue.plugin || 'the plugin' })
    : t(key)
}

const FIX_KEYS: Record<FixAction, string> = {
  'exclude-bundle': 'excludeBundle',
  restore: 'restore',
  'install-deps': 'installDeps',
  reinstall: 'reinstall',
  'rebuild-source': 'rebuildSource',
  restart: 'restart',
  'reopen-url': 'reopenUrl',
}

/**
 * How each diagnosis stage presents itself.
 *
 * `refused` deliberately breaks the red of the other two: nothing failed. The
 * harness is up and serving, and what it refused was the URL it printed — so the
 * panel says that, in amber, instead of calling a running harness a failed boot.
 * The stage is a closed set from the backend (`LaunchDiagnosis.stage`), which is
 * why this is a record rather than a lookup with a default.
 */
const STAGES: Record<
  LaunchDiagnosis['stage'],
  { heading: string; body: string; panel: string; icon: string; title: string }
> = {
  crashed: {
    heading: 'crash.title',
    body: 'crash.crashedBody',
    panel: 'rounded-lg border border-red-500/25 bg-red-500/5 p-4',
    icon: 'text-red-300',
    title: 'text-red-200',
  },
  degraded: {
    heading: 'crash.stalled',
    body: 'crash.stalledBody',
    panel: 'rounded-lg border border-red-500/25 bg-red-500/5 p-4',
    icon: 'text-red-300',
    title: 'text-red-200',
  },
  refused: {
    heading: 'crash.refused',
    body: 'crash.refusedBody',
    panel: 'rounded-lg border border-amber-500/25 bg-amber-500/5 p-4',
    icon: 'text-amber-300',
    title: 'text-amber-200',
  },
}

/**
 * The fixes the launcher can carry out itself.
 *
 * The rest (`install-deps` / `reinstall` / `rebuild-source`) are dependency-level
 * repairs that need the repair library from absorb-plan 2.5 — until that exists
 * the diagnosis states what to do and offers no button, rather than a button
 * that cannot work.
 */
const ACTIONABLE: FixAction[] = ['exclude-bundle', 'restore', 'restart']
