import { useEffect, useMemo, useRef, useState, type WheelEvent } from 'react'
import {
  Activity as ActivityIcon,
  BarChart3,
  Clock3,
  Coins,
  Download,
  Gauge,
  History,
  ListRestart,
  LineChart,
  Play,
  ScrollText,
  Square,
  Terminal,
  Trash2,
  TriangleAlert,
  WalletCards,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { ipc } from '../lib/ipc'
import { useT } from '../lib/i18n'
import { StatusDot } from '../components/StatusDot'
import type { LaunchSession, UsageSummary } from '../lib/types'

type MonitorMode = 'runtime' | 'usage'
type UsageRange = 'today' | '7d' | 'month' | 'year' | 'all'

function usageWindow(range: UsageRange) {
  const now = Math.floor(Date.now() / 1000)
  const start = new Date()
  if (range === 'today') {
    start.setHours(0, 0, 0, 0)
  } else if (range === '7d') {
    start.setHours(0, 0, 0, 0)
    start.setDate(start.getDate() - 6)
  } else if (range === 'month') {
    start.setDate(1)
    start.setHours(0, 0, 0, 0)
  } else if (range === 'year') {
    start.setMonth(0, 1)
    start.setHours(0, 0, 0, 0)
  } else {
    return { from: 0, to: now + 1 }
  }
  return { from: Math.floor(start.getTime() / 1000), to: now + 1 }
}

function formatClock(secs: number | null | undefined) {
  if (secs == null) return '-'
  return new Date(secs * 1000).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatDuration(start?: number | null, end?: number | null) {
  if (!start) return '-'
  const stop = end ?? Math.floor(Date.now() / 1000)
  const s = Math.max(0, stop - start)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${sec}s` : `${sec}s`
}

function formatSeconds(seconds: number | null) {
  if (seconds == null) return '-'
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const sec = seconds % 60
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${sec}s` : `${sec}s`
}

function StatusBadge({ status }: { status: string }) {
  const t = useT()
  const styles: Record<string, string> = {
    running: 'bg-emerald-500/10 text-emerald-400',
    stopped: 'bg-zinc-500/15 text-zinc-400',
    crashed: 'bg-red-500/10 text-red-400',
    starting: 'bg-blue-500/10 text-blue-300',
    degraded: 'bg-blue-500/10 text-blue-300',
  }
  return (
    <span className={`rounded-full px-2 py-1 text-[10px] font-medium uppercase tracking-wide ${styles[status] ?? styles.stopped}`}>
      {t(`status.${status}`)}
    </span>
  )
}

function StatCell({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof Terminal
  label: string
  value: string | number
}) {
  return (
    <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-4 py-3">
      <div className="flex items-center gap-2 text-[11px] text-zinc-500">
        <Icon className="h-3.5 w-3.5" strokeWidth={1.75} />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-1 truncate text-xl font-semibold tabular-nums text-zinc-100">{value}</div>
    </div>
  )
}

function fmtTokens(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return n.toLocaleString()
}

function UsageBars({
  values,
  secondary,
}: {
  values: number[]
  secondary?: number[]
}) {
  const max = Math.max(...values, ...(secondary ?? []), 1)
  return (
    <div className="flex h-full items-end gap-1.5">
      {values.map((v, i) => {
        const h = Math.max((v / max) * 100, v > 0 ? 8 : 3)
        const s = secondary?.[i] ?? 0
        const sh = Math.max((s / max) * 100, s > 0 ? 8 : 0)
        return (
          <div key={i} className="flex min-w-0 flex-1 items-end gap-0.5">
            <div className="w-full rounded-sm bg-blue-400/85" style={{ height: `${h}%` }} />
            {secondary && <div className="w-full rounded-sm bg-cyan-300/80" style={{ height: `${sh}%` }} />}
          </div>
        )
      })}
    </div>
  )
}

function UsageArea({ values }: { values: number[] }) {
  const max = Math.max(...values, 1)
  const w = 360
  const h = 120
  const pts = values
    .map((v, i) => {
      const x = (i / Math.max(values.length - 1, 1)) * w
      const y = h - 8 - (v / max) * (h - 18)
      return `${x.toFixed(1)},${y.toFixed(1)}`
    })
    .join(' ')
  const area = `0,${h} ${pts} ${w},${h}`
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-full w-full">
      <defs>
        <linearGradient id="usage-area" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stopColor="#3b82f6" stopOpacity="0.55" />
          <stop offset="100%" stopColor="#3b82f6" stopOpacity="0.02" />
        </linearGradient>
      </defs>
      <polygon points={area} fill="url(#usage-area)" />
      <polyline points={pts} fill="none" stroke="#60a5fa" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function ModelBar({ label, value, max }: { label: string; value: number; max: number }) {
  return (
    <div>
      <div className="mb-1 flex items-center justify-between gap-3 text-xs">
        <span className="truncate text-zinc-400">{label}</span>
        <span className="font-mono tabular-nums text-zinc-300">{fmtTokens(value)}</span>
      </div>
      <div className="h-2 overflow-hidden rounded-full bg-zinc-800/80">
        <div className="h-full rounded-full bg-blue-400" style={{ width: `${Math.max((value / Math.max(max, 1)) * 100, 4)}%` }} />
      </div>
    </div>
  )
}

function FilterSelect({
  label,
  value,
  onChange,
  options,
}: {
  label: string
  value: string
  onChange: (value: string) => void
  options: { label: string; value: string }[]
}) {
  return (
    <label className="flex min-w-0 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950/25 px-3 py-2 text-xs">
      <span className="shrink-0 text-zinc-500">{label}</span>
      <select
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="min-w-0 flex-1 bg-transparent text-zinc-200 outline-none"
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  )
}

function SessionRow({
  session,
  name,
}: {
  session: LaunchSession
  name: string
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-3 py-2.5">
      <div className="min-w-0">
        <div className="truncate text-sm font-medium text-zinc-200">{name}</div>
        <div className="mt-0.5 flex items-center gap-2 text-xs tabular-nums text-zinc-500">
          <span>{formatClock(session.startedAt)}</span>
          <span className="text-zinc-700">/</span>
          <span>{formatDuration(session.startedAt, session.endedAt)}</span>
        </div>
      </div>
      <div className="flex flex-col items-end gap-1">
        <StatusBadge status={session.status} />
        {session.exitCode != null && (
          <span className="font-mono text-[10px] text-zinc-600">{session.exitCode}</span>
        )}
      </div>
    </div>
  )
}

export function Activity() {
  const t = useT()
  const [mode, setMode] = useState<MonitorMode>('runtime')
  const [usageRange, setUsageRange] = useState<UsageRange>('today')
  const [usageInstance, setUsageInstance] = useState('active')
  const [usageModel, setUsageModel] = useState('all')
  const [usageProvider, setUsageProvider] = useState('all')
  const [filteredUsage, setFilteredUsage] = useState<UsageSummary | null>(null)
  const [exportNotice, setExportNotice] = useState<string | null>(null)
  const logs = useAppStore((s) => s.logs)
  const history = useAppStore((s) => s.history)
  const usageSummary = useAppStore((s) => s.usageSummary)
  const instances = useAppStore((s) => s.instances)
  const activeId = useAppStore((s) => s.activeId)
  const processState = useAppStore((s) => s.processState)
  const runningId = useAppStore((s) => s.runningId)
  const clearLogs = useAppStore((s) => s.clearLogs)
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [logs])

  useEffect(() => {
    if (mode !== 'usage') return
    const { from, to } = usageWindow(usageRange)
    const instanceId = usageInstance === 'active' ? activeId : usageInstance === 'all' ? null : usageInstance
    void ipc
      .usageSummary(
        instanceId,
        from,
        to,
        usageModel === 'all' ? null : usageModel,
        usageProvider === 'all' ? null : usageProvider,
      )
      .then(setFilteredUsage)
      .catch(() => setFilteredUsage(null))
  }, [mode, usageRange, usageInstance, usageModel, usageProvider, usageSummary?.records.length, activeId])

  const now = Math.floor(Date.now() / 1000)
  const todayStart = new Date()
  todayStart.setHours(0, 0, 0, 0)
  const todaySecs = todayStart.getTime() / 1000
  const todaySessions = history.filter((h) => h.startedAt >= todaySecs)
  const recent = history
  const stderrCount = logs.filter((l) => l.stream === 'stderr').length
  const stdoutCount = logs.length - stderrCount
  const crashed = history.filter((h) => h.status === 'crashed').length
  const completed = history.filter((h) => h.endedAt != null)
  const avgSeconds = useMemo(() => {
    const durations = completed
      .map((h) => (h.endedAt ?? now) - h.startedAt)
      .filter((d) => d >= 0)
    if (durations.length === 0) return null
    return Math.round(durations.reduce((a, b) => a + b, 0) / durations.length)
  }, [completed, now])
  const runningName = instances.find((i) => i.id === runningId)?.name ?? runningId ?? '-'
  const status = processState?.status ?? 'stopped'
  const usage = filteredUsage ?? usageSummary
  const usageInput = usage?.byHour.map((b) => b.inputTokens) ?? []
  const usageOutput = usage?.byHour.map((b) => b.outputTokens) ?? []
  const usageTrend = usage?.byHour.map((b) => b.totalTokens) ?? []
  const totalTokens = usage?.totalTokens ?? 0
  const requests = usage?.requests ?? 0
  const estimatedCost = usage?.totalCost ?? 0
  const modelUsage = usage?.byModel.map((m) => ({ label: m.model, value: m.totalTokens })) ?? []
  const modelOptions = Array.from(new Set([...(usageSummary?.byModel.map((m) => m.model) ?? []), ...(usage?.byModel.map((m) => m.model) ?? [])]))
  const providerOptions = Array.from(new Set((usageSummary?.records ?? usage?.records ?? []).map((r) => r.apiKeyAlias).filter(Boolean)))
  const maxModel = Math.max(...modelUsage.map((m) => m.value), 1)
  const peakBucket = usage?.byHour.reduce((best, bucket) => (bucket.requests > best.requests ? bucket : best), usage?.byHour[0] ?? null)
  const nonZeroCosts = usage?.byHour.map((b) => b.cost).filter((cost) => cost > 0) ?? []
  const avgCost = nonZeroCosts.length > 0 ? nonZeroCosts.reduce((a, b) => a + b, 0) / nonZeroCosts.length : 0
  const maxCost = Math.max(...nonZeroCosts, 0)
  const costAlert = maxCost > 0 && (maxCost > avgCost * 2.5 || maxCost >= 1)
  const topModelShare = totalTokens > 0 && modelUsage[0] ? Math.round((modelUsage[0].value / totalTokens) * 100) : 0
  const exportUsage = async (format: 'csv' | 'json') => {
    const { from, to } = usageWindow(usageRange)
    const instanceId = usageInstance === 'active' ? activeId : usageInstance === 'all' ? null : usageInstance
    try {
      const result = await ipc.usageExport(
        instanceId,
        from,
        to,
        format,
        usageModel === 'all' ? null : usageModel,
        usageProvider === 'all' ? null : usageProvider,
      )
      setExportNotice(t('activity.exported', { path: result.path }))
    } catch {
      setExportNotice(t('activity.exportFailed'))
    }
  }
  const holdWheelInPanel = (event: WheelEvent<HTMLDivElement>) => {
    const panel = event.currentTarget
    if (panel.scrollHeight <= panel.clientHeight) return
    panel.scrollTop += event.deltaY
    event.preventDefault()
    event.stopPropagation()
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('activity.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">{t('activity.subtitle')}</p>
        </div>
        <div className="flex items-center gap-2">
          <div className="relative grid w-48 grid-cols-2 rounded-lg border border-zinc-800 bg-zinc-900/60 p-0.5">
            <div className={`absolute left-0.5 top-0.5 h-8 w-[calc((100%_-_4px)_/_2)] rounded-md bg-blue-600/25 transition-transform duration-200 ${mode === 'usage' ? 'translate-x-full' : 'translate-x-0'}`} />
            {(['runtime', 'usage'] as const).map((m) => (
              <button key={m} onClick={() => setMode(m)} className={`relative z-10 h-8 text-sm font-medium transition-colors ${mode === m ? 'text-blue-100' : 'text-zinc-500 hover:text-zinc-200'}`}>
                {t(`activity.${m}`)}
              </button>
            ))}
          </div>
          <button
            onClick={clearLogs}
            className="flex min-h-10 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 text-sm font-medium text-zinc-300 hover:border-red-500/40 hover:text-red-300"
          >
            <Trash2 className="h-4 w-4" strokeWidth={1.75} />
            {t('activity.clear')}
          </button>
        </div>
      </div>

      {mode === 'usage' ? (
      <div className="grid min-h-0 flex-1 grid-cols-12 grid-rows-[auto_minmax(0,1.15fr)_minmax(0,0.85fr)] gap-5">
        <section className="col-span-12 grid grid-cols-[1.2fr_1fr_1fr_1fr_auto] gap-3">
          <div className="grid grid-cols-5 rounded-lg border border-zinc-800 bg-zinc-950/25 p-0.5">
            {(['today', '7d', 'month', 'year', 'all'] as const).map((range) => (
              <button
                key={range}
                onClick={() => setUsageRange(range)}
                className={`h-9 rounded-md text-xs font-medium transition-colors ${
                  usageRange === range ? 'bg-blue-600/20 text-blue-100' : 'text-zinc-500 hover:text-zinc-200'
                }`}
              >
                {t(`activity.range.${range}`)}
              </button>
            ))}
          </div>
          <FilterSelect
            label={t('activity.instance')}
            value={usageInstance}
            onChange={setUsageInstance}
            options={[
              { label: t('activity.activeInstance'), value: 'active' },
              { label: t('activity.all'), value: 'all' },
              ...instances.map((instance) => ({ label: instance.name, value: instance.id })),
            ]}
          />
          <FilterSelect
            label={t('activity.model')}
            value={usageModel}
            onChange={setUsageModel}
            options={[{ label: t('activity.all'), value: 'all' }, ...modelOptions.map((model) => ({ label: model, value: model }))]}
          />
          <FilterSelect
            label={t('activity.provider')}
            value={usageProvider}
            onChange={setUsageProvider}
            options={[{ label: t('activity.all'), value: 'all' }, ...providerOptions.map((provider) => ({ label: provider, value: provider }))]}
          />
          <div className="flex items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950/25 p-1">
            <button onClick={() => exportUsage('csv')} className="flex h-8 items-center gap-1.5 rounded-md px-2 text-xs font-medium text-zinc-300 hover:bg-zinc-800/80">
              <Download className="h-3.5 w-3.5" strokeWidth={1.75} />
              CSV
            </button>
            <button onClick={() => exportUsage('json')} className="flex h-8 items-center rounded-md px-2 text-xs font-medium text-zinc-300 hover:bg-zinc-800/80">
              JSON
            </button>
          </div>
        </section>
        <section className="col-span-12 grid grid-cols-4 gap-3">
          <StatCell icon={BarChart3} label={t('activity.totalTokens')} value={fmtTokens(totalTokens)} />
          <StatCell icon={LineChart} label={t('activity.requests')} value={requests} />
          <StatCell icon={Coins} label={t('activity.estimatedCost')} value={`$${estimatedCost.toFixed(3)}`} />
          <StatCell icon={WalletCards} label={t('activity.topModel')} value={modelUsage[0]?.label ?? '-'} />
        </section>

        <section className="col-span-8 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="flex items-start justify-between">
            <div>
              <h2 className="text-sm font-semibold text-zinc-200">{t('activity.tokensOverTime')}</h2>
              <p className="mt-1 text-xs text-zinc-500">{t('activity.usageEstimated')}</p>
            </div>
            <div className="flex items-center gap-3 text-xs text-zinc-500">
              <span className="flex items-center gap-1"><span className="h-2 w-2 rounded-full bg-blue-400" />{t('activity.input')}</span>
              <span className="flex items-center gap-1"><span className="h-2 w-2 rounded-full bg-cyan-300" />{t('activity.output')}</span>
            </div>
          </div>
          <div className="mt-5 min-h-0 flex-1 rounded-lg border border-zinc-800/70 bg-zinc-950/30 p-4">
            {totalTokens === 0 ? (
              <div className="flex h-full items-center justify-center text-center text-sm text-zinc-600">{t('activity.noUsage')}</div>
            ) : (
            <UsageBars values={usageInput} secondary={usageOutput} />
            )}
          </div>
        </section>

        <aside className="col-span-4 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-semibold text-zinc-200">{t('activity.modelRanking')}</h2>
            <span className="text-xs font-mono text-zinc-500">{topModelShare}%</span>
          </div>
          <div className="mt-5 space-y-4">
            {modelUsage.length === 0 ? (
              <p className="text-sm text-zinc-600">{t('activity.noUsage')}</p>
            ) : (
              modelUsage.slice(0, 5).map((m) => <ModelBar key={m.label} label={m.label} value={m.value} max={maxModel} />)
            )}
          </div>
        </aside>

        <section className="col-span-5 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <h2 className="text-sm font-semibold text-zinc-200">{t('activity.costTrend')}</h2>
          <div className="mt-4 min-h-0 flex-1 rounded-lg border border-zinc-800/70 bg-zinc-950/30 p-4">
            <UsageArea values={usageTrend} />
          </div>
        </section>

        <section className="col-span-7 grid min-h-0 grid-cols-[0.9fr_1.1fr] gap-5">
          <div className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
            <h2 className="text-sm font-semibold text-zinc-200">{t('activity.diagnostics')}</h2>
            <div className="mt-4 space-y-3">
              <div className={`rounded-lg border px-3 py-2.5 ${costAlert ? 'border-red-500/25 bg-red-500/10' : 'border-emerald-500/20 bg-emerald-500/10'}`}>
                <div className={costAlert ? 'text-xs font-medium text-red-300' : 'text-xs font-medium text-emerald-300'}>
                  {costAlert ? t('activity.costAnomaly') : t('activity.costNormal')}
                </div>
                <div className="mt-1 text-[11px] text-zinc-500">${maxCost.toFixed(3)} peak / ${avgCost.toFixed(3)} avg</div>
              </div>
              <div className="rounded-lg border border-zinc-800 bg-zinc-950/25 px-3 py-2.5">
                <div className="text-xs font-medium text-zinc-300">{t('activity.peakRequests')}</div>
                <div className="mt-1 text-[11px] text-zinc-500">
                  {peakBucket ? `${formatClock(peakBucket.timestamp)} · ${peakBucket.requests} ${t('activity.requests')}` : '-'}
                </div>
              </div>
              {exportNotice && <div className="truncate text-[11px] text-blue-300">{exportNotice}</div>}
            </div>
          </div>
          <div className="flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-zinc-200">{t('activity.usageLedger')}</h2>
              <span className="rounded-full bg-blue-500/10 px-2 py-1 text-[10px] font-medium uppercase tracking-wide text-blue-300">
                {t('activity.liveLedger')}
              </span>
            </div>
            <div className="mt-3 min-h-0 flex-1 divide-y divide-zinc-800/60 overflow-hidden">
              {usage?.records.slice(0, 8).map((record) => (
                <div key={record.id} className="grid grid-cols-[minmax(0,1fr)_auto_auto] gap-4 py-2.5 text-xs">
                  <span className="truncate text-zinc-300">{record.model}</span>
                  <span className="font-mono text-zinc-500">{formatClock(record.timestamp)}</span>
                  <span className="font-mono text-zinc-200">{fmtTokens(record.totalTokens)}</span>
                </div>
              ))}
              {(!usage || usage.records.length === 0) && <p className="py-8 text-center text-sm text-zinc-600">{t('activity.noUsage')}</p>}
            </div>
          </div>
        </section>
      </div>
      ) : (
      <div className="grid min-h-0 flex-1 grid-cols-12 grid-rows-[auto_minmax(0,1fr)] gap-5">
        <section className="col-span-12 grid grid-cols-5 gap-3">
          <StatCell icon={ActivityIcon} label={t('activity.current')} value={runningName} />
          <StatCell icon={Gauge} label={t('activity.status')} value={t(`status.${status}`)} />
          <StatCell icon={History} label={t('activity.today')} value={todaySessions.length} />
          <StatCell icon={ScrollText} label={t('activity.logLines')} value={logs.length} />
          <StatCell icon={TriangleAlert} label={t('activity.crashes')} value={crashed} />
        </section>

        <section className="col-span-8 flex min-h-0 flex-col overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60">
          <div className="flex shrink-0 items-center justify-between border-b border-zinc-800 px-5 py-3">
            <div className="flex items-center gap-2">
              <Terminal className="h-4 w-4 text-blue-300" strokeWidth={1.75} />
              <h2 className="text-sm font-semibold text-zinc-200">{t('activity.liveStream')}</h2>
            </div>
            <div className="flex items-center gap-3 text-xs tabular-nums text-zinc-500">
              <span>{t('activity.out')} {stdoutCount}</span>
              <span>{t('activity.err')} {stderrCount}</span>
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto bg-zinc-950/55 p-4 font-mono text-xs leading-6">
            {logs.length === 0 ? (
              <div className="flex h-full items-center justify-center text-center text-zinc-600">
                {t('activity.noLogs')}
              </div>
            ) : (
              logs.map((log, i) => (
                <div
                  key={i}
                  className={`grid grid-cols-[42px_minmax(0,1fr)] gap-3 border-b border-zinc-900/70 py-0.5 ${
                    log.stream === 'stderr' ? 'text-red-300' : 'text-zinc-300'
                  }`}
                >
                  <span className="select-none text-zinc-700">
                    {log.stream === 'stderr' ? t('activity.err') : t('activity.out')}
                  </span>
                  <span className="min-w-0 break-words">{log.line}</span>
                </div>
              ))
            )}
            <div ref={bottomRef} />
          </div>
        </section>

        <aside className="col-span-4 grid min-h-0 overflow-hidden grid-rows-[minmax(0,0.9fr)_minmax(0,1.1fr)] gap-5">
          <section className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-zinc-200">{t('activity.process')}</h2>
              <StatusDot status={status} />
            </div>
            <div className="mt-4 grid grid-cols-2 gap-3">
              <StatCell icon={Play} label={t('activity.started')} value={formatClock(processState?.startedAt)} />
              <StatCell icon={Clock3} label={t('activity.uptime')} value={formatDuration(processState?.startedAt, null)} />
              <StatCell icon={Square} label="PID" value={processState?.pid ?? '-'} />
              <StatCell icon={ListRestart} label={t('activity.avgRun')} value={formatSeconds(avgSeconds)} />
            </div>
          </section>

          <section className="flex min-h-0 flex-col overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-zinc-200">{t('activity.history')}</h2>
              <span className="text-xs text-zinc-600">{recent.length}</span>
            </div>
            {recent.length === 0 ? (
              <p className="flex flex-1 items-center justify-center text-center text-xs text-zinc-600">
                {t('activity.noHistory')}
              </p>
            ) : (
              <div
                onWheel={holdWheelInPanel}
                className="mt-3 min-h-0 flex-1 space-y-2 overflow-y-scroll overscroll-contain pr-1"
              >
                {recent.map((session) => (
                  <SessionRow
                    key={session.id}
                    session={session}
                    name={instances.find((i) => i.id === session.instanceId)?.name ?? session.instanceId}
                  />
                ))}
              </div>
            )}
          </section>
        </aside>
      </div>
      )}
    </div>
  )
}
