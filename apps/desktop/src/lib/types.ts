// Types mirroring the Rust command layer (serde camelCase).

export type ProcessStatus = 'stopped' | 'starting' | 'running' | 'degraded' | 'crashed'
export type Page = 'home' | 'instances' | 'market' | 'activity' | 'diagnostics' | 'settings'
export type LogStream = 'stdout' | 'stderr'
export type Lang = 'en' | 'zh'
/** Theme preference, mirroring DSH's `ui-theme.preference`. */
export type Theme = 'light' | 'dark' | 'system'

export interface ProcessState {
  pid?: number | null
  status: ProcessStatus
  startedAt?: number | null
  exitCode?: number | null
}

export interface RuntimeRef {
  id: string
  version: string
}

export interface InstanceManifest {
  id: string
  name: string
  runtime: RuntimeRef
  profile: string
  providerRef: string
  plugins: string[]
  skills: string[]
  mcp: string[]
  workspace: string
}

export interface ProviderProfile {
  id: string
  name: string
  baseUrl?: string | null
  model?: string | null
  /** Model catalog ids (a LiteLLM-style subset) shown to DSH. */
  models?: string[]
}

export interface ProviderPreset {
  id: string
  name: string
  baseUrl: string
  needsKey: boolean
  models: string[]
}

export interface ProviderView {
  profile: ProviderProfile
  hasKey: boolean
}

export interface RuntimeInfo {
  id: string
  version: string
  binPath: string
  nodeVersion: string
}

export interface EnvItem {
  name: string
  present: boolean
  version?: string | null
  note?: string | null
}

export interface SystemInfo {
  node: EnvItem
  git: EnvItem
  dsh: RuntimeInfo | null
  dshError: string | null
}

export interface AppSettings {
  dshPath?: string | null
  nodePath?: string | null
  lastInstance?: string | null
  language?: string | null
  theme?: string | null
}

/** A managed Node runtime, as the Settings → Runtime panel reports it. */
export interface NodeInfo {
  present: boolean
  path?: string | null
  version?: string | null
  error?: string | null
}

/** One installed DSH runtime under `runtimes/dsh-<version>/`. */
export interface RuntimeEntry {
  version: string
  binPath: string
  dir: string
  verified: boolean
}

/** Structural verify of a runtime. */
export interface VerifyReport {
  version: string
  nodeOk: boolean
  nodeVersion?: string | null
  dshOk: boolean
  dshVersion?: string | null
  message: string
}

/** Everything the Settings → Runtime panel renders in one shot. */
export interface RuntimeManagerView {
  node: NodeInfo
  active?: string | null
  runtimes: RuntimeEntry[]
  error?: string | null
}

export interface LogLine {
  stream: LogStream
  line: string
}

export interface LaunchSession {
  id: number
  instanceId: string
  startedAt: number
  endedAt: number | null
  exitCode: number | null
  status: 'running' | 'stopped' | 'crashed' | string
}

export interface RegistryPlugin {
  name: string
  owner: string
  url: string
  category: string[]
  description: Record<string, string>
  npm?: string | null
  tarball?: string | null
  screenshots: string[]
  stars?: number | null
  downloads?: number | null
  install: string
  added: string
  deprecated?: boolean | null
  replacement?: string | null
  /** Computed install target (npm | tarball | github:owner/repo). */
  spec: string
}

export interface Registry {
  updated: string
  count: number
  categories: Record<string, Record<string, string>>
  plugins: RegistryPlugin[]
}

export interface PlanPlugin {
  name: string
  reason: string
}

export interface RecommendPlan {
  id: string
  title: string
  rationale: string
  plugins: PlanPlugin[]
}

export interface RecommendResult {
  plans: RecommendPlan[]
  candidates: string[]
  raw: string
}

export interface InstalledPlugin {
  name: string
  enabled: boolean
}

export interface PluginUpdate {
  name: string
  installed: string
  latest: string
  updatable: boolean
}

export interface BundleInfo {
  name: string
  resolved: boolean
  entryIds: string[]
  error?: string | null
}

export interface OrderViolation {
  name: string
  message: string
}

export interface DiagnosticsReport {
  profile: string
  bundles: BundleInfo[]
  duplicates: string[]
  orphans: string[]
  orderViolations: OrderViolation[]
  suggestedOrder: string[]
}
