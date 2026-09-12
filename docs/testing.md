# 测试基线（2026-09-12 实测）

> 本文件记录 AHL 测试面的**实测**基线：跑什么、有多少、覆盖什么、哪些明确没覆盖。
>
> 所有数字来自 2026-09-12 在本机（Windows 11）全量跑一遍的结果，命令在下文逐条给出；
> 不是静态 grep 计数（grep 在 `launcher-core/src/process.rs` 这类带 `#[cfg(windows)]`
> 分组的测试模块上会少数 4 条，实测为准）。
>
> 与私有文档的分工：`docs/release-test-plan.md`（不随仓库发布）是发版前的**人工**验收
> 清单，本文件是**自动**测试面的清单。

## 一、怎么跑

### Rust（本机必带 debuginfo 关闭）

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -j 1 --workspace
```

两个 `DEBUG=0` 不是可选项：本机链接带 debuginfo 的测试二进制会 OOM，链接阶段必失败，
所以**不要** `cargo clean`（清掉后要重编全部依赖，代价几十分钟）。`-j 1` 同理，避免
链接并发把内存打满。CI runner 没有这个限制，所以 `.github/workflows/release.yml`
的 Gate 1 是干净的 `cargo test --workspace`。

```
单个 crate：  cargo test -p launcher-core --lib
单条测试：    cargo test -p dsh-adapter --lib web_check::a_dead_port_is_unreadable
看清单：      cargo test -p dsh-adapter --lib -- --list      # 带模块路径，做统计用这个
跑 ignored：  cargo test -p dsh-adapter --test probe_e2e -- --ignored --nocapture
```

### 前端

```bash
cd apps/desktop && npx tsc --noEmit && npx vitest run
```

（`vitest run` 也可用 `pnpm --filter desktop test`。）

### Lint（测试代码同样被 lint）

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

`--all-targets` 不能省：省掉就只 lint `src`，把每个 `#[cfg(test)]` 模块和 `tests/` 下的
集成测试全部放过。

### CI 的四道 gate（`.github/workflows/release.yml:56-69`）

| Gate | 命令 |
| --- | --- |
| 1 | `cargo test --workspace` |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` |
| 3 | `npx tsc --noEmit`（`working-directory: apps/desktop`） |
| 4 | `cargo test -p launcher-core --test instance_system` |

Gate 1 已经覆盖 Gate 4，Gate 4 保留是因为实例系统的失败信息值得单独一步定位。
**vitest 不在 gate 里**——27 条前端测试只在本地跑（见 §五）。

## 二、实测总数

| 目标 | 定义 | 通过 | ignored |
| --- | ---: | ---: | ---: |
| `launcher-core` lib | 126 | 126 | 0 |
| `launcher-core` tests/instance_system.rs | 1 | 1 | 0 |
| `dsh-adapter` lib | 198 | 194 | 4 |
| `dsh-adapter` tests/acceptance_e2e.rs | 1 | 0 | 1 |
| `dsh-adapter` tests/git_local_e2e.rs | 1 | 0 | 1 |
| `dsh-adapter` tests/install_matrix_e2e.rs | 8 | 0 | 8 |
| `dsh-adapter` tests/probe_e2e.rs | 8 | 0 | 8 |
| `ai-harness-launcher` lib | 33 | 32 | 1 |
| `ai-harness-launcher` bin（`main.rs`） | 0 | — | — |
| `ai-harness-launcher` tests/gui_launch_e2e.rs | 1 | 0 | 1 |
| 3 × Doc-tests（三个 crate 各 0） | 0 | 0 | 0 |
| **合计** | **377** | **353** | **24** |

**0 failed。** 断言耗时（不含编译/链接）：`launcher-core` lib 13.9s（最慢，含真实文件系统
与本地 HTTP 服务器用例）、`dsh-adapter` lib 2.1s、`ai-harness-launcher` lib 0.06s。

前端：

| 文件 | 条数 |
| --- | ---: |
| `src/stores/appStore.test.ts` | 11 |
| `src/lib/buildScript.test.ts` | 5 |
| `src/lib/mcpConfig.test.ts` | 5 |
| `src/lib/errors.test.ts` | 3 |
| `src/lib/theme.test.ts` | 3 |
| **5 文件** | **27** |

前端 27 条**全部通过**，vitest 自身耗时 1.5s。

## 三、测什么：按模块

计数为 `--list` 实测，顺序即条数；模块名后面的短语是该模块的职责，即它的测试钉住的范围。

### `crates/launcher-core`（126）

| 模块 | 条数 | 范围 |
| --- | ---: | --- |
| `market` | 21 | 内嵌目录解析、远端目录抓取与 404 回退、按 kind 过滤 |
| `redact` | 13 | 密钥/敏感字段脱敏（`SECRET_KEYS` 的匹配边界） |
| `instance` | 12 | 实例元数据读写、workspace 布局 |
| `diagnose` | 12 | 日志→诊断项的规则与去重 |
| `process` | 10 | pid 存活判定、ledger 归属/回收时间指纹、进程树 kill |
| `paths` | 9 | 数据根/各子目录解析 |
| `error_code` | 7 | 错误码分类与文案映射 |
| `download` | 7 | 流式下载、缓存命中、本地 HTTP 测试服务器惯例的出处 |
| `usage` | 6 | 用量累计与计价入口 |
| `telemetry` | 6 | 遥测事件落盘 |
| `pricing` | 5 | 价格表查询与换算 |
| `jobs` | 4 | 后台任务队列状态机 |
| `github` | 3 | `gh` 调用与结果解析 |
| `crash` | 3 | 崩溃分类（core 侧） |
| `provider` | 2 | 供应商配置 |
| `mcp_config` | 2 | MCP 配置改写 |
| `environment` | 2 | 环境包导出/导入 |
| `bundle` | 2 | 插件 bundle 改写 |

另：`tests/instance_system.rs` 1 条，跨模块串「建实例→落盘→读回」。

### `crates/dsh-adapter`（198）

| 模块 | 条数 | 范围 |
| --- | ---: | --- |
| `content` | 43 | skill/主题等内容的获取与落盘（含 2 条 ignored 真克隆） |
| `crash` | 25 | DSH stdout/stderr → `CrashIssue` 的规则表（含 1.4 新增的 `web_auth_refused`） |
| `lib`（crate 根） | 20 | 适配层公共入口；含 2 条 ignored 真机 e2e |
| `pnpm` | 14 | pnpm/命令调用与超时 |
| `mcp_probe` | 13 | MCP stdio 握手探测（含 HTTP 误判的快速失败） |
| `health` | 13 | 实例健康判定 |
| `mcp_import` | 10 | 未知/已知来源的 MCP 导入（严格 JSON 契约） |
| `web_check` | 9 | **1.4**：根路径 `?token=` 的 303/401/其余 分类 |
| `page_signature` | 9 | 页面签名分类器——**已标注未接线**，9 条全部计入通过数但无调用方 |
| `mcp_local` | 8 | 本地 MCP 的启动描述生成 |
| `runtimes` | 7 | 托管运行时探测与版本判定 |
| `rescue` | 7 | 救援点写入/读取 |
| `mcp_prefetch` | 6 | MCP 安装前预热 |
| `mcp_resolver` | 5 | MCP 包名/来源解析 |
| `diagnostics` | 3 | 诊断包内容组装 |
| `theme` | 2 | 主题标记读写 |
| `language` | 2 | 语言标记读写 |
| `events` | 2 | `settings/document-updated` 帧解析 |

### `apps/desktop/src-tauri`（33）

| 模块 | 条数 | 范围 |
| --- | ---: | --- |
| `commands::diagnose` | 10 | 诊断命令的输入输出与修复动作分发 |
| `usage_proxy` | 9 | chunked 透传、usage 采集、上游错误透传 |
| `commands::plugins` | 6 | 插件清单投影与启停（含 1 条 ignored 真机基线） |
| `commands::process` | 4 | 启动/停止命令的状态迁移 |
| `commands::environment` | 4 | 环境包命令 |

`tests/gui_launch_e2e.rs` 1 条，ignored（见 §四）。

## 四、24 条 ignored 是什么

它们不是「跳过」，是**有环境前提的测试**，仓库约定每条 `#[ignore]` 都带完整运行命令字符串，
`--ignored` 即可跑。按前提分四类：

| 前提 | 条数 | 位置 |
| --- | ---: | --- |
| 网络 / `npx`（真实 MCP 包、github 克隆） | 11 | `probe_e2e` 7、`content.rs` 2、`git_local_e2e` 1、`acceptance_e2e` 1 |
| 本机工具链（node/npm/cargo/uv 在 PATH 上） | 4 | `install_matrix_e2e` 3、`probe_e2e` 1（uv 缺席即断言） |
| 真机 checkout / 托管运行时（需要兄弟目录 `deepseek-harness-master` 或 P0 运行时；含 10 轮真实启停） | 2 | `lib.rs` 2 |
| 真实机器现状（读本机 `library-inventory.json` 做 161+ 插件基线） | 1 | `commands/plugins.rs` 1 |
| GUI 行走（需 tauri-driver + msedgedriver + debug 构建） | 1 | `gui_launch_e2e.rs` 1 |
| 其余（本地 git fixture、localhost 连接，跑起来很快） | 5 | `install_matrix_e2e` 5 |

两条真机 e2e 共享同一托管运行时目录，用 `REAL_E2E_LOCK` 串行，**不要并发跑**。

## 五、明确没覆盖的

按「知道它没测，且知道为什么」记录，不假装：

1. **DSH 是否真能渲染页面。** 1.4 的 `web_check` 只测到「根路径可达 + 打印的 token 被接受」，
   测不到渲染结果。AHL 把 DSH 放在**跨域 iframe**里（`apps/desktop/src/App.tsx:118`），
   Rust 侧 `eval` 够不到 iframe DOM，所以「进程活着、URL 200、页面是错误屏」这一格
   **没有**自动信号。`page_signature.rs` 就是为这一格写的，但没有可接线的执行位置，因此
   它的 9 条测试测的是**纯分类逻辑**，不是真页面。
2. **GUI 端到端。** `gui_launch_e2e` 需要 tauri-driver + msedgedriver，默认 ignored；
   窗口出现、按钮可用、iframe 真加载了 DSH 这些都在人工清单里（私有 `release-test-plan.md`）。
3. **真实 DSH 二进制。** 默认跑的所有用例都在本地夹具/本地 HTTP 服务器上；真 dsh 只在
   2 条 ignored 真机用例里出现。`web_check` 的测试服务器复刻的是读 0.1.5-rc.2
   `lib/index.js` 得到的响应形状（303+Set-Cookie / 401 text/plain），**不是**真 dsh 回包；
   真回包的形状变化只能靠 ignored 真机用例或人工发现。
4. **前端不在 CI gate 里。** 27 条 vitest + `tsc` 只有 `tsc` 进了 CI（Gate 3），
   vitest 全靠本地跑。这是当前四道 gate 的实际覆盖，不在本文件里改。
5. **Doc-tests 为 0。** 三个 crate 都没有可执行的 rustdoc 示例，文档里的代码块不受测试保护。
6. **跨 crate 契约只有一处集成测试。** `launcher-core` 与 `dsh-adapter` 之间的一致性
   （错误码、路径、清单形状）除了 `instance_system` 一条，其余靠各自的单元测试隐式对齐。
7. **并发/多实例场景。** ledger 的并发写有单元测试，但「两个启动器同时跑」的真实场景
   没有自动用例（第 1–6 条优化项的验收口径是人工的）。

## 六、约定

- **测试不算独立 commit。** 每条改动自带它需要的测试与验证；不出现「只加测试」的提交，
  也不出现「代码改了、测试下一提交再补」。
- **新增测试放哪**：纯逻辑单元测试贴在被测模块的 `#[cfg(test)] mod tests` 里；
  需要真机/网络/工具链的进 `tests/*_e2e.rs` 并 `#[ignore = "…完整命令…"]`。
  不要在单元测试里碰网络。
- **本轮（1.4）起测试冻结**：剩余计划项在冻结期内不加新测试代码；冻结期内改动的
  `page_signature.rs` 采取「标注未接线、保留 9 条测试」而非删除（删掉会净减 9 条通过数）。
- **本文件随测试面变动更新**，更新时重新跑一遍并对齐 §二 的数字。
