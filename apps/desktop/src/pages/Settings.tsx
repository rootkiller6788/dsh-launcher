import { useEffect, useState } from 'react'
import { useAppStore } from '../stores/appStore'
import { useT } from '../lib/i18n'
import { ipc } from '../lib/ipc'
import type { RuntimeManagerView, Theme, VerifyReport } from '../lib/types'

const DEFAULT_BASE_URL = 'https://api.deepseek.com'

/** Mirrors DSH's Appearance row: 深色 / 浅色 / 跟随系统. */
const THEMES: { value: Theme; label: string }[] = [
  { value: 'dark', label: 'settings.themeDark' },
  { value: 'light', label: 'settings.themeLight' },
  { value: 'system', label: 'settings.themeSystem' },
]

const MODELS = [
  { id: 'deepseek-v4-flash', label: 'DeepSeek-V4-Flash (default)' },
  { id: 'deepseek-v4-pro', label: 'DeepSeek-V4-Pro' },
]

function Field({
  label,
  hint,
  children,
}: {
  label: string
  hint?: string
  children: React.ReactNode
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-zinc-400">{label}</label>
      {children}
      {hint && <p className="mt-1 text-[11px] text-zinc-500">{hint}</p>}
    </div>
  )
}

type Flash = 'saving' | 'saved' | 'failed' | null

export function Settings() {
  const t = useT()
  const provider = useAppStore((s) => s.provider)
  const settings = useAppStore((s) => s.settings)
  const system = useAppStore((s) => s.system)
  const busy = useAppStore((s) => s.busy)
  const language = useAppStore((s) => s.language)
  const setLanguage = useAppStore((s) => s.setLanguage)
  const theme = useAppStore((s) => s.theme)
  const setTheme = useAppStore((s) => s.setTheme)
  const saveProvider = useAppStore((s) => s.saveProvider)
  const saveSettings = useAppStore((s) => s.saveSettings)
  const removeProviderKey = useAppStore((s) => s.removeProviderKey)
  const refreshProvider = useAppStore((s) => s.refreshProvider)
  const refreshSystem = useAppStore((s) => s.refreshSystem)

  const [name, setName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [baseUrl, setBaseUrl] = useState(DEFAULT_BASE_URL)
  const [model, setModel] = useState(MODELS[0].id)
  const [dshPath, setDshPath] = useState('')
  const [flash, setFlash] = useState<Flash>(null)

  useEffect(() => {
    if (provider) {
      setName(provider.profile.name)
      setBaseUrl(provider.profile.baseUrl ?? DEFAULT_BASE_URL)
      setModel(provider.profile.model ?? MODELS[0].id)
    }
    if (settings) setDshPath(settings.dshPath ?? '')
  }, [provider, settings])

  const save = async () => {
    setFlash('saving')
    const okProvider = await saveProvider(
      { id: 'default', name, baseUrl: baseUrl.trim() || null, model: model.trim() || null },
      apiKey || null,
    )
    const okSettings = await saveSettings({
      dshPath: dshPath.trim() || null,
      lastInstance: useAppStore.getState().settings?.lastInstance ?? null,
      language,
      theme: useAppStore.getState().theme,
    })
    await refreshProvider()
    setFlash(okProvider && okSettings ? 'saved' : 'failed')
    if (okProvider && okSettings) setApiKey('')
  }

  const clearKey = async () => {
    await removeProviderKey()
    await refreshProvider()
    setFlash('saved')
  }

  return (
    <div className="mx-auto max-w-2xl p-8">
      <h1 className="mb-6 text-2xl font-bold">{t('settings.title')}</h1>

      {/* Appearance — same three choices as DSH, synced with the DSH window */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h2 className="text-base font-semibold text-zinc-100">{t('settings.appearance')}</h2>
            <p className="text-[11px] text-zinc-500">{t('settings.appearanceHint')}</p>
          </div>
          <div className="flex rounded-lg border border-zinc-700 bg-zinc-950 p-1">
            {THEMES.map((th) => (
              <button
                key={th.value}
                onClick={() => void setTheme(th.value)}
                className={`rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
                  theme === th.value ? 'bg-blue-500 text-white' : 'text-zinc-400 hover:text-zinc-200'
                }`}
              >
                {t(th.label)}
              </button>
            ))}
          </div>
        </div>
      </section>

      {/* Language */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h2 className="text-base font-semibold text-zinc-100">{t('settings.language')}</h2>
            <p className="text-[11px] text-zinc-500">{t('settings.languageHint')}</p>
          </div>
          <div className="flex rounded-lg border border-zinc-700 bg-zinc-950 p-1">
            <button
              onClick={() => void setLanguage('en')}
              className={`rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
                language === 'en' ? 'bg-blue-500 text-white' : 'text-zinc-400 hover:text-zinc-200'
              }`}
            >
              English
            </button>
            <button
              onClick={() => void setLanguage('zh')}
              className={`rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${
                language === 'zh' ? 'bg-blue-500 text-white' : 'text-zinc-400 hover:text-zinc-200'
              }`}
            >
              中文
            </button>
          </div>
        </div>
      </section>

      {/* Provider */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <h2 className="mb-4 text-base font-semibold text-zinc-100">{t('settings.provider')}</h2>
        <div className="space-y-4">
          <Field label={t('settings.name')}>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-zinc-100 outline-none focus:border-blue-500"
            />
          </Field>
          <Field
            label={t('settings.apiKey')}
            hint={provider?.hasKey ? t('settings.keyStored') : t('settings.keyHint')}
          >
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={provider?.hasKey ? t('settings.stored') : t('settings.sk')}
              className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-zinc-100 outline-none focus:border-blue-500"
            />
          </Field>
          <Field label={t('settings.baseUrl')} hint={t('settings.baseUrlHint')}>
            <input
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder={DEFAULT_BASE_URL}
              className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-zinc-100 outline-none focus:border-blue-500"
            />
          </Field>
          <Field label={t('settings.model')} hint={t('settings.modelHint')}>
            <select
              value={model}
              onChange={(e) => setModel(e.target.value)}
              className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 text-zinc-100 outline-none focus:border-blue-500"
            >
              {MODELS.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </Field>
        </div>
      </section>

      {/* Provider list — what's actually saved */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <h2 className="mb-4 text-base font-semibold text-zinc-100">{t('settings.savedProviders')}</h2>
        {provider ? (
          <div className="flex items-start justify-between gap-4 rounded-xl border border-zinc-800 bg-zinc-950/60 p-4">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="font-semibold text-zinc-100">{provider.profile.name}</span>
                <KeyBadge hasKey={provider.hasKey} />
              </div>
              <dl className="mt-2 space-y-1 text-xs text-zinc-400">
                <div className="flex gap-2">
                  <dt className="w-16 shrink-0 text-zinc-500">{t('settings.baseUrl')}</dt>
                  <dd className="truncate font-mono">
                    {provider.profile.baseUrl ?? t('settings.default')}
                  </dd>
                </div>
                <div className="flex gap-2">
                  <dt className="w-16 shrink-0 text-zinc-500">{t('settings.model')}</dt>
                  <dd className="truncate font-mono">
                    {provider.profile.model ?? t('settings.default')}
                  </dd>
                </div>
              </dl>
            </div>
            {provider.hasKey && (
              <button
                onClick={() => void clearKey()}
                disabled={busy}
                className="shrink-0 rounded-lg border border-zinc-700 px-3 py-1.5 text-xs text-zinc-400 hover:border-red-500/50 hover:text-red-400 disabled:opacity-50"
              >
                {t('settings.clearKey')}
              </button>
            )}
          </div>
        ) : (
          <p className="text-sm text-zinc-500">{t('settings.nothingSaved')}</p>
        )}
      </section>

      {/* Runtime */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <h2 className="mb-4 text-base font-semibold text-zinc-100">{t('settings.runtime')}</h2>
        <Field label={t('settings.dshPath')} hint={t('settings.dshHint')}>
          <input
            value={dshPath}
            onChange={(e) => setDshPath(e.target.value)}
            placeholder="…/deepseek-harness-master/apps/cli/lib/bin.js"
            className="mt-1 w-full rounded-lg border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:border-blue-500"
          />
        </Field>
        <RuntimePanel />
      </section>

      {/* Environment */}
      <section className="mb-8 rounded-2xl border border-zinc-800 bg-zinc-900/60 p-6">
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-base font-semibold text-zinc-100">{t('settings.environment')}</h2>
          <button
            onClick={() => void refreshSystem()}
            className="rounded-lg bg-zinc-800 px-3 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-700"
          >
            {t('settings.recheck')}
          </button>
        </div>
        <ul className="space-y-2 text-sm">
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
        </ul>
      </section>

      <button
        onClick={() => void save()}
        disabled={busy}
        className={`w-full rounded-lg py-3 font-semibold text-white transition-colors disabled:opacity-50 ${
          flash === 'failed' ? 'bg-red-600' : 'bg-blue-500 hover:bg-blue-400'
        }`}
      >
        {flash === 'saving' ? t('settings.saving') : flash === 'saved' ? t('settings.saved') : t('settings.save')}
      </button>
      {flash === 'saved' && (
        <p className="mt-2 text-center text-xs text-emerald-400">{t('settings.savedMsg')}</p>
      )}
      {flash === 'failed' && (
        <p className="mt-2 text-center text-xs text-red-400">{t('settings.failedMsg')}</p>
      )}
    </div>
  )
}

function KeyBadge({ hasKey }: { hasKey: boolean }) {
  const t = useT()
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium ${
        hasKey ? 'bg-emerald-500/15 text-emerald-400' : 'bg-amber-500/15 text-amber-400'
      }`}
    >
      <span className={`h-1.5 w-1.5 rounded-full ${hasKey ? 'bg-emerald-400' : 'bg-amber-400'}`} />
      {hasKey ? t('settings.keyStoredBadge') : t('settings.noKeyBadge')}
    </span>
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
