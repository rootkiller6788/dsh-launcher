import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  Activity as ActivityIcon,
  Boxes,
  Copy,
  LayoutDashboard,
  Minus,
  PackageCheck,
  Play,
  Settings as SettingsIcon,
  Square,
  Store,
  X,
  type LucideIcon,
} from 'lucide-react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { Page } from './lib/types'
import { useAppStore } from './stores/appStore'
import { applyTheme } from './lib/theme'
import { useT } from './lib/i18n'
import { StatusDot } from './components/StatusDot'
import logo from './assets/dshl-logo.png'
import { Overview } from './pages/Overview'
import { Instances } from './pages/Instances'
import { Market } from './pages/Market'
import { Library } from './pages/Library'
import { Installs } from './pages/Installs'
import { Activity } from './pages/Activity'
import { Settings } from './pages/Settings'

const NAV: { id: Page; Icon: LucideIcon }[] = [
  { id: 'overview', Icon: LayoutDashboard },
  { id: 'instances', Icon: Boxes },
  { id: 'market', Icon: Store },
  { id: 'library', Icon: PackageCheck },
  { id: 'activity', Icon: ActivityIcon },
  { id: 'settings', Icon: SettingsIcon },
]

/** The app runs frameless (native title bar off) — these replace it. */
const appWindow = getCurrentWindow()

/** Windows-style title-bar control (minimize / maximize-restore / close). */
function TitleBarButton({
  onClick,
  close,
  children,
}: {
  onClick: () => void
  close?: boolean
  children: ReactNode
}) {
  return (
    <button
      onClick={onClick}
      className={`flex h-full w-[46px] items-center justify-center transition-colors ${
        close
          ? 'text-zinc-300 hover:bg-red-500 hover:text-white'
          : 'text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100'
      }`}
    >
      {children}
    </button>
  )
}

function Workspace() {
  const t = useT()
  const activeId = useAppStore((s) => s.activeId)
  const activeInstance = useAppStore((s) => s.activeInstance)
  const busy = useAppStore((s) => s.busy)
  const dshUrl = useAppStore((s) => s.dshUrl)
  const processState = useAppStore((s) => s.processState)
  const launch = useAppStore((s) => s.launch)
  const setShellMode = useAppStore((s) => s.setShellMode)
  const status = processState?.status ?? 'stopped'
  const live = status === 'running' || status === 'starting' || status === 'degraded'
  const paintedRef = useRef(false)

  // Stage timing (Stage 11): once the workspace iframe first renders after a
  // launch, emit a debug log with the end-to-end launch→paint latency. The
  // launch action stamps `launchStartedAt`; this clears it so the next launch
  // starts a fresh measurement.
  useEffect(() => {
    if (!(live && dshUrl) || paintedRef.current) return
    paintedRef.current = true
    const { launchStartedAt, appendLog } = useAppStore.getState()
    if (launchStartedAt != null) {
      appendLog({
        stream: 'stdout',
        level: 'debug',
        line: `workspace first paint +${Date.now() - launchStartedAt}ms`,
      })
      useAppStore.setState({ launchStartedAt: null })
    }
  }, [live, dshUrl])

  if (live && dshUrl) {
    return (
      <iframe
        title="DeepSeek Harness Workspace"
        src={dshUrl}
        className="h-full w-full border-0 bg-zinc-950"
        allow="clipboard-read; clipboard-write"
      />
    )
  }

  return (
    <div className="flex h-full items-center justify-center bg-zinc-950 p-8">
      <section className="w-full max-w-xl rounded-lg border border-zinc-800 bg-zinc-900/60 p-6 shadow-2xl shadow-black/20">
        <div className="flex items-start gap-4">
          <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg bg-blue-500/15">
            <img src={logo} alt="" className="h-9 w-9 rounded-full object-cover" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h1 className="truncate text-lg font-semibold text-zinc-100">
                {activeInstance?.name ?? 'DeepSeek Harness'}
              </h1>
              <StatusDot status={status} />
            </div>
            <p className="mt-1 text-sm text-zinc-500">{t('workspace.subtitle')}</p>
          </div>
        </div>

        <div className="mt-6 grid grid-cols-3 gap-3">
          <div className="rounded-lg border border-zinc-800 bg-zinc-950/30 p-3">
            <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('overview.runtime')}</div>
            <div className="mt-1 truncate text-sm font-medium text-zinc-200">
              {activeInstance?.runtime.version ?? '-'}
            </div>
          </div>
          <div className="rounded-lg border border-zinc-800 bg-zinc-950/30 p-3">
            <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('overview.provider')}</div>
            <div className="mt-1 truncate text-sm font-medium text-zinc-200">
              {activeInstance?.providerRef ?? '-'}
            </div>
          </div>
          <div className="rounded-lg border border-zinc-800 bg-zinc-950/30 p-3">
            <div className="text-[11px] uppercase tracking-wide text-zinc-500">{t('activity.status')}</div>
            <div className="mt-1 truncate text-sm font-medium text-zinc-200">{t(`status.${status}`)}</div>
          </div>
        </div>

        <div className="mt-6 flex items-center justify-between gap-3">
          <button
            onClick={() => setShellMode('manage')}
            className="h-10 rounded-lg border border-zinc-800 px-4 text-sm font-medium text-zinc-300 hover:border-zinc-700 hover:text-zinc-100"
          >
            {t('shell.manage')}
          </button>
          <button
            onClick={() => activeId && void launch(activeId)}
            disabled={!activeId || busy}
            className="flex h-10 items-center gap-2 rounded-lg bg-blue-600 px-4 text-sm font-semibold text-white shadow-lg shadow-blue-950/25 hover:bg-blue-500 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Play className="h-4 w-4" strokeWidth={1.75} />
            {busy ? t('overview.working') : t('overview.launchDsh')}
          </button>
        </div>
      </section>
    </div>
  )
}

export default function App() {
  const t = useT()
  const page = useAppStore((s) => s.page)
  const setPage = useAppStore((s) => s.setPage)
  const shellMode = useAppStore((s) => s.shellMode)
  const setShellMode = useAppStore((s) => s.setShellMode)
  const error = useAppStore((s) => s.error)
  const setError = useAppStore((s) => s.setError)
  const processState = useAppStore((s) => s.processState)
  const busy = useAppStore((s) => s.busy)
  const theme = useAppStore((s) => s.theme)
  const [displayPage, setDisplayPage] = useState<Page>(page)
  const [pageVisible, setPageVisible] = useState(true)

  const status = processState?.status ?? 'stopped'

  // Drive the palette from the store's theme (settings-seeded on boot, toggled
  // by the Appearance row, or adopted from the running DSH).
  useEffect(() => {
    applyTheme(theme)
  }, [theme])

  useEffect(() => {
    if (page === displayPage) return
    setPageVisible(false)
    const swap = window.setTimeout(() => {
      setDisplayPage(page)
      window.requestAnimationFrame(() => setPageVisible(true))
    }, 70)
    return () => window.clearTimeout(swap)
  }, [displayPage, page])

  // Track maximized so the title-bar button can swap between the maximize and
  // restore glyphs (refreshed on boot and on every window resize).
  const [maximized, setMaximized] = useState(false)
  useEffect(() => {
    let mounted = true
    const refresh = () =>
      appWindow
        .isMaximized()
        .then((m) => mounted && setMaximized(m))
        .catch(() => {})
    refresh()
    const unlisten = appWindow.onResized(refresh)
    return () => {
      mounted = false
      unlisten.then((fn) => fn()).catch(() => {})
    }
  }, [])

  return (
    <div className="flex h-full flex-col overflow-hidden bg-zinc-950 text-zinc-200">
      <header
        data-tauri-drag-region
        className="grid h-[52px] shrink-0 grid-cols-[1fr_auto_1fr] items-center border-b border-zinc-800 bg-zinc-950/95"
      >
        <div className="flex w-52 shrink-0 items-center gap-2.5 justify-self-start pl-4">
          <img src={logo} alt="DSH" className="h-8 w-8 shrink-0 rounded-full object-cover" />
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold text-zinc-100">DSH Launcher</div>
          </div>
        </div>

        <div className="relative grid w-48 grid-cols-2 justify-self-center rounded-lg border border-zinc-800 bg-zinc-900/60 p-0.5">
          <div
            className={`absolute left-0.5 top-0.5 h-7 w-[calc((100%_-_4px)_/_2)] rounded-md bg-blue-600/30 shadow-sm shadow-blue-950/30 transition-transform duration-300 ease-out ${
              shellMode === 'manage' ? 'translate-x-full' : 'translate-x-0'
            }`}
          />
          {(['workspace', 'manage'] as const).map((mode) => (
            <button
              key={mode}
              onClick={() => setShellMode(mode)}
              className={`relative z-10 h-7 min-w-24 rounded-md px-4 text-sm font-medium transition-colors duration-200 ${
                shellMode === mode
                  ? 'text-blue-100'
                  : 'text-zinc-500 hover:text-zinc-200'
              }`}
            >
              {t(`shell.${mode}`)}
            </button>
          ))}
        </div>
        <div className="flex h-full items-center justify-end gap-2 justify-self-end">
          <TitleBarButton onClick={() => void appWindow.minimize()}>
            <Minus className="h-3.5 w-3.5" strokeWidth={1.75} />
          </TitleBarButton>
          <TitleBarButton onClick={() => void appWindow.toggleMaximize()}>
            {maximized ? (
              <Copy className="h-3 w-3" strokeWidth={1.75} />
            ) : (
              <Square className="h-3 w-3" strokeWidth={1.75} />
            )}
          </TitleBarButton>
          <TitleBarButton close onClick={() => void appWindow.close()}>
            <X className="h-3.5 w-3.5" strokeWidth={1.75} />
          </TitleBarButton>
        </div>
      </header>

      {error && (
        <div className="flex shrink-0 items-center gap-3 border-b border-red-500/30 bg-red-500/10 px-6 py-2 text-sm text-red-300">
          <span className="flex-1">{error}</span>
          <button onClick={() => setError(null)} className="font-bold text-red-400 hover:text-red-200">
            x
          </button>
        </div>
      )}

      <div className="relative min-h-0 flex-1 overflow-hidden">
        <div
          className={`shell-panel absolute inset-0 min-h-0 ${
            shellMode === 'workspace' ? 'shell-panel-active' : 'shell-panel-idle'
          }`}
          aria-hidden={shellMode !== 'workspace'}
        >
          <Workspace />
        </div>

        {shellMode === 'manage' && (
        <div className="absolute inset-0 min-h-0 animate-[manageIn_200ms_ease-out]" aria-hidden={false}>
          <div className="flex h-full min-h-0">
            <aside className="flex w-52 shrink-0 flex-col border-r border-zinc-800 bg-zinc-900/60">
              <nav className="flex-1 space-y-1 px-3 py-5">
                {NAV.map(({ id, Icon }) => (
                  <button
                    key={id}
                    onClick={() => setPage(id)}
                    className={`flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm transition-colors ${
                      page === id
                        ? 'bg-blue-600/20 text-blue-300'
                        : 'text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200'
                    }`}
                  >
                    <Icon className="h-4 w-4 shrink-0" strokeWidth={1.75} />
                    {t(`nav.${id}`)}
                  </button>
                ))}
              </nav>

              <div className="border-t border-zinc-800 px-5 py-4">
                <div className="flex items-center gap-2 text-xs text-zinc-400">
                  <StatusDot status={status} />
                  <span className="capitalize">{t(`status.${status}`)}</span>
                  {busy && <span className="ml-auto text-zinc-500">...</span>}
                </div>
              </div>
            </aside>

            <main className="min-w-0 flex-1 overflow-hidden bg-zinc-950">
              <div
                className={`h-full min-h-0 transition-all duration-[160ms] ease-out ${
                  pageVisible
                    ? 'translate-y-0 opacity-100 blur-0'
                    : 'translate-y-1 opacity-0 blur-[1px]'
                }`}
              >
                {displayPage === 'overview' && <Overview />}
                {displayPage === 'instances' && <Instances />}
                {displayPage === 'market' && <Market />}
                {displayPage === 'library' && <Library />}
                {displayPage === 'installs' && <Installs />}
                {displayPage === 'activity' && <Activity />}
                {displayPage === 'settings' && <Settings />}
              </div>
            </main>
          </div>
        </div>
        )}
      </div>
    </div>
  )
}
