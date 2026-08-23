# 架构 — herdr 与 herdr-mcp

读者：决定能力该放在 MCP 还是 herdr 原生 API 的贡献者。

## 两个进程

| 进程 | 角色 |
|---|---|
| **herdr** | 本机终端复用器 + agent 运行时。Unix socket API 很大（`herdr api schema`，约 90 个方法）。 |
| **herdr-mcp** | HTTP MCP 门面（Streamable HTTP + OAuth），让 **远程** 客户端驱动 herdr 与工作站。 |

herdr-mcp **不会** 把每个 herdr 方法都做成 MCP 工具。那会烧上下文，也重复原生 schema。

## 工具面：本地默认 18，生产 ChatGPT epoch 1 为 17

MCP 工具面是**固定的**；live herdr schema 只服务 `herdr_methods` / `herdr_call`。0.3.17 起默认 17 个；**0.3.26** 增加只读 `herdr_skill`，独立/本地默认现为 18。当前生产 ChatGPT Edge 仍冻结 contract epoch 1，因此通过 `HERDR_MCP_CONTRACT_PROFILE=epoch1` 广告精确的 17-tool 0.3.23 ABI；runtime 实现已经是 0.3.26，但不强迫现有 Connector/tool snapshot 改变。

| 层 | 工具 | 说明 |
|---|---|---|
| 技能 | `herdr_skill` | 本机进程拉上游 Herdr `SKILL.md`（master，非钉版本）；失败用 `assets/herdr-agent-SKILL.md`。ChatGPT 不访问 GitHub。 |
| 透传 | `herdr_methods`、`herdr_call` | 反射并调用原生 socket 方法 |
| 远程编排 | `herdr_inspect`、`herdr_since`、`herdr_prompt` | 适合聊天型客户端的一瞥 / 续读 / 投递提示 |
| 远程工作站 | `herdr_fs_*`、`herdr_exec` / `herdr_exec_*`、`herdr_git` | **不是** herdr 能力 — 远程客户端本身没有磁盘 |

`HERDR_MCP_ALL_TOOLS=1` 会加上高级/废弃生命周期工具（`herdr_wait`、`herdr_reap`、session 等，共 30）。给 ChatGPT 时建议关掉以省上下文。若当前 catalog 含 `herdr_skill`，会话开始可用 `herdr_inspect` → `herdr_skill`（一次）→ 干活；production epoch 1 不含该工具时直接从 `herdr_inspect` 开始。

## 设计规则

1. **当前产品只保留一条正确路径** — 不为想象中的第二种客户端预留配置。
2. **变更** 限制在托管 git 根内；可选 `HERDR_MCP_READONLY` / `HERDR_MCP_WRITE_ROOTS`。
3. **投递不确定** — 传输失败后不要对非幂等 prompt 盲目重试；先用 inspect/since 核对。mutation 默认走 `herdr_prompt`（省略 `wait`）并带 `idempotency_key`；状态用 `herdr_since` / `herdr_inspect`。
4. **版本是缓存键** — 工具面或握手语义变了就 bump `src/version.ts` + `package.json`。
5. **网页主编排** — 规划与调度在网页模型；本机优先 `herdr_fs_*` / `herdr_exec`；需要 agent 时直接打便宜 worker，禁止本机 Claude/OMP/main 当中间指挥。
6. **Agent 软隐藏** — `herdr_inspect` / `herdr_since` 默认只列出执行 agent（`pi`/`cline`/`opencode`/`anti`）与审计（`droid`/`grok`）；Claude/OMP/Codex 不出现在列表。`herdr_prompt` **不拦**。`HERDR_MCP_AGENT_ALLOW=*` 显示全部；逗号名单可覆盖默认。
7. **本地默认 18 / production epoch1 17** — `HERDR_MCP_ALL_TOOLS=1` 时 30；`inspect` 含 `boot_id` + `exec_sessions` + `agent_skill` 状态。epoch1 下会明确标记 `herdr_skill` 已由 contract 隐藏，而不是提示调用一个不存在的工具。`HERDR_MCP_READONLY=1` 挡住含 `herdr_prompt` 在内的 mutation（`herdr_fs_patch` 的 `dry_run` 除外）。
8. **工作站稳健性（≥0.3.17）** — `commitAtomic` 失败会删掉本次新增文件；exec journal 仅杀掉仍带 `HERDR_MCP_EXEC_ID` 的孤儿；`exec_read stream=both` 按写入顺序交错；`fs_read` 字节截断只返回完整行。
9. **`herdr_exec` 控制面降级（≥0.3.18）** — utility 窗格在 `send_text` **之前**若连续撞 TaskGroup，自动改本地 zsh（`backend:local_fallback`）；一旦已投递则绝不重发、也不降级（避免双跑）。
10. **`herdr_git` 本机降级（≥0.3.20）** — `session.snapshot` / managed-roots 闸门因 TaskGroup 不可用时，对 `$HOME`（或 `HERDR_MCP_WRITE_ROOTS`）下的真实 git 根仍直接跑本地 `git`（带 `warnings`）；`pane.read` 等只读 RPC 对 TaskGroup 透明重试加长到最多 4 次尝试。
11. **`herdr_inspect` / `liveSnapshot` list 降级（≥0.3.21）** — `session.snapshot` 撞 TaskGroup 且 cache 不够用时，改拼 `workspace.list` + `pane.list` + `agent.list`，`warnings` 含 `snapshot_failed_used_list_apis`；勿把控制面异常当成仓库阻塞。
12. **Unix socket 读超时 + 60s RPC 上限（≥0.3.23）** — 每条 socket RPC 在连接后 `setTimeout`（不再无限等 `session.snapshot`）；`herdr_exec` / `herdr_wait` / `herdr_prompt` wait 均 **≤60s**；`herdr api schema` 启动预热 + stale 缓存（tools/list 不依赖 live herdr）；SnapshotCache bootstrap 优先 bounded snapshot，失败走 list APIs（对齐 coding-tools-mcp：**固定 MCP 工具面**，live 只作运行时）。

## 错误语义（`herdr_call` / `herdr_prompt` / 只读聚合）

| `failure` / `failure_phase` | 含义 | 可否盲重试 |
|---|---|---|
| `herdr_transport` | 真连接/socket 问题 | 视方法；mutation 仍先核对 |
| `agent_status_wait_timeout` / `post_submission_status_wait` | 投递后等 agent 状态超时（常见于带 `wait` 的 `agent.prompt`） | **否** — 先 inspect/since |
| `herdr_internal` / `control_plane_taskgroup`（或 `snapshot_refresh`） | daemon 控制面 TaskGroup / ExceptionGroup 偶发；**不是** pane 没了、也不是 prompt 投递超时。`herdr_prompt` ≥0.3.22 也归这类（带 `delivery_uncertain`），不再只回裸 `UNKNOWN` | 只读可重试；**`agent.prompt` 不可盲重试** — 先 inspect/since |

| `herdr_error` | 其它 daemon 业务错误 | 视 `retryable` |

## 控制面瞬时失败（ExceptionGroup / TaskGroup）

范围：ChatGPT ↔ herdr-mcp ↔ herdr daemon/socket ↔ workspace/pane/agent 状态层。  
**不是** 业务仓库代码，也不是 Claude/OMP runtime。

典型现象：agent 仍在 `working`，但 `inspect` / `since` / `pane.read` / `fs_*` 间歇返回失败；几秒后同样请求又成功。根因在 herdr daemon 的并发聚合（snapshot / events.subscribe / socket 重连），某个 child task 抛错时未隔离，整次 RPC 被包成裸 `ExceptionGroup`。

### 本机 watchdog（macOS LaunchAgent，≥0.3.22，随 0.3.26 发布）

`herdr-mcp watchdog install` 注册 LaunchAgent（默认每 120s）。Linux / Windows 没有等价 CLI。

- MCP 进程没了或本机 `/mcp` 非 200/401：连续失败达到阈值后 `herdr-mcp restart`（默认冷却 10 分钟，**无每日次数上限**）
- `agent.list` / `workspace.list` 撞 TaskGroup 或 socket 缺失：**只记日志**，不重启 herdr daemon，也不重发 `herdr_prompt`
- 状态：`~/.config/herdr-mcp/watchdog.state.json`；日志：`watchdog.log`

herdr-mcp 侧（≥0.3.12，exec 降级 ≥0.3.18）能做的：

- 只读自动重试（控制面毛刺最多 2 次）
- `failure=herdr_internal` + `code=snapshot_refresh_failed` + `retryable=true`（展开子异常文案，不让裸 ExceptionGroup 当唯一信息）
- `fs_*` / inspect 在 snapshot 失败时尽量用 SnapshotCache
- `herdr_exec`：`send_text` **之前** TaskGroup → 重试后 `backend:local_fallback`；已投递则返回结构化错误，禁止降级/重发
- `herdr_git`（≥0.3.20）：snapshot/managed-roots 失败时对本机 `$HOME` 下 git 根直接 `spawn git`（不经 pane）
- `herdr_inspect` / `liveSnapshot`（≥0.3.21）：snapshot 失败时拼 `workspace.list`(+pane/agent.list)，带 `warnings`
- mutation 失败后仍要求先 inspect/since，禁止盲重试

消不掉的：daemon 里 TaskGroup 未隔离 / 未 flatten 的真正根因，需 herdr 上游修。`pane.read` / 全量 `session.snapshot` 仍可能偶发；编排侧应继续优先 `herdr_fs_*` / `herdr_git` / `herdr_exec`（utility 投递前 TaskGroup → `local_fallback` / 独立本机 session），不要把控制面毛刺当业务阻塞。

`herdr_prompt` 成功时带 `state_observation: { changed: true\|false\|"unknown", fresh }`；无 `wait` 时未变快照为 `"unknown"`（不是「没投递」）。兼容字段 `state_changed` 仍保留。

## 远程工作站闸门

| 闸门 | `herdr_fs_*` | `herdr_exec` |
|---|---|---|
| readonly / write_roots | 有 | 有 |
| secret-path（路径校验） | 有 | **无**（自由 shell 可 `cat .env`；文件 IO 请用 fs） |
| working agent | edit/write 默认拒绝，`confirm_busy` 可过 | 默认拒绝，`confirm_busy` 可过 |
| dirty confirm | edit/write 有 | **无**（脏树上跑命令是常态） |

## 传输

- MCP：`/mcp` 上的 `POST/GET/DELETE`（ChatGPT 探测还会用 issuer 根 `/` 别名）
- 鉴权：OAuth JWT（connector）或静态 `HERDR_MCP_TOKEN`（Cursor / curl / 扩展）。不要把静态 token 贴进 ChatGPT connector UI。
- 推送（扩展，同一 Bearer）：
  - `GET /push/events` SSE（可 `?workspace=`）
  - `GET /push/state` 当前 agent / workspace / pane 快照
  - `GET /push/mcp-activity` 最近 `tools/call` 计数（进程内环形缓冲；当前扩展催促走小模型判定，不再用「零工具」启发式）

## Environment variables

环境变量。plist 示例里的 `HERDR_MCP_HOST` **未被** Node 进程读取；服务听在端口上，由隧道/本机回环访问。版本演进见 [CHANGELOG.md](../CHANGELOG.md)。

| 变量 | 默认 | 作用 |
|---|---|---|
| `HERDR_MCP_TOKEN` | 空 | `/mcp` 与 `/push` 静态 Bearer |
| `HERDR_MCP_PORT` | `8772` | 监听端口 |
| `HERDR_MCP_BASE_URL` | 空 | 公网 origin，**不要** `/mcp` 后缀；OAuth `iss`/`aud` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | herdr API socket |
| `HERDR_MCP_READONLY` | 关 | 挡住 mutation（含 `herdr_prompt`；`fs_patch` `dry_run` 除外） |
| `HERDR_MCP_WRITE_ROOTS` | 全部 managed root | 允许写入的根，CSV |
| `HERDR_MCP_ALL_TOOLS` | 关 | 18 → 30 工具 |
| `HERDR_MCP_AGENT_ALLOW` | worker + 审计 | `*` 或逗号名单；影响 inspect/since 列表，不拦 `herdr_prompt` |
| `HERDR_MCP_STATE_DIR` | `~/.config/herdr-mcp` | exec journal / sessions |
| `HERDR_MCP_OAUTH_DIR` | `~/.config/herdr-mcp/oauth` | JWT 密钥与 client 登记 |
| `HERDR_MCP_OAUTH_ACCESS_TTL_S` | `86400` | access token TTL |
| `HERDR_MCP_OAUTH_REFRESH_TTL_S` | `2592000` | refresh token TTL |
| `HERDR_MCP_PUSH_DEBUG` | 关 | `/push` 调试日志 |
| `HERDR_MCP_BUILD_COMMIT` / `HERDR_MCP_BUILT_AT` | `dev` / 启动时刻 | `inspect.workstation_info` |
| `HERDR_SKILL_URL` | herdr master `SKILL.md` raw URL | `herdr_skill` 上游 |
| `HERDR_SKILL_CACHE_SEC` | `3600` | skill 缓存 |
| `HERDR_SKILL_FETCH_TIMEOUT_MS` | `15000` | skill 拉取超时 |
| `HERDR_SKILL_NETWORK` | 开 | `0` = 只用内置副本 |

## 相关文档

- [CHANGELOG.md](../CHANGELOG.md) — 版本与工具面
- [capability-benchmark.md](./capability-benchmark.md) — 官方 Herdr / 其他 Herdr MCP / coding-tools-mcp 能力吸收与“不吸收”决策
- [extension.md](./extension.md) — 扩展总览（A 已可用，B 未完成）
- [chatgpt-connector.md](./chatgpt-connector.md) — ChatGPT OAuth、schema、权限卡
- [extension-wake.md](./extension-wake.md) — 主线 A：进度回推（检查间隔 + 新摘要才发 + 可配置兜底）
- [extension-bridge.md](./extension-bridge.md) — 主线 B：JSON→MCP（解析有、闭环无）
