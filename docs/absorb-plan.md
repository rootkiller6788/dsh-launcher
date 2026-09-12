# AHL 强化计划：吸收五个同类启动器的核心机制

> 目标：把同类项目里**被真实使用验证过**的机制，按 AHL 的架构与边界，落到 `crates/launcher-core` / `crates/dsh-adapter` / `apps/desktop`。
>
> 参照对象在 `D:\Opencode\dsh-plugin\`：
> `superlauncher\DSH-Launcher\`（MarcoG-h，Electron+React）、`superlauncher\1\dsh-launcher\`（Ruler4396，WinForms+.NET）、`superlauncher\2\dsh-launcher\`（dsh-plugins，Tauri+Vue）、`superlauncher\3\zat-dsh-launcher\`（mishibeikejie，纯 JS Electron）、`superlauncher\dsh-manager\`（SherlockGougou，Electron+React+TS）。
>
> 本文档中的所有"AHL 现状"结论均经代码核实（grep/读文件），核不到的不写。

---

## 0. 三条判断原则

### 原则一：只吸收有实现可指的机制，不吸收口号

五个项目里，只有两个在文档里明确写了"我们不做什么"：`1/` 的克制声明、本项目的 DSH-first 能力边界。其余三个 README 都偏乐观，其中 `dsh-manager` 已核实存在**文档与实现的落差**（README 称"16+ 项诊断"实际约 13 项；`DshForm` 里声明了 `source-checkout` 形态，但 `detect.ts` 的 `detectDsh` 从未识别它）。

**吸收标准**：能在源码里指出实现位置、且能给出验收口径的机制。纯文案承诺不吸收。

### 原则二：必须服从 AHL 的 DSH-first 能力边界

见 `docs/dsh-first-capability-boundaries.md`：**launcher 负责下载/分类/校验/缓存/诊断增强，永不当"DSH 能否看见/使用资源"的裁决者。**

具体约束：

- 插件/技能/MCP 的启用状态，最终事实源是 DSH（`pluginInventory.list` / `cordis.patch.yml`），不是 AHL 自己的数据库。
- 因此**不吸收**任何"由启动器直接改 `package.json` 的 bundles 数组来启停插件"的做法——`DSH-Launcher` 的历史实现走过这条路，它的 `repairProfile` 正是为了修由此产生的 `invalid plugin (received object)`。
- 新增的"修复"动作必须是**让 DSH 能跑**，而不是**替 DSH 决定加载什么**。
- 反面参照：`dsh-manager` 的 `profiles.ts` 把插件操作**转发给官方 `dsh plugin`**、保留官方 reconcile 语义——这与 AHL 的边界一致，AHL 也已在做（`dsh-adapter/src/lib.rs:605`），属于已覆盖项。

### 原则三：栈不同，移植的是机制不是代码

| 来源 | 栈 | 可复用程度 |
|---|---|---|
| `1/` | C# / .NET 10 | 仅**方法论**（错误码体系、契约清单、安全模式设计、测试粒度） |
| `DSH-Launcher` | Electron + React + TS | 仅**算法与阈值**（自适应超时判定、脱敏正则、泄露检测规则） |
| `2/` | Tauri + Rust + Vue | **逻辑可直译**（同为 Rust，但模块结构不同） |
| `3/zat` | 纯 JS Electron | **规则表可直接搬**（13 类崩溃正则、删除保护范围） |
| `dsh-manager` | Electron + React + TS | **清单与流程可直接搬**（健康检查项、修复动作定义、CI 矩阵） |

**结论**：五家没有一个能"抄代码"。可搬运的是**清单、规则表、阈值、流程编排**，Rust 侧实现全部重写。这不降低价值——`3/zat` 的 13 类崩溃诊断和 `dsh-manager` 的 6 个修复动作，其价值 90% 在"知道要检查什么"，10% 在"怎么检查"。

---

## 1. 现状盘点

### 1.1 AHL 已经有的（不要重复做）

已用 grep 核实，**以下项目不需要从别家吸收**：

| 能力 | 实现位置 | 易被误认为缺失 |
|---|---|---|
| 单实例锁 | `apps/desktop/src-tauri/src/lib.rs:48`（`tauri_plugin_single_instance`） | `optimization-backlog.md` 第 1 条仍写"全仓零命中"——**该条已过时** |
| PID 归属校验 | `crates/launcher-core/src/process.rs:303-341`（`owner` 字段） | backlog 第 2 条已过时 |
| PID 复用防护 | 同上（`created_at` + `GetProcessTimes` / `/proc/stat` field22） | backlog 第 3 条已过时 |
| 跨进程 Ledger 写锁 | 同上（`struct LedgerLock`） | backlog 第 5 条已过时 |
| Windows 隐藏控制台 | `crates/launcher-core/src/process.rs:649-659`（`CREATE_NO_WINDOW`） | zat 的同类能力无需吸收 |
| 进程树强杀 | Windows Job Object（`KILL_ON_JOB_CLOSE`）+ `taskkill /T /F` 兜底 | — |
| **DSH 事件流订阅** | `crates/dsh-adapter/src/events.rs`：`ws://127.0.0.1:{port}/api/events.host`，`settings/document-updated` | ⚠️ **已有，但只订阅了 `ui-theme` / `locale`**。DSH-Launcher 那条是"扩展到生命周期"，不是新增 |
| usage proxy 流式转发 | `apps/desktop/src-tauri/src/usage_proxy.rs` | backlog 第 7/8 条已过时 |
| 多版本运行时 | `crates/dsh-adapter/src/runtimes.rs`（install/verify/repair/remove） | — |
| 凭证库 | `crates/launcher-core/src/provider.rs`（Windows Credential Manager） | — |
| MCP 全生命周期 | `mcp_resolver` / `mcp_prefetch` / `mcp_probe` / `mcp_local` / `mcp_import` | — |
| 转发官方 `dsh plugin` | `crates/dsh-adapter/src/lib.rs:605` | 与 `dsh-manager` 的"保留官方 reconcile 语义"一致，已覆盖 |
| 插件的五源标注视图 | `apps/desktop/src-tauri/src/commands/plugins.rs` | 等价于 `DSH-Launcher` 的"插件×实例矩阵" |
| 持久化任务队列 | `crates/launcher-core/src/jobs.rs`（SQLite `JobStore` + `claim_next`） | — |

> ⚠️ **行动项**：动手前先修 `docs/optimization-backlog.md`，把已完成的条目划掉（第 1/2/3/5/7/8 条）。留着过时的缺口清单会持续误导决策——本计划的 §1.2 已经踩过这个坑，每条都重新核实过。

### 1.2 AHL 确实缺的（已逐条 grep 核实）

| 缺口 | 对应痛点 | 吸收来源 | 核实方式 |
|---|---|---|---|
| DSH 启动用**硬编码超时**（`commands/process.rs:280` 20s、`:359` 240s） | 慢启动被误杀 | `DSH-Launcher` | 读源码 |
| 事件流未用于**生命周期**（仅设置变更） | 状态只能靠猜 stdout | `DSH-Launcher` | 读 `events.rs` |
| 无**CLI 参数兼容性探测** | 传了 dsh 不认的 flag | `3/zat` | `--no-open` 零命中 |
| 无 **token-401 页面识别**（健康页 vs 认证页） | 假就绪 | `3/zat` | 只等 URL 行 |
| 无**安全模式**（插件崩了只能手动救） | 起不来 | `1/` | 零命中 |
| 无**上游契约清单** | 上游改了不知道 | `1/` | `CONTRACT` 零命中 |
| 无**崩溃诊断规则引擎** | 不知道该修什么 | `3/zat` | 零命中 |
| 无**救援点快照/还原** | 改坏了回不去 | `3/zat` | 零命中 |
| 无**健康检查项集合**（`diagnostics.rs` 仅 `check_tool`） | 事前看不见隐患 | `dsh-manager` | 读源码 |
| 无**修复动作库** | 看见了也不会修 | `dsh-manager` | 零命中 |
| 无**DSH_HOME 备份/恢复** | 数据资产无保障 | `dsh-manager` | 零命中 |
| 无**系统服务化**（launchd/systemd） | 关掉管理器就停 | `dsh-manager` | 零命中 |
| 无**日志轮转**（Activity 日志无限增长） | 磁盘被吃 | `dsh-manager` | `rotate` 命中全是 CSS/JS 噪声 |
| 无**会话日志解码**（`session.jsonl.zstd`） | 对话内容看不到 | `3/zat` + `dsh-manager` | 零命中 |
| 无**配置全链路校验**（`--patch --dump-config`） | 改坏了才发现 | `dsh-manager` | 零命中 |
| 无**诊断包导出**（`zip` crate 已在依赖里） | 求助时说不清 | `1/` | 零命中 |
| 无**包完整性校验与回滚**（`bundle.rs` 仅 94 行，无 sha256/rollback） | 装坏了回不去 | `2/` | 读源码 |
| 无**三版本通道**（`market.rs` 只有 `latest` dist-tag） | 拿不到 beta/alpha | `2/` | 读源码 |
| 无**外部实例扫描与采纳** | 用不上已有的 dsh | `2/` | 零命中 |
| 仅一种 DSH_HOME 模式（每实例独立） | 无法复用已有环境 | `2/` | 读源码 |
| 无**核心层冒烟关卡** | CI 只测端到端 | `dsh-manager` | 零命中 |
| P2/P3/P5 未开工 | 出不了安装包/不自更新/未签名 | `dsh-manager` 的 CI 模板 | 读 `TODO.md` |

---

## 2. 五家的"最值钱"清单

每张表三列：机制 → 价值 → AHL 现状与移植方式。"价值"按「解决痛点的彻底程度 ÷ 移植成本」评，星数不表示工作量。

### 2.1 从 `1/`（Ruler4396，C#）吸收 —— 痛点：**失败不可观测**

| 机制 | 价值 | AHL 现状 → 移植方式 |
|---|---|---|
| **两级安全模式**<br>`SafeProfileBuilder`：Tier1 保留 `@deepseek-ai` 核心 / Tier2 Minimal 空 profile | ★★★★★ | 无 → 用 Rust 重建：生成隔离 profile 目录，只保留核心 bundle、剥离第三方。触发条件接启动健康判定 |
| **页面层自检 + 坏签名优先**<br>`BootHealthMonitor` 四个观测点（进程 / 日志 / HTTP / 页面），**坏签名一票判死，好符号才算健康** | ★★★★★ | 只等 URL 行，不等页面真渲染 → AHL 已有 "spawn → URL ready → web ready" 三段（`commands/process.rs:782` 注释），补页面层探测 |
| **上游契约清单**<br>`docs/DSH_CONTRACT_INVENTORY.md` 把依赖的 33 条上游接口逐条登记 | ★★★★★ | **零命中** → 对 AHL 尤其该补：它的 DSH-first 边界本身就是"依赖面最小化"的声明，但没有清单就察觉不到上游变更打断了假设。成本低（一次盘点 + 一份文档） |
| **诊断包导出**<br>`--diagnose` 生成脱敏 zip（env / errors / log 三段） | ★★★★ | `diagnostics.rs` 薄、无导出 → `zip` crate 已在 `Cargo.toml`，成本极低 |
| **结构化错误码**<br>`ErrorCodes.cs`（E1xxx 运行时 / E2xxx 服务 / E4xxx 更新 / E9001 内部），`Describe()` 被弹窗、日志、诊断包三处共用 | ★★★★ | 有"下一步动作"文案、无编号 → 补编号与分类，三处共用同一码 |
| **Outcome Contract 测试**<br>只断言系统最终物理状态，不关心内部调用顺序 | ★★★ | 有 e2e、无此粒度 → 补在 `crates/*/tests/`：如"启动失败后必定存在安全模式入口" |

### 2.2 从 `DSH-Launcher`（MarcoG-h，Electron）吸收 —— 痛点：**冷启动摩擦**

| 机制 | 价值 | AHL 现状 → 移植方式 |
|---|---|---|
| **自适应启动超时**<br>不用固定秒数判死：进程活着且持续有输出 → 判"仍在启动"，每 15s 提示耗时；**120s 无任何输出**才判超时 | ★★★★★ | **硬编码 20s / 240s**（`commands/process.rs:280` / `:359`）→ 直接替换这两个常量。这是全计划性价比最高的一条 |
| **超时自愈**<br>即便被误判，端口真就绪后状态自动恢复 | ★★★★ | 无 → 超时后不销毁句柄，保留 watcher，让晚到的 URL 仍能触发 `finalize_ready` |
| **事件流驱动状态**<br>托盘状态用 `/api/events.host` + `/api/events.mux`，而不是刮 stdout | ★★★★ | ⚠️ **已订阅 `events.host`，但仅 `ui-theme` / `locale`**（`events.rs`）→ 扩到生命周期信号 + 补 `events.mux`。注意 apiproxy 可能比端口晚就绪，它的重连退避写法可直接照搬 |
| **launchToken 日志脱敏**<br>正则 `([?&](?:token\|launchToken)=)[^&\s"']+` → `***` | ★★★★ | token 存在，脱敏待核 → 改动极小。AHL 的日志会进 Activity 面板和 `logs/launcher.log`，暴露面比同类更大 |
| **泄露检测规则**<br>`securityRisk`：必须**同时**满足"含 credential 标记 + type 是 tool/call \| result + actor 不在官方工具集 + 不在白名单"，分红/橙/黄三级 | ★★★ | 有 keyring、无运行时检测 → 规则表可直接搬。保持它的**保守判定**：宁可漏报不可误报 |
| **内置说明问答**<br>Q&A 入口紧贴功能标题 | ★★ | 无 → 纯 UI，放 UI polish pass |

### 2.3 从 `2/`（dsh-plugins，Tauri 2 + Rust）吸收 —— 痛点：**环境耦合**

| 机制 | 价值 | AHL 现状 → 移植方式 |
|---|---|---|
| **三种 DSH_HOME 模式**<br>复用现有 / 自动采纳 `~/.dsh` / 每实例独立 | ★★★★★ | 仅"每实例独立" → `instance.rs` 的 manifest 加 mode 枚举。让用户能接入已有环境而不必重建 |
| **包完整性与回滚**<br>`modpack.rs`：manifest v2–v5 + `.dspack` 打包 + sha256 校验 + 回滚 | ★★★★ | `bundle.rs` **仅 94 行，无 sha256 / 无 rollback** → AHL 有 `Bundle{...}` 任务类型但缺完整性保障。补校验和失败回滚，与 §2.4 的救援点快照共用同一套"改动前先留退路"机制 |
| **三版本通道**<br>`PluginChannel` stable（releases / npm `latest`）/ beta（pre-release / `next`）/ alpha（GitHub 最新 commit），另有 `PluginSource` 双市场 | ★★★★ | `market.rs` **只有 `latest` dist-tag** → 补 dist-tag 与 GitHub commit 两种通道。alpha 通道尤其重要——很多 dsh 插件只在 GitHub 上发预发布 |
| **外部实例扫描与采纳**<br>`scan_local_dsh`（`~/.dsh*` + `DSH_HOME` 环境变量）+ `probe_external_port` TCP 探测 | ★★★★ | 无 → 与第一条同批做，逻辑相邻 |
| **CLI 特性探测**<br>`--no-open` 不比对版本号，而是**扫描已装代码里有无该 flag 字面量** | ★★★ | **零命中** → 比版本比对可靠得多（预发布版本号不可比）。与 `3/zat` 的 cli-probe 是同一思路的两种实现，取长补短 |
| **每 profile 操作串行化**<br>profile 级锁，防并发安装互踩 | ★★★ | 有 `JobStore.claim_next` 原子 FIFO → 基本覆盖，可跳过 |
| **deep-link 协议**<br>`dsh-launcher://launch` / `pack` | ★★ | 无 → Tauri 有 `deep-link` 插件，成本低但优先级低 |
| **PTY 内嵌终端 + TUI 会话**<br>portable-pty + xterm | ★★ | 无 → ⚠️ **建议不吸收**：终端是旁路，与 DSH-first 边界冲突；AHL 已有 Activity 面板 |

### 2.4 从 `3/zat`（mishibeikejie，纯 JS Electron）吸收 —— 痛点：**多实例污染 + 崩溃自救**

| 机制 | 价值 | AHL 现状 → 移植方式 |
|---|---|---|
| **崩溃诊断规则引擎**<br>`rescue.js` 的 `diagnoseCrash`：13+ 类正则（missing-bundle / plugin-failed / bad-profile / missing-module / source-deps / native-deps / client-module-missing / bundle-mismatch / source-mixed / duplicate-plugin / tool-missing / cli-arg / cli-error），**每条映射真实的 GitHub issue 编号**（#880 / #1677 / #2130 / #2990 / #3263 / #2889） | ★★★★★ | 无 → **规则表直接搬**。本计划最值钱的一条。注意它"每条对应真实 issue"的做法——这是规则可信度的来源，不要写成拍脑袋的匹配 |
| **救援点快照 / 还原**<br>`createRescueSnapshot`：快照 profile 的 `cordis.yml` / `cordis.patch.yml` / `package.json` / `pnpm-workspace.yaml` | ★★★★★ | 无 → 成本极低（4 个文件 + 时间戳目录），收益极高。应成为所有破坏性动作的**强制前置步骤** |
| **三级恢复阶梯**<br>L1 对症 → L2 完整恢复 → L3 工厂重置（保留引擎注册） | ★★★★ | 无 → 与 `1/` 的安全模式**合并设计为一条阶梯**，不要做两套 |
| **会话日志实时逆解析**<br>`session-activity.js`：zstd 逐帧 + `fromByte` 回退 64KB 找帧魔数 `28 B5 2F FD`，流式碎片聚合（text-chunks / tool-call-chunks） | ★★★★ | 无 → Rust 侧有 zstd crate，比 JS 更好做。另可调 DSH 的 `/api/session.list` 拿权威标题 |
| **CLI 参数兼容性探测**<br>`cli-probe.js`：启动前先探明这版 dsh 认不认某个 flag | ★★★★ | **零命中** → 与 §2.3 的 `--no-open` 字面量扫描合并成一套"启动前探明能力"的机制。AHL 会向 dsh 传 flag，传错就是启动失败 |
| **token-401 页识别**<br>`terminal-supervisor.js` 的 `check()`：TCP → HTTP → cmdline → 归属四级递进，能识别"token 失效的 401 页"而不是当成健康页 | ★★★★ | 只等 URL 行 → 直接进 §2.1 的页面层自检签名表。这是"假就绪"最典型的形态 |
| **工具链自举**<br>node / pnpm / npm / git 自举到用户目录，绝不摸系统工具 | ★★★ | 内置 node，pnpm / git 待核 → AHL 已有受管 `runtimes/`，补 pnpm / git 即可 |
| **删除安全规划**<br>`planTerminalDeletion`：保护 `~/.dsh`、主目录一级、盘根、其他实例的共享/嵌套路径 | ★★★ | 有 `sanitize_mcp_segment`，删除保护待核 → 删除前先算"删除根范围"，返回 blocked / roots 结构 |
| ~~copy 模式物理隔离~~ | — | ⚠️ **不吸收**，见 §3 |
| ~~隐藏控制台~~ | — | 已有 `CREATE_NO_WINDOW`（`process.rs:649-659`） |

### 2.5 从 `dsh-manager`（SherlockGougou，Electron+React+TS）吸收 —— 痛点：**机器级运维缺位**

| 机制 | 价值 | AHL 现状 → 移植方式 |
|---|---|---|
| **健康检查项集合**<br>约 13 项分 6 组：运行时环境（node/pnpm/dsh 版本、磁盘）/ Home（存在可写、凭据权限位、YAML 可解析）/ Profile（bundle 声明 vs 实装一致性）/ 运行态（端口）/ 数据文件（会话日志总量）/ 管理器（最近备份）。每项输出 `status + detail + fixHint + 可选 repair 动作` | ★★★★★ | `diagnostics.rs` **仅 `check_tool`** → 清单直接搬。全部只读、风险极低。`fixHint → repair 动作`的关联设计要一起搬——诊断必须能导向修复 |
| **修复动作库**<br>6 个：`fix-permissions`（只收紧不放开）/ `restore-yaml-from-bak` / `pnpm-install-profile` / `repair-session-log`（截断到最后一个完整 zstd 帧）/ `clean-cache` / `add-allowbuilds` | ★★★★★ | 无 → 每个动作统一"确认 → 快照 → 执行 → 报告"。**`add-allowbuilds` 对 AHL 尤其重要**——pnpm ≥10 拦构建会同时打击 AHL 现有的 MCP / skill 安装路径 |
| **备份与恢复**<br>全量 cp + `manifest.json`；`restorePreview` dry-run 给 toAdd/toOverwrite/toDelete/unchanged；恢复前把现状移入 `restore-trash/<ts>`；保留策略 | ★★★★★ | 无 → 全量搬运。默认排除 `.credentials.yaml` / `.env` / `node_modules` / `cache` |
| **系统服务化**<br>launchd LaunchAgent（`KeepAlive=true`）/ systemd user unit（`Restart=always`）/ Windows 启动文件夹，**独立于管理器进程** | ★★★★ | 无 → 让实例在管理器关闭后继续常驻 |
| **配置编辑器安全栈**<br>凭据掩码 + 写前 `.bak-<ts>` + 同目录 tmp→rename 原子写（防 DSH 热重载读到半截）+ `dsh --profile X --patch tmp --dump-config` 全链路校验 + LCS diff | ★★★★ | 有编辑、无全链路校验 → 这条最关键：AHL 的目录清单是 `include_str!` 内嵌的，更需要"写进去之前先让 dsh 自己验一遍" |
| **核心层冒烟关卡**<br>`core:smoke`：核心层不 import electron，用 tsx 直接驱动，CI 里以空 DSH_HOME 跑 | ★★★★ | 有 e2e、无此关卡 → AHL 的 `launcher-core` 天生框架无关（无 tauri 依赖），加这个关卡成本极低 |
| **日志轮转**<br>实例日志采集 + 轮转 | ★★★ | **无**（`rotate` 命中全是 CSS/JS 噪声）→ AHL 的 Activity 日志与 `logs/launcher.log` 无上限增长。小改动，防磁盘被吃 |
| **CI 三平台矩阵**<br>macos/windows/ubuntu + 签名公证按 secret 有无分支回退 + 空 home 场景 | ★★★★ | P2 未开工 → 借结构：`typecheck → core:smoke → build → 打包 → publish` |
| **更新检查双渠道**<br>npm + PyPI 直连 registry | ★★★ | 有 MCP 侧的 uv pip，无工具自身更新检查 → 与 Phase 5 的自动更新合并做 |
| ~~转发官方 `dsh plugin`~~ | — | 已在做（`dsh-adapter/src/lib.rs:605`），与它的"保留官方 reconcile 语义"一致 |

---

## 3. 明确不吸收的清单

不吸收同样需要理由，否则下次review还会重新讨论一遍。

| 不吸收 | 原因 |
|---|---|
| **copy 模式物理隔离**<br>`3/zat` 的 `--config.package-import-method=copy` | **与 AHL 的设计直接冲突，是取舍不是缺口**。AHL 的模型是「runtimes 按版本共享 + 实例只在 DSH_HOME 层隔离」，且 `runtimes.rs` 用 `robocopy /E /SL` **故意保留** pnpm 的 junction 森林以保证运行时自包含。照抄 zat 会破坏这个自包含性。**但要写进用户文档**——"删一个实例会不会影响另一个"是用户一定会问的问题 |
| **WSL2 桥接**<br>`2/` 的实现 | AHL 是 Windows-first；`2/` 的实现涉及 UNC 路径映射、tarball 经 stdin 流入 distro、`DSH_PID` 标记防孤儿，成本高、收益窄 |
| **隐藏控制台** | 已有 `CREATE_NO_WINDOW`（`process.rs:649-659`） |
| **单实例锁** | 已有 `tauri-plugin-single-instance`（`lib.rs:48`） |
| **插件×实例矩阵** | AHL 的 Library 混合视图五源标注已覆盖同等信息 |
| **PTY 内嵌终端**<br>`2/` 的 portable-pty + xterm | 旁路能力，与 DSH-first 边界冲突；AHL 已有 Activity 面板 |
| **悬浮球 / 开屏动画 / 托盘三态灯** | 与 AHL「生态管理平台」的定位不符，那是消费级产品的语言 |
| **直接改 `package.json` bundles 数组启停插件** | 违反 DSH-first 边界。`DSH-Launcher` 走过这条路，其 `repairProfile` 就是为修此而生 |
| **未知来源的 MCP 安装命令** | AHL 已有的铁律不能松：LLM 只返回严格 JSON `{kind,entry,args,env}`，且 entry 走组件级路径校验（`resolve_inside`），超出即丢 |

---

## 4. 分阶段计划

排序依据：**（价值 ÷ 成本）× 是否解除其他工作的阻塞**。每阶段内的条目按依赖顺序排列。

### Phase 0 — 地基核对（先做，否则后续基于错误输入排期）

| # | 任务 | 来源 | 验收 |
|---|---|---|---|
| 0.1 | 更新 `optimization-backlog.md`，划掉已完成的第 1/2/3/5/7/8 条 | 自查 | 清单与代码一致，不再误导决策 |
| 0.2 | 加 `core:smoke` 式关卡：无 GUI 直驱 `launcher-core` 冒烟，进 CI | `dsh-manager` | 一条命令跑通核心层，不启动窗口 |
| 0.3 | 统一错误编号 + 分类，每条带"下一步动作" | `1/` | 弹窗 / Activity 日志 / 诊断包三处共用同一码 |
| 0.4 | 核实并补 launchToken 日志脱敏 | `DSH-Launcher` | 日志里 token 显示为 `***` |
| 0.5 | **上游契约清单**：把 AHL 依赖的 dsh 接口逐条登记（`pluginInventory.list`、`/api/events.host`、`cordis.patch.yml` 语义、CLI flag、DSH_HOME 布局……） | `1/` | 上游任一改动能对照清单判断影响了哪条吸收项 |

**为什么先做**：0.1 消除错误输入（本计划已踩过这个坑）；0.2 让后续每条改动都有低成本回归网；0.3 是 Phase 2 诊断输出的前提；0.5 是一次性盘点，之后每次跟进 dsh 新版本都靠它。

### Phase 1 — 启动可靠性（最高优先，直接对症现有硬编码超时）

| # | 任务 | 来源 | 涉及文件 |
|---|---|---|---|
| 1.1 | **自适应启动超时**：进程活着 + 持续有输出 → 不判死；每 15s 提示耗时；N 秒无输出才超时 | `DSH-Launcher` | `commands/process.rs:280`、`:359` |
| 1.2 | 超时自愈：晚到的 URL 仍能触发 `finalize_ready` | `DSH-Launcher` | `commands/process.rs` |
| 1.3 | **启动前能力探明**：合并 `2/` 的"扫代码字面量"与 `3/zat` 的 cli-probe，启动前判定这版 dsh 认不认某个 flag | `2/` + `3/zat` | `dsh-adapter` |
| 1.4 | 页面层自检：坏签名一票判死、好符号算健康、`Rendered` 豁免；**签名表含 token-401 页** | `1/` + `3/zat` | `dsh-adapter` |
| 1.5 | 事件流驱动生命周期：把已有的 `events.host` 订阅从设置扩展到生命周期，补 `events.mux` | `DSH-Launcher` | `events.rs` |
| 1.6 | 两级安全模式（Tier1 保核心 / Tier2 Minimal） | `1/` | `launcher-core` + `dsh-adapter` |

**验收 1.1**：一个装了 100+ 插件的实例冷启动（真实耗时 > 240s）不再被误判失败；一个真卡死的实例在无输出 N 秒内被判定。
**验收 1.4**：token 失效时不再报"启动成功"。
**验收 1.6**：装一个会让页面崩的插件后，实例仍能通过安全模式启动到可用。

### Phase 2 — 崩溃自救与运维（把"坏了怎么办"补齐）

| # | 任务 | 来源 |
|---|---|---|
| 2.1 | 崩溃诊断规则引擎（搬 13 类规则表，每条标注对应的真实 issue） | `3/zat` |
| 2.2 | 救援点快照 / 还原（4 个关键文件 + 时间戳目录） | `3/zat` |
| 2.3 | 三级恢复阶梯（L1 对症 / L2 完整恢复 / L3 工厂重置）—— 与 1.6 的安全模式**合并为一条阶梯** | `3/zat` + `1/` |
| 2.4 | 健康检查项集合（6 组约 13 项，只读，输出 `status + detail + fixHint + repair`） | `dsh-manager` |
| 2.5 | 修复动作库（6 个，统一"确认 → 快照 → 执行 → 报告"） | `dsh-manager` |
| 2.6 | 诊断包导出（脱敏 zip：env / errors / log） | `1/` |

**交叉收益**：2.5 的 `add-allowbuilds` 同时修复 AHL 现有的 MCP / skill 安装失败路径；2.2 的快照机制应成为 2.5 所有破坏性动作以及 §2.3 包回滚的**前置强制步骤**——一处实现，三处受益。

### Phase 3 — 数据与常驻（用户资产保障）

| # | 任务 | 来源 |
|---|---|---|
| 3.1 | DSH_HOME 备份 / 恢复（manifest + dry-run 预览 + restore-trash + 保留策略） | `dsh-manager` |
| 3.2 | 系统服务化（launchd / systemd / Windows 启动项） | `dsh-manager` |
| 3.3 | 会话日志解码 / 导出（zstd 多帧 + torn-tail 检测 + Markdown/JSONL 导出） | `3/zat` + `dsh-manager` |
| 3.4 | 配置编辑器安全栈（掩码 + `.bak` + 原子写 + `--patch --dump-config` 全链路校验 + diff） | `dsh-manager` |
| 3.5 | 日志轮转（Activity 与 `logs/launcher.log` 加上限与按大小切分） | `dsh-manager` |

### Phase 4 — 环境模型与生态

| # | 任务 | 来源 |
|---|---|---|
| 4.1 | 三种 DSH_HOME 模式（复用 / 自动采纳 `~/.dsh` / 每实例独立） | `2/` |
| 4.2 | 外部实例扫描与采纳（`scan_local_dsh` + TCP 探测） | `2/` |
| 4.3 | **包完整性与回滚**：`bundle.rs` 补 sha256 校验 + 失败回滚（复用 2.2 的快照机制） | `2/` |
| 4.4 | **三版本通道**：`market.rs` 补 beta（`next` dist-tag）与 alpha（GitHub 最新 commit） | `2/` |
| 4.5 | 工具链自举补齐 pnpm / git | `3/zat` |
| 4.6 | 删除安全规划（保护 `~/.dsh`、主目录一级、盘根、实例间共享路径） | `3/zat` |

### Phase 5 — 发布工程（AHL 自己的 P2 / P3 / P5）

这一阶段不是"吸收"，是 AHL roadmap 上的必做项，但**结构可以借 `dsh-manager` 的 CI**：

| # | 任务 | 借什么 |
|---|---|---|
| 5.1 | CI 三平台矩阵 + `typecheck → core:smoke → build → 打包 → 发布` | `dsh-manager` 的 `release.yml` |
| 5.2 | 签名 + 公证（有 secret 则签，无则回退 unsigned） | `dsh-manager` 的分支回退写法 |
| 5.3 | 应用内自动更新 + npm/PyPI 更新检查 | `2/` + `DSH-Launcher` |
| 5.4 | 一条 tag 出安装包（v0.6 里程碑） | — |

---

## 5. 风险与取舍

| 风险 | 说明 | 对策 |
|---|---|---|
| **功能膨胀导致 DSH-first 边界失守** | 吸收"修复 / 备份 / 服务化"很容易滑向"启动器替 DSH 做决定" | 每条修复动作过一遍 §0 原则二；启停插件永远走 DSH 原生路径（`dsh plugin` / `cordis.patch.yml`） |
| **五家都不做的，我们也不做** | 五家都没碰的领域（MCP 深度、用量账本、凭证库）正是 AHL 的差异化 | 本计划只补短板。Phase 2/3 完成后应回头继续推进 MCP Phase 4 的真机验证，不要一路补短板补到失去长板 |
| **照搬别家的 bug** | `dsh-manager` 有文档/实现落差，`DSH-Launcher` 无测试 | 只吸收能在源码里指出实现位置的机制；每条吸收项自带验收口径 |
| **`3/zat` 的崩溃规则会腐坏** | 那 13 类规则绑定 dsh 0.6.x~1.5.7 的具体报错文本 | 规则表要可外部覆盖（沿用 AHL 已有的 `DSH_BOOT_SIGNATURES` 式做法），不硬编码进二进制 |
| **安全模式的误触发** | 频繁误判会让用户失去信任 | 照 `1/` 的做法：每会话只询问一次；重启窗口期屏蔽误报 |
| **备份的凭据泄露** | 备份文件可能被同步到云盘 | 默认排除 `.credentials.yaml` / `.env`；UI 里显式说明 |
| **吸收项之间互相依赖被忽略** | 包回滚（4.3）、修复动作（2.5）、救援点（2.2）都需要"改动前留退路" | 2.2 必须先落地，成为其余破坏性操作的公共前置 |

---

## 6. 与现有 TODO 的关系

| 现有条目 | 本计划的影响 |
|---|---|
| P2（CI 发布流水线） | 由 Phase 5.1 承接，借 `dsh-manager` 结构 |
| P3（自动更新） | 由 Phase 5.3 承接 |
| P4（测试补齐：E2E tauri-driver、页面性能 gate、大实例压测、端到端验收脚本） | **Phase 0.2 的核心层冒烟是它的前置**，先做 |
| P5（代码签名） | 由 Phase 5.2 承接 |
| P6（crash / telemetry / resume / portable） | 已完成，不动 |
| DSH-first 阶段 11（页面切换 gate、大实例压测）、阶段 13（验收脚本、UI polish、发布衔接） | 与本计划 Phase 0 / 1 / 5 部分重叠，需合并排期避免重复劳动 |
| `docs/optimization-backlog.md` 27 条 | **先执行 Phase 0.1 校准**，剩余条目按本计划优先级重排 |
