# dsh 契约清单（DSH Contract Inventory）

> **性质**：活文档。列出 AHL 与外部系统 dsh（`@deepseek-ai/dsh`，不受本项目控制的上游）之间的**全部**依赖点。
> **谁维护**：任何改动"与 dsh 交互"的代码的 PR，必须同步更新本表（新增行 / 修改兜底列 / 更新 file:line）。
> **何时读**：动 `crates/dsh-adapter/src/`（`lib.rs` 的 launch / plugin_inventory / cordis.patch 编译、`theme/llm/language/events/content/diagnostics/runtimes.rs`）、`apps/desktop/src-tauri/src/commands/{process,plugins,content}.rs`、`tui/` 之前，**先查本表**。
> **评审基准日**：2026-09-12（对照 AHL `main`，由 `docs/absorb-plan.md` Phase 0.5 产生）。

## 使用规则

1. **强假设**（字符串匹配 / 全量解析 / 无文档布局）是升级脆弱点，dsh 发新版时按"脆弱度排序"逐行过一遍。
2. **哨兵**列 = 锁定该契约的测试。样本测试失败 = dsh 变了：先确认 dsh 行为变化是否可接受，再改样本。
3. 新增与 dsh 的交互点时，优先选"弱假设 + 超时兜底 + fail-open"形态，并在本表登记。
4. 脆弱度总排序见文末「附二」。

---

## 一、CLI 输出格式（字符串匹配）—— 最脆弱

AHL 不调用就绪 API，而是 grep dsh 打印的 ready-line。桌面与 TUI 侧车各有一套独立解析器。

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设（上游改动即断裂的点） |
|---|---|---|---|---|
| 1 | ready-line `dsh web: http://127.0.0.1:<port>/?token=<token>` | 强 | `apps/desktop/src-tauri/src/commands/process.rs:535-548`（`parse_dsh_url`）+ `:551`（`url_port`） | 靠字符串定位 `http://127.0.0.1:` 前缀与 `/?token=` 切出端口/token；任何措辞、host 写法（IPv6 `[::1]`、`localhost`、`0.0.0.0`）、参数顺序改动都会取不到端口或 token |
| 2 | 正则 `/http:\/\/127\.0\.0\.1:(\d+)\/\?token=(\S+)/` | 强 | `tui/host/index.js:32`（`WEB_URL_RE`）+ 扫描 stdout/stderr `:119-142` | `\S+` 假设 token 不含空白；旧 build 打印裸 URL（无 token）则永不匹配、侧车永不 `DSH_READY` |
| 3 | `DSH_READY <port> <token>` 行协议 | 强 | `tui/src-tauri/src/sidecar.rs:122-137` | host 重打包成 `DSH_READY` 前缀行，侧车按空格切两个字段 |
| 4 | ready-line 里的 `?token=` 查询参数 | 强 | `crates/launcher-core/src/redact.rs`（`SECRET_KEYS` 含 `token`）+ 测试 | 假设 token 以 `token=` 查询参数出现；脱敏依赖该 key 名。**哨兵**：`redact.rs` 13 单测 |

---

## 二、$DSH_HOME 磁盘布局（无文档，脆弱）

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 5 | `$DSH_HOME/profiles/<profile>/` | 强 | `crates/dsh-adapter/src/lib.rs:363-368`（`profile_dir`） | profile 目录固定为该相对路径 |
| 6 | `$DSH_HOME/profiles/<profile>/package.json` | 强 | `lib.rs:380-464` | profile 元数据在该 JSON 文件 |
| 7 | `$DSH_HOME/node_modules/`（扁平回退 `profiles/node_modules`） | 强 | `diagnostics.rs:70-78`（`resolve_bundle_dir`） | bundle 实际落在 node_modules |
| 8 | `$DSH_HOME/skills/<id>/SKILL.md` | 强 | `content.rs`（`install_skill`） | skill 落地为该路径单文件；frontmatter 的 `name` 字段是元数据 |
| 9 | `$DSH_HOME/cordis.patch.yml`（home 级） | 强 | `content.rs:889-905`（`sync_mcp_patch`） | 存在 home 级 patch 文件 |

---

## 三、cordis.patch.yml 配置格式（无文档，脆弱）

格式约定：顶层 YAML 数组；行有 `- id:` + `disabled: true|false`；插入块 `- insert:`；`[]` 占位 / `# []` 注释恢复；id 前缀 `skin-` / `mcp-`；包名 `@deepseek-ai/dsh-mcp-client`；行 id 字符集 `[A-Za-z0-9._-]`；serverName `[A-Za-z0-9_-]{1,32}`。

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 10 | `- id:` 行 + `disabled: true|false` | 强 | `lib.rs:765-1301`（`parse_id`/`read_patch_state`/`parse_inserted_ids`） | 每行靠 `id` 标识，开关靠 `disabled` 布尔；解析/重写依赖该 YAML 结构 |
| 11 | `- insert:` 块 | 强 | `lib.rs:765-1301`（`append_block_to_text`/`remove_*_insert_blocks`） | 安装即追加 insert 块；卸载靠识别自己插入的块删除 |
| 12 | `[]` 占位 / `# []` 注释恢复 | 强 | `lib.rs`（`restore_placeholder`） | 空 patch 用 `[]` 占位，`# []` 标记可恢复 |
| 13 | 行 id 前缀 `mcp-`、包名 `@deepseek-ai/dsh-mcp-client` | 强 | `content.rs:861-873`（`mcp_insert_row`） | MCP 行 id = `mcp-<serverName>`，条目引用该客户端包 |
| 14 | 行 id 前缀 `skin-` | 强 | `content.rs:912-924`（`skin_id_from_package`） | skin 行 id = `skin-<…>` |
| 15 | 行 id 字符集 `[A-Za-z0-9._-]` | 强 | `lib.rs:765-1301`（`is_valid_row_id`） | 校验/生成 id 只允许这些字符 |
| 16 | serverName `[A-Za-z0-9_-]{1,32}` | 强 | `content.rs:861-873` | MCP server 名限制 |
| 17 | 双位置读取：`<profile>/cordis.patch.yml` 与 `<workspace>/cordis.patch.yml` | 强 | `diagnostics.rs:261-264` | 两个层级都可能存在 patch，诊断要都读 |

---

## 四、WebSocket 事件

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 18 | 路径 `ws://127.0.0.1:{port}/api/events.host` | 强 | `events.rs:33` | host 事件流固定挂在该路径（普通 GET 应答 `426`） |
| 19 | 帧类型 `host/remote-event`（`payload.type`） | 强 | `events.rs:73-87`（`handle_frame`） | 每帧 JSON，靠 `type` 字段区分业务事件 |
| 20 | 事件名 `settings/document-updated` | 强 | `events.rs:73-87` | 设置变更通知固定用该事件名 |
| 21 | `args[0]` = namespace（仅 `ui-theme`/`locale`） | 强 | `events.rs:19-20, 73-87` | 事件首参即命名空间，AHL 据此判断刷新主题还是语言 |
| 22 | `s/applies: "live"` 热生效 | 强 | `events.rs:5-10`、`theme.rs`、`language.rs` | 设置写入对运行中的 DSH 窗口热生效（依赖 launcher→DSH 主题/语言推送） |

---

## 五、HTTP API / RPC 信封

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 23 | `POST http://127.0.0.1:{port}/api/{method}`（路径即方法） | 强 | `theme.rs:55-77`（`host_rpc`） | 所有 host RPC 走 `/api/<method>` 路径拼接；apiproxy POST-only |
| 24 | 信封 `{type:"client-request", rpcId, method, payload}` | 强 | `theme.rs:55-77` | 请求体固定该结构；rpcId 用 `launcher-{method}-{pid}` 生成 |
| 25 | 响应取 `.result` 字段 | 强 | `theme.rs:55-77` | 业务结果在 `result` 字段，不在顶层 |
| 26 | 业务层 `{ok:true, value}` / `{ok:false, error}` | 强 | `theme.rs:81-89`（`ensure_ok`） | 错误也走 HTTP 200，靠 `ok` 布尔区分成败，不看状态码 |

---

## 六、SDK-RPC 方法面

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 27 | 方法 `pluginInventory/list` | 强 | `lib.rs:468-512`（`plugin_inventory`） | 返回插件清单；AHL 兼容单层/双层包裹的响应形状 |
| 28 | 方法 `settings.mutate`（`{ns, ops:[{op:"set", path, value}]}`） | 强 | `theme.rs` / `llm.rs` / `language.rs` | 用 `set` 操作按 JSON path 写设置 |
| 29 | 方法 `settings.describe` | 强 | `theme.rs` / `language.rs` | 读取当前设置 |
| 30 | namespace `ui-theme` | 强 | `theme.rs` | 主题偏好存在该命名空间 |
| 31 | namespace `locale`（path `locale.preference`，`zh`/`en`） | 强 | `language.rs` | 语言偏好字段与取值 |
| 32 | namespace `llm-deepseek`（path `baseURL`、`models`） | 强 | `llm.rs` | 模型/端点配置存在该命名空间 |

---

## 七、package.json `dsh.*` 清单语义（部分文档）

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 33 | `dsh.profile.bundles`（JSON path `/dsh/profile/bundles`） | 强 | `lib.rs:380-464`（`installed_plugins`）+ `content.rs` + `diagnostics.rs` | profile 清单声明 bundle 列表；AHL 据此识别已装 bundle |
| 34 | `dsh.bundle` / `dsh.client` | 中 | `diagnostics.rs` + `content.rs` | bundle/client 元数据字段 |
| 35 | `dsh.bundle.patch` | 中 | `diagnostics.rs` | bundle 自带的 patch 声明 |
| 36 | `dsh.bundle.order.before/after` | 中 | `diagnostics.rs` | bundle 排序约束 |

---

## 八、环境变量（进程间契约）

| # | 依赖项 | 读/写 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 37 | `DSH_HOME` | 写（= instance.workspace） | `lib.rs:304-318`（`build_env`） | dsh 用该变量定位 home；AHL 用实例 workspace 覆盖 |
| 38 | `DEEPSEEK_API_KEY` | 写 | `lib.rs:304-318` | dsh 从该变量取 key |
| 39 | `DEEPSEEK_BASE_URL` | 写 | `lib.rs:304-318` + `process.rs:134`（改写指向 usage proxy） | dsh 从该变量取端点 |
| 40 | `DSH_TELEMETRY_DISABLED=1` | 写 | `lib.rs:304-318` | 用该变量关遥测 |
| 41 | `DSH_CLI_BIN` | 读 | `lib.rs:155-193`（`resolve_bin`） | 用户可用该变量指定 dsh 二进制位置 |

> AHL 自身的 `AHL_HOME` / `AHL_PORTABLE` 不属 dsh，未列入。

---

## 九、CLI 子命令 / flag（有文档，最稳）

| # | 依赖项 | 强/弱 | 使用位置 | AHL 的假设 |
|---|---|---|---|---|
| 42 | 子命令 `web`（`dsh web` = web profile） | 弱 | `lib.rs:321-359`（`launch`）：`cmd.arg("web")` | `web` 是合法子命令且对应 web profile |
| 43 | flag `--host 127.0.0.1` | 弱 | `lib.rs:321-359` | 绑定回环地址 |
| 44 | flag `--port 0` | 弱 | `lib.rs:321-359` | `0` 让 dsh 自选端口（配合 ready-line 解析，避免实例间 3080 冲突） |
| 45 | 默认端口 `3080`（`DEFAULT_WEB_PORT`） | 弱 | `lib.rs:53` | 仅默认值，实际以 ready-line 为准 |
| 46 | 子命令 `plugin` + flag `--profile <profile>` | 弱 | `lib.rs:589-624`（`run_plugin_command`） | 插件管理走 `dsh plugin --profile <profile> <args>` |
| 47 | `plugin add` / `remove` / `update` | 弱 | `commands/plugins.rs:1337/1351/1382/1459/1666` | 经 `run_plugin_command` 转发 |
| 48 | flag `--no-open` | 弱 | `tui/host/index.js:109` | 阻止 dsh 自动打开浏览器（**仅 TUI 侧传此 flag**；桌面侧源码 checkout 不需要，见 `lib.rs:337-342` 注释） |

---

## 十、前端 TS 侧（IPC / 契约假设）

| # | 依赖项 | 使用位置 | AHL 的假设 |
|---|---|---|---|
| 49 | `dshUrl` 状态 + `portFromUrl` | `apps/desktop/src/stores/appStore.ts:62-68` | 就绪 URL 字符串的前端解析；端口从 URL 提取 |
| 50 | `dsh-settings-changed` 事件监听 | `appStore.ts` | 设置变更后重新拉取主题/语言 |
| 51 | `dsh_theme` / `dsh_language` | `appStore.ts` | 主题/语言设置值经 IPC 暴露给前端 |
| 52 | 插件 IPC（install/uninstall/toggle/update） | `appStore.ts` | 前端插件操作最终映射到 `dsh plugin` 子命令 |

---

## 附一：已核实为「不存在」的依赖（未在已执行代码中出现）

以下 dsh 接口 AHL **目前不依赖**（仅在 `docs/` 路线图里被提及，或属于计划中的待补项）。dsh 升级时这些无需检查：

- `--dump-config`（配置树导出）
- `--patch`（配置补丁校验）
- `settings.yaml`（设置文件）
- `cordis.yml`（主配置）
- `.credentials.yaml`（凭据文件）
- `pnpm-workspace.yaml`
- `session.jsonl.zstd`（会话日志）
- `/api/session.list`（会话列表 API）

> 这几项恰是 `docs/absorb-plan.md` Phase 3 计划**新增**的能力（配置全链路校验、会话日志解码）。落地时**必须回到本表补登记**，不要出现"代码用了、表里没有"的孤儿依赖。

**排除项**（非 dsh 依赖，不登记）：
- `mcp_import.rs` 解析的是 Claude/Cursor/VSCode 的 MCP 配置格式，不是 dsh 格式。
- `tui/host/reference/*.ts` 是拷贝的上游 dsh 源码，仅作参考，不被执行。

---

## 附二：脆弱度总排序（高 → 低）

1. ready-line 字符串匹配（最脆）
2. $DSH_HOME 磁盘布局（无文档）
3. cordis.patch.yml 格式（无文档）
4. WebSocket 路径 / 事件名
5. `/api/{method}` RPC 信封 + 业务 `{ok, value|error}`
6. SDK-RPC 方法面（`pluginInventory/list`、`settings.mutate/describe`、namespaces）
7. package.json `dsh.*` 语义（部分文档）
8. 环境变量（`DSH_HOME` 等）
9. CLI 子命令 / flag（有文档，最稳）

## 附三：关键源文件

- `crates/dsh-adapter/src/lib.rs`（launch / plugin_inventory / cordis.patch 编译 / resolve_bin / build_env）
- `crates/dsh-adapter/src/{theme,llm,language,events,content,diagnostics,runtimes}.rs`
- `apps/desktop/src-tauri/src/commands/{process,plugins,content}.rs`
- `crates/launcher-core/src/redact.rs`
- `tui/host/index.js`、`tui/src-tauri/src/sidecar.rs`
- `apps/desktop/src/stores/appStore.ts`

---

## 维护记录

| 日期 | 变更 |
|---|---|
| 2026-09-12 | 首版。由 `docs/absorb-plan.md` Phase 0.5 产出，覆盖 52 条依赖 + 8 条已核实不存在 + 脆弱度排序。 |
