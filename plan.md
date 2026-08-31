是。第一版就应该明确成：

> **Windows 双击运行的 AI Harness Launcher**
>
> 类似 PCL：用户下载安装 `.exe`，打开 Launcher，管理 DSH 实例、Provider、插件，然后点击 **Launch**。

而且我建议第一版不要追求“通用 AI Harness 平台”，而是：

$$
\boxed{\text{Windows + DSH First + Runtime Adapter Architecture}}
$$

也就是**产品只支持 DSH，但架构允许未来接其他 Harness**。

---

# 一、最终技术栈选型

我会直接定这一套：

| 层             | 技术                                 |    推荐 |
| ------------- | ---------------------------------- | ----: |
| Desktop Shell | **Tauri 2**                        | ⭐⭐⭐⭐⭐ |
| Launcher Core | **Rust**                           | ⭐⭐⭐⭐⭐ |
| UI            | **React + TypeScript**             | ⭐⭐⭐⭐⭐ |
| Build         | **Vite**                           | ⭐⭐⭐⭐⭐ |
| UI CSS        | Tailwind CSS                       | ⭐⭐⭐⭐⭐ |
| Components    | Radix / shadcn 思路                  |  ⭐⭐⭐⭐ |
| 前端状态          | Zustand                            | ⭐⭐⭐⭐⭐ |
| IPC           | Tauri Commands / Events            | ⭐⭐⭐⭐⭐ |
| 本地 DB         | **SQLite**                         | ⭐⭐⭐⭐⭐ |
| DB access     | Rust `rusqlite`                    | ⭐⭐⭐⭐⭐ |
| Manifest      | JSON + Serde                       | ⭐⭐⭐⭐⭐ |
| HTTP/下载       | `reqwest`                          | ⭐⭐⭐⭐⭐ |
| Async Runtime | `tokio`                            | ⭐⭐⭐⭐⭐ |
| Process       | Rust `tokio::process`              | ⭐⭐⭐⭐⭐ |
| Secret        | Windows Credential Manager / DPAPI | ⭐⭐⭐⭐⭐ |
| 日志            | `tracing`                          | ⭐⭐⭐⭐⭐ |
| Hash          | SHA-256                            | ⭐⭐⭐⭐⭐ |
| Installer     | **NSIS `.exe`**                    | ⭐⭐⭐⭐⭐ |
| Auto Update   | Tauri Updater                      |  ⭐⭐⭐⭐ |
| CI/CD         | GitHub Actions Windows Runner      | ⭐⭐⭐⭐⭐ |

Tauri 2 官方目前仍原生支持 Windows，并可以生成 NSIS `setup.exe` 或 MSI；对于这种面向个人用户的 PCL 类 Launcher，我优先选 **NSIS `.exe`**。([Tauri][1])

Tauri 还原生支持 updater；Windows 更新包可以直接使用 NSIS installer，并有签名更新 artifact。([Tauri][2])

---

# 二、为什么不是 Electron

这个项目跟普通桌面 CRUD App 不一样。

你需要大量：

```text
文件系统
下载
Hash
解压
运行进程
结束进程
抓 stdout/stderr
检查 PID
检测 Node/Python/Git
维护 Instance
启动 Runtime
密钥管理
自动更新
```

这些正好是 Rust 擅长的。

所以：

```text
Electron
Node
React
```

虽然开发快，但最后核心还是得写大量系统代码。

而：

```text
Tauri
├── React/TS → UI
└── Rust → Launcher Core
```

天然更符合 Launcher。

Tauri 官方也支持 sidecar/external binary，并通过 capability/permission 对可执行命令做约束，很适合后续管理 DSH 或其他 Runtime。([Tauri][3])

---

# 三、语言边界一定要定死

这是整个项目不膨胀的关键。

## TypeScript 只管

```text
UI
页面
表单
视图状态
Smart Plugin Search
Registry presentation
用户交互
```

## Rust 只管

```text
Runtime
Instance
Process
Filesystem
Download
Install
Update
Secrets
SQLite
Diagnostics
```

也就是：

```text
┌───────────────────────────────┐
│ React + TypeScript            │
│                               │
│ “用户想做什么”                 │
└──────────────┬────────────────┘
               │ typed IPC
               ▼
┌───────────────────────────────┐
│ Rust Launcher Core            │
│                               │
│ “机器实际上做什么”              │
└──────────────┬────────────────┘
               ▼
          Windows / DSH
```

不要让 React：

```ts
exec("dsh")
fs.writeFile(...)
```

也不要让 Rust：

```text
管理 UI 状态
管理搜索框
管理 Modal
```

---

# 四、总体软件架构

最终 Windows 版：

```text
AI Harness Launcher.exe
│
├─────────────────────────────────────┐
│ PRESENTATION                        │
│ React + TypeScript                  │
│                                     │
│ Home                                │
│ Instances                           │
│ Market                              │
│ Packs                               │
│ Activity / Logs                     │
│ Settings                            │
├─────────────────────────────────────┤
│ APPLICATION                         │
│                                     │
│ CreateInstance                      │
│ InstallRuntime                      │
│ InstallPlugin                       │
│ LaunchInstance                      │
│ StopInstance                        │
│ DiagnoseInstance                    │
│ ImportSetupPack                     │
├─────────────────────────────────────┤
│ DOMAIN                              │
│                                     │
│ Runtime                             │
│ RuntimeAdapter                      │
│ Instance                            │
│ Package                             │
│ Provider                            │
│ SetupPack                           │
│ LaunchPlan                          │
├─────────────────────────────────────┤
│ RUST LAUNCHER CORE                  │
│                                     │
│ Runtime Manager                     │
│ Instance Manager                    │
│ Package Manager                     │
│ Provider Vault                      │
│ Dependency Resolver                 │
│ Downloader                          │
│ Process Supervisor                  │
│ Diagnostics Engine                  │
│ Update Manager                      │
├─────────────────────────────────────┤
│ INFRASTRUCTURE                      │
│                                     │
│ SQLite                              │
│ Filesystem                          │
│ HTTP                                │
│ Credential Manager                  │
│ Windows Process API                 │
│ Registry / Cache                    │
└─────────────────────────────────────┘
                  │
                  ▼
             DSH Runtime
```

---

# 五、最核心对象：Instance

整个产品不要围绕插件建。

一定围绕：

# `Instance`

例如：

```text
Coding Agent
Research Agent
Personal Assistant
Minimal DeepSeek
```

每一个都是独立运行环境。

```rust
Instance {
    id,
    name,

    runtime,
    runtime_profile,

    provider_ref,

    plugins,
    skills,
    mcp,

    config,

    workspace,

    state
}
```

状态：

```text
Created
   ↓
Preparing
   ↓
Ready
   ↓
Starting
   ↓
Running
   ↓
Stopping
   ↓
Stopped
```

异常：

```text
Broken
Crashed
NeedsRepair
```

---

# 六、目录结构

Windows 第一版我会直接这样做：

```text
%LOCALAPPDATA%/
└── AIHarnessLauncher/
    │
    ├── launcher.db
    │
    ├── runtimes/
    │   └── dsh/
    │       ├── 1.3.2/
    │       └── ...
    │
    ├── instances/
    │   ├── coding-agent/
    │   │   ├── instance.json
    │   │   ├── config/
    │   │   ├── plugins/
    │   │   ├── skills/
    │   │   ├── mcp/
    │   │   ├── workspace/
    │   │   └── logs/
    │   │
    │   └── research-agent/
    │
    ├── packages/
    │
    ├── cache/
    │   ├── registry/
    │   └── downloads/
    │
    ├── logs/
    │
    └── temp/
```

Secrets 不放这里。

---

# 七、四个 Manifest 是项目骨架

第一阶段最先定义这四个：

```text
RuntimeManifest
InstanceManifest
PackageManifest
SetupPackManifest
```

## 1. RuntimeManifest

```json
{
  "id": "dsh",
  "version": "1.3.2",
  "platform": ["windows-x64"],
  "requirements": {
    "node": ">=22"
  },
  "launch": {
    "command": "dsh",
    "args": []
  }
}
```

告诉 Launcher：

> DSH 怎么安装、需要什么、怎么启动。

---

## 2. InstanceManifest

```json
{
  "id": "coding-agent",
  "name": "Coding Agent",

  "runtime": {
    "id": "dsh",
    "version": "1.3.2"
  },

  "profile": "developer",

  "providerRef": "deepseek-main",

  "plugins": [],

  "skills": [],

  "mcp": []
}
```

---

## 3. PackageManifest

统一：

```text
Plugin
Skill
MCP
Prompt
Config
```

例如：

```json
{
  "id": "github-plugin",
  "type": "plugin",
  "version": "2.1.0",

  "runtime": {
    "dsh": ">=1.3"
  },

  "dependencies": [],
  "conflicts": []
}
```

---

## 4. SetupPackManifest

对应 Minecraft Modpack。

```json
{
  "name": "DeepSeek Coding",

  "runtime": {
    "id": "dsh",
    "version": "^1.3"
  },

  "plugins": [
    "github",
    "memory"
  ],

  "skills": [
    "code-review"
  ]
}
```

以后：

```text
deepseek-coding.dshpack
```

就靠这个。

---

# 八、RuntimeAdapter：必须提前做，但不要过度抽象

接口保持非常小：

```rust
trait RuntimeAdapter {
    fn detect();
    fn install();
    fn verify();

    fn prepare_instance();

    fn launch();
    fn stop();

    fn diagnose();
}
```

第一版只有：

```text
RuntimeAdapter
    │
    └── DshAdapter
```

以后：

```text
RuntimeAdapter
├── DshAdapter
├── MineHarnessAdapter
├── IronClawAdapter
└── CustomAdapter
```

这样就行。

千万别现在设计几十个 abstract interface。

---

# 九、Launcher Core 建议拆成 8 个 Rust 模块

```text
launcher-core
│
├── runtime
├── instance
├── package
├── process
├── download
├── provider
├── diagnostics
└── storage
```

## runtime

```text
detect
install
verify
remove
```

## instance

```text
create
clone
delete
validate
```

## package

```text
install
uninstall
resolve
```

## process

```text
spawn
stop
kill
restart
stdout
stderr
```

## download

```text
HTTP
resume
checksum
cache
```

## provider

```text
API key
base URL
model
credential ref
```

## diagnostics

```text
environment check
runtime check
package check
crash analysis
```

## storage

```text
SQLite
manifest
cache
```

够了。

---

# 十、SQLite 怎么用

只存：

```text
instances
runtimes
providers metadata
packages
install history
launch history
diagnostics
settings
```

例如：

```text
launcher.db

instances
runtime_installations
provider_profiles
installed_packages
launch_sessions
diagnostic_events
settings
```

但是：

> **不要把 Instance 完全存在数据库里。**

Instance 主事实最好是：

```text
instance.json
```

SQLite 是：

```text
Index + Cache + History
```

这样用户复制 Instance 文件夹依然可迁移。

---

# 十一、Secret 怎么存

绝对不要：

```json
{
  "apiKey": "sk-xxxxx"
}
```

放进 `instance.json`。

正确：

```text
Instance
   ↓
providerRef
   ↓
Provider Profile
   ↓
credentialId
   ↓
Windows Credential Manager
```

Tauri 本身也提供 Stronghold secret storage 插件，并支持 Windows，但 Windows-first 产品我更倾向于直接把 Credential Manager/DPAPI 封装进 Rust Core；以后做跨平台时再抽象 `SecretStore`。Tauri Stronghold 可以作为备选跨平台方案。([Tauri][4])

---

# 十二、Smart Plugin Market 放哪

不要写进 Rust Launcher Core。

放：

```text
packages/market
```

TypeScript。

架构：

```text
Registry
   ↓
Normalize
   ↓
Local Index
   ↓
Traditional Search
          +
Smart Search
   ↓
Candidate top-40
   ↓
DSH LLM
   ↓
Re-rank
   ↓
Registry validation
   ↓
Result
```

满足：

$$
Result\subseteq Registry
$$

第一版它仍然只是：

> **智能搜索**

不要做 Capability Solver。

---

# 十三、Windows 安装包

用户最终应该拿到：

```text
AI-Harness-Launcher-Setup.exe
```

双击：

```text
安装
↓
桌面快捷方式
↓
开始菜单
↓
Launch
```

首发推荐：

# **NSIS**

而不是 MSI。

原因很简单：

```text
NSIS → 消费者软件 / Launcher
MSI  → 企业部署
```

Tauri 官方两种都支持。([Tauri][1])

后面再同时提供：

```text
Setup.exe
MSI
Portable.zip
```

---

# 十四、Windows 签名

开发阶段：

```text
unsigned
```

可以。

真正公开 Release 前：

```text
Code Signing
```

要做。

否则 Windows SmartScreen 很容易把刚发布的 Launcher 提示成未知发布者；Tauri 官方文档也明确说明 Windows code signing 对降低浏览器下载后的 SmartScreen 不信任警告非常重要。([Tauri][5])

所以它应该属于：

```text
Productization 阶段
```

而不是 MVP 阶段。

---

# 十五、阶段化落地路线

我会砍成 **7 个阶段**。

不是每阶段堆功能，而是每阶段必须有明确 Gate。

---

# Phase 0 — Launcher Skeleton

目标：

> 双击 `.exe`，看到真正的 Windows Launcher。

只做：

```text
Tauri
React
Rust IPC
Installer
App directory
logging
```

UI：

```text
Home
Instances
Market
Settings
```

但大部分页面可以为空。

验收：

```text
✓ npm/pnpm build
✓ cargo build
✓ tauri build
✓ 生成 Setup.exe
✓ 安装
✓ 双击
✓ 卸载
✓ React ↔ Rust IPC 工作
```

这一步以后产品已经不是 Web App 了。

---

# Phase 1 — DSH Launcher MVP

这是第一个真正有价值的版本。

只支持：

```text
DSH
+
1 Instance
+
1 Provider
```

功能：

```text
检测 Node
检测 DSH

配置：
DeepSeek API Key
Base URL
Model

Launch
Stop

stdout/stderr
```

核心链：

```text
Launcher
 ↓
Check DSH
 ↓
Check Provider
 ↓
Generate Env
 ↓
spawn dsh
 ↓
Capture Logs
 ↓
Running
```

验收必须非常简单：

> 用户不用打开 Terminal，就能从 `.exe` 启动 DSH。

这就是第一个关键 Gate。

---

# Phase 2 — Instance System

从：

```text
一个 DSH
```

升级到：

```text
多个独立 DSH Instance
```

实现：

```text
Create
Rename
Clone
Delete
Switch
```

首页：

```text
Coding Agent      Ready
Research Agent    Stopped
Minimal           Stopped
```

每个 Instance 有：

```text
Runtime
Provider
Config
Workspace
Plugin list
```

验收：

```text
Instance A 的插件/配置
不能污染
Instance B
```

这是第二个大 Gate。

---

# Phase 3 — Package Manager + Smart Market

这一步才把现在两个 Market 项目接进来。

## Package Manager

实现：

```text
list
install
uninstall
enable
disable
```

先只做：

```text
DSH Plugin
```

Skill/MCP 可以后补。

## Market

直接使用：

```text
dsh-market 基础设施
+
smart-plugin-market 智能搜索
```

最终：

```text
Market

[ Search plugins... ]

或：

“我想让 DeepSeek 操作 GitHub”
```

返回真实 Registry 插件。

验收：

```text
Smart Search
 ↓
Select Plugin
 ↓
Install
 ↓
Instance
 ↓
Launch
```

整个闭环跑通。

---

# Phase 4 — Runtime Management

到这里再开始像 PCL。

支持：

```text
DSH 1.2.x
DSH 1.3.x
latest
```

Runtime Manager：

```text
Install
Switch
Remove
Verify
Repair
```

Instance：

```text
Coding
DSH 1.3.2

Legacy
DSH 1.2.8
```

互不干扰。

然后再引入：

```text
Runtime Profile

Minimal
Web
Headless
Developer
```

验收：

> 两个不同 DSH 版本可以同时存在并由不同 Instance 使用。

---

# Phase 5 — Setup Pack

这就是开始出现真正的“Minecraft Launcher 味”。

增加：

```text
Import Pack
Export Pack
```

格式：

```text
.dshpack
```

例如：

```text
DeepSeek Coding.dshpack
```

里面声明：

```text
Runtime
Plugins
Skills
MCP
Config Templates
```

用户：

```text
Import
 ↓
Resolve
 ↓
Install
 ↓
Create Instance
 ↓
Launch
```

验收：

> 一个人导出的环境，可以在另一台 Windows 电脑上恢复为同样结构。

API Key 除外。

---

# Phase 6 — Diagnostics + Repair

这一步非常重要。

也是真正超过普通插件管理 GUI 的地方。

用户看到：

```text
Coding Agent failed to start

Cause
github-plugin requires DSH >= 1.3

Current
DSH 1.2.8

[Fix]
```

诊断顺序：

```text
Rule Engine
 ↓
Structured Diagnosis
 ↓
Optional LLM Explanation
```

不是：

```text
把全部日志扔给 LLM 瞎猜
```

实现：

```text
Node missing
DSH missing
plugin missing
runtime mismatch
version conflict
invalid config
invalid provider
port occupied
crash exit code
```

验收：

> 常见启动失败不需要用户打开 Terminal。

---

# Phase 7 — Productization

最后才做：

```text
Auto Update
Code Signing
Crash Reporting
Telemetry opt-in
Download Resume
Checksums
Rollback
Snapshot
Portable
Installer polish
i18n
Release Channel
```

Tauri 官方 updater 已支持 Windows 更新包及签名 artifact，因此这里不需要自己重新发明更新器。([Tauri][6])

正式版本：

```text
Stable
Beta
Nightly
```

---

# 十六、版本路线可以直接这样命名

```text
v0.1
DSH Launcher

v0.2
Multi Instance

v0.3
Smart Plugin Market

v0.4
Runtime Manager

v0.5
Setup Pack

v0.6
Diagnostics

v0.7
Repair / Snapshot

v1.0
Signed + Updater + Stable
```

非常清晰。

---

# 十七、第一版 UI 也不要搞复杂

我甚至建议只有五页：

```text
┌─────────────────────┐
│ Home                │
│ Instances           │
│ Market              │
│ Activity            │
│ Settings            │
└─────────────────────┘
```

Home：

```text
Coding Agent

DSH 1.3.2
DeepSeek
8 Plugins

● Ready

       [ ▶ Launch ]
```

**这个 Launch 按钮就是整个产品的视觉中心。**

这才像 PCL。

---

# 十八、仓库最终结构

我推荐 monorepo：

```text
ai-harness-launcher/
│
├── apps/
│   └── desktop/
│       ├── src/                   # React
│       │
│       └── src-tauri/
│
├── packages/
│   │
│   ├── ui/
│   │
│   ├── domain-types/
│   │
│   ├── smart-market/
│   │
│   └── registry/
│
├── crates/
│   │
│   ├── launcher-core/
│   ├── runtime-manager/
│   ├── instance-manager/
│   ├── package-manager/
│   ├── process-supervisor/
│   ├── downloader/
│   ├── diagnostics/
│   └── credential-store/
│
├── adapters/
│   └── dsh/
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

但是注意：

**物理上可以 monorepo，逻辑上先不要拆 20 个 Cargo crate。**

V0.1 最好实际只有：

```text
desktop
launcher-core
dsh-adapter
smart-market
```

四块。

后面真的膨胀再拆。

---

# 十九、哪些现有东西直接复用

你的情况最适合：

```text
dsh-market
     │
     ├── Registry
     ├── Plugin metadata
     ├── Install/update
     └── Market UI ideas
             │
             ▼
        Smart Market
             ▲
             │
smart-plugin-market
     ├── prefilter
     ├── prompt
     ├── schema
     └── candidate validation
```

然后：

```text
Smart Market
     ↓
AI Harness Launcher
```

而不是：

```text
把 Launcher
写进 dsh-market
```

层次应该永远是：

$$
\boxed{
Launcher
>
Market
>
Plugin
}
$$

---

# 二十、第一阶段真正该写什么

如果现在立即开始工程化，我会按这个顺序：

```text
① Tauri Windows shell
        ↓
② Rust ↔ React IPC contract
        ↓
③ DSH detect()
        ↓
④ Provider Vault
        ↓
⑤ DSH launch()
        ↓
⑥ stdout/stderr streaming
        ↓
⑦ stop()/restart()
        ↓
⑧ InstanceManifest
        ↓
⑨ 多 Instance
        ↓
⑩ Smart Market
```

**不要第一天做 Market。**

因为：

> Launcher 最重要的第一性验收不是“能不能搜到插件”。

而是：

$$
\boxed{
\text{用户双击 Launcher}
\rightarrow
\text{点 Launch}
\rightarrow
\text{DSH 真正运行}
}
$$

只要这条链首先打通，后面的 Instance、Market、Setup Pack、Diagnostics 都是在一个真实产品骨架上长出来。

最终我会把 Windows V1 技术架构收敛成这一行：

```text
Tauri 2
 + React/TypeScript
 + Rust Launcher Core
 + SQLite
 + Windows Credential Manager
 + DSH Runtime Adapter
 + Smart Plugin Market
 + NSIS
```

这套已经足够做出真正可以下载、双击安装、创建实例、安装插件并启动 Agent 的 **“PCL for AI Harness”**，同时不会把首版做成过重的“AI 操作系统”。

[1]: https://v2.tauri.app/distribute/windows-installer/?utm_source=chatgpt.com "Windows Installer | Tauri"
[2]: https://v2.tauri.app/zh-cn/reference/javascript/updater/?utm_source=chatgpt.com "@tauri-apps/plugin-updater | Tauri"
[3]: https://v2.tauri.app/zh-cn/develop/sidecar/?utm_source=chatgpt.com "嵌入外部二进制文件 | Tauri"
[4]: https://v2.tauri.app/plugin/stronghold/?utm_source=chatgpt.com "Stronghold | Tauri"
[5]: https://v2.tauri.app/zh-cn/distribute/sign/windows/?utm_source=chatgpt.com "Windows 代码签名 | Tauri"
[6]: https://v2.tauri.app/zh-cn/plugin/updater/?utm_source=chatgpt.com "更新 | Tauri"

---

# 二十一、产品化补齐计划（现状 → v1.0）

> 截至 2026-08-31：Phase 0–3 已完成并验证（多实例 + SQLite 历史 + 完整市场 + 诊断 + 主题双向同步 + DSH 窗口化）。功能面已接近"可演示产品"，**工程面仍差 7 项**，按下表补齐后才能"发给陌生人装机"。

## 现状基线（实测锚点）

```text
① DSH 运行时不携带      dsh-adapter/src/lib.rs:97  靠找兄弟目录 deepseek-harness-master 的 bin.js
② Node 不携带           spawn 走 PATH 的 node（0xc0000142 出在 node --version spawn）
③ 无发布流水线          没有 .github/workflows，构建全手动
④ 无自动更新            Tauri updater 未接
⑤ 进程树 teardown 不可靠  stop 只杀顶层，僵尸树叠加 → 重启崩溃
⑥ 测试薄                少量 Rust 单测 + 1 个集成测试，无前端/E2E/安装包冒烟
⑦ 未签名                NSIS setup.exe 无签名 → SmartScreen 红屏
```

## 落地顺序（按依赖，P0 最急）

### P0 — DSH + Node 运行时自给（缺口 ①②）★ 唯一阻断项

干净机器装完 Setup 跑不起来 = 不是产品。这一阶段把"运行依赖"从**开发机源码树**变成**随安装包自带 / 首次启动拉取**。

```text
runtimes/
└── node-v22.x/                  # 捆绑的 Node 运行时（外置 binary）
└── dsh-1.2.x/                   # 版本化 DSH（apps/cli 全套 + node_modules）
        └── apps/cli/lib/bin.js
```

1. **Node 打包**：下载固定 Node 22.x win-x64，作为 `resources/` 打进 NSIS；`tauri.conf.json` 加 `bundle.resources`。
2. **DSH 打包**：把 `deepseek-harness-master` 的 CLI 产物收敛成一个版本目录（`runtimes/dsh-<ver>/`，含 pnpm-installed node_modules），随包携带或首次启动按 `install` 流程拉取（复用 reqwest + SHA-256，Phase 4 的 Runtime Manager 前移）。
3. **`detect()` 解析链重构**（`dsh-adapter/src/lib.rs:145`），顺序为：
   ```text
   ① settings.dsh_path（用户显式覆盖）
   ② 捆绑 resources/dsh
   ③ %LOCALAPPDATA%/AIHarnessLauncher/runtimes/<ver>/
   ④ （仅 dev 构建）兄弟源码树 deepseek-harness-master —— 发布构建里移除
   ```
   每次解析必须 `node --version` + `bin.js --version` 双验证，失败给出"缺少运行时 → [修复]"。
4. **spawn 改绝对路径**：所有 `Command::new("node")` 改为捆绑 node 的绝对路径，杜绝 PATH 污染 + 0xc0000142 类环境干扰。
5. **Runtime Manager 命令**：`install / switch / remove / verify / repair`，版本并存（互不干扰），Instance 记录所绑定的 DSH 版本。

**验收（Gate）**：

> 全新 Windows（无 Node、无任何源码树）→ 双击 Setup → 填 Provider → Launch → DSH 窗口打开可用。卸载后系统无残留。

### P1 — 进程树 teardown 硬化（缺口 ⑤）

```text
stop/launch 切换/crash
        ↓
Windows Job Object（KILL_ON_JOB_CLOSE）整树随句柄关闭
        ↓
兜底：递归 taskkill /T /F（已用过的机制，固化成库）
        ↓
启动前僵尸清扫：孤儿 ai-harness-launcher/node/cargo 自动清
```

- launcher-core 的 child 管理（`commands/process.rs` 的 `RunningChild`）挂 Job Object，保证 stop 即整树死，不再有"stop 后再启动 0xc0000142"。
- 集成测试：**启动→stop ×5**，每次断言无残留 `node`/`dsh` 进程、端口回收。

**验收（Gate）**：> 连续 stop/start 10 轮无僵尸、无端口冲突、无 DLL init 失败。

### P2 — CI 发布流水线（缺口 ③）

```text
.github/workflows/release.yml（windows-latest）
  pnpm install
    ↓
  gate 1: cargo test --workspace
  gate 2: cargo clippy -- -D warnings
  gate 3: npx tsc --noEmit
  gate 4: instance_system 集成测试
    ↓
  pnpm tauri build --release
    ↓
  安装包冒烟：静默装到 %TEMP% → 启动 exe → 断言窗口存在 → 卸载
    ↓
  artifact: DeepSeek-Harness-Launcher-<tag>-setup.exe（+ updater json）
```

- tag `v0.4.x` 触发；产物可回溯、可复现。

**验收（Gate）**：> 一条 `git tag` 从零出安装包，CI 全绿，产物可下载。

### P3 — 自动更新（缺口 ④）

- 接 `tauri-plugin-updater`；发布 `latest.json` 到 GitHub Releases；NSIS 更新包 + 签名 artifact。
- Release Channel：`stable / beta`（稳定推 stable，热修推 beta 先行）。

**验收（Gate）**：> 改一行版本号 → tag → 老版本应用内提示更新 → 一键升级到新版本。

### P4 — 测试补齐（缺口 ⑥）

- Rust：`detect()` 解析链单测（四层 fallback 各一例）、进程树 teardown 测试、市场 reconcile 幂等测试。
- 前端：tsc + 关键 store 单测（主题 sync、市场状态机）。
- 安装包冒烟：CI 里跑（见 P2）。
- E2E（可选后置）：tauri-driver 启动真窗口点 Launch。

**验收（Gate）**：> 三个"重构护栏"——发版前全绿，且每阶段至少 1 个集成测试守护核心链路（Launch→窗口→Stop）。

### P5 — 代码签名（缺口 ⑦）

- OV 或 EV 代码签名证书；`signtool` 签 setup.exe + updater artifact；CI 里在 P2 产物上签名。
- SmartScreen 声誉随下载量累积；未签名的 nightly 走 `Unsigned` 通道（仅高级用户）。

**验收（Gate）**：> 干净机器下载安装无 SmartScreen 拦截（或仅一次"仍要运行"）。

### P6 — 补齐小件（不成阶段，随各阶段带走）

```text
Crash reporting     panic hook → 崩溃日志文件（%LOCALAPPDATA%/…/logs/crash-*.txt）
Telemetry opt-in    默认关；上报 = 崩溃 + 版本，不上报任何会话内容
Download resume    reqwest 已具备，补断点续传 + 校验和（安装/市场已用）
Portable 模式       绿色版：runtimes 放 exe 同目录
```

## 与既有 Phase 4–7 的对应

| 本计划 | 原 plan.md | 关系 |
| --- | --- | --- |
| P0 | Phase 4（Runtime Management） | 前移并落到可执行；补齐 Node 捆绑这一原计划漏掉的关键件 |
| P1 | Phase 6（Repair 一部分） | 新增的稳定性硬化，原计划未覆盖 |
| P2/P3/P5 | Phase 7（Productization） | 拆成可验收的子项 |
| P4 | （贯穿各阶段） | 新增质量护栏 |
| P6 | Phase 7（Telemetry/Portable/Download Resume） | 收进 P6 |

## 里程碑与 Gate

```text
v0.4    P0 完成      全新机器开箱即用（无 Node/源码树依赖）★ 产品化第一关
v0.6    P1 + P4 完成   stop/start 无疤；CI 全绿
v0.7    P2 + P3 + P5   一条 tag 出签名安装包 + 应用内更新
v1.0    Stable        签名 + 自动更新 + 开箱即用 + CI 守护
```

## 第一性验收（呼应「二十」）

$$
\boxed{
\text{全新 Windows 电脑，双击 Setup}
\rightarrow
\text{填一个 Provider}
\rightarrow
\text{点 Launch}
\rightarrow
\text{DSH 真正运行，无需预装 Node / 源码树}
}
$$

这条链一旦打通，才是"产品化"而不是"开发机上能跑的项目"。
