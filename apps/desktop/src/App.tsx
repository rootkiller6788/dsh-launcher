import { useEffect } from 'react'
import type { Page } from './lib/types'
import { useAppStore } from './stores/appStore'
import { applyTheme } from './lib/theme'
import { useT } from './lib/i18n'
import { StatusDot } from './components/StatusDot'
import logo from './assets/dshl-logo.png'
import { Home } from './pages/Home'
import { Instances } from './pages/Instances'
import { Market } from './pages/Market'
import { Activity } from './pages/Activity'
import { Diagnostics } from './pages/Diagnostics'
import { Settings } from './pages/Settings'

const NAV: { id: Page; icon: string }[] = [
  { id: 'home', icon: '▣' },
  { id: 'instances', icon: '▤' },
  { id: 'market', icon: '◈' },
  { id: 'activity', icon: '≋' },
  { id: 'diagnostics', icon: '✦' },
  { id: 'settings', icon: '⚙' },
]

export default function App() {
  const t = useT()
  const page = useAppStore((s) => s.page)
  const setPage = useAppStore((s) => s.setPage)
  const error = useAppStore((s) => s.error)
  const setError = useAppStore((s) => s.setError)
  const processState = useAppStore((s) => s.processState)
  const busy = useAppStore((s) => s.busy)
  const theme = useAppStore((s) => s.theme)

  const status = processState?.status ?? 'stopped'

  // Drive the palette from the store's theme (settings-seeded on boot, toggled
  // by the Appearance row, or adopted from the running DSH).
  useEffect(() => {
    applyTheme(theme)
  }, [theme])

  return (
    <div className="flex h-full">
      {/* Sidebar */}
      <aside className="flex w-52 shrink-0 flex-col border-r border-zinc-800 bg-zinc-900/60">
        <div className="px-5 py-5">
          <div className="flex items-center gap-2">
            <img src={logo} alt="DSH" className="h-10 w-10 shrink-0 rounded-full object-cover" />
            <div className="leading-tight">
              <div className="text-sm font-bold text-zinc-100">DSH</div>
              <div className="text-[11px] text-zinc-500">{t('brand.subtitle')}</div>
            </div>
          </div>
        </div>

        <nav className="flex-1 px-3 space-y-1">
          {NAV.map((item) => (
            <button
              key={item.id}
              onClick={() => setPage(item.id)}
              className={`flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm transition-colors ${
                page === item.id
                  ? 'bg-blue-600/20 text-blue-300'
                  : 'text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200'
              }`}
            >
              <span className="w-4 text-center">{item.icon}</span>
              {t(`nav.${item.id}`)}
            </button>
          ))}
        </nav>

        <div className="border-t border-zinc-800 px-5 py-4">
          <div className="flex items-center gap-2 text-xs text-zinc-400">
            <StatusDot status={status} />
            <span className="capitalize">{t(`status.${status}`)}</span>
            {busy && <span className="ml-auto text-zinc-500">…</span>}
          </div>
        </div>
      </aside>

      {/* Main */}
      <main className="flex min-w-0 flex-1 flex-col">
        {error && (
          <div className="flex items-center gap-3 border-b border-red-500/30 bg-red-500/10 px-6 py-2 text-sm text-red-300">
            <span className="flex-1">{error}</span>
            <button onClick={() => setError(null)} className="font-bold text-red-400 hover:text-red-200">
              ×
            </button>
          </div>
        )}
        <div className="flex-1 overflow-y-auto">
          {page === 'home' && <Home />}
          {page === 'instances' && <Instances />}
          {page === 'market' && <Market />}
          {page === 'activity' && <Activity />}
          {page === 'diagnostics' && <Diagnostics />}
          {page === 'settings' && <Settings />}
        </div>
      </main>
    </div>
  )
}
