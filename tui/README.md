# dsh-tauri

基于 DSH Desktop 的**轻量重壳**：用 Tauri v2（Rust）替代 Electron，保留「起 Host → loopback Web carrier → webview 加载」的架构，目标是 Windows + macOS 双平台、更小更快。

> 参考项目：`dsh-desktop`（Electron）。完整方案见 `PLAN-tauri-reshell.md`（位于 dsh-desktop 仓库）。

## 现状（Phase 1 · 最小可运行骨架 + 真实 Host）

✅ 已完成：

- Tauri v2 (Rust) 壳，单实例窗口 + 启动器加载页
- **真实上游 Host 已接入**：`host/index.js` 拉起 vendored runtime 里的 `@deepseek-ai/dsh`（`web --host 127.0.0.1 --no-open --port 0`），从 `dsh web: http://…/?token=…` 解析实际端口 + 浏览器信任 token，回传 `DSH_READY <port> <token>`
- **浏览器信任闭环已验证**：裸 `/` → 401；`/?token=<t>` → 303 + 下发绑定 authority 的签名 cookie；带 cookie 的 `/` → 200 真实 DSH SPA（webview 的 cookie jar 自动完成该流程）
- **Node sidecar 进程协议**：`spawn → DSH_READY <port> <token> → shutdown`
  - 端口：真实 Host 用 `--port 0`（OS 分配，报告实际端口，免冲突重试）；`--retry-limit 32` 保留给 `--mock` 占位路径 / 后续固定端口策略
  - 清理双保险：优雅退出走 `RunEvent::Exit → taskkill /T`；强杀靠 **stdin 关闭自退**
- 端到端已验证：窗口加载启动器 → Rust 起真实 Host → 就绪后 `navigate` 到 `http://127.0.0.1:<port>/?token=<token>` → 关窗/强杀均无残留 Node 进程
- 可复用逻辑已搬到 `host/reference/`（20 个文件，待逐模块移植）

⚠️ 待办：

- 端口重试、LAN（固定端口）、设置、安装 ID 等逻辑从 `host/reference/` 移植
- 崩溃检测 / 自动恢复（Host 在 ready 后异常退出时 shell 的重建）

## 目录结构

```
dsh-tauri/
├── src-tauri/            # Rust 壳
│   └── src/
│       ├── main.rs       # Windows 入口
│       ├── lib.rs        # 窗口 / 生命周期 / 清理 / 启动 host
│       └── sidecar.rs    # Host 进程管理（spawn / 就绪 / 进程树清理）
├── host/
│   ├── index.js          # sidecar：拉起真实 dsh Host，解析 DSH_READY（--mock 保底）
│   ├── runtime/          # vendored 上游 runtime（@deepseek-ai/dsh 等 241 包，npm 装配）
│   └── reference/        # 从 dsh-desktop 搬来的可复用 TS 模块（待移植）
├── index.html            # 启动器加载页
├── src/                  # 前端（启动器）
└── dist/                 # 构建产物（gitignore）
```

## 开发

```sh
npm install
npm run tauri dev     # 开发：窗口加载启动器，Rust 起 mock sidecar，自动跳转
```

只编译 Rust（不弹窗）：

```sh
cd src-tauri && cargo build
```

## Sidecar 协议

`host/index.js` 拉起真实上游 Host，与 Rust 壳走同一协议：

```
argv:    node index.js [--port 0] [--retry-limit <n>] [--home <dir>] [--mock]
stdout:  DSH_READY <port> <token>   # 就绪；port 为实际绑定端口，token 为浏览器信任令牌
stderr:  DSH_ERROR <msg>            # 致命启动错误
stdin:   关闭即退出（父进程失联保护）
```

webview 加载 `http://127.0.0.1:<port>/?token=<token>`。Host 首次收到该 URL 时用 token
换取绑定 authority 的签名 cookie（303 → `/` + Set-Cookie），之后靠 cookie 鉴权——webview
的 cookie jar 自动完成，无需壳侧参与。`--mock` 走 Phase 1 占位页（runtime 未装时用）。

真实 Host 的命令行等价形式（供调试）：

```sh
DSH_HOME=<home> node node_modules/@deepseek-ai/dsh/lib/bin.js web \
  --host 127.0.0.1 --no-open --port 0
```

## 打包（Phase 5 待做）

- Windows：NSIS + 签名
- macOS：Universal + notarization
- 目标：安装包 ≤ 30MB，自动更新走 `tauri-plugin-updater`
