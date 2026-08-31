import { create } from 'zustand'
import { listen } from '@tauri-apps/api/event'
import { ipc } from '../lib/ipc'
import { applyTheme } from '../lib/theme'
import type {
  AppSettings,
  DiagnosticsReport,
  InstanceManifest,
  InstalledPlugin,
  Lang,
  LaunchSession,
  LogLine,
  Page,
  PluginUpdate,
  ProcessState,
  ProviderPreset,
  ProviderProfile,
  ProviderView,
  RecommendResult,
  Registry,
  SystemInfo,
  Theme,
} from '../lib/types'

/**
 * Last time the *user* (or a DSH-side adopt) wrote the theme, for the poll's
 * cooldown: a `dsh_theme` read right after we push could otherwise bounce the
 * UI back to the DSH's stale value. Writes are idempotent, so this only skips
 * needless round-trips.
 */
let lastThemeWriteAt = 0

interface AppStore {
  page: Page
  instances: InstanceManifest[]
  activeId: string | null
  activeInstance: InstanceManifest | null
  system: SystemInfo | null
  provider: ProviderView | null
  presets: ProviderPreset[]
  settings: AppSettings | null
  language: Lang
  theme: Theme
  processState: ProcessState | null
  runningId: string | null
  history: LaunchSession[]
  logs: LogLine[]
  registry: Registry | null
  registryError: string | null
  installedPlugins: InstalledPlugin[]
  recommendations: RecommendResult | null
  searching: boolean
  updates: PluginUpdate[]
  diagnostics: DiagnosticsReport | null
  busy: boolean
  error: string | null

  setPage: (p: Page) => void
  setError: (e: string | null) => void
  bootstrap: () => void
  refresh: () => Promise<void>
  refreshSystem: () => Promise<void>
  refreshProvider: () => Promise<void>
  refreshState: () => Promise<void>
  refreshHistory: () => Promise<void>
  appendLog: (l: LogLine) => void
  clearLogs: () => void
  createInstance: (name: string) => Promise<boolean>
  renameInstance: (id: string, name: string) => Promise<boolean>
  cloneInstance: (id: string, name: string) => Promise<boolean>
  deleteInstance: (id: string) => Promise<boolean>
  switchInstance: (id: string) => Promise<boolean>
  saveProvider: (profile: ProviderProfile, apiKey: string | null) => Promise<boolean>
  saveSettings: (s: AppSettings) => Promise<boolean>
  setLanguage: (lang: Lang) => Promise<boolean>
  setTheme: (t: Theme) => void
  syncTheme: () => Promise<void>
  removeProviderKey: () => Promise<void>
  launch: (id: string) => Promise<void>
  stop: () => Promise<void>
  loadRegistry: () => Promise<void>
  refreshInstalledPlugins: () => Promise<void>
  recommend: (need: string) => Promise<void>
  installPlugin: (target: string) => Promise<boolean>
  uninstallPlugin: (name: string) => Promise<boolean>
  togglePlugin: (name: string, enabled: boolean) => Promise<boolean>
  refreshUpdates: () => Promise<void>
  updatePlugin: (name: string) => Promise<boolean>
  refreshDiagnostics: () => Promise<void>
}

export const useAppStore = create<AppStore>((set, get) => ({
  page: 'home',
  instances: [],
  activeId: null,
  activeInstance: null,
  system: null,
  provider: null,
  presets: [],
  settings: null,
  language: 'en',
  theme: 'system',
  processState: null,
  runningId: null,
  history: [],
  logs: [],
  registry: null,
  registryError: null,
  installedPlugins: [],
  recommendations: null,
  searching: false,
  updates: [],
  diagnostics: null,
  busy: false,
  error: null,

  setPage: (page) => set({ page }),
  setError: (error) => set({ error }),

  bootstrap: () => {
    listen<LogLine>('logs', (event) => get().appendLog(event.payload)).catch(() => {})
    const poll = () => {
      ipc
        .processState()
        .then((s) => set({ processState: s }))
        .catch(() => {})
      ipc
        .runningInstance()
        .then((id) => set({ runningId: id }))
        .catch(() => {})
      // Pull the running DSH's theme back into the launcher (one lamp).
      void get().syncTheme()
    }
    poll()
    window.setInterval(poll, 1500)
    window.setInterval(() => get().refreshHistory(), 3000)
    void get().refresh()
  },

  refresh: async () => {
    const [instances, active, provider, settings, system] = await Promise.allSettled([
      ipc.listInstances(),
      ipc.getInstance(),
      ipc.getProvider(),
      ipc.getSettings(),
      ipc.systemInfo(),
    ])
    const list = instances.status === 'fulfilled' ? instances.value : get().instances
    const activeInstance =
      active.status === 'fulfilled'
        ? active.value
        : list.find((i) => i.id === get().activeId) ?? list[0] ?? null
    const settingsValue = settings.status === 'fulfilled' ? settings.value : null
    set({
      instances: list,
      activeId: activeInstance?.id ?? null,
      activeInstance,
      provider: provider.status === 'fulfilled' ? provider.value : null,
      settings: settingsValue,
      language: settingsValue?.language === 'zh' ? 'zh' : 'en',
      // Seed the theme from persistence; a missing value keeps the current one
      // (initial `system`) so an in-flight toggle isn't clobbered by a stale read.
      theme: settingsValue?.theme ? (settingsValue.theme as Theme) : get().theme,
      system: system.status === 'fulfilled' ? system.value : null,
    })
  },

  refreshSystem: async () => {
    try {
      set({ system: await ipc.systemInfo() })
    } catch (e) {
      set({ error: String(e) })
    }
  },
  refreshProvider: async () => {
    try {
      const [provider, presets] = await Promise.all([
        ipc.getProvider(),
        ipc.listProviderPresets(),
      ])
      set({ provider, presets })
    } catch (e) {
      set({ error: String(e) })
    }
  },
  refreshState: async () => {
    try {
      set({ processState: await ipc.processState() })
    } catch (e) {
      set({ error: String(e) })
    }
  },
  refreshHistory: async () => {
    try {
      set({ history: await ipc.recentSessions(50) })
    } catch {
      /* non-fatal */
    }
  },

  appendLog: (line) => set((s) => ({ logs: [...s.logs.slice(-1999), line] })),
  clearLogs: () => set({ logs: [] }),

  createInstance: async (name) => {
    set({ busy: true, error: null })
    try {
      await ipc.createInstance(name)
      await get().refresh()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },
  renameInstance: async (id, name) => {
    set({ busy: true, error: null })
    try {
      await ipc.renameInstance(id, name)
      await get().refresh()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },
  cloneInstance: async (id, name) => {
    set({ busy: true, error: null })
    try {
      await ipc.cloneInstance(id, name)
      await get().refresh()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },
  deleteInstance: async (id) => {
    set({ busy: true, error: null })
    try {
      await ipc.deleteInstance(id)
      await get().refresh()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },
  switchInstance: async (id) => {
    set({ busy: true, error: null })
    try {
      await ipc.switchInstance(id)
      await get().refresh()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  saveProvider: async (profile, apiKey) => {
    set({ busy: true, error: null })
    try {
      const saved = await ipc.saveProvider(profile, apiKey)
      set({
        provider: {
          profile: saved,
          hasKey: apiKey !== null && apiKey.trim() !== '',
        },
      })
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  saveSettings: async (settings) => {
    set({ busy: true, error: null })
    try {
      set({ settings: await ipc.setSettings(settings) })
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  setLanguage: async (lang) => {
    set({ language: lang })
    try {
      const cur = get().settings ?? {}
      const saved = await ipc.setSettings({ ...cur, language: lang })
      set({ settings: saved })
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },

  setTheme: (theme) => {
    lastThemeWriteAt = Date.now()
    set((s) => ({
      theme,
      settings: s.settings ? { ...s.settings, theme } : s.settings,
    }))
    applyTheme(theme)
    // Persist (Rust also saves the whole settings doc) + push to the running
    // DSH. Non-fatal either way — the launcher keeps its own preference when
    // no harness is up.
    void ipc
      .setTheme(theme)
      .then(() => {})
      .catch(() => {})
  },

  syncTheme: async () => {
    // Give a just-pushed write time to land before reading DSH back.
    if (Date.now() - lastThemeWriteAt < 2500) return
    try {
      const pref = await ipc.dshTheme()
      if (pref && pref !== get().theme) {
        get().setTheme(pref as Theme)
      }
    } catch {
      /* non-fatal */
    }
  },

  removeProviderKey: async () => {
    set({ busy: true, error: null })
    try {
      await ipc.removeProviderKey()
      const cur = get().provider
      if (cur) set({ provider: { ...cur, hasKey: false } })
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ busy: false })
    }
  },

  launch: async (id) => {
    set({ busy: true, error: null })
    try {
      set({ processState: await ipc.launch(id) })
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ busy: false })
    }
  },

  stop: async () => {
    set({ busy: true, error: null })
    try {
      set({ processState: await ipc.stop() })
      await get().refreshHistory()
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ busy: false })
    }
  },

  loadRegistry: async () => {
    set({ registryError: null })
    try {
      set({ registry: await ipc.marketRegistry() })
    } catch (e) {
      set({ registryError: String(e) })
    }
  },

  refreshInstalledPlugins: async () => {
    const id = get().activeId
    if (!id) {
      set({ installedPlugins: [] })
      return
    }
    try {
      set({ installedPlugins: await ipc.pluginsList(id) })
    } catch {
      /* non-fatal */
    }
  },

  recommend: async (need) => {
    set({ searching: true, error: null })
    try {
      set({ recommendations: await ipc.marketRecommend(need) })
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ searching: false })
    }
  },

  installPlugin: async (target) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.pluginInstall(id, target)
      await get().refreshInstalledPlugins()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  uninstallPlugin: async (name) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.pluginUninstall(id, name)
      await get().refreshInstalledPlugins()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  togglePlugin: async (name, enabled) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.pluginToggle(id, name, enabled)
      await get().refreshInstalledPlugins()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  refreshUpdates: async () => {
    const id = get().activeId
    if (!id) {
      set({ updates: [] })
      return
    }
    try {
      set({ updates: await ipc.pluginUpdates(id) })
    } catch {
      /* non-fatal */
    }
  },

  updatePlugin: async (name) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.pluginUpdate(id, name)
      await get().refreshInstalledPlugins()
      await get().refreshUpdates()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  refreshDiagnostics: async () => {
    const id = get().activeId
    if (!id) {
      set({ diagnostics: null })
      return
    }
    try {
      set({ diagnostics: await ipc.profileDiagnostics(id) })
    } catch (e) {
      set({ error: String(e) })
    }
  },
}))
