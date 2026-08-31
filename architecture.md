可以。这个 **AI Harness Launcher** 最适合按 PCL 的思路设计，但不要照搬 Minecraft 的“游戏启动器”，而是抽象成：

> **Runtime / Instance / Package / Provider / Launch / Diagnose 六大核心域。**

整个产品的核心目标不是“管理插件”，而是：

$$
\boxed{
Install \rightarrow Configure \rightarrow Compose \rightarrow Launch \rightarrow Observe \rightarrow Repair
}
$$

也就是把一个原本需要用户手工折腾的 AI Harness 环境，变成像 PCL 启动 Minecraft 一样简单。

---

# 1. 产品总定位

## AI Harness Launcher

一个统一管理：

* AI Harness Runtime
* AI Instance
* Model Provider
* Plugin
* Skill
* MCP
* Prompt / Persona / Config
* Setup Pack
* Runtime Dependencies
* Launch / Stop / Restart
* Logs / Diagnostics / Repair

的桌面启动器。

用户最终只需要：

```text
下载 Launcher
   ↓
安装 Runtime
   ↓
创建 Instance / 导入 Setup Pack
   ↓
配置 API
   ↓
Launch
```

---

# 2. Minecraft / PCL 到 AI Launcher 的完整映射

| Minecraft / PCL       | AI Harness Launcher                    |
| --------------------- | -------------------------------------- |
| Minecraft             | AI Harness Runtime                     |
| Minecraft Version     | Harness Version                        |
| Instance              | AI Instance                            |
| Forge/Fabric/NeoForge | Runtime Profile / Harness Distribution |
| Mod                   | Plugin                                 |
| Data Pack             | Skill                                  |
| Mod API               | MCP                                    |
| Modpack               | Setup Pack                             |
| Resource Pack         | Prompt / Persona / Config Pack         |
| Microsoft Account     | AI Provider Account                    |
| Java Runtime          | Node / Python / Bun / Native Runtime   |
| JVM Args              | Harness Runtime Args                   |
| Game Directory        | Instance Workspace                     |
| Libraries             | Runtime Dependencies                   |
| Assets                | Shared Resources                       |
| Mod Download          | Smart Plugin Market                    |
| Dependency Resolution | Plugin/MCP Dependency Resolver         |
| Mod Conflict          | Plugin Compatibility Conflict          |
| Crash Report          | Agent Diagnostics                      |
| Game Log              | Agent / Tool / Harness Logs            |
| Launch                | Launch Agent                           |
| Version Isolation     | Instance Isolation                     |
| Modpack Import        | Setup Pack Import                      |
| Modpack Export        | Setup Pack Export                      |
| Auto Update           | Runtime / Plugin Update                |
| Repair Game Files     | Runtime Repair                         |
| Server List           | Remote Harness / Agent Endpoint        |

---

# 3. 整体架构

我建议最终采用：

```text
┌───────────────────────────────────────────────┐
│               AI Harness Launcher             │
├───────────────────────────────────────────────┤
│ Presentation                                  │
│ Home / Instances / Market / Packs / Logs      │
├───────────────────────────────────────────────┤
│ Application                                   │
│ Install / Launch / Update / Import / Repair   │
├───────────────────────────────────────────────┤
│ Domain                                        │
│ Runtime / Instance / Plugin / Provider / Pack │
├───────────────────────────────────────────────┤
│ Infrastructure                                │
│ FS / Process / HTTP / Git / npm / IPC / DB    │
├───────────────────────────────────────────────┤
│ Execution                                     │
│ DSH / Mine Harness / Other Harness Runtime    │
└───────────────────────────────────────────────┘
```

这里最关键的是：

> Launcher 不应该和 DSH 强绑定。

DSH 只是：

```text
Runtime Provider #1
```

未来完全可以：

```text
AI Harness Launcher
├── DSH
├── OpenClaw
├── IronClaw
├── Mine-Harness
└── Custom Runtime
```

这样产品寿命长很多。

---

# 4. 六大核心域

## A. Runtime Manager

负责“游戏版本”。

即：

```text
Runtime / Harness
```

例如：

```text
DSH
├── 1.2.0
├── 1.3.0
└── nightly

Mine-Harness
├── 0.1
└── 0.2

IronClaw
└── stable
```

核心模型：

```ts
interface RuntimeManifest {
  id: string
  name: string

  version: string
  channel: "stable" | "beta" | "nightly"

  platform: PlatformConstraint[]

  runtimeRequirements: RuntimeRequirement[]

  install: InstallSpec
  launch: LaunchSpec

  capabilities: string[]
}
```

Runtime Manager 负责：

```text
Discover
Install
Verify
Upgrade
Downgrade
Repair
Remove
```

---

# 5. Instance Manager 是整个 Launcher 的真正核心

类似 PCL 的独立游戏实例。

一个 Instance 就是一套完整 AI 环境。

```text
Instance
├── Runtime
├── Model
├── Plugins
├── Skills
├── MCP
├── Prompts
├── Config
├── Credentials Ref
├── Workspace
└── Runtime State
```

例如：

```text
Instances
│
├── Coding Agent
│   ├── DSH 1.3
│   ├── DeepSeek
│   ├── GitHub
│   ├── Memory
│   └── Browser
│
├── Research Agent
│   ├── DSH 1.3
│   ├── Claude
│   ├── Search
│   └── PDF
│
└── Minimal
    ├── DSH 1.3
    └── DeepSeek
```

Instance 必须隔离。

推荐目录：

```text
~/.aiharness/
│
├── runtimes/
│   ├── dsh/
│   └── mine/
│
├── instances/
│   ├── coding/
│   ├── research/
│   └── minimal/
│
├── cache/
├── packages/
├── logs/
└── launcher.db
```

单个 Instance：

```text
instances/coding/
│
├── instance.json
├── config/
├── plugins/
├── skills/
├── mcp/
├── workspace/
├── logs/
└── state/
```

---

# 6. Provider Manager

这相当于 Minecraft 的账号管理。

但这里管理：

```text
OpenAI
Anthropic
DeepSeek
Gemini
Qwen
OpenRouter
Custom OpenAI-compatible
```

Provider 与 Instance 解耦。

例如：

```text
Providers
│
├── DeepSeek Main
├── OpenAI Work
└── Local Ollama
```

Instance 只引用：

```text
providerRef: "deepseek-main"
```

而不是复制 Key。

正确结构：

```text
Instance
    ↓ reference
Provider Profile
    ↓
Credential Store
```

Credentials 不要存 `instance.json`。

Windows：

```text
Credential Manager / DPAPI
```

macOS：

```text
Keychain
```

Linux：

```text
Secret Service
```

---

# 7. Package System

这是第二核心。

统一管理：

```text
Plugin
Skill
MCP
Prompt
Persona
Config Pack
Setup Pack
```

不要让这几个对象完全混在一起。

统一抽象：

```ts
interface PackageManifest {
  id: string
  type:
    | "plugin"
    | "skill"
    | "mcp"
    | "prompt"
    | "persona"
    | "config"
    | "setup"

  name: string
  version: string

  dependencies: PackageDependency[]
  conflicts: PackageConflict[]

  platforms?: PlatformConstraint[]
  runtimes?: RuntimeConstraint[]
}
```

然后具体扩展。

---

# 8. Plugin / Skill / MCP 的边界

建议非常明确：

## Plugin

扩展 Harness 本身。

例如：

```text
Memory Plugin
GitHub Plugin
Scheduler Plugin
Backup Plugin
Web UI Plugin
```

## Skill

告诉 Agent：

```text
如何完成某类任务
```

主要由：

```text
Prompt
Instructions
Workflow
Examples
Schema
```

组成。

例如：

```text
Code Review
PR Analysis
Market Research
Document Writer
```

## MCP

提供外部工具能力。

例如：

```text
GitHub MCP
Postgres MCP
Browser MCP
Slack MCP
```

所以：

```text
Plugin = 扩展 Runtime
Skill  = 扩展 Agent 行为
MCP    = 扩展 Agent IO
```

这个边界要从一开始定死。

---

# 9. Setup Pack

这是最值得做成标准格式的东西。

类似 Minecraft Modpack。

比如：

```text
coding-agent.dshpack
```

内部：

```text
coding-agent.dshpack
│
├── manifest.json
├── plugins.json
├── skills.json
├── mcp.json
├── config/
└── README.md
```

Manifest：

```ts
interface SetupPackManifest {
  manifestVersion: 1

  id: string
  name: string
  version: string

  runtime: RuntimeRequirement

  plugins: PackageRequirement[]
  skills: PackageRequirement[]
  mcp: PackageRequirement[]

  configTemplates: ConfigTemplate[]

  providerRequirements?: ProviderRequirement[]

  healthChecks: HealthCheck[]
}
```

导入：

```text
Setup Pack
   ↓
Manifest Parse
   ↓
Runtime Check
   ↓
Dependency Resolve
   ↓
Download
   ↓
Install
   ↓
Configure
   ↓
Verify
   ↓
Create Instance
```

---

# 10. Smart Plugin Market

你刚才已经收敛得很好：

> Smart Plugin Market 不再负责整个 Launcher。

它只是 Package Discovery 层。

结构：

```text
Market
├── Browse
├── Search
├── Smart Search
├── Categories
├── Trending
├── Installed
└── Updates
```

智能搜索：

```text
User Query
    ↓
Query Normalization
    ↓
Lexical Search
    ↓
Semantic Prefilter
    ↓
Candidate Set
    ↓
LLM Re-rank
    ↓
Registry Validation
    ↓
Results
```

满足：

$$
Result \subseteq Registry
$$

LLM 不允许生成不存在的插件。

---

# 11. Market Registry

建议做统一 Registry。

```text
Registry
├── runtime
├── plugin
├── skill
├── mcp
├── prompt
└── setup-pack
```

每项：

```text
Package
├── metadata
├── versions
├── downloads
├── compatibility
├── dependencies
├── source
├── publisher
├── checksum
└── signatures
```

本地：

```text
Remote Registry
      ↓
Cache
      ↓
Embedded Snapshot
```

优先级：

```text
Remote
→ Cache
→ Snapshot
```

---

# 12. Dependency Resolver

这就是类似 Fabric/Forge mod loader 的依赖处理。

输入：

```text
Package A
├── requires B >=1.4
├── requires C
└── conflicts D <2
```

Resolver：

```text
Selected Package
      ↓
Dependency Expansion
      ↓
Version Constraint
      ↓
Runtime Compatibility
      ↓
Platform Compatibility
      ↓
Conflict Detection
      ↓
Resolution Plan
```

第一版用简单 semver solver 就够。

不用一开始搞 SAT。

---

# 13. Runtime Profile

Forge / Fabric 对应的不是插件，而是：

```text
Runtime Profile
```

例如：

```text
DSH Vanilla
DSH Web
DSH Headless
DSH Dev
DSH Secure
Mine-Harness Minimal
Mine-Harness Full
```

Profile：

```ts
interface RuntimeProfile {
  id: string

  runtime: string

  features: string[]

  defaults: {
    plugins?: string[]
    config?: Record<string, unknown>
  }

  launchArgs?: string[]
}
```

用户创建 Instance 时：

```text
Runtime
DSH 1.3

Profile
○ Minimal
○ Web
○ Headless
● Developer
```

非常像：

```text
Minecraft
+
Fabric
```

---

# 14. Runtime Dependency Manager

类似 PCL 自动寻找 Java。

你的 Launcher 自动检查：

```text
Node
Python
Git
Docker
WSL
Bun
pnpm
npm
uv
```

例如：

```text
Runtime Requirements

Node >= 22      ✓
Git             ✓
Docker          ✗
Python >=3.12   ✓
```

然后：

```text
[Repair]
```

第一版可以只检测，不要全部自动安装。

后面再加入 managed runtime。

---

# 15. Launch Pipeline

真正的 Launch 不应该只是：

```text
spawn("dsh")
```

而要有完整启动状态机。

```text
IDLE
 ↓
PREPARING
 ↓
VALIDATING
 ↓
RESOLVING
 ↓
STARTING
 ↓
RUNNING
 ↓
STOPPING
 ↓
STOPPED
```

异常：

```text
VALIDATING
    ↓
INVALID

STARTING
    ↓
CRASHED

RUNNING
    ↓
DEGRADED
```

完整链：

```text
Launch
 ↓
Validate Instance
 ↓
Validate Runtime
 ↓
Validate Provider
 ↓
Resolve Dependencies
 ↓
Prepare Environment
 ↓
Generate Runtime Config
 ↓
Spawn Process
 ↓
Attach Logs
 ↓
Health Probe
 ↓
RUNNING
```

---

# 16. Process Supervisor

这是 Launcher 和普通 GUI Wrapper 的分界线。

Launcher 必须掌握 Harness Process 生命周期。

```text
Supervisor
├── spawn
├── stop
├── kill
├── restart
├── stdout
├── stderr
├── exit code
├── health
└── pid
```

状态：

```ts
interface ProcessState {
  pid?: number

  status:
    | "stopped"
    | "starting"
    | "running"
    | "degraded"
    | "crashed"

  startedAt?: number
  exitCode?: number
}
```

---

# 17. Agent Diagnostics

对应 Minecraft Crash Analyzer。

这是非常有用户价值的模块。

输入：

```text
stdout
stderr
runtime logs
plugin logs
environment
version information
```

诊断：

```text
Crash
 ↓
Signature Extraction
 ↓
Known Rule Match
 ↓
Dependency Analysis
 ↓
Configuration Analysis
 ↓
Optional LLM Explain
 ↓
Repair Suggestions
```

结果：

```text
Launch failed

Cause:
GitHub plugin requires DSH >=1.3

Current:
DSH 1.2.7

Recommended:
Upgrade runtime to 1.3.2

[Repair]
```

不要让 LLM 直接瞎诊断。

顺序应该是：

```text
Rule Engine
→ Structured Evidence
→ LLM Explanation
```

---

# 18. Repair Engine

PCL 很重要的一点是“坏了能修”。

你的 Launcher 也应该有：

```text
Repair Instance
```

检查：

```text
Runtime missing
Package missing
Checksum mismatch
Broken config
Dependency mismatch
Invalid provider
Port collision
Environment missing
```

然后生成 Repair Plan：

```text
Repair Plan

✓ Re-download runtime
✓ Restore plugin github@2.1
✓ Regenerate config
✗ API key requires user action
```

---

# 19. Update System

分四种：

```text
Launcher Update
Runtime Update
Package Update
Setup Pack Update
```

一定要分开。

而且 Instance 不要默认全部漂移。

建议：

```text
Runtime:
Pinned / Latest / Channel

Plugin:
Pinned / Compatible / Latest
```

例如：

```text
Coding Agent

DSH     1.3.1 → 1.3.2
GitHub  2.4.0 → 2.5.0
Memory  1.1.0

[Update compatible]
```

---

# 20. Snapshot / Backup

非常推荐。

用户每次重大升级前：

```text
Snapshot
```

保存：

```text
runtime version
package versions
config
instance manifest
```

不保存：

```text
明文 secrets
大 workspace
cache
```

可以：

```text
Snapshot
 ↓
Update
 ↓
Failure
 ↓
Rollback
```

这个非常像游戏版本管理。

---

# 21. UI 信息架构

建议控制在 6 个一级页面。

```text
Home
Instances
Market
Packs
Activity
Settings
```

## Home

```text
Current Instance

Coding Agent
DSH 1.3.2

DeepSeek V4
12 Plugins
3 MCP

● Ready

[▶ Launch]
```

## Instances

```text
Coding
Research
Personal
Minimal
```

## Market

```text
Smart Search
Plugins
Skills
MCP
Setup Packs
```

## Packs

```text
Installed
Import
Export
Community Packs
```

## Activity

```text
Downloads
Installs
Updates
Launch Logs
Diagnostics
```

## Settings

```text
Providers
Runtime
Network
Storage
Security
Appearance
```

---

# 22. 创建实例流程

不要让用户面对几十个配置。

第一版：

```text
Create Instance

① Name
Coding Agent

② Runtime
DSH 1.3

③ Profile
Developer

④ Provider
DeepSeek

⑤ Optional setup
Coding Essentials

        [Create]
```

创建完：

```text
Ready
```

---

# 23. Advanced Mode

PCL 有普通用户和高级玩家。

你这里也必须分。

普通模式：

```text
Launch
Install Setup
Smart Search
Provider
```

高级模式：

```text
Environment Variables
Runtime Arguments
Raw Config
Plugin Load Order
Ports
Sandbox
Proxy
Working Directory
Logs
Dependency Tree
```

这非常重要。

否则 Launcher 很容易变成“另一个配置面板”。

---

# 24. 安全模型

至少建立四条边界。

## Secrets

永远进入 OS Credential Store。

## Package Source

记录：

```text
source
version
checksum
publisher
```

## Install Scripts

插件不能无限执行任意 install script。

要有：

```text
allow
warn
deny
```

## Process Isolation

未来可以支持：

```text
Native
Docker
WSL
Sandbox
```

作为 Instance Runtime Mode。

---

# 25. 数据库不要复杂

本地 Launcher 完全不需要 PostgreSQL。

推荐：

```text
SQLite
+
JSON manifests
+
Filesystem
```

分工：

```text
SQLite
→ index / metadata / history

JSON
→ portable manifest

Filesystem
→ actual packages / runtime / logs
```

很好维护。

---

# 26. 推荐技术栈

如果目标是桌面 PCL 类产品：

```text
Desktop
├── Tauri
└── React + TypeScript

Core
├── TypeScript
└── Rust

Storage
├── SQLite
└── JSON

Networking
└── Rust reqwest / Node fetch

Process
└── Rust

Package / Registry Logic
└── TypeScript

Smart Search
└── TypeScript

Security / Credential
└── Rust + OS APIs
```

我会推荐：

> **Tauri + React + TypeScript + Rust**

而不是 Electron。

因为 Launcher 本身非常适合 Tauri：

* 文件系统
* 下载
* 进程控制
* 原生密钥存储
* 系统托盘
* 更新
* Windows/macOS/Linux

Rust 负责真正的 Launcher Core。

TypeScript 负责产品逻辑和 UI。

---

# 27. 代码结构

建议：

```text
ai-harness-launcher/
│
├── apps/
│   └── desktop/
│       ├── src/
│       └── src-tauri/
│
├── packages/
│
│   ├── domain/
│   │   ├── runtime/
│   │   ├── instance/
│   │   ├── package/
│   │   ├── provider/
│   │   └── setup/
│
│   ├── registry/
│   ├── market/
│   ├── resolver/
│   ├── setup-pack/
│   ├── provider/
│   └── diagnostics/
│
├── crates/
│
│   ├── launcher-core/
│   ├── process-supervisor/
│   ├── runtime-manager/
│   ├── downloader/
│   ├── credential-store/
│   └── filesystem/
│
├── schemas/
│   ├── runtime.schema.json
│   ├── instance.schema.json
│   ├── package.schema.json
│   └── setup-pack.schema.json
│
└── registry/
    └── builtin.snapshot.json
```

架构边界：

```text
React
 ↓
TypeScript Application Layer
 ↓
Tauri Commands
 ↓
Rust Launcher Core
 ↓
OS / Runtime / Filesystem / Processes
```

---

# 28. 最重要的几个 Manifest

整个系统实际上由四个 Manifest 驱动：

```text
RuntimeManifest
InstanceManifest
PackageManifest
SetupPackManifest
```

可以视为系统的四大数据契约。

### RuntimeManifest

描述：

> Harness 怎么安装和启动。

### InstanceManifest

描述：

> 用户当前这套环境是什么。

### PackageManifest

描述：

> Plugin / MCP / Skill 是什么。

### SetupPackManifest

描述：

> 一整套环境应该怎么组成。

只要这四个 Schema 定好，整个 Launcher 基本就立住了。

---

# 29. 最终系统状态关系

可以压成：

```text
                    Registry
                       │
             ┌─────────┴─────────┐
             ▼                   ▼
          Runtime             Packages
             │                   │
             └────────┬──────────┘
                      ▼
                 Setup Pack
                      │
                      ▼
                   Instance
                      │
        ┌─────────────┼──────────────┐
        ▼             ▼              ▼
     Provider       Config        Workspace
        │             │              │
        └─────────────┼──────────────┘
                      ▼
                 Launch Plan
                      │
                      ▼
                 Supervisor
                      │
                      ▼
                Harness Process
                      │
               ┌──────┴──────┐
               ▼             ▼
            Health          Logs
               │             │
               └──────┬──────┘
                      ▼
                 Diagnostics
```

---

# 30. V0.1 应该砍到什么程度

第一版只做：

```text
1 Runtime install
2 Runtime version switch
3 Instance create/delete
4 Provider/API config
5 Plugin install/uninstall
6 Smart Plugin Search
7 Setup Pack import/export
8 Launch/Stop/Restart
9 Logs
10 Basic diagnostics
```

不要做：

```text
复杂 Capability Resolver
AI 自动生成完整整合包
SAT dependency solver
云同步
社区账户体系
在线支付
复杂权限系统
自动 Repair Agent
多机 Runtime
```

这些后面再加。

---

# 31. 最终产品心智

最终不要宣传：

> 一个 DSH 管理 GUI。

而是：

> **PCL for AI Harnesses.**

更完整地说：

> **A launcher, instance manager, package manager and setup ecosystem for AI harnesses.**

中文就是：

> **AI Harness 的启动器、实例管理器、插件管理器和整合包平台。**

而产品内部真正的主干就是：

$$
\boxed{
Runtime
+
Instance
+
Package
+
Setup
+
Provider
+
Supervisor
}
$$

Smart Plugin Market 只是其中：

$$
Package\ Discovery
$$

这一层。

这样整个产品既不会被“插件市场”绑死，也不会一下膨胀成一个巨型 AI OS。它更像真正的 PCL：**前台极简启动，后台把版本、依赖、实例、插件、配置、诊断全部吃掉。**
