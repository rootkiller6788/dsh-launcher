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
