import { invoke } from '@tauri-apps/api/core'
import type {
  AppSettings,
  BundleManifest,
  BundleSummary,
  DiagnosticsReport,
  EnvironmentExportResult,
  EnvironmentImportResult,
  EnvironmentPreviewResult,
  InstanceManifest,
  InstalledPlugin,
  LibraryInventoryDetail,
  LibraryInventorySummary,
  LaunchSession,
  PluginUpdate,
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
  marketInstall: (id: string, entry: RegistryPlugin) => call<void>('market_install', { id, entry }),
  pluginsList: (id: string, dshPort?: number | null) =>
    call<InstalledPlugin[]>('plugins_list', { id, dshPort }),
  libraryInventorySummaries: () =>
    call<LibraryInventorySummary[]>('library_inventory_summaries'),
  libraryInventoryDetail: (id: string) =>
    call<LibraryInventoryDetail>('library_inventory_detail', { id }),
  libraryInventoryRefresh: (id: string) =>
    call<LibraryInventoryDetail>('library_inventory_refresh', { id }),
  pluginInstall: (id: string, target: string, entry?: RegistryPlugin | null) =>
    call<void>('plugin_install', { id, target, entry }),
  pluginUninstall: (id: string, name: string) => call<void>('plugin_uninstall', { id, name }),
  pluginToggle: (id: string, name: string, enabled: boolean) =>
    call<void>('plugin_toggle', { id, name, enabled }),
  pluginUpdates: (id: string) => call<PluginUpdate[]>('plugin_updates', { id }),
  pluginUpdate: (id: string, name: string) => call<void>('plugin_update', { id, name }),
  profileDiagnostics: (id: string) => call<DiagnosticsReport>('profile_diagnostics', { id }),

  skillList: (id: string) => call<string[]>('skill_list', { id }),
  skillInstall: (id: string, entry: RegistryPlugin) => call<void>('skill_install', { id, entry }),
  skillUninstall: (id: string, skill: string) => call<void>('skill_uninstall', { id, skill }),

  mcpList: (id: string) => call<string[]>('mcp_list', { id }),
  mcpInstall: (id: string, entry: RegistryPlugin) => call<void>('mcp_install', { id, entry }),
  mcpUninstall: (id: string, entry: RegistryPlugin) => call<void>('mcp_uninstall', { id, entry }),

  bundleImport: (id: string, manifest: BundleManifest) =>
    call<BundleSummary>('bundle_import', { id, manifest }),
  environmentExport: (id: string) =>
    call<EnvironmentExportResult>('environment_export', { id }),
  environmentPreview: (bytes: number[]) =>
    call<EnvironmentPreviewResult>('environment_preview', { bytes }),
  environmentImport: (path: string, name?: string | null) =>
    call<EnvironmentImportResult>('environment_import', { path, name }),
  environmentImportPackage: (bytes: number[], name?: string | null) =>
    call<EnvironmentImportResult>('environment_import_package', { bytes, name }),

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
