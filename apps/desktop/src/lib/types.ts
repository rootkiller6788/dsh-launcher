// Types mirroring the Rust command layer (serde camelCase).

export type ProcessStatus = 'stopped' | 'starting' | 'running' | 'degraded' | 'crashed'
export type Page = 'overview' | 'instances' | 'market' | 'library' | 'installs' | 'activity' | 'settings'
export type ShellMode = 'workspace' | 'manage'
export type LogStream = 'stdout' | 'stderr'
export type LogLevel = 'debug' | 'info' | 'warn' | 'error'
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

/**
 * One installed MCP server — the launcher-side source of truth for the full
 * connection definition, from which `cordis.patch.yml` is compiled. `id` is the
 * catalog key (`owner/name`); `serverName` is the DSH-facing server name that
 * becomes the patch row id `mcp-<serverName>`. A record with an empty
 * `serverName` is a legacy bare id awaiting catalog backfill.
 */
export interface McpServerRecord {
  id: string
  serverName: string
  /** `stdio` or `streamable-http`. */
  transport: string
  command: string
  args: string[]
  env: Record<string, string>
  /** streamable-http endpoint URL (catalog `mcpUrl`). */
  url: string
  headers: Record<string, string>
  /** `false` = compiled out of the patch (DSH no longer loads it). */
  enabled: boolean
}

/**
 * One MCP's persisted runtime snapshot (roadmap Phase 2). The launcher
 * *self-proves* a server boots and answers an `initialize` handshake; this is
 * that verdict — it describes a probe the launcher ran, never DSH's live process
 * (which the launcher cannot observe). Mirrors the Rust `McpRuntimeState`.
 */
export type McpHealthState = 'untested' | 'ok' | 'degraded' | 'error'

export interface McpRuntimeState {
  state: McpHealthState
  /** Transport the last probe exercised (`stdio` | `streamable-http`). */
  transport: string
  /** Epoch ms of the last check (any outcome); absent until first probe. */
  checkedAt?: number | null
  /** Epoch ms of the last `ok` check. */
  okAt?: number | null
  /** Consecutive non-ok outcomes since the last ok. */
  failCount: number
  /** Exit code when the probe's child exited non-zero. */
  exitCode?: number | null
  /** Human-readable verdict detail (spawn error, timeout, auth, …). */
  error?: string | null
  /** Tool names surfaced by a best-effort tools/list after initialize. */
  tools: string[]
}

/** One installed server's snapshot for the Library badge (Rust `McpRuntimeEntry`). */
export interface McpRuntimeEntry {
  id: string
  serverName: string
  state: McpRuntimeState
}

/**
 * One MCP server parsed out of an external tool's config by the import path
 * (roadmap Phase 3). Mirrors Rust `dsh_adapter::mcp_import::ImportedMcp`.
 * `warning` is a localized-key code (e.g. `mcp.import.warnNeedsToken`) shown in
 * the Import modal; `null` means the row is clean to import.
 */
export interface ImportedMcp {
  serverName: string
  record: McpServerRecord
  warning?: string | null
}

/** One scannable import source (Rust `McpImportSource`). */
export interface McpImportSource {
  /** `claude` | `cursor` | `vscode`. */
  kind: string
  path: string
  found: boolean
  servers: ImportedMcp[]
  error?: string | null
}

/**
 * One installed skill — the manifest carries full provenance so an update can
 * be judged and re-fetched without re-deriving anything. `id` is the catalog
 * key (`owner/name`). A record with empty `source`/`hash` and `installed === 0`
 * is a legacy bare id predating provenance tracking (backfilled by the next
 * update pass, which hashes the file on disk).
 */
export interface SkillRecord {
  id: string
  /** URL the `SKILL.md` was actually fetched from (raw or repo fallback). */
  source: string
  /** Content SHA-256 (hex lowercase) of the installed `SKILL.md`. */
  hash: string
  /** Install epoch in milliseconds (0 = legacy unknown). */
  installed: number
}

export interface InstanceManifest {
  id: string
  name: string
  runtime: RuntimeRef
  profile: string
  providerRef: string
  plugins: string[]
  skills: SkillRecord[]
  mcp: McpServerRecord[]
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

/** A catalog-declared env var an MCP server needs configured to operate.
 * Carries key/label/secret only — the value is never stored here. */
export interface McpEnvRequirement {
  key: string
  label?: string | null
  secret: boolean
}

/** One configured key on a server (Rust `McpConfigVar`). Values never cross the
 * IPC boundary back to the UI — the app can only see that a key is set. */
export interface McpConfigVar {
  key: string
  secret: boolean
}

export interface LibraryInventoryItem {
  id: string
  kind: ContentKind
  title: string
  packageName?: string | null
  /** Installed "version" label: npm version (plugins/skins), `#<hash>` (skills), null (MCP). */
  version?: string | null
  enabled?: boolean | null
  toggleable: boolean
  source: LibraryItemSource
  stateSource: LibraryStateSource
  detail?: string | null
  market?: MarketInstallMetadata | null
  issues?: string[]
  /** Catalog-declared required env vars still unset on this MCP row. */
  missingConfig?: McpEnvRequirement[]
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

/** Data root + edition flag for the Settings → Storage card (#604 portable). */
export interface AppPathsInfo {
  /** True when the data root lives next to the exe (green edition). */
  portable: boolean
  root: string
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
  /** Crash-telemetry consent (#602). Default off; nothing leaves the machine until enabled. */
  telemetryEnabled?: boolean
  /** User-owned crash-ingest URL. Sending also requires `telemetryEnabled`. */
  telemetryEndpoint?: string | null
  /** Plugin/skin GitHub fetches route through the gh-proxy relay (Install Center toggle, default off). */
  githubMirror?: boolean
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
  level?: LogLevel
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
  /** `true` when cost is provider-reported or price-table derived; `false` = unknown (cost is 0). */
  costKnown?: boolean
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

/** A generic dimension aggregate (provider alias / instance). */
export interface UsageDimension {
  key: string
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
  costKnownRecords: number
  unknownCostRecords: number
  totalRecords: number
  recordsTruncated: boolean
  byHour: UsageBucket[]
  byDay: UsageBucket[]
  byModel: UsageModelTotal[]
  byProvider: UsageDimension[]
  byInstance: UsageDimension[]
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

export interface DiagnosticsExportResult {
  path: string
  bytes: number
  entries: number
  /** Distinct coded failures the summary found; 0 means the Activity lines had none. */
  codedFailures: number
}

export interface EnvironmentPreviewItem {
  kind: ContentKind
  name: string
  source: string
  version: string | null
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
  compatibleWith: string
  items: EnvironmentPreviewItem[]
  conflicts: string[]
  missingTokens: string[]
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

/**
 * What approving a package's build scripts did to the profile's
 * `pnpm-workspace.yaml` (wire form of `dsh_adapter::pnpm::AllowBuildOutcome`).
 */
export interface AllowBuildOutcome {
  /** `false` when the package was already approved — nothing was written. */
  written: boolean
  /** The exact line added or rewritten, e.g. `esbuild: true`. */
  line: string
  /** Where the pre-edit copy went, when a file existed to back up. */
  backup: string | null
}

/**
 * One skill's update status. Skills have no version number, so both sides are
 * content SHA-256s: `installed` is what's on disk (record hash, or a hash of
 * the file for legacy records), `latest` is what the source serves now.
 */
export interface SkillUpdate {
  id: string
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

// --- Install jobs (Stage 8: backend persistence) -----------------------------

/** Install job status, mirrored from `launcher_core::JobStatus` (lowercase serde). */
export type JobStatus = 'waiting' | 'running' | 'done' | 'failed' | 'cancelled'

/** What the Install Center badge shows, mirrored from `launcher_core::JobKind`. */
export type JobKind = 'plugin' | 'theme' | 'skill' | 'mcp' | 'bundle' | 'environment'

/** A persisted install-job row (camelCase wire form of `launcher_core::Job`). */
export interface Job {
  id: number
  instanceId: string
  /** Content key (e.g. `owner/name`) a Market card is matched against. */
  key: string
  kind: JobKind
  label: string
  status: JobStatus
  /** Current backend stage (download / dsh-install / inventory-sync…). */
  stage: string | null
  progress: number
  error: string | null
  /** Tail of the most recent sub-process stderr (pnpm / git / dsh). */
  stderrTail: string | null
  exitCode: number | null
  createdAt: number
  startedAt: number | null
  finishedAt: number | null
}

export interface DiagnosticsReport {
  profile: string
  bundles: BundleInfo[]
  duplicates: string[]
  orphans: string[]
  orderViolations: OrderViolation[]
  suggestedOrder: string[]
}

/** An instance's rescue point: the pre-change copy of its profile files. */
export interface RescueStatus {
  exists: boolean
  /** Epoch millis the point was taken; 0 when it does not exist. */
  at: number
  /** Rescue file names captured (e.g. `package.json`). */
  files: string[]
}

/**
 * A crash class the boot log can be diagnosed as. Mirrors the Rust `CrashKind`
 * kebab-case tokens; the UI maps each to localized copy.
 */
export type CrashKind =
  | 'missing-bundle'
  | 'plugin-failed'
  | 'bad-profile'
  | 'source-deps'
  | 'missing-module'
  | 'native-deps'
  | 'client-module-missing'
  | 'bundle-mismatch'
  | 'source-mixed'
  | 'duplicate-plugin'
  | 'tool-missing'
  | 'cli-arg'
  | 'cli-error'
  /** The one kind no log rule produces: the launcher's own probe of the
   * workspace URL found dsh refusing it. */
  | 'web-auth-refused'

/** The repair a diagnosis recommends. */
export type FixAction =
  | 'exclude-bundle'
  | 'restore'
  | 'install-deps'
  | 'reinstall'
  | 'rebuild-source'
  | 'restart'
  /** The one action no code performs — dsh's own "reopen the URL" instruction,
   * carried out by stopping and launching again. Not in `ACTIONABLE`. */
  | 'reopen-url'

/** One diagnosed cause of a failed boot. */
export interface CrashIssue {
  kind: CrashKind
  /** The offending plugin / dependency / flag, when the failure names one. */
  plugin: string
  /** English message from the backend rule table. */
  message: string
  fix: FixAction
}

/** Emitted on the `launch-diagnosis` event when a boot goes wrong. */
export interface LaunchDiagnosis {
  instanceId: string
  /**
   * `crashed` (child died), `degraded` (silent but still alive), or `refused`
   * (alive and serving, but the workspace URL it printed was refused).
   */
  stage: 'crashed' | 'degraded' | 'refused'
  issues: CrashIssue[]
}

/** How bad a health check's finding is. */
export type HealthStatus = 'ok' | 'warn' | 'fail'

/** Which part of the instance a health check looks at. */
export type HealthGroup = 'runtime' | 'profile' | 'plugins' | 'mcp' | 'rescue'

/**
 * A repair the launcher can carry out itself. Each maps to a command that
 * already exists, so a fix button can never do more than that command could.
 */
export type HealthFix = 'restore-rescue' | 'create-rescue' | 'exclude-bundle'

/** One measured finding about an instance. */
export interface HealthCheck {
  /** Stable token the UI localizes (`bundles-loadable`). */
  id: string
  group: HealthGroup
  status: HealthStatus
  /** What was measured, specifically — names, counts, paths. */
  detail: string
  /** Repairs that apply, in the order to try them. */
  fixes: HealthFix[]
  /** The names a fix acts on (plugin / bundle names). */
  targets: string[]
}

export interface HealthReport {
  checks: HealthCheck[]
  worst: HealthStatus
}
