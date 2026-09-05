import { create } from 'zustand'
import { listen } from '@tauri-apps/api/event'
import { ipc } from '../lib/ipc'
import { applyTheme } from '../lib/theme'
import type {
  AppPathsInfo,
  AppSettings,
  BundleManifest,
  DiagnosticsReport,
  EnvironmentExportResult,
  InstanceManifest,
  InstalledPlugin,
  Job,
  LibraryInventoryDetail,
  LibraryInventorySummary,
  Lang,
  LaunchSession,
  LogLine,
  McpServerRecord,
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
  SkillRecord,
  SkillUpdate,
  SystemInfo,
  SystemStats,
  Theme,
  UsageSummary,
} from '../lib/types'

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
  appPaths: AppPathsInfo | null
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
  installedSkills: SkillRecord[]
  installedMcps: McpServerRecord[]
  libraryInventory: Record<string, LibraryInventorySummary>
  libraryDetail: LibraryInventoryDetail | null
  /** Backend-persisted install jobs (Stage 8): queue + history, newest first. */
  jobs: Job[]
  recommendations: RecommendResult | null
  searching: boolean
  updates: PluginUpdate[]
  skillUpdates: SkillUpdate[]
  diagnostics: DiagnosticsReport | null
  busy: boolean
  error: string | null
  /** Epoch ms captured at launch-invoke; cleared once the workspace first paints. */
  launchStartedAt: number | null

  setPage: (p: Page) => void
  setShellMode: (m: ShellMode) => void
  setError: (e: string | null) => void
  upsertJob: (job: Job) => void
  removeJob: (id: number) => void
  refreshJobs: () => Promise<void>
  clearFinishedJobs: () => Promise<void>
  retryJob: (id: number) => Promise<boolean>
  cancelJob: (id: number) => Promise<boolean>
  deleteJob: (id: number) => Promise<void>
  openJobInLibrary: () => void
  openJobInInstalls: () => void
  revealJobWorkspace: (id: number) => Promise<void>
  revealJobConfig: (id: number) => Promise<void>
  bootstrap: () => void
  refresh: () => Promise<void>
  refreshSystem: () => Promise<void>
  refreshSystemStats: () => Promise<void>
  refreshAppPaths: () => Promise<void>
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
  /** Enqueue a backend install job; resolves once the job row is queued. */
  installMarketEntry: (entry: RegistryPlugin) => Promise<boolean>
  installPlugin: (target: string, entry?: RegistryPlugin | null) => Promise<boolean>
  uninstallPlugin: (name: string) => Promise<boolean>
  togglePlugin: (name: string, enabled: boolean) => Promise<boolean>
  installSkill: (entry: RegistryPlugin) => Promise<boolean>
  uninstallSkill: (id: string) => Promise<boolean>
  refreshInstalledSkills: () => Promise<void>
  installMcp: (entry: RegistryPlugin) => Promise<boolean>
  uninstallMcp: (mcpId: string) => Promise<boolean>
  setMcpEnabled: (mcpId: string, enabled: boolean) => Promise<boolean>
  refreshInstalledMcps: () => Promise<void>
  importBundle: (manifest: BundleManifest) => Promise<Job | null>
  exportEnvironment: () => Promise<EnvironmentExportResult | null>
  importEnvironment: (path: string, name?: string | null) => Promise<Job | null>
  importEnvironmentPackage: (bytes: number[], name?: string | null) => Promise<Job | null>
  refreshUpdates: () => Promise<void>
  updatePlugin: (name: string) => Promise<boolean>
  refreshSkillUpdates: () => Promise<void>
  updateSkill: (id: string) => Promise<boolean>
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
  appPaths: null,
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
  jobs: [],
  recommendations: null,
  searching: false,
  updates: [],
  skillUpdates: [],
  diagnostics: null,
  busy: false,
  error: null,
  launchStartedAt: null,

  setPage: (page) => set({ page }),
  setShellMode: (shellMode) => {
    set({ shellMode })
  },
  setError: (error) => set({ error }),

  // Insert-or-update a job row pushed by the backend `job-updated` event.
  upsertJob: (job) =>
    set((s) => {
      const exists = s.jobs.some((j) => j.id === job.id)
      return {
        jobs: exists
          ? s.jobs.map((j) => (j.id === job.id ? job : j))
          : [job, ...s.jobs].slice(0, 200),
      }
    }),
  removeJob: (id) => set((s) => ({ jobs: s.jobs.filter((j) => j.id !== id) })),
  refreshJobs: async () => {
    try {
      set({ jobs: await ipc.jobsList() })
    } catch {
      /* non-fatal */
    }
  },
  clearFinishedJobs: async () => {
    try {
      await ipc.jobsClearFinished()
      set((s) => ({
        jobs: s.jobs.filter((j) => j.status === 'waiting' || j.status === 'running'),
      }))
    } catch (e) {
      set({ error: String(e) })
    }
  },
  retryJob: async (id) => {
    set({ error: null })
    try {
      const job = await ipc.jobsRetry(id)
      get().upsertJob(job)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },
  deleteJob: async (id) => {
    try {
      await ipc.jobsDelete(id)
      get().removeJob(id)
    } catch (e) {
      set({ error: String(e) })
    }
  },
  cancelJob: async (id) => {
    set({ error: null })
    try {
      const job = await ipc.jobsCancel(id)
      if (job) get().upsertJob(job)
      return job != null
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },
  openJobInLibrary: () => set({ shellMode: 'manage', page: 'library' }),
  openJobInInstalls: () => set({ shellMode: 'manage', page: 'installs' }),
  revealJobWorkspace: async (id) => {
    const job = get().jobs.find((j) => j.id === id)
    const instanceId = job?.instanceId ?? get().activeId
    if (!instanceId) return
    try {
      await ipc.revealInstanceWorkspace(instanceId)
    } catch (e) {
      set({ error: String(e) })
    }
  },
  revealJobConfig: async (id) => {
    const job = get().jobs.find((j) => j.id === id)
    const instanceId = job?.instanceId ?? get().activeId
    if (!instanceId) return
    try {
      await ipc.revealInstanceConfig(instanceId)
    } catch (e) {
      set({ error: String(e) })
    }
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
    // DSH pushes settings changes over its host SSE stream; the backend forwards
    // each `settings/document-updated` as this event, keyed by namespace. Adopt
    // immediately — the poll below is only the reconnect/fallback path.
    listen<string>('dsh-settings-changed', (event) => {
      if (event.payload === 'ui-theme') void get().syncTheme()
      else if (event.payload === 'locale') void get().syncLanguage()
    }).catch(() => {})
    // Stage 8: the backend pushes every job-row change here. Terminal rows on
    // the active instance also nudge the Library indexes, since install actions
    // now return at enqueue time (nobody is left to await completion).
    listen<Job>('job-updated', (event) => {
      const state = get()
      state.upsertJob(event.payload)
      const { status, instanceId } = event.payload
      if (
        (status === 'done' || status === 'failed' || status === 'cancelled') &&
        instanceId === state.activeId &&
        state.shellMode === 'manage' &&
        (state.page === 'library' || state.page === 'market')
      ) {
        // A finished install changed the instance's content indexes; refresh
        // them here because the install call returned at enqueue time.
        if (status === 'done') {
          void state.refreshInstalledPlugins()
          void state.refreshInstalledSkills()
          void state.refreshInstalledMcps()
        }
        void state.refreshLibraryInventory()
        void state.refreshLibraryDetail()
      }
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
    // Fallback appearance/language sync. The primary path is the backend's SSE
    // subscription (`dsh-settings-changed`); this poll only re-converges after a
    // dropped stream or a missed event, so it can be slow and infrequent.
    window.setInterval(() => {
      const state = get()
      if (!state.dshUrl) return
      void state.syncTheme()
      void state.syncLanguage()
    }, 3000)
    window.setInterval(() => {
      const state = get()
      if (state.shellMode !== 'manage') return
      if (state.page !== 'overview' && state.page !== 'activity') return
      void state.refreshHistory()
      void state.refreshUsage()
    }, 3000)
    void get().refresh()
    void get().refreshJobs()
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
  refreshAppPaths: async () => {
    try {
      set({ appPaths: await ipc.appPaths() })
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
      // 7-day window so the Overview snapshot's 6 prior days resolve to real
      // byDay buckets instead of zero.
      const start = new Date()
      start.setHours(0, 0, 0, 0)
      start.setDate(start.getDate() - 6)
      set({ usageSummary: await ipc.usageSummary(activeId, Math.floor(start.getTime() / 1000), now + 1) })
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
    set({ busy: true, error: null, launchStartedAt: Date.now() })
    try {
      const processState = await ipc.launch(id)
      const dshUrl = await ipc.currentDshUrl().catch(() => get().dshUrl)
      set({ processState, dshUrl, shellMode: 'workspace' })
      void get().refreshLibraryInventory()
      void get().refreshUpdates()
      void get().refreshSkillUpdates()
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
    set({ busy: true, error: null, launchStartedAt: Date.now() })
    try {
      await ipc.stop()
      await get().refreshHistory()
      const processState = await ipc.launch(id)
      const dshUrl = await ipc.currentDshUrl().catch(() => get().dshUrl)
      set({ processState, dshUrl, shellMode: 'workspace' })
      void get().refreshLibraryInventory()
      void get().refreshUpdates()
      void get().refreshSkillUpdates()
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

  // Stage 8: installs enqueue a backend job and return as soon as it's queued.
  // Progress/result arrives as `job-updated` events; a rejection here means the
  // job was never accepted (e.g. no active instance).
  installMarketEntry: async (entry) => {
    const id = get().activeId
    if (!id) return false
    set({ error: null })
    try {
      await ipc.marketInstall(id, entry)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },

  installPlugin: async (target, entry = null) => {
    const id = get().activeId
    if (!id) return false
    set({ error: null })
    try {
      await ipc.pluginInstall(id, target, entry)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
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
    set({ error: null })
    try {
      await ipc.skillInstall(id, entry)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
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
    set({ error: null })
    try {
      await ipc.mcpInstall(id, entry)
      return true
    } catch (e) {
      set({ error: String(e) })
      return false
    }
  },

  uninstallMcp: async (mcpId) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.mcpUninstall(id, mcpId)
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

  setMcpEnabled: async (mcpId, enabled) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.mcpSetEnabled(id, mcpId, enabled)
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
    set({ error: null })
    try {
      // Enqueues a backend bundle job; per-item progress streams via
      // `job-updated`. Bundle items can install plugins/skills/MCP — the
      // terminal-job handler on the active pages refreshes every index.
      return await ipc.bundleImport(id, manifest)
    } catch (e) {
      set({ error: String(e) })
      return null
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
    set({ error: null })
    try {
      // Enqueues an environment-import job; per-leaf progress streams via
      // `job-updated`. The instance is created eagerly by the backend, so
      // refresh + select it now and let Install Center drive the install.
      const job = await ipc.environmentImport(path, name)
      await get().refresh()
      await get().switchInstance(job.instanceId)
      void get().refreshJobs()
      return job
    } catch (e) {
      set({ error: String(e) })
      return null
    }
  },

  importEnvironmentPackage: async (bytes, name) => {
    set({ error: null })
    try {
      const job = await ipc.environmentImportPackage(bytes, name)
      await get().refresh()
      await get().switchInstance(job.instanceId)
      void get().refreshJobs()
      return job
    } catch (e) {
      set({ error: String(e) })
      return null
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

  refreshSkillUpdates: async () => {
    const id = get().activeId
    if (!id) {
      set({ skillUpdates: [] })
      return
    }
    try {
      set({ skillUpdates: await ipc.skillUpdates(id) })
    } catch {
      /* non-fatal */
    }
  },

  updateSkill: async (skillId) => {
    const id = get().activeId
    if (!id) return false
    set({ busy: true, error: null })
    try {
      await ipc.skillUpdate(id, skillId)
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
