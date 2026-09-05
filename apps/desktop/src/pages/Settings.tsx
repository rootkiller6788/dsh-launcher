import { useEffect, useState, type ReactNode } from 'react'
import {
  Archive,
  Cpu,
  FileCheck2,
  FolderOpen,
  RotateCw,
  Save,
  Send,
  ServerCog,
  ShieldCheck,
  Terminal,
} from 'lucide-react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { ipc } from '../lib/ipc'
import type { RuntimeManagerView, VerifyReport } from '../lib/types'

function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: ReactNode
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-zinc-400">{label}</label>
      {children}
      {hint && <p className="mt-1 text-[11px] text-zinc-500">{hint}</p>}
    </div>
  )
}

function PreferenceCard({
  icon: Icon,
  title,
  subtitle,
  children,
}: {
  icon: typeof ServerCog
  title: string
  subtitle?: string
  children: ReactNode
}) {
  return (
    <section className="min-h-0 rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
      <div className="mb-4 flex items-start gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-blue-500/10 text-blue-300">
          <Icon className="h-4 w-4" strokeWidth={1.75} />
        </div>
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-zinc-100">{title}</h2>
          {subtitle && <p className="mt-0.5 text-xs leading-5 text-zinc-500">{subtitle}</p>}
        </div>
      </div>
      {children}
    </section>
  )
}

/** Minimal role=switch toggle used by the telemetry consent row. */
function Toggle({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      onClick={() => onChange(!on)}
      className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
        on ? 'bg-blue-500' : 'bg-zinc-700'
      }`}
    >
      <span
        className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-all ${
          on ? 'left-[22px]' : 'left-0.5'
        }`}
      />
    </button>
  )
}

type Flash = 'saving' | 'saved' | 'failed' | null

export function Settings() {
  const t = useT()
  const settings = useAppStore((s) => s.settings)
  const system = useAppStore((s) => s.system)
  const busy = useAppStore((s) => s.busy)
  const appPaths = useAppStore((s) => s.appPaths)
  const saveSettings = useAppStore((s) => s.saveSettings)
  const refreshSystem = useAppStore((s) => s.refreshSystem)
  const refreshAppPaths = useAppStore((s) => s.refreshAppPaths)
  const exportEnvironment = useAppStore((s) => s.exportEnvironment)

  const [dshPath, setDshPath] = useState('')
  const [telemetryEnabled, setTelemetryEnabled] = useState(false)
  const [telemetryEndpoint, setTelemetryEndpoint] = useState('')
  const [flash, setFlash] = useState<Flash>(null)
  const [exportedPath, setExportedPath] = useState<string | null>(null)

  // The data root + edition flag is a one-shot read (cheap); it never changes
  // while the app runs, so load it once on mount.
  useEffect(() => {
    void refreshAppPaths()
  }, [refreshAppPaths])

  useEffect(() => {
    if (settings) {
      setDshPath(settings.dshPath ?? '')
      setTelemetryEnabled(settings.telemetryEnabled ?? false)
      setTelemetryEndpoint(settings.telemetryEndpoint ?? '')
    }
  }, [settings])

  const save = async () => {
    setFlash('saving')
    const current = useAppStore.getState()
    // Spread the persisted doc first so fields this page doesn't edit (nodePath
    // and anything added later) survive a Save instead of being dropped.
    const okSettings = await saveSettings({
      ...(current.settings ?? {}),
      dshPath: dshPath.trim() || null,
      lastInstance: current.settings?.lastInstance ?? null,
      language: current.language,
      theme: current.theme,
      telemetryEnabled,
      telemetryEndpoint: telemetryEndpoint.trim() || null,
    })
    setFlash(okSettings ? 'saved' : 'failed')
  }

  const readyCount = Number(!!system?.node?.present) + Number(!!system?.git?.present) + Number(!!system?.dsh)
  const runtimeLabel = system?.dsh?.version ?? (system?.dshError ? t('settings.notChecked') : t('settings.loading'))
  const exportCurrentEnvironment = async () => {
    setExportedPath(null)
    const result = await exportEnvironment()
    if (result) setExportedPath(result.path)
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-5 overflow-hidden p-6">
      <div className="flex shrink-0 items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-50">{t('settings.title')}</h1>
          <p className="mt-0.5 text-sm text-zinc-500">{t('settings.subtitle')}</p>
        </div>
        <button
          onClick={() => void save()}
          disabled={busy}
          className={`flex min-h-10 items-center gap-2 rounded-lg px-4 text-sm font-semibold text-white transition-colors disabled:opacity-50 ${
            flash === 'failed' ? 'bg-red-600' : 'bg-blue-600 hover:bg-blue-500'
          }`}
        >
          <Save className="h-4 w-4" strokeWidth={1.75} />
          {flash === 'saving' ? t('settings.saving') : flash === 'saved' ? t('settings.saved') : t('settings.save')}
        </button>
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-12 gap-5">
        <aside className="col-span-4 flex min-h-0 flex-col rounded-lg border border-zinc-800 bg-zinc-900/60 p-5">
          <div className="flex items-center gap-3">
            <div className="flex h-11 w-11 items-center justify-center rounded-lg bg-blue-500/10 text-blue-300">
              <ServerCog className="h-5 w-5" strokeWidth={1.75} />
            </div>
            <div>
              <h2 className="text-sm font-semibold text-zinc-100">{t('settings.infrastructure')}</h2>
              <p className="mt-0.5 text-xs text-zinc-500">{t('settings.infrastructureHint')}</p>
            </div>
          </div>

          <div className="mt-5 grid grid-cols-2 gap-3">
            <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
              <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('settings.readiness')}</div>
              <div className="mt-1 text-2xl font-semibold tabular-nums text-zinc-100">{readyCount}/3</div>
            </div>
            <div className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
              <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('settings.runtime')}</div>
              <div className="mt-2 truncate font-mono text-xs text-zinc-300">{runtimeLabel}</div>
            </div>
          </div>

          <div className="mt-5 space-y-2 rounded-lg border border-zinc-800/70 bg-zinc-950/25 p-4">
            <EnvRow name="Node" item={system?.node} />
            <EnvRow name="Git" item={system?.git} />
            <EnvRow
              name="DSH"
              item={
                system?.dsh
                  ? { present: true, version: system.dsh.version, note: system.dsh.binPath }
                  : { present: false, version: null, note: system?.dshError ?? t('settings.notChecked') }
              }
            />
          </div>

          <div className="mt-auto rounded-lg border border-blue-500/20 bg-blue-500/10 p-4">
            <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-blue-300">
              <ShieldCheck className="h-3.5 w-3.5" strokeWidth={1.75} />
              {t('settings.scope')}
            </div>
            <p className="mt-3 text-xs leading-5 text-blue-100/80">{t('settings.scopeHint')}</p>
          </div>
        </aside>

        <section className="col-span-8 min-h-0 overflow-y-auto pr-1">
          <div className="grid gap-5">
            <PreferenceCard icon={Archive} title={t('settings.environmentPackage')} subtitle={t('settings.environmentPackageHint')}>
              <div className="flex items-center justify-between gap-4 rounded-lg border border-zinc-800/70 bg-zinc-950/35 p-4">
                <div className="min-w-0">
                  <div className="flex items-center gap-2 text-sm font-medium text-zinc-200">
                    <FileCheck2 className="h-4 w-4 text-blue-300" strokeWidth={1.75} />
                    {t('settings.environmentPackageTitle')}
                  </div>
                  <p className="mt-1 text-xs leading-5 text-zinc-500">{t('settings.environmentPackageCopy')}</p>
                  {exportedPath && (
                    <p className="mt-3 truncate font-mono text-[11px] text-emerald-400">
                      {t('settings.exportedTo', { path: exportedPath })}
                    </p>
                  )}
                </div>
                <button
                  onClick={() => void exportCurrentEnvironment()}
                  disabled={busy}
                  className="shrink-0 rounded-lg bg-blue-500 px-4 py-2 text-sm font-semibold text-white hover:bg-blue-400 disabled:opacity-40"
                >
                  {busy ? t('settings.rtBusy') : t('settings.exportEnvironment')}
                </button>
              </div>
            </PreferenceCard>

            <PreferenceCard icon={Terminal} title={t('settings.runtime')} subtitle={t('settings.runtimeConsoleHint')}>
              <Field label={t('settings.dshPath')} hint={t('settings.dshHint')}>
                <input value={dshPath} onChange={(e) => setDshPath(e.target.value)} placeholder=".../deepseek-harness-master/apps/cli/lib/bin.js" className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500" />
              </Field>
              <RuntimePanel />
            </PreferenceCard>

            <PreferenceCard icon={Cpu} title={t('settings.environment')} subtitle={t('settings.environmentHint')}>
              <button onClick={() => void refreshSystem()} className="flex items-center gap-2 rounded-lg bg-zinc-800 px-3 py-2 text-xs font-medium text-zinc-300 hover:bg-zinc-700">
                <RotateCw className="h-3.5 w-3.5" strokeWidth={1.75} />
                {t('settings.recheck')}
              </button>
            </PreferenceCard>

            <PreferenceCard icon={FolderOpen} title={t('settings.storage')} subtitle={t('settings.storageHint')}>
              {appPaths ? (
                <div className="flex items-start justify-between gap-4 rounded-lg border border-zinc-800/70 bg-zinc-950/35 p-4">
                  <div className="min-w-0">
                    <span
                      className={`inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-semibold ${
                        appPaths.portable
                          ? 'bg-emerald-500/15 text-emerald-300'
                          : 'bg-blue-500/15 text-blue-300'
                      }`}
                    >
                      {appPaths.portable ? t('settings.portableMode') : t('settings.installedMode')}
                    </span>
                    <p className="mt-2 text-[11px] uppercase tracking-wide text-zinc-500">{t('settings.dataDir')}</p>
                    <p className="mt-1 truncate font-mono text-xs text-zinc-200" title={appPaths.root}>
                      {appPaths.root}
                    </p>
                  </div>
                  <button
                    onClick={() => void ipc.revealDataDir()}
                    className="shrink-0 rounded-lg bg-zinc-800 px-3 py-2 text-xs font-medium text-zinc-300 hover:bg-zinc-700"
                  >
                    {t('settings.openDataDir')}
                  </button>
                </div>
              ) : (
                <p className="text-xs text-zinc-500">{t('settings.loading')}</p>
              )}
            </PreferenceCard>

            <PreferenceCard icon={Send} title={t('settings.telemetry')} subtitle={t('settings.telemetryHint')}>
              <div className="space-y-3">
                <div className="flex items-start justify-between gap-4 rounded-lg border border-zinc-800/70 bg-zinc-950/35 p-4">
                  <div className="min-w-0">
                    <p className="text-sm font-medium text-zinc-200">{t('settings.telemetryConsent')}</p>
                    <p className="mt-1 text-xs leading-5 text-zinc-500">{t('settings.telemetryCopy')}</p>
                  </div>
                  <Toggle on={telemetryEnabled} onChange={setTelemetryEnabled} />
                </div>
                {telemetryEnabled && (
                  <Field label={t('settings.telemetryEndpoint')} hint={t('settings.telemetryEndpointHint')}>
                    <input
                      value={telemetryEndpoint}
                      onChange={(e) => setTelemetryEndpoint(e.target.value)}
                      placeholder="https://ingest.example.com/v1/crashes"
                      className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500"
                    />
                  </Field>
                )}
                <p className="rounded-lg border border-zinc-800/70 bg-zinc-950/25 px-3 py-2 text-[11px] leading-5 text-zinc-500">
                  {t('settings.telemetryScope')}
                </p>
              </div>
            </PreferenceCard>

            {flash === 'saved' && <p className="text-center text-xs text-emerald-400">{t('settings.savedMsg')}</p>}
            {flash === 'failed' && <p className="text-center text-xs text-red-400">{t('settings.failedMsg')}</p>}
          </div>
        </section>
      </div>
    </div>
  )
}

/**
 * Managed-runtime manager: Node status + installed DSH runtimes (set active /
 * verify / remove) + import a DSH checkout into the managed folder.
 */
function RuntimePanel() {
  const t = useT()
  const [view, setView] = useState<RuntimeManagerView | null>(null)
  const [source, setSource] = useState('')
  const [version, setVersion] = useState('')
  const [busy, setBusy] = useState(false)
  const [verify, setVerify] = useState<Record<string, VerifyReport>>({})
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null)

  const refresh = async () => {
    try {
      setView(await ipc.runtimeList())
    } catch (e) {
      setMsg({ ok: false, text: String(e) })
    }
  }
  useEffect(() => {
    void refresh()
  }, [])

  const run = async (action: () => Promise<unknown>, okText: string) => {
    setBusy(true)
    setMsg(null)
    try {
      await action()
      setMsg({ ok: true, text: okText })
      await refresh()
    } catch (e) {
      setMsg({ ok: false, text: String(e) })
    } finally {
      setBusy(false)
    }
  }

  const importTree = () =>
    run(
      () => ipc.runtimeInstall(source.trim(), version.trim() || null),
      t('settings.rtImported', { v: version.trim() || '…' }),
    )
  const setActive = (v: string) =>
    run(() => ipc.runtimeSetActive(v), t('settings.rtActivated', { v }))
  const removeRuntime = (v: string) =>
    run(() => ipc.runtimeRemove(v), t('settings.rtRemoved', { v }))
  const verifyRuntime = async (v: string) => {
    setBusy(true)
    try {
      const rep = await ipc.runtimeVerify(v)
      setVerify((m) => ({ ...m, [v]: rep }))
    } catch (e) {
      setMsg({ ok: false, text: String(e) })
    } finally {
      setBusy(false)
    }
  }

  const active = view?.active
  const shownActive =
    active ??
    (view && view.runtimes.length > 0 ? t('settings.rtAuto') : t('settings.rtNone'))

  return (
    <div className="mt-6 space-y-4 border-t border-zinc-800 pt-5">
      {/* Node */}
      <div className="flex items-start gap-2 text-sm">
        <span
          className={`mt-1 h-2 w-2 shrink-0 rounded-full ${
            view?.node.present ? 'bg-emerald-400' : 'bg-red-500'
          }`}
        />
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium text-zinc-300">{t('settings.rtNode')}</span>
            <span className={view?.node.present ? 'text-zinc-500' : 'text-red-400'}>
              {view
                ? view.node.present
                  ? `${view.node.version ?? ''} · ${t('settings.rtNodeReady')}`
                  : t('settings.rtNodeMissing')
                : t('settings.loading')}
            </span>
          </div>
          {view?.node.path && (
            <p className="truncate font-mono text-xs text-zinc-500">{view.node.path}</p>
          )}
          {view?.node.error && <p className="text-xs text-red-400">{view.node.error}</p>}
        </div>
      </div>

      {/* Active */}
      <div className="text-sm text-zinc-400">
        <span className="font-medium text-zinc-300">{t('settings.rtActive')}: </span>
        <span className="font-mono text-xs">{shownActive}</span>
      </div>

      {/* Installed list */}
      <div>
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-zinc-500">
          {t('settings.rtInstalled')}
        </h3>
        {!view ? (
          <p className="text-xs text-zinc-500">{t('settings.loading')}</p>
        ) : view.runtimes.length === 0 ? (
          <p className="text-xs text-zinc-500">{t('settings.rtEmpty')}</p>
        ) : (
          <ul className="space-y-2">
            {view.runtimes.map((r) => {
              const rep = verify[r.version]
              const isActive = r.version === active
              return (
                <li
                  key={r.version}
                  className="rounded-xl border border-zinc-800 bg-zinc-950/60 p-3"
                >
                  <div className="flex items-center gap-2">
                    <span
                      className={`h-2 w-2 shrink-0 rounded-full ${
                        r.verified ? 'bg-emerald-400' : 'bg-red-500'
                      }`}
                    />
                    <span className="font-mono text-sm font-medium text-zinc-100">
                      {r.version}
                    </span>
                    {isActive && (
                      <span className="rounded-full bg-blue-500/15 px-2 py-0.5 text-[11px] font-medium text-blue-400">
                        {t('settings.rtActiveBadge')}
                      </span>
                    )}
                    <span
                      className={`text-[11px] ${r.verified ? 'text-emerald-400' : 'text-red-400'}`}
                    >
                      {r.verified ? t('settings.rtOk') : t('settings.rtBroken')}
                    </span>
                    <span className="ml-auto flex gap-1">
                      {!isActive && (
                        <button
                          onClick={() => void setActive(r.version)}
                          disabled={busy}
                          className="rounded-md border border-zinc-700 px-2 py-1 text-[11px] text-zinc-300 hover:border-blue-500/50 hover:text-blue-400 disabled:opacity-50"
                        >
                          {t('settings.rtSetActive')}
                        </button>
                      )}
                      <button
                        onClick={() => void verifyRuntime(r.version)}
                        disabled={busy}
                        className="rounded-md border border-zinc-700 px-2 py-1 text-[11px] text-zinc-300 hover:border-blue-500/50 hover:text-blue-400 disabled:opacity-50"
                      >
                        {t('settings.rtVerify')}
                      </button>
                      <button
                        onClick={() => void removeRuntime(r.version)}
                        disabled={busy}
                        className="rounded-md border border-zinc-700 px-2 py-1 text-[11px] text-zinc-300 hover:border-red-500/50 hover:text-red-400 disabled:opacity-50"
                      >
                        {t('settings.rtRemove')}
                      </button>
                    </span>
                  </div>
                  <p className="mt-1 truncate font-mono text-[11px] text-zinc-500">
                    {r.binPath}
                  </p>
                  {rep && (
                    <p
                      className={`mt-1 text-[11px] ${
                        rep.dshOk && rep.nodeOk ? 'text-emerald-400' : 'text-amber-400'
                      }`}
                    >
                      DSH {rep.dshVersion ?? '—'} · Node {rep.nodeVersion ?? '—'} · {rep.message}
                    </p>
                  )}
                </li>
              )
            })}
          </ul>
        )}
      </div>

      {/* Import */}
      <div className="space-y-2 rounded-xl border border-zinc-800 bg-zinc-950/60 p-3">
        <h3 className="text-xs font-medium uppercase tracking-wide text-zinc-500">
          {t('settings.rtImport')}
        </h3>
        <p className="text-[11px] text-zinc-500">{t('settings.rtImportHint')}</p>
        <div className="flex gap-2">
          <input
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder="D:\…\deepseek-harness-master"
            className="min-w-0 flex-1 rounded-lg border border-zinc-700 bg-zinc-900 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500"
          />
          <input
            value={version}
            onChange={(e) => setVersion(e.target.value)}
            placeholder={t('settings.rtVersionOpt')}
            className="w-40 rounded-lg border border-zinc-700 bg-zinc-900 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500"
          />
          <button
            onClick={() => void importTree()}
            disabled={busy || !source.trim()}
            className="rounded-lg bg-blue-500 px-4 py-2 text-xs font-semibold text-white hover:bg-blue-400 disabled:opacity-50"
          >
            {busy ? t('settings.rtBusy') : t('settings.rtImportBtn')}
          </button>
        </div>
      </div>

      {msg && (
        <p className={`text-xs ${msg.ok ? 'text-emerald-400' : 'text-red-400'}`}>{msg.text}</p>
      )}
      {view?.error && <p className="text-xs text-amber-400">{view.error}</p>}
    </div>
  )
}

function EnvRow({
  name,
  item,
}: {
  name: string
  item?: { present: boolean; version?: string | null; note?: string | null }
}) {
  const t = useT()
  if (!item) {
    return (
      <li className="flex items-center gap-2 text-zinc-500">
        <span className="h-2 w-2 rounded-full bg-blue-500" /> {name} — {t('settings.loading')}
      </li>
    )
  }
  return (
    <li className="flex items-center gap-2">
      <span className={`h-2 w-2 rounded-full ${item.present ? 'bg-emerald-400' : 'bg-red-500'}`} />
      <span className="w-12 font-medium text-zinc-300">{name}</span>
      <span className={item.present ? 'text-zinc-400' : 'text-red-400'}>
        {item.present ? item.version : item.note}
      </span>
    </li>
  )
}
