import { useEffect, useRef } from 'react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'

function formatTime(secs: number | null | undefined) {
  if (secs == null) return '—'
  const d = new Date(secs * 1000)
  return d.toLocaleTimeString()
}

function StatusBadge({ status }: { status: string }) {
  const t = useT()
  const styles: Record<string, string> = {
    running: 'bg-emerald-500/15 text-emerald-400',
    stopped: 'bg-zinc-500/15 text-zinc-400',
    crashed: 'bg-red-500/15 text-red-400',
  }
  return (
    <span className={`rounded-full px-2 py-0.5 text-[11px] font-medium ${styles[status] ?? 'bg-zinc-500/15 text-zinc-400'}`}>
      {t(`status.${status}`)}
    </span>
  )
}

export function Activity() {
  const t = useT()
  const logs = useAppStore((s) => s.logs)
  const history = useAppStore((s) => s.history)
  const clearLogs = useAppStore((s) => s.clearLogs)
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [logs])

  return (
    <div className="flex h-full flex-col p-8">
      <div className="mb-4 flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t('activity.title')}</h1>
        <button
          onClick={clearLogs}
          className="rounded-lg border border-zinc-700 px-3 py-1 text-sm text-zinc-400 hover:text-zinc-200"
        >
          {t('activity.clear')}
        </button>
      </div>

      {/* Launch history (persisted in SQLite) */}
      <div className="mb-6">
        <h2 className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
          {t('activity.history')}
        </h2>
        {history.length === 0 ? (
          <p className="text-xs text-zinc-600">{t('activity.noHistory')}</p>
        ) : (
          <div className="overflow-hidden rounded-lg border border-zinc-800">
            <table className="w-full text-left text-xs">
              <tbody className="divide-y divide-zinc-800/70">
                {history.slice(0, 8).map((h) => (
                  <tr key={h.id} className="bg-zinc-900/40">
                    <td className="px-3 py-2 font-mono text-zinc-300">{h.instanceId}</td>
                    <td className="px-3 py-2 text-zinc-400">
                      {formatTime(h.startedAt)} → {formatTime(h.endedAt)}
                    </td>
                    <td className="px-3 py-2">
                      <StatusBadge status={h.status} />
                    </td>
                    <td className="px-3 py-2 font-mono text-zinc-500">
                      {h.exitCode != null ? t('activity.exit', { n: h.exitCode }) : ''}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Live stdout/stderr stream */}
      <div className="flex-1 overflow-y-auto rounded-lg border border-zinc-800 bg-black/60 p-4 font-mono text-xs leading-relaxed">
        {logs.length === 0 ? (
          <div className="text-zinc-600">{t('activity.noLogs')}</div>
        ) : (
          logs.map((log, i) => (
            <div key={i} className={log.stream === 'stderr' ? 'text-orange-400' : 'text-zinc-300'}>
              <span className="select-none text-zinc-600">
                {log.stream === 'stderr' ? t('activity.err') : t('activity.out')}{' '}
              </span>
              {log.line}
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
