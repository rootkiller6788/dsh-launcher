import { invoke } from '@tauri-apps/api/core'
import type {
  AppSettings,
  DiagnosticsReport,
  InstanceManifest,
  InstalledPlugin,
  LaunchSession,
  PluginUpdate,
  ProcessState,
  ProviderPreset,
  ProviderProfile,
  ProviderView,
  RecommendResult,
  Registry,
  RuntimeEntry,
  RuntimeManagerView,
  SystemInfo,
  VerifyReport,
} from './types'

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => invoke<T>(cmd, args)

/** Typed wrappers over the Rust command layer. */
export const ipc = {
  systemInfo: () => call<SystemInfo>('system_info'),

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
  processState: () => call<ProcessState>('process_state'),
  runningInstance: () => call<string | null>('running_instance'),
  recentSessions: (limit?: number) =>
    call<LaunchSession[]>('recent_sessions', { limit }),

  marketRegistry: () => call<Registry>('market_registry'),
  marketRecommend: (need: string) => call<RecommendResult>('market_recommend', { need }),
  pluginsList: (id: string) => call<InstalledPlugin[]>('plugins_list', { id }),
  pluginInstall: (id: string, target: string) => call<void>('plugin_install', { id, target }),
  pluginUninstall: (id: string, name: string) => call<void>('plugin_uninstall', { id, name }),
  pluginToggle: (id: string, name: string, enabled: boolean) =>
    call<void>('plugin_toggle', { id, name, enabled }),
  pluginUpdates: (id: string) => call<PluginUpdate[]>('plugin_updates', { id }),
  pluginUpdate: (id: string, name: string) => call<void>('plugin_update', { id, name }),
  profileDiagnostics: (id: string) => call<DiagnosticsReport>('profile_diagnostics', { id }),

  getSettings: () => call<AppSettings>('get_settings'),
  setSettings: (settings: AppSettings) => call<AppSettings>('set_settings', { settings }),

  setTheme: (theme: string) => call<string>('set_theme', { theme }),
  dshTheme: () => call<string | null>('dsh_theme'),

  runtimeList: () => call<RuntimeManagerView>('runtime_list'),
  runtimeInstall: (source: string, version?: string | null) =>
    call<RuntimeEntry>('runtime_install', { source, version }),
  runtimeSetActive: (version: string) => call<void>('runtime_set_active', { version }),
  runtimeRemove: (version: string) => call<void>('runtime_remove', { version }),
  runtimeVerify: (version: string) => call<VerifyReport>('runtime_verify', { version }),
  runtimeRepair: (version: string, source?: string | null) =>
    call<RuntimeEntry>('runtime_repair', { version, source }),
}
