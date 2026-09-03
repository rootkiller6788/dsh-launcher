// Types mirroring the Rust command layer (serde camelCase).

export type ProcessStatus = 'stopped' | 'starting' | 'running' | 'degraded' | 'crashed'
export type Page = 'overview' | 'instances' | 'market' | 'library' | 'activity' | 'settings'
export type ShellMode = 'workspace' | 'manage'
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
  skins: string[]
  workspace: string
}

export interface LibraryInventorySummary {
  instanceId: string
  plugins: number
  skills: number
  mcp: number
  skins: number
  updatedAt: number
}

export type LibraryItemSource =
  | 'dshNative'
  | 'marketInstalled'
  | 'localFile'
  | 'importedEnvironment'
  | 'unknownDetected'

export type LibraryStateSource = 'dshInventory' | 'dshWorkspaceFiles' | 'launcherSnapshot'

export interface MarketInstallMetadata {
  key: string
  kind: ContentKind
  name: string
  owner: string
  installSpec: string
  installedAt: number
}

export interface LibraryInventoryItem {
  id: string
  kind: ContentKind
  title: string
  packageName?: string | null
  enabled?: boolean | null
  toggleable: boolean
  source: LibraryItemSource
  stateSource: LibraryStateSource
  detail?: string | null
  market?: MarketInstallMetadata | null
}

export interface LibraryInventoryDetail {
  instanceId: string
  updatedAt: number
  items: LibraryInventoryItem[]
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

/** One sample from the launcher's own resource sampler (Overview sparklines). */
export interface SystemStats {
  /** Global CPU usage, percent (since the previous poll). */
  cpu: number
  memoryUsed: number
  memoryTotal: number
  diskUsed: number
  diskTotal: number
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

export interface UsageRecord {
  id: number
  instanceId: string
  timestamp: number
  model: string
  inputTokens: number
  outputTokens: number
  totalTokens: number
  cost: number
  apiKeyAlias: string
  requestId?: string | null
}

export interface UsageBucket {
  timestamp: number
  inputTokens: number
  outputTokens: number
  totalTokens: number
  requests: number
  cost: number
}

export interface UsageModelTotal {
  model: string
  inputTokens: number
  outputTokens: number
  totalTokens: number
  requests: number
  cost: number
}

export interface UsageSummary {
  records: UsageRecord[]
  totalTokens: number
  inputTokens: number
  outputTokens: number
  requests: number
  totalCost: number
  byHour: UsageBucket[]
  byDay: UsageBucket[]
  byModel: UsageModelTotal[]
}

export interface UsageExportResult {
  path: string
  format: string
  records: number
}

export type ContentKind = 'plugin' | 'theme' | 'skill' | 'mcp' | 'bundle'

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
  /** Content type discriminator; omitted means `plugin`. */
  kind?: ContentKind
  /** theme (skin) specific */
  preview?: string | null
  previewCss?: string | null
  path?: string | null
  gist?: string | null
  /** skill specific */
  fetch?: string | null
  skillName?: string | null
  /** mcp specific */
  serverName?: string | null
  transport?: string | null
  command?: string | null
  args?: string[] | null
  env?: Record<string, string> | null
  mcpUrl?: string | null
  headers?: Record<string, string> | null
  /** bundle specific — item references (kind + owner/name + reason). */
  items?: PlanItem[] | null
  /** Computed install target (npm | tarball | github:owner/repo). */
  spec: string
}

export interface Registry {
  updated: string
  count: number
  categories: Record<string, Record<string, string>>
  plugins: RegistryPlugin[]
}

/** A bundle = a curated list of content items installed in one pass. */
export interface BundleManifest {
  name: string
  version: string
  description: string
  items: RegistryPlugin[]
}

export interface BundleItemResult {
  name: string
  kind: string
  ok: boolean
  error?: string | null
}

export interface BundleSummary {
  installed: number
  failed: number
  results: BundleItemResult[]
}

export interface EnvironmentExportResult {
  path: string
  checksum: string
  itemCount: number
}

export interface EnvironmentPreviewResult {
  name: string
  description: string
  checksum: string
  itemCount: number
  plugins: number
  skins: number
  skills: number
  mcps: number
  exportedAt: number
}

export interface EnvironmentImportResult {
  instance: InstanceManifest
  checksum: string
  summary: BundleSummary
}

export interface PlanItem {
  name: string
  kind: ContentKind
  reason: string
}

export interface RecommendPlan {
  id: string
  title: string
  rationale: string
  items: PlanItem[]
}

export interface RecommendResult {
  plans: RecommendPlan[]
  candidates: string[]
  raw: string
}

export interface InstalledPlugin {
  name: string
  enabled: boolean
  toggleable: boolean
  kind: 'plugin' | 'theme' | 'client'
  source: 'profile' | 'inventory'
  entryId?: string | null
  fiberPhase?: string | null
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
