import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { StatusDot } from '../components/StatusDot'

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg bg-zinc-800/60 px-4 py-3">
      <div className="text-xs uppercase tracking-wide text-zinc-500">{label}</div>
      <div className="mt-0.5 text-lg font-semibold text-zinc-100">{value}</div>
    </div>
  )
}

export function Home() {
  const t = useT()
  const instances = useAppStore((s) => s.instances)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const activeId = useAppStore((s) => s.activeId)
  const provider = useAppStore((s) => s.provider)
  const processState = useAppStore((s) => s.processState)
  const busy = useAppStore((s) => s.busy)
  const launch = useAppStore((s) => s.launch)
  const stop = useAppStore((s) => s.stop)
  const switchInstance = useAppStore((s) => s.switchInstance)
  const setPage = useAppStore((s) => s.setPage)

  const status = processState?.status ?? 'stopped'
  const running = status === 'running' || status === 'starting'
  const runtimeVersion = activeInstance?.runtime.version || '—'
  const providerName = provider?.profile.name ?? '—'
  const model = provider?.profile.model || 'deepseek-v4-flash'
  const pluginCount = activeInstance?.plugins.length ?? 0
  const hasKey = provider?.hasKey ?? false

  return (
    <div className="mx-auto max-w-3xl p-8">
      <div className="mb-4 flex items-center justify-between">
        <h1 className="text-2xl font-bold">{t('home.title')}</h1>
        {instances.length > 1 && (
          <select
            value={activeId ?? ''}
            onChange={(e) => void switchInstance(e.target.value)}
            className="rounded-lg border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-sm text-zinc-200 outline-none focus:border-blue-500"
          >
            {instances.map((i) => (
              <option key={i.id} value={i.id}>
                {i.name}
              </option>
            ))}
          </select>
        )}
      </div>

      <div className="rounded-2xl border border-zinc-800 bg-zinc-900/60 p-8">
        <div className="mb-6 flex items-center justify-between">
          <div>
            <h2 className="text-3xl font-extrabold text-zinc-50">{activeInstance?.name ?? '…'}</h2>
            <p className="mt-1 text-sm text-zinc-400">
              {t('home.subtitle')} · <span className="font-mono">{activeInstance?.id}</span>
            </p>
          </div>
          <div className="flex items-center gap-2 rounded-full border border-zinc-700 bg-zinc-800/60 px-4 py-1.5 text-sm text-zinc-300">
            <StatusDot status={status} />
            <span className="capitalize">{t(`status.${status}`)}</span>
          </div>
        </div>

        <div className="mb-8 grid grid-cols-2 gap-4">
          <Stat label={t('home.runtime')} value={`DSH ${runtimeVersion}`} />
          <Stat label={t('home.provider')} value={providerName} />
          <Stat label={t('home.model')} value={model} />
          <Stat label={t('home.plugins')} value={String(pluginCount)} />
        </div>

        {!hasKey && (
          <div className="mb-6 rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-300">
            {t('home.noKey')}{' '}
            <button
              onClick={() => setPage('settings')}
              className="font-semibold underline underline-offset-2"
            >
              {t('home.configure')}
            </button>
          </div>
        )}

        <button
          onClick={() => (running ? void stop() : void launch(activeId ?? ''))}
          disabled={busy || !activeId}
          className={`w-full rounded-xl py-4 text-lg font-bold text-white transition-colors disabled:opacity-50 ${
            running ? 'bg-red-500 hover:bg-red-600' : 'bg-blue-500 hover:bg-blue-400'
          }`}
        >
          {busy
            ? t('home.working')
            : running
              ? t('home.stop')
              : t('home.launch', { name: activeInstance?.name ?? '' })}
        </button>
        <p className="mt-3 text-center text-xs text-zinc-500">{t('home.footer')}</p>
      </div>
    </div>
  )
}
