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
| **诊断包导出**<br>`--diagnose` 生成脱敏 zip（env / errors / log 三段） | ★★★★ | `diagnostics.rs` 薄、无导出 → **2026-09-12 已落地**为六段包 + 失败启动自动落盘（`commands/diagnose.rs`）。原计划的"三段"不够：错误摘要是给收件人看的，脱敏规则得写在包里，见 Phase 2 实施记录 |
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
| **三级恢复阶梯**<br>L1 对症 → L2 完整恢复 → L3 工厂重置（保留引擎注册） | ★★★★ | **L1 / L2 已存在**（`BootRecovery.tsx` + `rescue_restore`）→ 与 `1/` 的安全模式合并为一条阶梯的设计不变，但 **L3 不做**：它替 DSH 重写 profile 且自动触发无确认，违反 §0 原则二，见 §3 与 Phase 2 实施记录 |
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
| **健康检查项集合**<br>约 13 项分 6 组：运行时环境（node/pnpm/dsh 版本、磁盘）/ Home（存在可写、凭据权限位、YAML 可解析）/ Profile（bundle 声明 vs 实装一致性）/ 运行态（端口）/ 数据文件（会话日志总量）/ 管理器（最近备份）。每项输出 `status + detail + fixHint + 可选 repair 动作` | ★★★★★ | `diagnostics.rs` **仅 `check_tool`** → **2026-09-12 已落地为 5 组 12 项**（`health.rs`）。⚠️ **清单不能直接搬**：磁盘 / 权限位 / clean-cache / repair-session-log 四项经逐项核对是桩或坏实现，已弃用，详见 Phase 2 实施记录。`fixHint → repair` 的关联设计搬了，但只导向 AHL 已有的命令 |
| **修复动作库**<br>6 个：`fix-permissions`（只收紧不放开）/ `restore-yaml-from-bak` / `pnpm-install-profile` / `repair-session-log`（截断到最后一个完整 zstd 帧）/ `clean-cache` / `add-allowbuilds` | ★★★ | 无 → **2026-09-12 核后只留 1 个并已落地**：`add-allowbuilds`（键名与目标文件已核实，见 Phase 2 实施记录）。其余 5 个各有明确理由不搬。**"确认 → 快照 → 执行 → 报告"这个形状照搬**；它要求 `pnpm-workspace.yaml` 进救援点集合，这一前提已一并处理（`rescue.rs`） |
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
| **L3 工厂重置**<br>`3/zat` 的 `factoryResetProfile` | **同上一条，是它的加强版**。它不只启停插件，而是**替 DSH 重写整个 profile 该长什么样**：`package.json` 的 `dsh.profile.bundles` 被改写成 `['@deepseek-ai/dsh-base','@deepseek-ai/dsh-web-app', …引擎]`，`cordis.yml` / `cordis.patch.yml` 被直接写成 `[]`——patch 里那些 `mcp-<name>` 行是 DSH 的实装事实，清空它 = 启动器单方面宣布"这些不存在"。**而且它是自动的、无确认的**：`main.js:1789` 在崩溃处理里直接 `tryLevel(currentLevel + 1)`，L1、L2 失败后的第三次崩溃就走到 L3，用户只看到一行日志；UI 里既没有入口也没有确认弹窗。公平地说，它**确实先备份** `RESCUE_FILES` 到 `factory-backups/<terminalId>/<ts>`；但 AHL 的 L2（`rescue_restore`）在要紧的那一维上更强——它还原的是**一次真的启动成功过的状态**，而不是当场发明一个。**决策（2026-09-12）：不做，只记录**，详见 Phase 2 实施记录 |
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
| 1.4 | ~~页面层自检：坏签名一票判死、好符号算健康、`Rendered` 豁免；签名表含 token-401 页~~ **已落地**，但取的是签名表里唯一能测的那一条（token-401 页），**DOM 签名表本身未接线**（原因见实施记录） | `1/` + `3/zat` | `dsh-adapter` |
| 1.5 | 事件流驱动生命周期：把已有的 `events.host` 订阅从设置扩展到生命周期，补 `events.mux` | `DSH-Launcher` | `events.rs` |
| 1.6 | 两级安全模式（Tier1 保核心 / Tier2 Minimal） | `1/` | `launcher-core` + `dsh-adapter` |

**验收 1.1**：一个装了 100+ 插件的实例冷启动（真实耗时 > 240s）不再被误判失败；一个真卡死的实例在无输出 N 秒内被判定。
**验收 1.4**：token 失效时不再报"启动成功"。（**已达成**，见下方实施记录）
**验收 1.6**：装一个会让页面崩的插件后，实例仍能通过安全模式启动到可用。

#### Phase 1 实施记录（2026-09-12）

**1.4 已落地**（`702295d` `589057a` `9a1b03c` `ed9c2c5` `122ea43`），但**落地形态与计划写的不一样**，
先说清楚变了什么、为什么，再说丢了什么。

**原定义不可实现的那一半**。计划写"坏签名一票判死、好符号算健康、`Rendered` 豁免"，即照 `1/` 的
`EvaluatePageProbe` 把探针脚本注入页面、读 DOM。这个机制在 AHL 上**没有实现可指**：探针必须跑在
**渲染 dsh 的那个页面里**，而 AHL 把 dsh 渲染在**跨域 iframe**（`apps/desktop/src/App.tsx` 的
`<iframe src={dshUrl}>`）——Rust 侧 `eval` 只能进启动器自己的顶层窗口，够不到 iframe 的 DOM。
AHL 确实拥有一个能 eval 的 dsh 页面：`process.rs::open_dsh_external` 开的那个 dsh 窗口；但它是
**用户手动开的逃生窗口**，不是启动路径，那里的判定没有任何人消费。按原则一（只吸收有实现可指的机制）
与 2.4 的同一条纪律（量不了就不做检查），这一半**不做**，`crates/dsh-adapter/src/page_signature.rs`
（254 行、9 单测）保留并**在模块头标注为"无调用点、主路径够不到"**，不删——它仍是"启动器哪天在
主路径上自己拥有 dsh webview"时的现成探针。

**留下的那一半，量法换了**。签名表里唯一不需要 DOM 就能测的是 **token-401 页**，而它恰好是验收
1.4 要的那个形态（"假就绪"最典型的样子）。它测的是 **URL 本身**，不是页面内容：

| 量法 | 结论 |
|---|---|
| `GET <ready URL>`（**不跟随重定向**）→ **303** | 交换成功：dsh 认这个 token，并把 `dsh-auth-…` 会话 cookie 交给窗口 |
| 同上 → **200** | 根路径直接提供页面（这版 dsh 没有浏览器鉴权） |
| 同上 → **401** + `dsh web authentication required` | **拒绝**：URL 里的 token 不被接受 |
| 其它一切（500 / 403 / 超时 / 连接被拒 / 401 但措辞不同） | `Unreadable`，**fail-open**，不算故障 |

这条判定不是猜的，是**读本机已装的 dsh 得出的**（`@deepseek-ai/dsh-client-connection` 0.1.5-rc.2
`lib/index.js:386` 的 `authorizeIndex` / `:442` 的 `writeUnauthorized`）——**原计划里"需要一次真机
确认（token-401 页的状态码形态）"这一步因此不需要做了**，那条卡点是"没读源码"。两个坑也是读出来的：
① 必须**不跟随重定向**，因为 303 本身就是答案，跟随会去取一个**不带 cookie 的 `/`**，而 dsh 连它
也拒——那会在健康启动上**造出假故障**；② 只有 `401` **且正文含 dsh 自己那句话**才算判定，因为启动器
没有资格替别的服务器/别的 dsh 版本宣布故障。落地在 `crates/dsh-adapter/src/web_check.rs`。

**验收 1.4 对照**："token 失效时不再报'启动成功'"落到代码上是三件事：`finalize_ready` 不再无条件
emit 那行 ready 日志；401 时改发 `emit_warn`（带 dsh 原文与状态码）+ 一条 `launch-diagnosis`
（新 stage `refused`，新 `CrashKind::WebAuthRefused`，新 `FixAction::ReopenUrl`），UI 用**琥珀色**
而不是红色显示——**没有任何东西失败**，harness 在跑、在服务，被拒的只是它自己打印的 URL；进程
**保持 Running**：重启不会改变"它的服务器认哪个 token"，而为此停掉一个能用的 harness 是过度反应
（用户浏览器里还有效的会话 cookie 可能本来就能打开那个 URL——所以文案说的是"dsh 拒绝了这个 URL"，
不是"工作区打不开"）。

**2.2 那条已知局限已兑现**：`refresh_rescue_point` 从"端口在收"移到了这条 URL 判定之后；**被拒时
不刷新**（拒绝意味着那个 URL 下的工作区从未可用，而救生点的定义是"值得回去的状态"），`Unreadable`
**仍然刷新**（读不到答案不等于有故障，不能拿盲区当证据）。

**明确丢了什么**（写在这里，别让下一个人以为页面层已经被覆盖）：能测到的只有"可答性 + token
被接受"。**client bundle 坏掉但仍然能服务根路径**、**致命面板**这类失败**仍然测不到**——AHL 对 dsh
页面没有 DOM 访问。这一类里 `failed to import loader entry` 本来就有 stdout 版本（`crash.rs` 规则 7），
所以它也一直是能诊断的；DOM 独有的那几个标记，实测只有 `bootstrap facade is missing` 真存在于
dsh 前端 bundle（`@deepseek-ai/dsh-web-frontend/dist/assets/index-*.js`），另两个
（`plugin fatal` / `dsh-boot-failed`）在装好的 dsh 里**搜不到**，是当初照搬 `1/` 时带进来的字符串，
其可靠性从未被上游证实过——这也是"DOM 探针不值得为了它接线"的一个侧面证据。

**解锁了什么**：1.6（两级安全模式）原本卡在"没有页面层信号"，现在有了这条 URL 判定（残余盲区见上），
2.3 的编排也随之可做。

**测试面见 `docs/testing.md`**（同日实测基线）：本轮新增 9 条 `web_check` 单测（本地双连接服务器证明
303 不被跟随），`page_signature` 的 9 条保留计入通过数但无调用方；该文件同时记下 1.4 的验证边界——
测的是「根路径可达 + token 被接受」，不是页面真渲染。

### Phase 2 — 崩溃自救与运维（把"坏了怎么办"补齐）

| # | 任务 | 来源 |
|---|---|---|
| 2.1 | 崩溃诊断规则引擎（搬 13 类规则表，每条标注对应的真实 issue） | `3/zat` |
| 2.2 | 救援点快照 / 还原（关键 profile 文件 + 时间戳目录） | `3/zat` |
| 2.3 | 三级恢复阶梯（L1 对症 / L2 完整恢复 / L3 工厂重置）—— 与 1.6 的安全模式**合并为一条阶梯** | `3/zat` + `1/` |
| 2.4 | 健康检查项集合（**已落地为 5 组 12 项**，只读，输出 `status + detail + fixes[]`） | `dsh-manager` |
| 2.5 | 修复动作库（**已落地，1 个：`add-allowbuilds`**，统一"确认 → 快照 → 执行 → 报告"） | `dsh-manager` |
| 2.6 | 诊断包导出（脱敏 zip：env / errors / log）—— **已落地**为六段包，含失败启动自动落盘 | `1/` |

**交叉收益**：2.5 的 `add-allowbuilds` 修复的是 **git 源安装**这一条路径（核实后缩窄，
原写"MCP / skill 安装失败路径"过宽，见 Phase 2 实施记录）；2.2 的快照机制应成为 2.5 所有
破坏性动作以及 §2.3 包回滚的**前置强制步骤**——一处实现，三处受益。

#### Phase 2 实施记录（2026-09-12）

**2.1 + 2.2 已落地**（`0276af1` `9c3a4b0` `98544f3` `80d6d2f` `e673476`），两件事一次做完，
因为它们本就是一条链路：起不来**说得出原因**，且**一键退回**改动前的状态。

- 规则引擎在 `crates/dsh-adapter/src/crash.rs`：14 条有序规则 → `CrashKind` + `FixAction`，
  每条对应真实 issue（#880 / #1677 / #2130 / #2990 / #3263 / #2889）。**手写子串匹配，不引入
  `regex`**，沿用 `launcher-core/src/redact.rs` 的既有风格。fail-open：日志不认识就返回空，
  绝不误报。按 `(kind, plugin)` 去重。
- 救援点在 `crates/dsh-adapter/src/rescue.rs`。**AHL 实际要保的是 3 个文件**，不是 zat 的 4 个：
  `profiles/<profile>/package.json`、`profiles/<profile>/cordis.patch.yml`、
  `$DSH_HOME/cordis.patch.yml`。zat 另外两个（`cordis.yml`、`pnpm-workspace.yaml`）已核实
  在 AHL 所跑的源码 checkout `dsh web` 下不存在（见 `dsh-contract-inventory.md` 附一）。
- **两个钩子是刻意不对称的**，这是 2.2 的关键设计：`refresh_rescue_point` 在**一次成功启动之后**
  跑（刚跑起来的文件才是"已知可用"），`reserve_rescue_point` 在**破坏性操作之前**跑且**仅在
  尚无救援点时**写入。若前置钩子也覆盖写，那么"一次启动都没成功就做第二次改动"会把已经损坏的
  文件存成快照，把唯一的好副本毁掉——正是最需要它的时刻。回归测试锁死了这一点。
- 前置钩子挂在 `jobs.rs` 的 `dispatch_plan` 上——所有 job 型破坏性动作（plugin/skill/mcp/market/
  bundle/environment 的装、更新、导入）的**唯一收口**。放在这里而不是 9 个 `*_job` 体内，
  新增 `JobPlan` 变体就不可能漏掉。`rescue_restore` 要求实例先停止（跑着恢复会被 harness 下次
  写配置时覆盖掉）。
- 崩溃诊断的日志尾巴 `LogTail`（400 行，**捕获时即脱敏**）：`emit_log_at` 本就脱敏，而带诊断
  的事件是唯一可能绕过它的出口，所以在入口处就洗掉，插件名/摘录天然安全。

**已知局限**（接线时已在调用点注明）：refresh 触发的条件是"服务器起来了 + 端口在收"，
这是目前能拿到的最强信号，但**不证明页面渲染成功**——client bundle 坏掉时服务器照样能服务。
1.4 的页面自检落地后，这个 refresh 应移到该检查之后。

**2.1 待补 → 已补（2026-09-12）**：§5 风险里写的"崩溃规则可外部覆盖"当时标注为**未实现**
（原文说"沿用 AHL 已有的 `DSH_BOOT_SIGNATURES` 式做法"——该机制在本仓库中并不存在，属当时的
目标设想而非现状）。现已落地为 `<root>/crash-signatures.json`：

- 文档形态：裸 JSON 数组，或带 `signatures` 数组的对象（手写友好，两种都收）。
- 条目形态：`{kind, fix, contains[], capture, message}` 的**扁平合取式**——`contains` 全部
  按序出现才命中，`capture` 决定把哪个 token 读作插件名（`none` / `quoted` / `bare`），
  `message` 里的 `{name}` 被替换。刻意不做模式语言：内建规则需要的花活（拆插件列表、
  `.node` 后缀、tsx 源依赖判定）不是合理的数据格式能表达的，外部覆盖只负责最常见的
  "dsh 换了措辞"。
- **内建优先**：内建规则已识别的行不交给外部签名，所以数据文件只能**补充**诊断，无法遮蔽
  出厂行为。为此把原第 14 条兜底（`error:` 开头）从特定规则表里**拆了出去**——它几乎能吃掉
  所有 `error:` 行，若留在表内会正好吞掉外部签名存在的理由（dsh 改措辞后新出现的 `error:` 行）。
- 容错：缺文件 / 读不出 / JSON 坏 → 空签名表（**不能**让"诊断本身失败"取代诊断）；数组里
  单条解析失败只损失该条；`contains` 为空的条目被拒（否则它会命中每一行，包括健康启动）。
- 读取时机：每次诊断时重读（`process.rs::diagnose_and_emit`），因此**为这次崩溃新加的签名
  下一次启动就生效**，不必重启启动器。
- 测试：`crash.rs` 新增 13 项（合取有序性、三种 capture、内建优先、兜底让位、去重、
  空 `contains` 拒收、两种文档形态、坏条目跳过、文件缺失）。

**2.5 的前置核实已完成，2.5 本身从 6 个动作缩到 1 个并落地**（2026-09-12）。核实结论进了
`docs/dsh-contract-inventory.md` 的 #53 / #54，落地记录见本节末尾；这里记核实对本计划的
影响：

| 原计划的说法 | 核实后 |
|---|---|
| 键名待核（`allowBuilds` vs `onlyBuiltDependencies`） | **`allowBuilds`**（pnpm 11 的映射形状），写进 `<DSH_HOME>/profiles/<profile>/pnpm-workspace.yaml`——**这是 dsh 自己打印的指路目标**（`dsh plugin` 在 git 源失败时告知"把 pnpm 印出的键加到 `allowBuilds` 下再重跑"），不是我们替它决定。AHL 自己根目录的 `pnpm-workspace.yaml` 用的也是这个形状 |
| "`add-allowbuilds` 同时修复 AHL 现有的 MCP / skill 安装失败路径" | **范围要缩窄**：dsh 只在 **git 源**（`git+` / `github:` / `.git#`）失败时才给这条提示，且触发条件是 pnpm **忽略了构建脚本**。普通 npm 源的 MCP / skill 安装不受影响。这是"装 GitHub 上的插件"这一条路径的修复，不是全量安装路径的修复 |
| 6 个修复动作全搬 | **只留 1 个**：`add-allowbuilds`。`fix-permissions`（win32 必失败）、`clean-cache`（异常被吞导致 `rmSync` 永不执行）、`repair-session-log`（定义了从未接线）三个在 2.4 已按"照搬别家的 bug"砍过，理由不变；`restore-yaml-from-bak` 与 AHL 的 `rescue_restore` 重叠（后者还原的是**启动成功过**的状态，更强）；`pnpm-install-profile` 在 AHL 没有对应命令——按 2.4 的同一条纪律（**有检查无按钮也不给按不动的按钮**），等命令存在再说 |

**顺带发现的一条**：`pnpm` 不在 PATH 是**另一个**独立故障（dsh 打印
`pnpm not found on PATH` 并 `exit 127`）——AHL 不管理 pnpm，`dsh plugin` 是从 PATH 找的。
这属于 2.4 的健康检查能真实测量的东西，比再做一个修复按钮更对症。**落地时量法与这里
预想的不一样**：不是跑 `pnpm --version`，而是 `which::which("pnpm")`——见 2.5 落地记录。

**2.3 的处置：L1 / L2 已在，L3 不做**（2026-09-12）。先纠一处口径：计划里 2.3 写的是"无"，
但 AHL 已有 `BootRecovery.tsx`（按诊断结论给出的对症动作，含"停用某 bundle"与"还原救援点"）
与 `rescue_restore`——**L1 和 L2 缺的只是"自动升级"这层编排，不是能力**。

L3（工厂重置）**决定不做**，理由是它撞 §0 原则二，且比 §3 里已有的那条更严重：

- 它替 DSH 重写 profile 应该是什么样：`package.json` 的 `bundles` 改成两个官方 bundle， 
  `cordis.yml` / `cordis.patch.yml` 写成 `[]`。patch 是 DSH 的实装事实（`mcp-<name>` 行就在
  里面），清空它等于启动器单方面宣布这些不存在——正是 AHL 契约里不许做的事。
- 它是**自动且无确认**的：崩溃处理里 L1、L2 失败后第三次崩溃直接进 L3，UI 无入口无确认。
- 它确实先备份 `RESCUE_FILES`，所以"不可逆"不成立；但 AHL 的 L2 还原的是**一次真的启动
  成功过的状态**，比当场发明一个更诚实。**这一维上 L2 已经强于 L3，所以 L3 不是"更强的
  恢复手段"**——这是它不值得移植的根本原因，而不是"懒"。

**2.3 仍未落地的是编排**：把 L1 → L2 的升级做成自动的，并按 2.4 的健康报告决定何时进入
安全模式（1.6）。这条**卡在 1.6 上，而 1.6 曾卡在 1.4**（页面层自检）——1.4 当时的拦路石是
"token-401 页的状态码形态需要真机确认"。**该拦路石已于同日解除**（读本机已装的 dsh 就得到了答案，
不必真机实测：见 Phase 1 实施记录），所以 1.6 与 2.3 的编排现在都具备开工条件；本轮收掉的是
2.3 里那个不该做的部分。

**2.4 已落地**（`57aed39` 后端 + `03a3081` UI）：从"坏了能自救"往前推一步——**在坏掉之前
就看得见**。检查项在 `crates/dsh-adapter/src/health.rs`，命令是 `instance_health`，
UI 是 `apps/desktop/src/components/HealthPanel.tsx`（挂在 Overview 上）。

**清单定为 5 组**（`runtime` / `profile` / `plugins` / `mcp` / `rescue`）**11 项，随 2.5 再加
`pnpm` 一项，现 12 项**，不是计划里写的"6 组约 13 项"。砍的理由是逐项核对 dsh-manager
那份清单后发现**它自己就不成立**：

| dsh-manager 的项 | 处置 | 理由 |
|---|---|---|
| `disk-space` | 弃 | 源码里是硬编码 `'info'` 的桩，**从不真的量磁盘**；AHL 的 `dsh-adapter` 也没有 sysinfo / fs2 依赖，同样量不了——照搬等于搬来一个永远说"健康"的检查 |
| `fix-permissions`（凭据权限位） | 弃 | 实现在 win32 上直接失败返回；AHL 是 Windows-first，落地即是死代码 |
| `clean-cache` | 弃 | 其目录备份调用抛 `EISDIR` 且被吞掉，导致后面的 `rmSync` **永远不执行**——看着成功，实际没清 |
| `repair-session-log` | 弃 | 函数定义了但从未接线，是 §1 记录的"文档/实现落差"的又一例 |
| 端口 / 会话日志总量 / 最近备份 | 未做 | 与 AHL 已有的 Rescue 面板、Activity 面板信息重叠；重复展示只会让两个面板就同一件事给出不同答案 |

**新增的 `mcp` 组是 AHL 自己的**，dsh-manager 没有对应项：MCP 记录只检查启动形态
（`command` / `args` 是否成形）与"声明了但没赋值的环境变量"——**只读键名，从不读值**，
免得把密钥从健康报告这个出口带出去。

**三条纪律**：

1. **一项检查 = 一次真实测量。** 上表弃掉的四项全是这条的产物——没有测量能力的检查不写。
2. **只读，每次重测。** `health_report` 每次都重新测量，`HealthReport` 不缓存，
   `worst` / `has_at_least` 是对当次 `checks` 的现算，不存在"上次说是好的"。
3. **fix 只导向已存在的命令。** 计划里 6 个修复动作中其余几个（`install-deps` /
   `reinstall` / `rebuild-source`）在 AHL 没有对应命令——AHL 只有 `dsh plugin add/remove/update`，
   没有"重装 profile 依赖"这种通用入口——所以**有检查无按钮**，而不是给一个按不动的按钮。
   这与 `BootRecovery` 里 `ACTIONABLE` 的取舍同源（2.5 落地后才补得上）。

**刻意不给 fix 的一项**：`bundles-resolved` 失败（声明的 bundle 在磁盘上不存在）时**不提供
"停用它"**。bundle 都不在磁盘上，`cordis.patch.yml` 里未必有它的行可停——真去停可能改动
一份本就没坏的配置；恢复救援点也带不回包文件。UI 因此只陈述，不动手。

**复用而非另写读取器**：`profile` 与 `plugins` 两组复用 `diagnose_profile` 与
`skin_has_bundle` / `package_mounts_client` / `entry_artifact_exists`，`rescue` 组复用
`snapshot_status`。**同一份事实不能有两个读取器**，否则健康面板会和旁边的 Diagnostics 面板
就同一件事给出不同答案。

**顺带修掉一个反向条件**：`BootRecovery.tsx` 原先把 `restore` 从"运行中禁用"里豁免了，但
`rescue_restore` 和 `plugin_toggle` 一样走 `ensure_not_running`——跑着恢复会被后端拒绝。
后端行为没变，只是按钮现在提前说明，而不是点下去才报错。

**2.6 已落地**（`5bd75b7` 脱敏内核 + `bfd60e7` 命令 + `1439f41` UI + `cdfc3ab`/`f7d6a24`
自动落盘 + `71cb43b` 去重）：在"坏了能自救"之后再往前推一步——**坏了之后留下证据**。

一个包 = 一个 zip：`README.md`（这是什么 / 抹掉了什么 / 从没收集什么）、`env.txt`、
`state.txt`（manifest / settings / 救援点 / 健康报告 / profile 诊断）、`activity.txt`、
`errors.txt`（按码聚合的摘要）、`log.txt`，外加 `crashes/` 里最近 5 份崩溃报告。
命令 `export_diagnostics`，UI 在 Settings → 诊断包。

**从 `1/` 的 `DiagnoseExport.cs` 搬了什么、改了什么**：

| `1/` 的做法 | AHL 的做法 | 为什么改 |
|---|---|---|
| dump 整个进程环境变量 | 固定 8 个变量的白名单（`AHL_HOME` / `DSH_CLI_BIN` / `USERPROFILE` / `LOCALAPPDATA` …） | 进程环境里什么都有——用户 shell 里的 token、无关服务的密钥。白名单能让收件人**一眼读出"绝不会带出什么"**，黑名单不能 |
| 脱敏四条规则 | 两条（路径 + 密钥），且**先抹路径再抹密钥** | 顺序见下 |
| `SummarizeErrors` 按码聚合 | 同思路，但接 AHL 的 `ErrorCode` 码表：码 → 标题 → **下一步动作** | `1/` 只能列码；AHL 的码表本来就是给人看的三段式，包里的摘要直接复用，不再写第二张表 |
| 按行读日志到内存 | `read_shared_tail`：共享句柄 + 只读尾部 4MB + 丢掉被截断的首行 + 从半个 UTF-8 字符恢复 | `1/` 那份是**独占打开**，于是"应用开着时导不出诊断包"——恰恰是最需要导的时候。共享读修的正是这个 |

**抹除顺序是有讲究的**：先抹路径（用户名藏在路径里），再抹密钥值。反过来的话，
`C:\Users\ada\…token=abc` 会先被密钥规则吃掉尾巴，路径规则就再也匹配不上，留下的
`C:\Users\ada\` 里还有用户名。`%USER%` 占位符对 home 的**两种分隔符拼写 × 两种尾分隔符**
各来一遍——测试里就是这么抓到 `C:/Users/ada/...` 漏抹的。

**最要紧的一处架构判断**：Activity 面板里的 `[E1001] …` 只存在于**前端的
`appStore.logs`**——`emit_log_at` 把它们推给 UI，从不写文件。所以只从磁盘收集的话，
`errors.txt` 永远是空的，看上去像台健康机器。结论：**让 Activity 缓冲区随请求一起过去**
（`exportDiagnostics(id, activity)`）。这也是"同一份事实不能有两个读取器"的另一面——
当那份事实只存在于一个地方时，得把它搬到消费方，而不是在旁边重造一份。

**b：失败启动自动落盘**（`1/` 的 ADR-023）。手动导出只能服务"当时正看着窗口"的人；
自动落盘让"昨晚它崩了"第二天还能回答。挂在 `process.rs::diagnose_and_emit`，与手动导出
共用 `collect` / `write_package`（`collect` 定成 `pub(crate)` 就是为这个）。

- 落盘在"无规则命中就返回"**之前**：规则表没覆盖的失败，恰恰是只剩这份档案能说话的场合。
- Activity 在前端之外不可得，于是 `activity.txt` 由这条路上手上有的重建：先放启动器自己的
  诊断结论（级别 `warn`，与报告它的 `emit_warn` 一致），再放脱敏后的子进程尾部输出
  （级别 `error`——因为在这条路上它就是一次失败启动的输出，而不是每一行都算错误）。
  README 里写明了这两种来源，收件人才不会把重建的当原样抓的。
- 每一步失败都直接返回、不向上抛：**写诊断包失败不能把一次失败变成两次**。
- 保留"最新 5 份"，按**文件名里的时间戳**排序，不按目录序或 mtime（名字是启动器自己写的，
  目录序没有给过任何保证）；名字不认得的文件不删——保留清理不该用删除来发现那是什么文件。

**顺带收掉两处重复**（同一理由：同一份事实不能有两个写入者/读取器）：

- `zip::write_zip` 成为全应用唯一的 zip 写入者，环境包改走它，**字节不变**。
- `launcher_core::paths::slug` 成为唯一的 slug 化函数。原先有两份逐字相同的实现
  （`instance.rs` 的 `slugify` 写死 `instance`、`environment.rs` 的写死 `environment`），
  现在合成一个、fallback 变参数。这不是纯粹的整洁问题：**实例的 id 就是它的目录名**，
  而导出包的文件名用的是同一个归约——两份实现意味着"实例叫什么"可能在两处给出不同答案。
  `instance.rs` 里补了一条钉住这个契约的测试（改归约 = 把已有实例的目录改到别的名字下）。


**2.5 已落地**（`41cf6d1` 写入器 + `837fc11` 救援点 + `3d25659` 健康检查 + `9f960c8` 命令 +
`f663d0c` UI）：这一条的价值不在"多了一个修复按钮"，而在于它是**启动器转述 dsh 自己打印的
指路**——`dsh plugin` 在 git 源装不动时告诉用户"把 pnpm 印出的键加到 `allowBuilds` 下再重跑"，
AHL 只是替用户写那一行。§3 拒绝同项目的工厂重置，拒绝的正是"替 DSH 决定 profile 该是什么样"；
这里没有跨过那条线，因为**内容不是启动器决定的，是 dsh 打印出来、用户确认的**。

**写入器 `crates/dsh-adapter/src/pnpm.rs`**：文本级改写，不解析后重排。这份文件是**手写在编辑
的**（dsh 的提示就是让用户去编辑它），YAML 库往返一次会为了改一行而丢掉用户的注释、重排他们
的键。规则：锚定列 0 的 `allowBuilds:`，按已有条目的缩进插在其**后面**；键不存在就追加一整块；
键存在但带值（`allowBuilds: {}`）**直接拒绝**——追加会造出重复键，就地改要流式映射转块式，
猜错就是把一个故障变成两个。已是 `true` 是空操作且如实回报；是 `false` 则改写（这里静默跳过
会和它要修的故障长得一模一样）。包名要落进 YAML 当键，所以按 npm 允许的字符集校验，且**该
加引号的地方加引号**（`@` 与反引号是 YAML 保留指示符，`@scope/pkg` 不加引号就不是映射键了）。

两处不靠信任的地方：

- **先验后写**：改完的文本在内存里 parse 一遍、确认 `allowBuilds.<pkg> == true` 才落盘。
  写坏了是"半个文件"，没写是"原样"——两害相权，宁可后者。验不了的只有一件事：**键名对不对**
  （`allowBuilds` 是 pnpm 11 的形状，pnpm 10 用的是 `onlyBuiltDependencies` 列表，**形状错了
  不报错、只是静默不生效**，见 `docs/dsh-contract-inventory.md` #53）。
- **写前留 `.bak`**：救援点是另一条退路，但对**已经有救援点的实例它救不了这个文件**——救
  援点只在启动成功后刷新，今天才进救援集的文件不在昨天那份里（`rescue.rs` 里记了这条不对称）。

**救援点集合加了 `pnpm-workspace.yaml`**（`rescue.rs`）：旧的排除理由（"什么都没写过它"）在
`add_allow_build` 落地那一刻失效了——pnpm 每次 `dsh plugin` 都读这个文件，那它就属于"还原
要能撤销的东西"。测试改成从 `rescue_files` 取列表而不是手抄一份，下一次加减文件会被所有
往返测试走到，而不是只被那条断言集合内容的测试走到。

**健康检查加了 `runtime-pnpm`**（第 12 项，`runtime` 组）。**量法与预案不同**：预案写的是跑
`pnpm --version`，落地用的是 `which::which("pnpm")`。理由是这份报告**每次打开都重测**，而
`which` 是一次文件系统查询、`check_tool` 是一个子进程——dsh 自己就是按 PATH 找 pnpm 的，
"dsh 能不能找到它"这个问题文件系统已经答完了。代价要说清楚：**这条只说"dsh 能找到 pnpm"，
不说"找到的那个能用"**（corepack 垫片存在但报错的情况它答不了）。状态是 `warn` 不是 `fail`：
启动不需要 pnpm，改插件集才需要。

**UI 挂在失败 job 行上**（`InstallCenter.tsx`）：失败插件 job 上多一个"允许构建脚本"，
展开一个输入框，**预填 pnpm 那句 `Ignored build scripts:` 里的第一个名字**，但必须用户确认
才写。预填是省打字，不是判断——pnpm 的输出格式是 pnpm 的，猜错会替一个没人问过的包写批准。
`suggestBuildPackage` 只认"像包名的名字"，认不出就给空框。写完这一行只报"写了什么"
（`allowBuilds: <pkg>: true`）并指向 Retry，**不自己重跑安装**：批准够不够只有 `dsh plugin`
说了算。这条动作也不重取前端状态——变的是一个 profile 文件，不是已装插件集。

**至此 2.4 / 2.5 全部落地。** 剩下卡住的是 2.3 的编排（需要 1.6，而 1.6 需要的 1.4 已于同日落地，见 Phase 1 实施记录）。

**1.6 + 2.3 已落地**（`18960eb` `1b3f695` `f591ba0` `528a17f` `faad2bd` `3a8c7f8`）。这是
Phase 2 收尾的一项，把「两级安全模式」（1.6）与「恢复阶梯的编排」（2.3）合并成**一条阶梯**
落地——计划里就写了它们要合并，落的也是合并后的形态。至此 **Phase 2 全部条目收口**。

**阶梯形状**（`crates/dsh-adapter/src/safe_boot.rs`）：L1 保留用户 profile 里的 first-party
bundle（`@deepseek-ai/` 前缀，保序）并强制补入 minimal pair；L2 只留 minimal pair
（`@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-web-app`，即 dsh 自己的 `web` 模板，逐字）。
L3 不做（§3）。安全 profile 是**用户 profile 的平级目录** `.ahl-safe`，绝不读改写用户的
`package.json` / `cordis.patch.yml` / `pnpm-workspace.yaml`。

**一个关键修正：安全 profile 只写一个清单，不写 `cordis.yml` / `cordis.patch.yml` /
`pnpm-workspace.yaml`。** 从 `1/` 的 `SafeProfileBuilder` 移植时，原以为要照 zat 的
`RESCUE_FILES` 那样给安全 profile 备齐 profile 四件套；核实 dsh 实际行为后**既不需要也不该写**：
① `loadProfile` 把 profile patch 层读作 `existsSync(patchPath) ? load : []`——缺
`cordis.patch.yml` 只是"无 overlay"，不是故障；② `prepareProfile` 会把
`profiles/<name>/node_modules` 物化为指向安装锚点的 symlink 农场，所以手写清单里的 bundle
无需 pnpm install 就能解析。结论：写**恰好一个** `package.json`（`dsh.profile.bundles`，
无 `dependencies`）是"没有可错的东西"那一支。契约已登记进 `dsh-contract-inventory.md`
的 #58 / #59 / #60 / #61 / #62。

**boot 之前让 dsh 先裁决**：`dsh --profile .ahl-safe --dump-config`（父解析器上的 flag，
不是子命令）。合成成功 exit 0；不可解析 bundle 则 exit 1 并打印
`Error: dsh: cannot resolve profile bundle "…" …`。AHL 只在 dsh 说"能合成"之后才 boot，
拒绝时**原样引用 dsh 的话**（`SafeProfileVerdict::Refused.from_dsh` 区分"dsh 说的"与
"对 exit code 的解读"），绝不自造诊断、也绝不 boot 一个 dsh 刚拒绝过的 profile。

**编排（2.3 的"自动升级"）**：L1 失败自动爬向 L2，L2 失败即到底、如实说"minimal pair 也
起不来"，不循环。三个触发点：boot 前的 dump 被拒（`do_safe_launch` 内联爬梯）、子进程崩溃
（`on_exit` 收尾后爬）、静默降级（慢路径 watcher 爬）。爬梯是**一次性**的（`safe_escalating`
原子标记，`do_launch` 在安全子进程真正起来时复位），避免"降级 + 崩溃"两次失败赛跑出两趟 L2。
正常启动时清除 `.ahl-safe` 残骸——退出安全模式 = 回到用户自己的 profile、无痕。UI 侧
`BootRecovery.tsx` 出安全模式入口（失败面板）与 sky 色横幅（安全模式运行中 + 退出按钮）。

**1.4 的版本校准局限（在此一并记录）**：1.4 的 `web_check` 签名（契约 #56/#57）是从本机
全局安装的 `@deepseek-ai/dsh@0.1.5-rc.1/rc.2` **读源码**得到的；而安全模式这段契约
（`--dump-config` / `--profile .ahl-safe` / 缺失 patch 非致命 / 就绪行逐字相同）是**对照随
运行时发布的 dsh 0.1.0-rc.7 实测**得到的——两处校准的 dsh 版本不同。0.1.0-rc.7 是启动器
实际会跑的那个，所以安全模式的实测更接近生产；但两份契约的版本基准不一致，跟进 dsh 新版本时
要各自重核，不能拿其中一个版本的行为替另一个背书。


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
| **照搬别家的 bug** | `dsh-manager` 有文档/实现落差，`DSH-Launcher` 无测试 | 只吸收能在源码里指出实现位置的机制；每条吸收项自带验收口径。**2026-09-12 实证（一）**：2.4 按此规则逐项核对 dsh-manager 的健康清单，13 项里砍掉 4 项（桩 / win32 必失败 / 反被吞掉的异常 / 定义了但没接线），见 Phase 2 实施记录——**清单本身也要按这条审**，不能因为"只读、风险低"就整表照搬。**实证（二）**：2.6 核 `1/` 的 `DiagnoseExport`，发现它读取日志用独占打开，于是"应用开着时导不出诊断包"——恰好是最需要导的时候；改为共享句柄读尾部（`read_shared_tail`）。**能指到实现位置 ≠ 那个实现是对的**，两件事都要做 |
| **`3/zat` 的崩溃规则会腐坏** | 那 13 类规则绑定 dsh 0.6.x~1.5.7 的具体报错文本 | 规则表**已可外部覆盖**，不硬编码进二进制。**2026-09-12 核实**：原文所说的 "AHL 已有的 `DSH_BOOT_SIGNATURES` 式做法" 在本仓库中**并不存在**，属设想而非现状；**同日补实现**——`<root>/crash-signatures.json`（内建优先，只能补充不能遮蔽），见 §2 Phase 2 实施记录 |
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

---

## 附：如果只做三件事

时间有限时按这个顺序做，收益最集中：

1. **Phase 1.1 自适应启动超时** —— 直接消除现有的误杀，改动局限在一个文件。
2. **Phase 2.1 + 2.2 崩溃诊断 + 救援点快照** —— 把 AHL 从"起不来就没办法"变成"起不来能自救"。
3. **Phase 2.4 + 2.5 健康检查 + 修复动作库** —— 把"能自救"变成"能预防"，且 `add-allowbuilds` 顺带修掉现有的安装失败路径。

这三件做完，AHL 在"坏了怎么办"这个维度会超过除 `3/zat` 和 `dsh-manager` 之外的所有同类项目——而它本来就有的 MCP 全生命周期、用量账本、凭证库，是那两家都没有的。

---

### 关于本文档

- **核实时间**：2026-09-12，对照 AHL `v0.1.0`（`main` 分支）。§1 的每条"现状"都经 grep 或读源码确认；AHL 迭代快，引用具体行号前请重新核对。
- `docs/` 下随仓库发布的文档有两份：本文（吸收计划）与 `dsh-contract-inventory.md`（上游 dsh 契约清单，由 Phase 0.5 产出）。其余姐妹文档（`mcp-ecosystem-roadmap.md`、`release-test-plan.md`、`dsh-first-capability-boundaries.md`、`optimization-backlog.md`）逐个被 `.gitignore` 排除——所以本文与契约清单的表述都按公开文档的标准写。
