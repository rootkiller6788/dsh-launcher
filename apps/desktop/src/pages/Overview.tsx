import { useEffect, useState } from 'react'
import {
  Activity,
  BarChart3,
  Box,
  Cpu,
  ExternalLink,
  Gauge,
  HardDrive,
  MemoryStick,
  Play,
  Puzzle,
  RefreshCw,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  Square,
  Trash2,
  TriangleAlert,
  Waypoints,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { StatusDot } from '../components/StatusDot'
import type { LaunchSession, SystemStats, UsageSummary } from '../lib/types'

function formatUptime(startedAt?: number | null): string {
  if (!startedAt) return '-'
  const s = Math.max(0, Math.floor(Date.now() / 1000 - startedAt))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${sec}s` : `${sec}s`
}

function fmtGB(bytes: number): string {
  const gb = bytes / 1024 ** 3
  return gb >= 100 ? gb.toFixed(0) : gb.toFixed(1)
}

function fmtTokens(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return n.toLocaleString()
}

function ActivityTime({ secs, yesterday }: { secs: number; yesterday: string }) {
  const d = new Date(secs * 1000)
  const now = new Date()
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  const clock = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  if (d.getTime() >= today) return <>{clock}</>
  if (d.getTime() >= today - 86_400_000) return <>{yesterday} {clock}</>
  return <>{d.toLocaleDateString()}</>
}

function Sparkline({ values, color }: { values: number[]; color: string }) {
  if (values.length < 2) {
    return <div className="h-full w-full rounded bg-zinc-800/30" />
  }
  const max = Math.max(...values, 1)
  const min = Math.min(...values, 0)
  const range = max - min || 1
  const w = 120
  const h = 32
  const pts = values
    .map((v, i) => {
      const x = (i / (values.length - 1)) * w
      const y = h - 3 - ((v - min) / range) * (h - 6)
      return `${x.toFixed(1)},${y.toFixed(1)}`
    })
    .join(' ')
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-full w-full">
      <polyline
        points={pts}
        fill="none"
        stroke={color}
        strokeWidth="1.8"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  )
}

function Metric({
  icon: Icon,
  label,
  value,
  tone = 'text-zinc-200',
}: {
  icon: typeof Box
  label: string
  value: string | number
  tone?: string
}) {
  return (
    <div className="min-w-0 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="flex items-center gap-2 text-[11px] text-zinc-500">
        <Icon className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
        <span className="truncate">{label}</span>
      </div>
      <div className={`mt-1 truncate text-lg font-semibold tabular-nums ${tone}`}>{value}</div>
    </div>
  )
}

function ResourceRow({
  icon: Icon,
  label,
  value,
  values,
  color,
}: {
  icon: typeof Cpu
  label: string
  value: string
  values: number[]
  color: string
}) {
  return (
    <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-3 py-2.5">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2 text-xs text-zinc-400">
          <Icon className="h-3.5 w-3.5 shrink-0 text-zinc-500" strokeWidth={1.75} />
          <span className="truncate">{label}</span>
        </div>
        <span className="font-mono text-xs tabular-nums text-zinc-200">{value}</span>
      </div>
      <div className="mt-2 h-8">
        <Sparkline values={values} color={color} />
      </div>
    </div>
  )
}

function ControlButton({
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
      className={`flex min-h-12 items-center justify-center gap-2 rounded-lg border px-3 py-2 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${tone}`}
    >
      <Icon className={`h-4 w-4 ${primary ? 'fill-current' : ''}`} strokeWidth={1.8} />
      <span className="truncate">{label}</span>
    </button>
  )
}

function CheckRow({
  label,
  ok,
  note,
  muted,
}: {
  label: string
  ok: boolean
  note: string
  muted?: boolean
}) {
  const t = useT()
  return (
    <div className="flex items-center justify-between gap-3 py-3">
      <div className="flex min-w-0 items-center gap-2 text-xs text-zinc-300">
        {muted ? (
          <ShieldCheck className="h-3.5 w-3.5 shrink-0 text-zinc-600" strokeWidth={1.75} />
        ) : ok ? (
          <ShieldCheck className="h-3.5 w-3.5 shrink-0 text-emerald-400" strokeWidth={1.75} />
        ) : (
          <TriangleAlert className="h-3.5 w-3.5 shrink-0 text-amber-400" strokeWidth={1.75} />
        )}
        <span className="truncate">{label}</span>
      </div>
      <span
        className={`shrink-0 truncate text-xs ${ok ? 'text-emerald-400' : muted ? 'text-zinc-600' : 'text-amber-400'}`}
      >
        {ok ? t('overview.healthy') : note}
      </span>
    </div>
  )
}

function SignalRow({
  label,
  value,
  accent,
}: {
  label: string
  value: string | number
  accent?: boolean
}) {
  return (
    <div className="flex items-center justify-between gap-3 py-2">
      <span className="truncate text-xs text-zinc-500">{label}</span>
      <span className={`shrink-0 truncate text-xs font-medium tabular-nums ${accent ? 'text-emerald-400' : 'text-zinc-300'}`}>
        {value}
      </span>
    </div>
  )
}

function UsageSnapshot({ summary, locale }: { summary: UsageSummary | null; locale: string }) {
  const t = useT()
  const days = Array.from({ length: 7 }, (_, i) => {
    const d = new Date()
    d.setHours(0, 0, 0, 0)
    d.setDate(d.getDate() - (6 - i))
    const timestamp = Math.floor(d.getTime() / 1000)
    const bucket = summary?.byDay.find((b) => b.timestamp === timestamp)
    return {
      label: d.toLocaleDateString(locale, { weekday: 'short' }),
      tokens: bucket?.totalTokens ?? 0,
    }
  })
  const todayTokens = summary?.totalTokens ?? 0
  const requests = summary?.requests ?? 0
  const cost = summary?.totalCost ?? 0
  const topModel = summary?.byModel[0]?.model ?? '-'
  const max = Math.max(...days.map((d) => d.tokens), 1)
  return (
    <section className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5 xl:col-span-3">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-sm font-semibold text-zinc-200">{t('overview.usageSnapshot')}</h2>
          <p className="mt-1 text-xs text-zinc-500">{t('overview.liveLedger')}</p>
        </div>
        <div className="flex h-8 w-8 items-center justify-center rounded-lg border border-zinc-800 bg-zinc-950/30">
          <BarChart3 className="h-4 w-4 text-zinc-500" strokeWidth={1.75} />
        </div>
      </div>
      <div className="mt-4 grid grid-cols-2 gap-2">
        <SignalRow label={t('overview.todayTokens')} value={fmtTokens(todayTokens)} accent={todayTokens > 0} />
        <SignalRow label={t('overview.requests')} value={requests} accent={requests > 0} />
        <SignalRow label={t('overview.estimatedCost')} value={`$${cost.toFixed(3)}`} accent={cost > 0} />
        <SignalRow label={t('overview.topModel')} value={topModel} accent={todayTokens > 0} />
      </div>
      <div className="mt-auto flex h-20 items-end gap-2">
        {days.map((d, i) => (
          <div key={`${d.label}-${i}`} className="flex min-w-0 flex-1 flex-col items-center gap-2">
            <div className="flex h-14 w-full items-end">
              <div
                className={`w-full rounded-sm ${d.tokens > 0 ? 'bg-blue-400/85' : 'bg-zinc-800/70'}`}
                style={{ height: `${d.tokens > 0 ? Math.max((d.tokens / max) * 100, 12) : 8}%` }}
              />
            </div>
            <span className="truncate text-[10px] text-zinc-600">{d.label}</span>
          </div>
        ))}
      </div>
    </section>
  )
}

function RecentRuns({
  recent,
  instances,
}: {
  recent: LaunchSession[]
  instances: { id: string; name: string }[]
}) {
  const t = useT()
  return (
    <section className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5 xl:col-span-3">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-zinc-200">{t('overview.recentActivity')}</h2>
        <Activity className="h-4 w-4 text-zinc-500" strokeWidth={1.75} />
      </div>
      {recent.length === 0 ? (
        <p className="flex flex-1 items-center justify-center text-center text-xs text-zinc-600">
          {t('overview.noRecentActivity')}
        </p>
      ) : (
        <div className="mt-4 min-h-0 flex-1 space-y-2 overflow-hidden">
          {recent.map((h) => {
            const name = instances.find((x) => x.id === h.instanceId)?.name ?? h.instanceId
            const crashed = h.status === 'crashed'
            return (
              <div key={h.id} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-lg border border-zinc-800/60 bg-zinc-950/20 px-3 py-2">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-zinc-300">{name}</div>
                  <div className="mt-0.5 text-xs tabular-nums text-zinc-500">
                    <ActivityTime secs={h.startedAt} yesterday={t('overview.yesterday')} />
                  </div>
                </div>
                <span
                  className={`rounded-full px-2 py-1 text-[10px] font-medium uppercase tracking-wide ${
                    crashed ? 'bg-red-500/10 text-red-400' : 'bg-emerald-500/10 text-emerald-400'
                  }`}
                >
                  {crashed ? t('status.crashed') : t('overview.ok')}
                </span>
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}

export function Overview() {
  const t = useT()
  const language = useAppStore((s) => s.language)
  const instances = useAppStore((s) => s.instances)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const provider = useAppStore((s) => s.provider)
  const processState = useAppStore((s) => s.processState)
  const system = useAppStore((s) => s.system)
  const systemStats = useAppStore((s) => s.systemStats)
  const systemHistory = useAppStore((s) => s.systemHistory)
  const history = useAppStore((s) => s.history)
  const libraryInventory = useAppStore((s) => s.libraryInventory)
  const usageSummary = useAppStore((s) => s.usageSummary)
  const logs = useAppStore((s) => s.logs)
  const diagnostics = useAppStore((s) => s.diagnostics)
  const busy = useAppStore((s) => s.busy)
  const launch = useAppStore((s) => s.launch)
  const stop = useAppStore((s) => s.stop)
  const restart = useAppStore((s) => s.restart)
  const setShellMode = useAppStore((s) => s.setShellMode)
  const refreshSystem = useAppStore((s) => s.refreshSystem)
  const refreshState = useAppStore((s) => s.refreshState)
  const refreshDiagnostics = useAppStore((s) => s.refreshDiagnostics)
  const clearLogs = useAppStore((s) => s.clearLogs)

  const status = processState?.status ?? 'stopped'
  const running = status === 'running' || status === 'starting' || status === 'degraded'
  const pid = running ? (processState?.pid ?? null) : null
  const hasKey = provider?.hasKey ?? false

  const [, setTick] = useState(0)
  useEffect(() => {
    if (!running) return
    const id = window.setInterval(() => setTick((n) => n + 1), 1000)
    return () => window.clearInterval(id)
  }, [running])

  const pluginCount =
    (activeId ? libraryInventory[activeId]?.plugins : undefined) ??
    activeInstance?.plugins.length ??
    0
  const mcpCount = activeInstance?.mcp.length ?? 0
  const skillCount = activeInstance?.skills.length ?? 0
  const recent = history.slice(0, 3)
  const launchEnabled = !!activeId && !busy

  const cpuSeries = systemHistory.map((s) => s.cpu)
  const memPct = (s: SystemStats) =>
    s.memoryTotal ? (s.memoryUsed / s.memoryTotal) * 100 : 0
  const memSeries = systemHistory.map(memPct)
  const memNow = systemStats ? memPct(systemStats) : 0
  const diskPct = systemStats?.diskTotal ? (systemStats.diskUsed / systemStats.diskTotal) * 100 : 0

  const diag = diagnostics
  const pluginIssues = diag ? diag.duplicates.length + diag.orderViolations.length : 0
  const mcpIssues = diag?.orphans.length ?? 0
  const readiness = [
    {
      label: t('overview.runtime'),
      ok: !!system?.dsh,
      note: system?.dsh ? `DSH ${system.dsh.version}` : t('overview.notDetected'),
    },
    {
      label: t('overview.provider'),
      ok: hasKey,
      note: t('overview.noKey'),
    },
    {
      label: t('overview.plugins'),
      ok: pluginIssues === 0,
      note: t('overview.conflicts', { n: pluginIssues, s: pluginIssues === 1 ? '' : 's' }),
    },
    {
      label: t('overview.mcpServers'),
      ok: mcpIssues === 0,
      note: t('overview.conflicts', { n: mcpIssues, s: mcpIssues === 1 ? '' : 's' }),
    },
    {
      label: t('overview.skills'),
      ok: skillCount > 0,
      muted: skillCount === 0,
      note: skillCount > 0 ? t('overview.mounted', { n: skillCount }) : t('overview.none'),
    },
  ]
  const blocking = readiness.filter((r) => !r.ok && !r.muted).length
  const deckStatus = blocking > 0 ? t('overview.needsAttention') : t('overview.ready')
  const weekStart = Math.floor(Date.now() / 1000) - 7 * 86_400
  const weekRuns = history.filter((h) => h.startedAt >= weekStart).length
  const quickSignals = [
    {
      label: t('overview.activeRuntime'),
      value: system?.dsh?.version ?? t('overview.missing'),
      accent: !!system?.dsh,
    },
    {
      label: t('overview.sevenDayRuns'),
      value: weekRuns,
      accent: weekRuns > 0,
    },
    {
      label: t('overview.resourceFeed'),
      value: systemHistory.length >= 2 ? t('overview.live') : t('overview.warming'),
      accent: systemHistory.length >= 2,
    },
    {
      label: t('overview.logBuffer'),
      value: logs.length,
      accent: logs.length > 0,
    },
  ]

  const refreshChecks = () => {
    void (async () => {
      await refreshSystem()
      await refreshState()
      await refreshDiagnostics()
    })()
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('overview.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">{t('overview.subtitle')}</p>
        </div>
        <div className="hidden items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 py-2 text-xs text-zinc-400 md:flex">
          <StatusDot status={status} />
          <span className="capitalize">{t(`status.${status}`)}</span>
        </div>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-1 gap-5 xl:grid-cols-12 xl:grid-rows-[minmax(0,1.08fr)_minmax(0,0.92fr)]">
        <section className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60 xl:col-span-8">
          <div className="flex min-h-0 flex-1">
            <div className={`w-1.5 shrink-0 ${running ? 'bg-amber-400' : activeInstance ? 'bg-blue-400' : 'bg-zinc-700'}`} />
            <div className="flex min-w-0 flex-1 flex-col p-5">
              <div className="flex items-start justify-between gap-5">
                <div className="min-w-0">
                  <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-zinc-500">
                    <Box className="h-3.5 w-3.5" strokeWidth={1.75} />
                    {t('overview.instanceDeck')}
                  </div>
                  <h2 className="mt-2 truncate text-3xl font-semibold leading-tight text-zinc-50">
                    {activeInstance?.name ?? '-'}
                  </h2>
                  <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                    <span className="rounded bg-zinc-800/60 px-2 py-1 font-mono">
                      {activeInstance?.id ?? '-'}
                    </span>
                    <span>{provider?.profile.name ?? t('overview.provider')}</span>
                    <span className="text-zinc-700">/</span>
                    <span>{provider?.profile.model ?? t('overview.model')}</span>
                  </div>
                </div>
                <div className="shrink-0 text-right">
                  <div className="inline-flex items-center gap-2 rounded-full border border-zinc-700 bg-zinc-950/40 px-3 py-1.5 text-xs font-medium text-zinc-300">
                    <StatusDot status={status} />
                    {t(`status.${status}`)}
                  </div>
                  <div className="mt-2 text-xs text-zinc-500">{deckStatus}</div>
                </div>
              </div>

              <div className="mt-auto grid grid-cols-3 gap-3">
                <Metric icon={Gauge} label={t('overview.uptime')} value={running ? formatUptime(processState?.startedAt) : '-'} />
                <Metric icon={Activity} label={t('overview.pid')} value={pid != null ? pid : '-'} />
                <Metric icon={Puzzle} label={t('overview.plugins')} value={pluginCount} />
                <Metric icon={Waypoints} label={t('overview.mcpServers')} value={mcpCount} />
                <Metric icon={Sparkles} label={t('overview.skills')} value={skillCount} />
                <Metric icon={ShieldCheck} label={t('overview.readiness')} value={blocking === 0 ? t('overview.clear') : blocking} tone={blocking === 0 ? 'text-emerald-400' : 'text-amber-400'} />
              </div>
            </div>
          </div>
        </section>

        <section className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5 xl:col-span-4">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h2 className="text-sm font-semibold text-zinc-200">{t('overview.quickActions')}</h2>
              <p className="mt-1 text-xs text-zinc-500">{t('overview.commandCenter')}</p>
            </div>
            <span className={`rounded-full px-2 py-1 text-[10px] font-medium uppercase tracking-wide ${blocking > 0 ? 'bg-amber-500/10 text-amber-400' : 'bg-emerald-500/10 text-emerald-400'}`}>
              {blocking > 0 ? t('overview.warning') : t('overview.ready')}
            </span>
          </div>

          <div className="mt-5 grid grid-cols-2 gap-2">
            {running ? (
              <>
                <ControlButton icon={ExternalLink} label={t('overview.openDsh')} onClick={() => setShellMode('workspace')} primary />
                <ControlButton icon={RotateCcw} label={t('overview.restartDsh')} onClick={() => void restart()} disabled={!launchEnabled} />
                <ControlButton icon={Square} label={t('overview.stopDsh')} onClick={() => void stop()} disabled={busy} danger />
                <ControlButton icon={RefreshCw} label={t('overview.refreshChecks')} onClick={refreshChecks} />
              </>
            ) : (
              <>
                <ControlButton icon={Play} label={busy ? t('overview.working') : t('overview.launchDsh')} onClick={() => void launch(activeId ?? '')} disabled={!launchEnabled} primary />
                <ControlButton icon={RefreshCw} label={t('overview.refreshChecks')} onClick={refreshChecks} />
                <ControlButton icon={Trash2} label={t('overview.clearLogs')} onClick={clearLogs} />
                <ControlButton icon={RotateCcw} label={t('overview.restartDsh')} onClick={() => void restart()} disabled />
              </>
            )}
          </div>

          <div className="mt-4 min-h-0 flex-1 divide-y divide-zinc-800/60 overflow-hidden rounded-lg border border-zinc-800/70 bg-zinc-950/20 px-3">
            {quickSignals.map((signal) => (
              <SignalRow
                key={signal.label}
                label={signal.label}
                value={signal.value}
                accent={signal.accent}
              />
            ))}
          </div>
        </section>

        <section className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5 xl:col-span-3">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-semibold text-zinc-200">{t('overview.runtimeResources')}</h2>
            <Cpu className="h-4 w-4 text-zinc-500" strokeWidth={1.75} />
          </div>
          {systemHistory.length < 2 ? (
            <p className="flex flex-1 items-center justify-center text-center text-xs text-zinc-600">
              {t('overview.noSystemData')}
            </p>
          ) : (
            <div className="mt-4 flex min-h-0 flex-1 flex-col gap-3">
              <ResourceRow icon={Cpu} label={t('overview.cpu')} value={`${(systemStats?.cpu ?? 0).toFixed(0)}%`} values={cpuSeries} color="#34d399" />
              <ResourceRow icon={MemoryStick} label={t('overview.memory')} value={`${memNow.toFixed(0)}%`} values={memSeries} color="#60a5fa" />
              <div className="mt-auto">
                <div className="flex items-center justify-between text-xs text-zinc-400">
                  <span className="flex items-center gap-2">
                    <HardDrive className="h-3.5 w-3.5 text-zinc-500" strokeWidth={1.75} />
                    {t('overview.disk')}
                  </span>
                  <span className="font-mono tabular-nums text-zinc-200">
                    {systemStats ? `${fmtGB(systemStats.diskUsed)} / ${fmtGB(systemStats.diskTotal)} GB` : '-'}
                  </span>
                </div>
                <div className="mt-2 h-2 overflow-hidden rounded-full bg-zinc-800">
                  <div className="h-full rounded-full bg-zinc-400 transition-[width] duration-500" style={{ width: `${Math.min(diskPct, 100)}%` }} />
                </div>
              </div>
            </div>
          )}
        </section>

        <UsageSnapshot summary={usageSummary} locale={language} />

        <section className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5 xl:col-span-3">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-semibold text-zinc-200">{t('overview.health')}</h2>
            <ShieldCheck className={`h-4 w-4 ${blocking > 0 ? 'text-amber-400' : 'text-emerald-400'}`} strokeWidth={1.75} />
          </div>
          <div className="mt-3 min-h-0 flex-1 divide-y divide-zinc-800/60 overflow-hidden">
            {readiness.map((r) => (
              <CheckRow key={r.label} label={r.label} ok={r.ok} note={r.note} muted={r.muted} />
            ))}
          </div>
        </section>

        <RecentRuns recent={recent} instances={instances} />
      </div>
    </div>
  )
}
