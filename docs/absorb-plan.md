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
