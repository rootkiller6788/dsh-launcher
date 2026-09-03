import { create } from 'zustand'
import { listen } from '@tauri-apps/api/event'
import { ipc } from '../lib/ipc'
import { applyTheme } from '../lib/theme'
import type {
  AppSettings,
  BundleManifest,
  BundleSummary,
  DiagnosticsReport,
  EnvironmentExportResult,
  EnvironmentImportResult,
  InstanceManifest,
  InstalledPlugin,
  LibraryInventoryDetail,
  LibraryInventorySummary,
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
  RegistryPlugin,
  ShellMode,
  SystemInfo,
  SystemStats,
  Theme,
  UsageSummary,
} from '../lib/types'

type InstallJobStatus = 'queued' | 'downloading' | 'dshInstalling' | 'inventorySync' | 'classifying' | 'done' | 'failed'
type InstallJobAction =
  | { type: 'market'; entry: RegistryPlugin }
  | { type: 'plugin'; target: string; entry?: RegistryPlugin | null }
  | { type: 'skill'; entry: RegistryPlugin }
  | { type: 'mcp'; entry: RegistryPlugin }

export interface InstallJob {
  status: InstallJobStatus
  label: string
  instanceId: string
  kind?: RegistryPlugin['kind']
  progress: number
  logs: string[]
  detail?: string
  action?: InstallJobAction
  updatedAt: number
}

/**
 * Last time the *user* (or a DSH-side adopt) wrote the theme, for the poll's
 * cooldown: a `dsh_theme` read right after we push could otherwise bounce the
 * UI back to the DSH's stale value. Writes are idempotent, so this only skips
 * needless round-trips.
 */
let lastThemeWriteAt = 0
let themeSyncInFlight = false

/** Same cooldown for the language poll (`dsh_language` / `syncLanguage`). */
let lastLanguageWriteAt = 0

let pluginInventoryInFlight = false
let lastPluginInventoryInstance: string | null = null
let lastPluginInventorySignature = ''

function matchesStopped(status: ProcessState['status']) {
  return status === 'stopped' || status === 'crashed'
}

const portFromUrl = (url: string | null): number | null => {
  if (!url) return null
  try {
    const parsed = new URL(url)
    const port = Number(parsed.port)
    return Number.isFinite(port) && port > 0 ? port : null
  } catch {
    return null
  }
}

function jobLabel(entry: RegistryPlugin) {
  return entry.owner ? `${entry.owner}/${entry.name}` : entry.name
}

function pushInstallJob(
  current: Record<string, InstallJob>,
  key: string,
  patch: Partial<InstallJob>,
): Record<string, InstallJob> {
  const previous = current[key]
  const nextLog = patch.detail
    ? [...(previous?.logs ?? []), patch.detail].slice(-8)
    : previous?.logs ?? []
  return {
    ...current,
    [key]: {
      status: patch.status ?? previous?.status ?? 'queued',
      label: patch.label ?? previous?.label ?? key,
      instanceId: patch.instanceId ?? previous?.instanceId ?? '',
      kind: patch.kind ?? previous?.kind,
      progress: patch.progress ?? previous?.progress ?? 0,
      logs: patch.logs ?? nextLog,
      detail: patch.detail ?? previous?.detail,
      action: patch.action ?? previous?.action,
      updatedAt: Date.now(),
    },
  }
}

interface AppStore {
  page: Page
  shellMode: ShellMode
  dshUrl: string | null
  instances: InstanceManifest[]
  activeId: string | null
  activeInstance: InstanceManifest | null
  system: SystemInfo | null
  systemStats: SystemStats | null
  systemHistory: SystemStats[]
  provider: ProviderView | null
  presets: ProviderPreset[]
  settings: AppSettings | null
  language: Lang
  theme: Theme
  processState: ProcessState | null
  runningId: string | null
  history: LaunchSession[]
  usageSummary: UsageSummary | null
  logs: LogLine[]
  registry: Registry | null
  registryError: string | null
  installedPlugins: InstalledPlugin[]
  installedSkills: string[]
  installedMcps: string[]
  libraryInventory: Record<string, LibraryInventorySummary>
  libraryDetail: LibraryInventoryDetail | null
  installJobs: Record<string, InstallJob>
  recommendations: RecommendResult | null
  searching: boolean
  updates: PluginUpdate[]
  diagnostics: DiagnosticsReport | null
  busy: boolean
  error: string | null

  setPage: (p: Page) => void
  setShellMode: (m: ShellMode) => void
  setError: (e: string | null) => void
  clearInstallJob: (key: string) => void
  retryInstallJob: (key: string) => Promise<boolean>
  openInstallJobInLibrary: (key: string) => void
  revealInstallWorkspace: (key: string) => Promise<void>
  revealInstallConfig: (key: string) => Promise<void>
  bootstrap: () => void
  refresh: () => Promise<void>
  refreshSystem: () => Promise<void>
  refreshSystemStats: () => Promise<void>
  refreshProvider: () => Promise<void>
  refreshState: () => Promise<void>
  refreshHistory: () => Promise<void>
  refreshUsage: () => Promise<void>
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
  syncLanguage: () => Promise<void>
  removeProviderKey: () => Promise<void>
  launch: (id: string) => Promise<void>
  stop: () => Promise<void>
  restart: () => Promise<void>
  loadRegistry: () => Promise<void>
  refreshInstalledPlugins: () => Promise<void>
  refreshLibraryInventory: () => Promise<void>
  refreshLibraryDetail: () => Promise<void>
  reconcileLibraryInventory: () => Promise<void>
  recommend: (need: string) => Promise<void>
  installMarketEntry: (entry: RegistryPlugin) => Promise<boolean>
  installPlugin: (target: string, entry?: RegistryPlugin | null) => Promise<boolean>
  uninstallPlugin: (name: string) => Promise<boolean>
  togglePlugin: (name: string, enabled: boolean) => Promise<boolean>
  installSkill: (entry: RegistryPlugin) => Promise<boolean>
  uninstallSkill: (id: string) => Promise<boolean>
  refreshInstalledSkills: () => Promise<void>
  installMcp: (entry: RegistryPlugin) => Promise<boolean>
  uninstallMcp: (entry: RegistryPlugin) => Promise<boolean>
  refreshInstalledMcps: () => Promise<void>
  importBundle: (manifest: BundleManifest) => Promise<BundleSummary | null>
  exportEnvironment: () => Promise<EnvironmentExportResult | null>
  importEnvironment: (path: string, name?: string | null) => Promise<EnvironmentImportResult | null>
  importEnvironmentPackage: (bytes: number[], name?: string | null) => Promise<EnvironmentImportResult | null>
  refreshUpdates: () => Promise<void>
  updatePlugin: (name: string) => Promise<boolean>
  refreshDiagnostics: () => Promise<void>
}

export const useAppStore = create<AppStore>((set, get) => ({
  page: 'overview',
  shellMode: 'manage',
  dshUrl: null,
  instances: [],
  activeId: null,
  activeInstance: null,
  system: null,
  systemStats: null,
  systemHistory: [],
  provider: null,
  presets: [],
  settings: null,
  language: 'en',
  theme: 'system',
  processState: null,
  runningId: null,
  history: [],
  usageSummary: null,
  logs: [],
  registry: null,
  registryError: null,
  installedPlugins: [],
  installedSkills: [],
  installedMcps: [],
  libraryInventory: {},
  libraryDetail: null,
  installJobs: {},
  recommendations: null,
  searching: false,
  updates: [],
  diagnostics: null,
  busy: false,
  error: null,

  setPage: (page) => set({ page }),
  setShellMode: (shellMode) => {
    set({ shellMode })
  },
  setError: (error) => set({ error }),
  openInstallJobInLibrary: () => set({ shellMode: 'manage', page: 'library' }),
  revealInstallWorkspace: async (key) => {
    const job = get().installJobs[key]
    const id = job?.instanceId ?? get().activeId
    if (!id) return
    try {
      await ipc.revealInstanceWorkspace(id)
    } catch (e) {
      set({ error: String(e) })
    }
  },
  revealInstallConfig: async (key) => {
    const job = get().installJobs[key]
    const id = job?.instanceId ?? get().activeId
    if (!id) return
    try {
      await ipc.revealInstanceConfig(id)
    } catch (e) {
      set({ error: String(e) })
    }
  },
  clearInstallJob: (key) =>
    set((s) => {
      const next = { ...s.installJobs }
      delete next[key]
      return { installJobs: next }
    }),
  retryInstallJob: async (key) => {
    const job = get().installJobs[key]
    const action = job?.action
    if (!action) return false
    if (action.type === 'market') return get().installMarketEntry(action.entry)
    if (action.type === 'plugin') return get().installPlugin(action.target, action.entry)
    if (action.type === 'skill') return get().installSkill(action.entry)
    if (action.type === 'mcp') return get().installMcp(action.entry)
    return false
  },

  bootstrap: () => {
    listen<LogLine>('logs', (event) => get().appendLog(event.payload)).catch(() => {})
    listen<string>('dsh-url', (event) => {
      set({ dshUrl: event.payload, shellMode: 'workspace' })
    }).catch(() => {})
    listen<ProcessState>('process-state', (event) => {
      set((state) => ({
        processState: event.payload,
        dshUrl: matchesStopped(event.payload.status) ? null : state.dshUrl,
      }))
    }).catch(() => {})
    listen('usage-recorded', () => {
      const state = get()
      if (state.shellMode === 'manage' && (state.page === 'overview' || state.page === 'activity')) {
        void state.refreshUsage()
      }
    }).catch(() => {})
    listen<string>('library-inventory-updated', (event) => {
      const state = get()
      if (
        event.payload === state.activeId &&
        state.shellMode === 'manage' &&
        (state.page === 'library' || state.page === 'market')
      ) {
        void state.refreshInstalledPlugins()
        void state.refreshLibraryDetail()
      }
      void state.refreshLibraryInventory()
    }).catch(() => {})
    const poll = () => {
      if (get().shellMode === 'workspace') return
      ipc
        .processState()
        .then((s) => {
          set((state) => ({
            processState: s,
            dshUrl: matchesStopped(s.status) ? null : state.dshUrl,
          }))
          if (!matchesStopped(s.status) && !get().dshUrl) {
            void ipc
              .currentDshUrl()
              .then((url) => {
                set({ dshUrl: url })
              })
              .catch(() => {})
          }
        })
        .catch(() => {})
      ipc
        .runningInstance()
        .then((id) => set({ runningId: id }))
        .catch(() => {})
      const state = get()
      if (state.shellMode === 'manage' && (state.page === 'overview' || state.page === 'activity')) {
        // Resource sampler for the visible dashboard only; the Workspace iframe
        // should not pay for hidden Manage-page charts.
        void state.refreshSystemStats()
      }
    }
    poll()
    window.setInterval(poll, 1500)
    window.setInterval(() => {
      const state = get()
      if (!state.dshUrl || state.shellMode !== 'manage') return
      if (state.page !== 'settings') return
      void state.syncTheme()
    }, 1000)
    window.setInterval(() => {
      const state = get()
      if (state.shellMode !== 'manage') return
      if (state.page !== 'overview' && state.page !== 'activity') return
      void state.refreshHistory()
      void state.refreshUsage()
    }, 3000)
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
    // Read the per-instance inventory snapshots. Deep DSH reconciliation is
    // reserved for launch background work, installs, and explicit Refresh.
    void get().refreshLibraryInventory()
    if (get().shellMode === 'manage' && (get().page === 'library' || get().page === 'market')) {
      void get().refreshInstalledPlugins()
    }
    if (get().shellMode === 'manage' && get().page === 'library') {
      void get().refreshLibraryDetail()
    }
    if (get().shellMode === 'manage') {
      if (get().page === 'overview' || get().page === 'activity') {
        void get().refreshUsage()
      }
    }
  },

  refreshSystem: async () => {
    try {
      set({ system: await ipc.systemInfo() })
    } catch (e) {
      set({ error: String(e) })
    }
  },
  refreshSystemStats: async () => {
    try {
      const sample = await ipc.systemStats()
      set((s) => ({
        systemStats: sample,
        // Ring buffer capped at ~3 min of 1.5s samples — enough for a readable
        // sparkline without unbounded growth.
        systemHistory: [...s.systemHistory.slice(-119), sample],
      }))
    } catch {
      /* non-fatal */
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
  refreshUsage: async () => {
    try {
      const activeId = get().activeId
      const now = Math.floor(Date.now() / 1000)
      const start = new Date()
      start.setHours(0, 0, 0, 0)
      set({ usageSummary: await ipc.usageSummary(activeId, Math.floor(start.getTime() / 1000), now + 1) })
    } catch {
      /* non-fatal */
    }
  },

  appendLog: (line) =>
    set((s) => {
      const installJobs = { ...s.installJobs }
      const text = line.line
      for (const [key, job] of Object.entries(installJobs)) {
        if (job.status === 'done' || job.status === 'failed') continue
        if (!text.includes(job.instanceId) && !text.toLowerCase().includes(job.label.toLowerCase())) continue
        installJobs[key] = {
          ...job,
          logs: [...job.logs, text].slice(-8),
          detail: text,
          updatedAt: Date.now(),
        }
      }
      return { logs: [...s.logs.slice(-1999), line], installJobs }
    }),
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
    lastLanguageWriteAt = Date.now()
    set((s) => ({
      language: lang,
      settings: s.settings ? { ...s.settings, language: lang } : s.settings,
    }))
    try {
      // Rust persists `settings.language` + pushes to the running DSH's
      // `locale.preference`. Non-fatal — the launcher keeps its own language
      // when no harness is up.
      await ipc.setLanguage(lang)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },

  syncLanguage: async () => {
    // Give a just-pushed write time to land before reading DSH back.
    if (Date.now() - lastLanguageWriteAt < 2500) return
    try {
      const lang = await ipc.dshLanguage()
      if (lang && lang !== get().language) {
        get().setLanguage(lang as Lang)
      }
    } catch {
      /* non-fatal */
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
    // Give a just-pushed launcher write time to land before reading DSH back.
    if (Date.now() - lastThemeWriteAt < 2500) return
    if (themeSyncInFlight) return
    themeSyncInFlight = true
    try {
      const pref = await ipc.dshTheme()
      if (pref !== 'light' && pref !== 'dark' && pref !== 'system') return
      if (pref === get().theme) return
      set((s) => ({
        theme: pref,
        settings: s.settings ? { ...s.settings, theme: pref } : s.settings,
      }))
      applyTheme(pref)
      const settings = get().settings
      if (settings) {
        void ipc.setSettings(settings).catch(() => {})
      }
    } catch {
      /* non-fatal */
    } finally {
      themeSyncInFlight = false
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
      const processState = await ipc.launch(id)
      const dshUrl = await ipc.currentDshUrl().catch(() => get().dshUrl)
      set({ processState, dshUrl, shellMode: 'workspace' })
      void get().refreshLibraryInventory()
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ busy: false })
    }
  },

  stop: async () => {
    set({ busy: true, error: null })
    try {
      set({ processState: await ipc.stop(), dshUrl: null })
      await get().refreshHistory()
    } catch (e) {
      set({ error: String(e) })
    } finally {
      set({ busy: false })
    }
  },

  // Restart = the same stop-then-launch path the user would do by hand; composes
  // the existing IPC instead of duplicating the process management.
  restart: async () => {
    const id = get().activeId
    if (!id) return
    set({ busy: true, error: null })
    try {
      await ipc.stop()
      await get().refreshHistory()
      const processState = await ipc.launch(id)
      const dshUrl = await ipc.currentDshUrl().catch(() => get().dshUrl)
      set({ processState, dshUrl, shellMode: 'workspace' })
      void get().refreshLibraryInventory()
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
      lastPluginInventoryInstance = null
      lastPluginInventorySignature = ''
      set({ installedPlugins: [] })
      return
    }
    if (pluginInventoryInFlight) return
    pluginInventoryInFlight = true
    try {
      const installedPlugins = await ipc.pluginsList(id, portFromUrl(get().dshUrl))
      const signature = JSON.stringify(
        installedPlugins.map((plugin) => [
          plugin.name,
          plugin.enabled,
          plugin.source,
          plugin.fiberPhase ?? '',
        ]),
      )
      if (id !== lastPluginInventoryInstance || signature !== lastPluginInventorySignature) {
        lastPluginInventoryInstance = id
        lastPluginInventorySignature = signature
        set({ installedPlugins })
      }
    } catch {
      /* non-fatal */
    } finally {
      pluginInventoryInFlight = false
    }
  },

  refreshLibraryInventory: async () => {
    try {
      const summaries = await ipc.libraryInventorySummaries()
      set({
        libraryInventory: Object.fromEntries(
          summaries.map((summary) => [summary.instanceId, summary]),
        ),
      })
    } catch {
      /* non-fatal */
    }
  },

  refreshLibraryDetail: async () => {
    const id = get().activeId
    if (!id) {
      set({ libraryDetail: null })
      return
    }
    try {
      set({ libraryDetail: await ipc.libraryInventoryDetail(id) })
    } catch {
      /* non-fatal */
    }
  },

  reconcileLibraryInventory: async () => {
    const id = get().activeId
    if (!id) {
      set({ libraryDetail: null })
      return
    }
    try {
      const detail = await ipc.libraryInventoryRefresh(id)
      set({ libraryDetail: detail })
      await get().refreshLibraryInventory()
      await get().refreshInstalledPlugins()
    } catch (e) {
      set({ error: String(e) })
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

  installMarketEntry: async (entry) => {
    const id = get().activeId
    if (!id) return false
    const key = jobLabel(entry)
    set((s) => ({
      busy: true,
      error: null,
      installJobs: pushInstallJob(s.installJobs, key, {
        status: 'downloading',
        label: key,
        instanceId: id,
        kind: entry.kind,
        progress: 18,
        detail: 'Download / manifest resolution started',
        action: { type: 'market', entry },
      }),
    }))
    try {
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'dshInstalling',
          progress: 45,
          detail: 'Installing through DSH',
        }),
      }))
      await ipc.marketInstall(id, entry)
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'inventorySync',
          progress: 74,
          detail: 'Syncing DSH Inventory snapshot',
        }),
      }))
      if (entry.kind === 'skill') {
        await get().refreshInstalledSkills()
      } else if (entry.kind === 'mcp') {
        await get().refreshInstalledMcps()
      } else {
        await get().refreshInstalledPlugins()
      }
      await get().refreshLibraryInventory()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'classifying',
          progress: 88,
          detail: 'Classifying Library metadata',
        }),
      }))
      await get().refreshLibraryDetail()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'done',
          progress: 100,
          detail: 'Ready in Library',
        }),
      }))
      return true
    } catch (e) {
      set((s) => ({
        error: String(e),
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'failed',
          progress: 100,
          detail: String(e),
        }),
      }))
      return false
    } finally {
      set({ busy: false })
    }
  },

  installPlugin: async (target, entry = null) => {
    const id = get().activeId
    if (!id) return false
    const jobKey = entry?.kind === 'theme' && entry.owner ? `${entry.owner}/${entry.name}` : target
    set((s) => ({
      busy: true,
      error: null,
      installJobs: pushInstallJob(s.installJobs, jobKey, {
        status: 'dshInstalling',
        label: jobKey,
        instanceId: id,
        kind: entry?.kind ?? 'plugin',
        progress: 40,
        detail: 'Installing through DSH',
        action: { type: 'plugin', target, entry },
      }),
    }))
    try {
      await ipc.pluginInstall(id, target, entry)
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, jobKey, {
          status: 'inventorySync',
          progress: 74,
          detail: 'Syncing DSH Inventory snapshot',
        }),
      }))
      await get().refresh()
      await get().refreshInstalledPlugins()
      await get().refreshLibraryInventory()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, jobKey, {
          status: 'classifying',
          progress: 88,
          detail: 'Classifying Library metadata',
        }),
      }))
      await get().refreshLibraryDetail()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, jobKey, {
          status: 'done',
          progress: 100,
          detail: 'Ready in Library',
        }),
      }))
      return true
    } catch (e) {
      set((s) => ({
        error: String(e),
        installJobs: pushInstallJob(s.installJobs, jobKey, {
          status: 'failed',
          progress: 100,
          detail: String(e),
        }),
      }))
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
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
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
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  refreshInstalledSkills: async () => {
    const id = get().activeId
    if (!id) {
      set({ installedSkills: [] })
      return
    }
    try {
      set({ installedSkills: await ipc.skillList(id) })
    } catch {
      /* non-fatal */
    }
  },

  installSkill: async (entry) => {
    const id = get().activeId
    if (!id) return false
    const key = jobLabel(entry)
    set((s) => ({
      busy: true,
      error: null,
      installJobs: pushInstallJob(s.installJobs, key, {
        status: 'downloading',
        label: key,
        instanceId: id,
        kind: entry.kind,
        progress: 22,
        detail: 'Downloading skill files',
        action: { type: 'skill', entry },
      }),
    }))
    try {
      await ipc.skillInstall(id, entry)
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'inventorySync',
          progress: 74,
          detail: 'Refreshing Library snapshot',
        }),
      }))
      await get().refreshInstalledSkills()
      await get().refreshLibraryInventory()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'classifying',
          progress: 88,
          detail: 'Classifying skill metadata',
        }),
      }))
      await get().refreshLibraryDetail()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'done',
          progress: 100,
          detail: 'Ready in Library',
        }),
      }))
      return true
    } catch (e) {
      set((s) => ({
        error: String(e),
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'failed',
          progress: 100,
          detail: String(e),
        }),
      }))
      return false
    } finally {
      set({ busy: false })
    }
  },

  uninstallSkill: async (skill) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.skillUninstall(id, skill)
      await get().refreshInstalledSkills()
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  refreshInstalledMcps: async () => {
    const id = get().activeId
    if (!id) {
      set({ installedMcps: [] })
      return
    }
    try {
      set({ installedMcps: await ipc.mcpList(id) })
    } catch {
      /* non-fatal */
    }
  },

  installMcp: async (entry) => {
    const id = get().activeId
    if (!id) return false
    const key = jobLabel(entry)
    set((s) => ({
      busy: true,
      error: null,
      installJobs: pushInstallJob(s.installJobs, key, {
        status: 'dshInstalling',
        label: key,
        instanceId: id,
        kind: entry.kind,
        progress: 42,
        detail: 'Writing MCP profile configuration',
        action: { type: 'mcp', entry },
      }),
    }))
    try {
      await ipc.mcpInstall(id, entry)
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'inventorySync',
          progress: 74,
          detail: 'Refreshing Library snapshot',
        }),
      }))
      await get().refreshInstalledMcps()
      await get().refreshLibraryInventory()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'classifying',
          progress: 88,
          detail: 'Classifying MCP metadata',
        }),
      }))
      await get().refreshLibraryDetail()
      set((s) => ({
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'done',
          progress: 100,
          detail: 'Ready in Library',
        }),
      }))
      return true
    } catch (e) {
      set((s) => ({
        error: String(e),
        installJobs: pushInstallJob(s.installJobs, key, {
          status: 'failed',
          progress: 100,
          detail: String(e),
        }),
      }))
      return false
    } finally {
      set({ busy: false })
    }
  },

  uninstallMcp: async (entry) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.mcpUninstall(id, entry)
      await get().refreshInstalledMcps()
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    } finally {
      set({ busy: false })
    }
  },

  importBundle: async (manifest) => {
    const id = get().activeId
    if (!id) return null
    set({ busy: true, error: null })
    try {
      const summary = await ipc.bundleImport(id, manifest)
      // Bundle items can install plugins/skills/MCP, so refresh every index.
      await get().refreshInstalledPlugins()
      await get().refreshInstalledSkills()
      await get().refreshInstalledMcps()
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return summary
    } catch (e) {
      set({ error: String(e) })
      return null
    } finally {
      set({ busy: false })
    }
  },

  exportEnvironment: async () => {
    const id = get().activeId
    if (!id) return null
    set({ busy: true, error: null })
    try {
      return await ipc.environmentExport(id)
    } catch (e) {
      set({ error: String(e) })
      return null
    } finally {
      set({ busy: false })
    }
  },

  importEnvironment: async (path, name) => {
    set({ busy: true, error: null })
    try {
      const result = await ipc.environmentImport(path, name)
      await get().refresh()
      await get().switchInstance(result.instance.id)
      await get().refreshInstalledPlugins()
      await get().refreshInstalledSkills()
      await get().refreshInstalledMcps()
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return result
    } catch (e) {
      set({ error: String(e) })
      return null
    } finally {
      set({ busy: false })
    }
  },

  importEnvironmentPackage: async (bytes, name) => {
    set({ busy: true, error: null })
    try {
      const result = await ipc.environmentImportPackage(bytes, name)
      await get().refresh()
      await get().switchInstance(result.instance.id)
      await get().refreshInstalledPlugins()
      await get().refreshInstalledSkills()
      await get().refreshInstalledMcps()
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
      return result
    } catch (e) {
      set({ error: String(e) })
      return null
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
      await get().refreshLibraryInventory()
      await get().refreshLibraryDetail()
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
    } catch {
      // Non-fatal: a diagnostics read failure shouldn't surface as a red error
      // banner in the middle of an install.
      set({ diagnostics: null })
    }
  },
}))
