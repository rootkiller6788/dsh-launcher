# Contributing to AI Harness Launcher

> 欢迎！**AI Harness Launcher** 是一个用 Tauri 2 + Rust 构建的「PCL for AI Harnesses」Windows 桌面启动器：管理 DSH 运行时、多实例、Provider、插件，然后一键 Launch。
>
> 动手前请先读 [`README.md`](./README.md)（怎么跑）和 [`plan.md`](./plan.md)（路线与每阶段的验收 Gate）。这份文档约 5 分钟，能帮你少走 90% 的弯路。

## 目录

- [技术栈与仓库结构](#技术栈与仓库结构)
- [环境准备](#环境准备)
- [常用命令](#常用命令)
- [分支规范](#分支规范)
- [提交信息规范](#提交信息规范)
- [提 PR 流程](#提-pr-流程)
- [代码规范](#代码规范)
- [测试要求](#测试要求)
- [评审与合并](#评审与合并)
- [Issue 规范](#issue-规范)

## 技术栈与仓库结构

pnpm workspace + Cargo workspace 的 monorepo：

```text
apps/desktop/                 Tauri 2 应用
  ├── src/                    React 19 + TypeScript 前端（Vite + Tailwind v4 + Zustand）
  └── src-tauri/              Rust 壳：commands/、state/、tauri.conf.json
crates/
  ├── launcher-core/          核心库（无 Tauri 依赖，可单测）：
  │                           paths / settings / instance / provider / process / runtime / history / market
  └── dsh-adapter/            DSH 专属适配器（实现 RuntimeAdapter）：
                              runtimes / theme / diagnostics
scripts/                      开发辅助脚本（如 gen-icon.mjs）
```

### 语言边界（红线）

项目刻意把「边界」定死（见 plan.md 第「三」节）：

- **TypeScript 只管 UI**：页面、表单、状态、展示。
- **Rust 只管系统**：进程、文件系统、网络、密钥、SQLite、运行时管理。

两条禁令：

1. 不要在 TS 里 `exec()` / 写文件 / 发进程——系统操作一律走 Rust command 经 typed IPC。
2. 不要在 Rust 里管 UI 状态——状态放 Zustand，Rust 只返回数据。

IPC 类型（`lib/ipc.ts` ↔ `commands/`）是唯一的跨语言契约，改一侧必须同步另一侧。

## 环境准备

| 依赖 | 版本 | 说明 |
| --- | --- | --- |
| Node.js | ≥ 22 | 开发工具链；运行时用的 Node 已捆绑进安装包 |
| pnpm | ≥ 10（仓库锁定 `pnpm@11.18.0`） | `corepack enable` 后自动用锁定版本 |
| Rust | MSVC toolchain（`stable-x86_64-pc-windows-msvc`） | 用 `rustup` 安装 |
| WebView2 | 系统自带（Win 11） | Tauri 运行时 |

```bash
pnpm install          # 安装前端依赖（esbuild postinstall 已在 pnpm-workspace.yaml 批准）
cargo build --workspace
```

## 常用命令

| 命令 | 作用 |
| --- | --- |
| `pnpm install` | 安装前端依赖 |
| `pnpm dev` | 启动 Tauri 开发窗口（根目录，委派到 apps/desktop） |
| `pnpm dev:web` | 只起 Vite，不挂 Tauri 壳（调试前端） |
| `cargo check --workspace` | Rust 快速编译检查 |
| `cargo test --workspace` | 全部 Rust 单测 + 集成测试 |
| `cargo clippy --all-targets -- -D warnings` | Rust 静态检查（**必须零告警**） |
| `npx tsc --noEmit` | 前端类型检查（在 `apps/desktop/` 下执行） |
| `pnpm build` | 打 NSIS 安装包（tauri build --release） |

> ⚠️ 根目录**没有** `tauri` script——用 `pnpm dev` / `pnpm build`，不要敲 `pnpm tauri dev`。

## 分支规范

- **`main`**：唯一长期分支，始终保持可发布。**禁止直接 push**，只接受合并后的 PR。
- **功能分支**：`feature/<area>-<short>`，如 `feature/market-reconcile`、`feature/portable-mode`。
- **修复分支**：`fix/<short>`，如 `fix/process-tree-zombie`。
- **实验分支**：`experiment/<name>`（不进入 PR，验证后即删）。
- 每次从最新的 `main` 拉分支；提交 PR 前先
  ```bash
  git fetch origin && git rebase origin/main
  ```
  保持历史线性，减少冲突。
- 绝不提交 `target/`、`node_modules/`、`*.log`、`*.pids`、密钥/证书（已在 `.gitignore`）。

## 提交信息规范

采用 [Conventional Commits](https://www.conventionalcommits.org/)，一条提交只做一件事：

```text
<type>(<scope>): <subject>

<为什么这么改 / 注意事项>
```

- **type**：`feat` `fix` `refactor` `test` `docs` `chore` `ci` `perf`
- **scope**：`launcher-core` `dsh-adapter` `desktop` `ui` `market` `ci` `docs`
- **subject**：祈使句、首字母小写、≤ 72 字符，不带句号

示例：

```text
feat(dsh-adapter): 解析链增加 bundled node 回退
fix(launcher-core): 作业对象挂载失败时降级 taskkill /T
test(launcher-core): 进程树 10 轮 teardown 集成测试
ci: 新增 release 流水线四道 gate
```

关联 issue：PR 标题或提交里写 `Closes #123` / `Refs #123`（合并时自动关 issue）。

## 提 PR 流程

1. **先开 issue，再动手**（大改动先出方案）。小修复/文档可直接 PR。
2. 从 `main` 拉分支（见[分支规范](#分支规范)）。
3. 本地全绿后再提交：
   ```bash
   cargo test --workspace && cargo clippy --all-targets -- -D warnings && npx tsc --noEmit
   ```
4. push 后提 PR，用模板填清楚：
   - **为什么**：关联 issue、要解决的问题
   - **改了什么**：涉及的文件/模块、设计取舍
   - **怎么测**：跑过的命令与结果；真机行为（如 Launch→窗口→Stop）附输出/截图
5. 打 `draft` 直到能通过全部 CI gate（P2 落地后会自动跑）。
6. 至少 1 人 review 通过 + CI 全绿才可合并。

## 代码规范

### Rust

- 必须 `cargo fmt` + `cargo clippy --all-targets -- -D warnings` **零告警**。
- 依赖收敛到根 `Cargo.toml` 的 `[workspace.dependencies]`，crate 里用 `xxx.workspace = true`，版本只改一处。
- 错误用 `anyhow::Result`；库路径**不允许 `unwrap()`/`expect()`**；系统调用错误保留上下文（`with_context`）。
- 日志用 `tracing`（`debug/info/warn/error` + 结构化字段），不用 `println!`。
- Windows 专属逻辑用 `#[cfg(windows)]`，并为非 Windows 提供**行为一致的桩函数**（如 `sweep_leftover` 返回 0），保持 `cargo check` 跨平台可过。
- 进程、文件、密钥等核心路径**必须带测试**（见[测试要求](#测试要求)）。

### TypeScript

- `npx tsc --noEmit` 零错误（严格模式）。
- 组件只负责渲染；状态放 Zustand store；IPC 统一封装在 `lib/ipc.ts`，类型与 Rust command 一一对应。
- 样式用 Tailwind 类，不用内联 `style`；主题色走 CSS 变量（`--color-*`），不写死 hex。

## 测试要求

- 改了 `crates/*`，**必须** `cargo test --workspace` 全绿。
- 核心链路（进程 teardown、运行时安装、市场开关）至少补一个集成测试，守护「Launch → 窗口 → Stop」这条主线。
- 依赖真机环境的测试用 `#[ignore = "..."]` 并在注释里写明前置条件（如 P0 managed runtime），单独跑：
  ```bash
  cargo test -p dsh-adapter --lib -- --ignored real_dsh_stop_start_10_rounds_no_scars
  ```
- 前端改动至少过 `npx tsc --noEmit`；store 逻辑改动尽量补单测。
- 测试要能稳定复现——「真机跑一次通过」不算，CI 能绿才算。

## 评审与合并

- Reviewer 重点看：正确性、错误处理与边界、测试覆盖、是否越界到别的 crate / 层。
- 阻塞项（会崩、会丢数据、会泄漏进程/文件）必须修完才能合并；风格小问题不阻塞。
- 合并用 **squash**，保持 `main` 线性；PR 标题即最终提交信息，写清 `Closes #NNN`。

## Issue 规范

- 用模板：**现象 / 期望 / 环境**；bug 附日志（`%LOCALAPPDATA%/AIHarnessLauncher/logs/launcher.log`）与复现步骤。
- 一个 issue 一件事；用标签 `bug` `enhancement` `ci` `docs` `p2`–`p6`。
- 完成后在 [`TODO.md`](./TODO.md) 勾掉对应项，并 `Closes #NNN`。
