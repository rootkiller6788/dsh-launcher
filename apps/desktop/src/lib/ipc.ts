import { invoke } from '@tauri-apps/api/core'
import type {
  AppPathsInfo,
  AppSettings,
  BundleManifest,
  DiagnosticsReport,
  EnvironmentExportResult,
  EnvironmentPreviewResult,
  InstanceManifest,
  InstalledPlugin,
  Job,
  LibraryInventoryDetail,
  LibraryInventorySummary,
  LaunchSession,
  McpServerRecord,
  PluginUpdate,
  SkillRecord,
  SkillUpdate,
  ProcessState,
  ProviderPreset,
  ProviderProfile,
  ProviderView,
  RecommendResult,
  Registry,
  RegistryPlugin,
  RuntimeEntry,
  RuntimeManagerView,
  SystemInfo,
  SystemStats,
  UsageRecord,
  UsageExportResult,
  UsageSummary,
  VerifyReport,
} from './types'

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => invoke<T>(cmd, args)

/** Typed wrappers over the Rust command layer. */
export const ipc = {
  systemInfo: () => call<SystemInfo>('system_info'),
  systemStats: () => call<SystemStats>('system_stats'),

  listInstances: () => call<InstanceManifest[]>('list_instances'),
  getInstance: (id?: string) => call<InstanceManifest>('get_instance', id ? { id } : {}),
  createInstance: (name: string) =>
    call<InstanceManifest>('create_instance', { request: { name } }),
  renameInstance: (id: string, name: string) =>
    call<InstanceManifest>('rename_instance', { request: { id, name } }),
  cloneInstance: (id: string, name: string) =>
    call<InstanceManifest>('clone_instance', { request: { id, name } }),
  deleteInstance: (id: string) => call<void>('delete_instance', { id }),
  switchInstance: (id: string) => call<InstanceManifest>('switch_instance', { id }),

  getProvider: () => call<ProviderView>('get_provider'),
  listProviderPresets: () => call<ProviderPreset[]>('list_provider_presets'),
  saveProvider: (profile: ProviderProfile, apiKey: string | null) =>
    call<ProviderProfile>('save_provider', { profile, apiKey }),
  removeProviderKey: () => call<void>('remove_provider_key', { id: 'default' }),

  launch: (id: string) => call<ProcessState>('launch', { id }),
  stop: () => call<ProcessState>('stop'),
  openDsh: () => call<void>('open_dsh'),
  openDshExternal: () => call<void>('open_dsh_external'),
  revealInstanceWorkspace: (id: string) => call<void>('reveal_instance_workspace', { id }),
  revealInstanceConfig: (id: string) => call<void>('reveal_instance_config', { id }),
  appPaths: () => call<AppPathsInfo>('app_paths'),
  revealDataDir: () => call<void>('reveal_data_dir'),
  currentDshUrl: () => call<string | null>('current_dsh_url'),
  processState: () => call<ProcessState>('process_state'),
  runningInstance: () => call<string | null>('running_instance'),
  recentSessions: (limit?: number) =>
    call<LaunchSession[]>('recent_sessions', { limit }),
  usageRecent: (instanceId?: string | null, limit?: number) =>
    call<UsageRecord[]>('usage_recent', { instanceId, limit }),
  usageSummary: (
    instanceId: string | null | undefined,
    from: number,
    to: number,
    model?: string | null,
    provider?: string | null,
  ) =>
    call<UsageSummary>('usage_summary', { instanceId, from, to, model, provider }),
  usageExport: (
    instanceId: string | null | undefined,
    from: number,
    to: number,
    format: 'csv' | 'json',
    model?: string | null,
    provider?: string | null,
  ) =>
    call<UsageExportResult>('usage_export', { instanceId, from, to, format, model, provider }),

  marketRegistry: () => call<Registry>('market_registry'),
  marketRecommend: (need: string) => call<RecommendResult>('market_recommend', { need }),
  // Install jobs (Stage 8): install commands now enqueue and return a Job row.
  marketInstall: (id: string, entry: RegistryPlugin) =>
    call<Job>('market_install', { id, entry }),
  jobsList: () => call<Job[]>('jobs_list'),
  jobsCancel: (id: number) => call<Job | null>('jobs_cancel', { id }),
  jobsRetry: (id: number) => call<Job>('jobs_retry', { id }),
  jobsDelete: (id: number) => call<void>('jobs_delete', { id }),
  jobsClearFinished: () => call<number>('jobs_clear_finished'),
  pluginsList: (id: string, dshPort?: number | null) =>
    call<InstalledPlugin[]>('plugins_list', { id, dshPort }),
  libraryInventorySummaries: () =>
    call<LibraryInventorySummary[]>('library_inventory_summaries'),
  libraryInventoryDetail: (id: string) =>
    call<LibraryInventoryDetail>('library_inventory_detail', { id }),
  libraryInventoryRefresh: (id: string) =>
    call<LibraryInventoryDetail>('library_inventory_refresh', { id }),
  pluginInstall: (id: string, target: string, entry?: RegistryPlugin | null) =>
    call<Job>('plugin_install', { id, target, entry }),
  pluginUninstall: (id: string, name: string) => call<void>('plugin_uninstall', { id, name }),
  pluginToggle: (id: string, name: string, enabled: boolean) =>
    call<void>('plugin_toggle', { id, name, enabled }),
  pluginUpdates: (id: string) => call<PluginUpdate[]>('plugin_updates', { id }),
  pluginUpdate: (id: string, name: string) => call<void>('plugin_update', { id, name }),
  profileDiagnostics: (id: string) => call<DiagnosticsReport>('profile_diagnostics', { id }),

  skillList: (id: string) => call<SkillRecord[]>('skill_list', { id }),
  skillInstall: (id: string, entry: RegistryPlugin) =>
    call<Job>('skill_install', { id, entry }),
  skillUninstall: (id: string, skill: string) => call<void>('skill_uninstall', { id, skill }),
  skillUpdates: (id: string) => call<SkillUpdate[]>('skill_updates', { id }),
  skillUpdate: (id: string, skill: string) => call<void>('skill_update', { id, skill }),

  mcpList: (id: string) => call<McpServerRecord[]>('mcp_list', { id }),
  mcpInstall: (id: string, entry: RegistryPlugin) =>
    call<Job>('mcp_install', { id, entry }),
  mcpUninstall: (id: string, mcp: string) => call<void>('mcp_uninstall', { id, mcp }),
  mcpSetEnabled: (id: string, mcp: string, enabled: boolean) =>
    call<McpServerRecord[]>('mcp_set_enabled', { id, mcp, enabled }),

  bundleImport: (id: string, manifest: BundleManifest) =>
    call<Job>('bundle_import', { id, manifest }),
  environmentExport: (id: string) =>
    call<EnvironmentExportResult>('environment_export', { id }),
  environmentPreview: (bytes: number[]) =>
    call<EnvironmentPreviewResult>('environment_preview', { bytes }),
  environmentImport: (path: string, name?: string | null) =>
    call<Job>('environment_import', { path, name }),
  environmentImportPackage: (bytes: number[], name?: string | null) =>
    call<Job>('environment_import_package', { bytes, name }),

  getSettings: () => call<AppSettings>('get_settings'),
  setSettings: (settings: AppSettings) => call<AppSettings>('set_settings', { settings }),

  setTheme: (theme: string) => call<string>('set_theme', { theme }),
  dshTheme: () => call<string | null>('dsh_theme'),
  setLanguage: (lang: string) => call<string>('set_language', { language: lang }),
  dshLanguage: () => call<string | null>('dsh_language'),

  runtimeList: () => call<RuntimeManagerView>('runtime_list'),
  runtimeInstall: (source: string, version?: string | null) =>
    call<RuntimeEntry>('runtime_install', { source, version }),
  runtimeSetActive: (version: string) => call<void>('runtime_set_active', { version }),
  runtimeRemove: (version: string) => call<void>('runtime_remove', { version }),
  runtimeVerify: (version: string) => call<VerifyReport>('runtime_verify', { version }),
  runtimeRepair: (version: string, source?: string | null) =>
    call<RuntimeEntry>('runtime_repair', { version, source }),
}
