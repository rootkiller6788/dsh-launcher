# AI Harness Launcher

**PCL for AI Harnesses** — a Windows launcher, instance manager, package manager
and setup ecosystem for AI harness runtimes (DSH first).

> 把「装 DSH、配 Provider、装插件、点 Launch」变成双击 `.exe` 就能完成。

## 现状（v0.1 — Phase 0 + 1）

- Tauri 2 + React + TypeScript + Rust 桌面骨架
- DSH 单实例启动 MVP：配置 Provider → 点 **Launch** → `dsh web` 后台启动并自动打开浏览器
- stdout/stderr 实时流式日志、启动/停止、环境检测

完整路线见 [`plan.md`](./plan.md)（Phase 0–7）与 [`architure.md`](./architure.md)（产品架构）。

## 目录结构

```
ai-harness-launcher/
├── apps/desktop/          # Tauri 2 应用（React + TS 前端 / src-tauri Rust 壳）
├── crates/
│   ├── launcher-core/     # 核心库：paths/settings/instance/provider/process/runtime
│   └── dsh-adapter/       # DSH 专有适配器（实现 RuntimeAdapter）
└── ...
```

## 开发

要求：Node ≥ 22、pnpm ≥ 10、Rust（MSVC toolchain）、WebView2。

```bash
pnpm install                 # 安装前端依赖
cargo build --workspace      # 编译全部 Rust crate
pnpm dev                     # 启动 Tauri 开发窗口（tauri dev）
```

## 打包

```bash
pnpm build                   # tauri build → NSIS Setup.exe
# 产物：apps/desktop/src-tauri/target/release/bundle/nsis/*-setup.exe
```

## 数据目录

```
%LOCALAPPDATA%/AIHarnessLauncher/
├── settings.json            # 应用设置（DSH 路径覆盖等）
├── providers.json           # Provider 元数据（API key 存 Windows Credential Manager）
├── instances/default/       # 实例（instance.json + workspace/ = 该实例的 $DSH_HOME）
└── logs/launcher.log
```

## 环境变量

| 变量 | 用途 |
|---|---|
| `AHL_HOME` | 覆盖数据根目录（默认 `%LOCALAPPDATA%/AIHarnessLauncher`，开发用） |
| `DSH_CLI_BIN` | 覆盖 DSH CLI 路径（默认自动探测 `deepseek-harness-master/apps/cli/lib/bin.js`） |
