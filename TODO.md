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

---

## P2 — CI 发布流水线 ⏳

> **验收**：一条 `git tag` 从零出安装包，CI 全绿，产物可下载、可回溯、可复现。
> 管线：`tag 触发 → 四道 gate → 出安装包 → 冒烟`。

- [ ] **流水线骨架**：新建 `.github/workflows/release.yml`（`windows-latest`），以 `v0.x.x` tag 触发；`pnpm install` + 环境准备 — [#201](https://github.com/ai-harness/ai-harness-launcher/issues/201)
- [ ] **Gate 1**：`cargo test --workspace` — [#202](https://github.com/ai-harness/ai-harness-launcher/issues/202)
- [ ] **Gate 2**：`cargo clippy --all-targets -- -D warnings` — [#203](https://github.com/ai-harness/ai-harness-launcher/issues/203)
- [ ] **Gate 3**：前端类型门 `npx tsc --noEmit`（apps/desktop）— [#204](https://github.com/ai-harness/ai-harness-launcher/issues/204)
- [ ] **Gate 4**：`instance_system` 集成测试（多实例隔离不互相污染）— [#205](https://github.com/ai-harness/ai-harness-launcher/issues/205)
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

- [ ] Rust · `detect()` 解析链单测（settings → bundled → managed → dev，四层各一例）— [#401](https://github.com/ai-harness/ai-harness-launcher/issues/401)
- [ ] Rust · 进程树 teardown 测试（含连续 10 轮 stop/start 无残留）— [#402](https://github.com/ai-harness/ai-harness-launcher/issues/402)
- [ ] Rust · 市场 `reconcilePlugins` 幂等测试（enable/disable 往返）— [#403](https://github.com/ai-harness/ai-harness-launcher/issues/403)
- [ ] 前端 · `npx tsc --noEmit` + 关键 store 单测（主题 sync、市场状态机）— [#404](https://github.com/ai-harness/ai-harness-launcher/issues/404)
- [ ] E2E（可选后置）· `tauri-driver` 启动真窗口点 Launch — [#405](https://github.com/ai-harness/ai-harness-launcher/issues/405)

## P5 — 代码签名 ⏳

> **验收**：干净机器下载安装**无 SmartScreen 拦截**（或仅一次「仍要运行」）。

- [ ] 购买 OV/EV 代码签名证书；`signtool` 签 `setup.exe` + updater artifact — [#501](https://github.com/ai-harness/ai-harness-launcher/issues/501)
- [ ] CI 中在 P2 产物上签名（P2 → P5 串成一条链）— [#502](https://github.com/ai-harness/ai-harness-launcher/issues/502)
- [ ] 未签名 `nightly` 走 `Unsigned` 通道（仅高级用户）— [#503](https://github.com/ai-harness/ai-harness-launcher/issues/503)

## P6 — 补齐小件 ⏳

> 不成独立阶段，随各阶段带走。

- [ ] **Crash reporting**：panic hook → `%LOCALAPPDATA%/…/logs/crash-*.txt` — [#601](https://github.com/ai-harness/ai-harness-launcher/issues/601)
- [ ] **Telemetry opt-in**：默认关；只上报「崩溃 + 版本号」，绝不包含会话内容 — [#602](https://github.com/ai-harness/ai-harness-launcher/issues/602)
- [ ] **Download resume**：reqwest 断点续传 + SHA-256 校验和（安装/市场已用）— [#603](https://github.com/ai-harness/ai-harness-launcher/issues/603)
- [ ] **Portable 模式**：绿色版，runtimes 放 exe 同目录 — [#604](https://github.com/ai-harness/ai-harness-launcher/issues/604)

---

## 维护说明

- 勾掉一项 = 合并对应 PR 并关闭 issue（PR 里写 `Closes #NNN`）。
- 里程碑推进时更新上方表格的「状态」，并在 `architecture.md` 记录验收证据（命令输出/截图）。
- 拆 issue 时尽量让每个 checkbox 对应**一个可关闭的 issue**，避免「多事一票」。
