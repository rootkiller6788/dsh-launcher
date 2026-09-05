# TODO

> **约定**：每一项用一个 GitHub issue 追踪。`- [ ]` = 待办，完成时勾 `- [x]` 并**关闭对应 issue**——GitHub 会自动把 `#数字` 渲染成链接，PR 里写 `Closes #123` 即可在合并时自动关 issue。
>
> 验收口径以 [`plan.md`](./plan.md) 第「二十一」节为准；每阶段验收证据回写 [`architecture.md`](./architecture.md)。

## 里程碑

| 版本 | 定义 | 状态 |
| --- | --- | --- |
| **v0.4** | 开箱即用：全新 Windows（无 Node / 无源码树）→ 双击 Setup → 填 Provider → Launch → DSH 可用 | ✅ 已达成（P0） |
| **v0.6** | 无疤 + CI 绿：stop/start 连续 10 轮无僵尸；一条 `git tag` 从零出安装包 | 🚧 进行中（P1 完成，待 P2 / P4） |
| **v0.7** | 签名 + 更新：一条 tag 出**签名**安装包 + 应用内自动更新 | ⏳ 未开始（P2 / P3 / P5） |
| **v1.0** | 签名 + 自动更新 + 开箱即用 + CI 守护的稳定版 | ⏳ 未开始 |

## 已完成 ✅

- [x] **Phase 0–3**：Tauri 骨架、DSH 单实例 MVP、多实例 + SQLite 历史、完整插件市场（Registry + 智能搜索 + 热开关 + 诊断 + 主题双向同步 + DSH 窗口化）— [#101](https://github.com/ai-harness/ai-harness-launcher/issues/101)
- [x] **P0 · 运行时自给**：Node 22 捆绑进 NSIS、`detect()` 四层解析链、spawn 绝对路径、Runtime Manager；真机验证全新机器开箱即用 — [#102](https://github.com/ai-harness/ai-harness-launcher/issues/102)
- [x] **P1 · 进程树硬化**：Windows Job Object + 递归 `taskkill /T` 兜底 + 启动前僵尸清扫；`real_dsh_stop_start_10_rounds_no_scars` 真机 10 轮 stop/start 无疤 — [#103](https://github.com/ai-harness/ai-harness-launcher/issues/103)
- [x] **DSH-first 阶段 1 · 能力边界确认**：明确 plugins / skins / skills / MCP / bundles 的安装入口、真实状态源、缓存来源和 Launcher 扫描职责；DSH 是运行时主干，Launcher 只做下载、分类、校验、缓存、诊断增强。
- [x] **DSH-first 阶段 2 · 统一安装入口**：Market leaf 安装统一走 `market_install`；先写入 DSH 或 DSH 认可的 workspace/config，再写 Launcher metadata，再刷新 Library snapshot。
- [x] **DSH-first 阶段 3 · Library 数据源重做**：Library 改为混合视图，区分 `DSH native` / `Market installed` / `Local file` / `Imported environment` / `Unknown detected`，不再只展示 Launcher 自己统计的插件。
- [x] **DSH-first 阶段 4 · 缓存机制重做**：`library-inventory.json` 升级为 schema v3 快照缓存；页面打开只读缓存，手动 Refresh / 安装完成 / 启动后台校准才触发 DSH Inventory 或深度扫描。
- [x] **DSH-first 阶段 5 · 后台任务队列**：launch、install、uninstall、inventory-sync、diagnostics、update-check、environment import/export、profile-mutation 进入同实例串行队列，避免安装、扫描、诊断、更新检查并发抢 profile。
- [x] **DSH-first 阶段 6 · 启动性能重构**：启动热路径收敛为 `launch instance → DSH web ready → show Workspace`；usage proxy 立即注入，inventory sync 延后后台跑，diagnostics/update check 改为手动触发。
- [x] **DSH-first 阶段 7 · Market 安装中心**：全局 `Install Center` 展示安装队列、阶段进度、近期日志、Retry、Open in Library、Reveal workspace/config；Market 只发起安装，不持有安装状态。

---

## 当前未完成阶段 ⏳

> 这组是 2026-09-03 之后的真实产品化剩余任务。旧的 P2–P6 发布工程清单仍保留在下方，但 DSH-first 主线应优先按这里推进。

### DSH-first 阶段 8 — Install Center 后端持久化

- [x] **后端 Job Store**：把 install job 从纯前端 Zustand 状态下沉到 Rust/SQLite；应用重载、窗口刷新后仍能恢复任务记录。
- [x] **真实下载进度事件**：下载、git shallow clone、pnpm/dsh install、inventory sync、metadata merge 都从后端发结构化 progress event，而不是前端估算阶段。
- [x] **任务历史与错误详情**：记录开始/结束时间、exit code、stderr 摘要、失败原因、可重试参数。
- [x] **Retry 从后端恢复**：Retry 使用保存的 install plan，不依赖当前页面仍保留原始 `RegistryPlugin` 对象。
- [x] **取消/排队可见性**：Install Center 能显示 waiting / running / failed / done，并支持取消尚未开始的 queued job。

### DSH-first 阶段 9 — Skills / MCP / Skins 真实可见性闭环

- [x] **Skills DSH discovery check**：运行中实例能区分“文件已下载”与“DSH/agent 实际已发现并可用”。
- [x] **MCP DSH config validation**：校验 MCP patch/config 是否被 DSH 认可，缺 token / command / transport 时给出结构化提示。
- [x] **Skins activation model**：皮肤安装后要能明确 installed / active / disabled，而不是只靠分类归档。
- [x] **Market 分类来源锁定**：从哪个 Market tab 安装，就在 Library 中稳定归到对应分类；DSH Inventory 只提供真实运行状态。
- [x] **Library row explainability**：每一项显示“为什么它在这里”：DSH inventory、manifest、market metadata、local file、import source。

### DSH-first 阶段 10 — Usage 真实账本收尾

- [x] **流式 / 非 JSON 响应覆盖**：继续硬化 usage proxy，对 SSE、chunked、provider 变体 usage 字段做兼容测试。
- [x] **成本模型表**：按 provider/model 维护价格表，无法定价时明确显示 unknown，不混用估算。
- [x] **统计维度补齐**：Today / 7 days / Month / Year / All，按 model、instance、provider、api key alias 过滤。
- [x] **导出与诊断**：Usage CSV/JSON 导出、异常费用提示、模型排行、请求峰值分析。
- [x] **可视化验收**：Overview snapshot 与 Monitor usage 图表均来自真实 ledger，不再使用启动历史估算。

### DSH-first 阶段 11 — 性能验收与遥测日志

- [x] **启动耗时分段日志**：记录 launch spawn、DSH URL ready、Workspace first paint、proxy inject、inventory sync 各阶段耗时。
- [ ] **页面切换性能 gate**：连续切 Workspace / Manage / Overview / Library / Market，确认不触发 hidden scan，不出现明显卡顿。
- [x] **后台任务节流审计**：确认没有 5 秒轮询 inventory、没有页面打开自动 diagnostics/update-check、没有隐藏页面渲染 161 插件明细。
- [ ] **大实例压测**：以 161+ 插件 inventory、多个 skills/mcp/skins 的实例做 Library/Overview/Instances 加载测试。
- [x] **Activity 日志降噪**：队列状态、usage proxy、inventory sync 日志分级，用户默认只看到可行动错误。

### DSH-first 阶段 12 — 环境包导入/导出增强

- [x] **`.dshenv` manifest schema 固化**：schema version、资源类型、来源、版本、checksum、兼容 DSH 版本写入规范。
- [x] **导入预览 UI 完整化**：展示将安装的 Plugins / Skins / Skills / MCP、潜在冲突、缺失 token、预计下载来源。
- [x] **导入任务接 Install Center**：environment-import 每个 leaf 资源都显示独立阶段和失败/重试结果。
- [x] **导入后 Library 校准**：创建新 instance 后立即写 snapshot，并在首次 launch 后用 DSH Inventory 校准。
- [x] **安全排除验证**：API key、本地日志、node_modules、workspace 私有状态必须不能进入 `.dshenv`。

### DSH-first 阶段 13 — 产品验收与发布前收口

- [ ] **端到端验收脚本**：Create instance → Market install skin/plugin/skill/mcp → Library 可见 → Launch → Usage ledger 记录 → Export/Import。
- [x] **回归测试补齐**：后台队列、snapshot cache、Market install、environment package、usage proxy 至少各有一个核心测试。
- [ ] **UI polish pass**：Install Center、Library、Market、Activity、Preferences 在 light/dark 下无对比度问题、无滚动失效、无拥挤/空洞布局。
- [x] **错误文案产品化**：pnpm timeout、git clone 失败、DSH plugin bundle 缺失、MCP token 缺失等错误给出下一步动作。
- [ ] **发布工程衔接**：确认下方 P2–P6 的 CI、更新、签名、crash reporting、portable 模式进入 release milestone。

---

## P2 — CI 发布流水线 ⏳

> **验收**：一条 `git tag` 从零出安装包，CI 全绿，产物可下载、可回溯、可复现。
> 管线：`tag 触发 → 四道 gate → 出安装包 → 冒烟`。

- [x] **流水线骨架**：新建 `.github/workflows/release.yml`（`windows-latest`），以 `v0.x.x` tag 触发；`pnpm install` + 环境准备 — [#201](https://github.com/ai-harness/ai-harness-launcher/issues/201)
- [x] **Gate 1**：`cargo test --workspace` — [#202](https://github.com/ai-harness/ai-harness-launcher/issues/202)
- [x] **Gate 2**：`cargo clippy --all-targets -- -D warnings` — [#203](https://github.com/ai-harness/ai-harness-launcher/issues/203)
- [x] **Gate 3**：前端类型门 `npx tsc --noEmit`（apps/desktop）— [#204](https://github.com/ai-harness/ai-harness-launcher/issues/204)
- [x] **Gate 4**：`instance_system` 集成测试（多实例隔离不互相污染）— [#205](https://github.com/ai-harness/ai-harness-launcher/issues/205)
- [ ] **出安装包**：四门全绿后 `pnpm --filter desktop tauri build --release` — [#206](https://github.com/ai-harness/ai-harness-launcher/issues/206)
- [ ] **安装包冒烟**：静默装到 `%TEMP%` → 启动 exe → 断言窗口存在 → 卸载；任一失败即 job 失败 — [#207](https://github.com/ai-harness/ai-harness-launcher/issues/207)
- [ ] **产物发布**：`DeepSeek-Harness-Launcher-<tag>-setup.exe` + updater 元数据上传 GitHub Releases；命名、SHA-256、可回溯性规范 — [#208](https://github.com/ai-harness/ai-harness-launcher/issues/208)

## P3 — 自动更新 ⏳

> **验收**：改一行版本号 → tag → 老版本应用内提示 → 一键升级到新版本。

- [ ] 接入 `tauri-plugin-updater`，更新包走 NSIS + 签名 artifact — [#301](https://github.com/ai-harness/ai-harness-launcher/issues/301)
- [ ] 发布 `latest.json` / updater 元数据到 GitHub Releases — [#302](https://github.com/ai-harness/ai-harness-launcher/issues/302)
- [ ] Release Channel：`stable` / `beta`（稳定推 stable，热修先行 beta）— [#303](https://github.com/ai-harness/ai-harness-launcher/issues/303)

## P4 — 测试补齐 ⏳

> **验收**：三个「重构护栏」——发版前全绿，且每阶段至少 1 个集成测试守护核心链路（Launch → 窗口 → Stop）。

- [x] Rust · `detect()` 解析链单测（settings → bundled → managed → dev，四层各一例）— [#401](https://github.com/ai-harness/ai-harness-launcher/issues/401)
- [x] Rust · 进程树 teardown 测试（含连续 10 轮 stop/start 无残留）— [#402](https://github.com/ai-harness/ai-harness-launcher/issues/402)
- [x] Rust · 市场 `reconcilePlugins` 幂等测试（enable/disable 往返）— [#403](https://github.com/ai-harness/ai-harness-launcher/issues/403)
- [x] 前端 · `npx tsc --noEmit` + 关键 store 单测（主题 sync、市场状态机）— [#404](https://github.com/ai-harness/ai-harness-launcher/issues/404)
- [ ] E2E（可选后置）· `tauri-driver` 启动真窗口点 Launch — [#405](https://github.com/ai-harness/ai-harness-launcher/issues/405)

## P5 — 代码签名 ⏳

> **验收**：干净机器下载安装**无 SmartScreen 拦截**（或仅一次「仍要运行」）。

- [ ] 购买 OV/EV 代码签名证书；`signtool` 签 `setup.exe` + updater artifact — [#501](https://github.com/ai-harness/ai-harness-launcher/issues/501)
- [ ] CI 中在 P2 产物上签名（P2 → P5 串成一条链）— [#502](https://github.com/ai-harness/ai-harness-launcher/issues/502)
- [ ] 未签名 `nightly` 走 `Unsigned` 通道（仅高级用户）— [#503](https://github.com/ai-harness/ai-harness-launcher/issues/503)

## P6 — 补齐小件 ⏳

> 不成独立阶段，随各阶段带走。

- [x] **Crash reporting**：panic hook → `%LOCALAPPDATA%/…/logs/crash-*.txt` — [#601](https://github.com/ai-harness/ai-harness-launcher/issues/601)
- [x] **Telemetry opt-in**：默认关；只上报「崩溃 + 版本号」，绝不包含会话内容 — [#602](https://github.com/ai-harness/ai-harness-launcher/issues/602)
- [x] **Download resume**：reqwest 断点续传 + SHA-256 校验和（`launcher-core::download`，npm 目录 tarball 已用）— [#603](https://github.com/ai-harness/ai-harness-launcher/issues/603)
- [x] **Portable 模式**：绿色版，数据根（含 runtimes）落 exe 同目录（`AHL_PORTABLE`/`portable` 标记，`launcher-core::paths`）— [#604](https://github.com/ai-harness/ai-harness-launcher/issues/604)

---

## 维护说明

- 勾掉一项 = 合并对应 PR 并关闭 issue（PR 里写 `Closes #NNN`）。
- 里程碑推进时更新上方表格的「状态」，并在 `architecture.md` 记录验收证据（命令输出/截图）。
- 拆 issue 时尽量让每个 checkbox 对应**一个可关闭的 issue**，避免「多事一票」。
