# 架构 — herdr 与 herdr-mcp

读者：决定能力该放在 MCP 还是 herdr 原生 API 的贡献者。

## 两个进程

| 进程 | 角色 |
|---|---|
| **herdr** | 本机终端复用器 + agent 运行时。Unix socket API 很大（`herdr api schema`，约 90 个方法）。 |
| **herdr-mcp** | HTTP MCP 门面（Streamable HTTP + OAuth），让 **远程** 客户端驱动 herdr 与工作站。 |

herdr-mcp **不会** 把每个 herdr 方法都做成 MCP 工具。那会烧上下文，也重复原生 schema。

## 默认工具面（11）

| 层 | 工具 | 说明 |
|---|---|---|
| 透传 | `herdr_methods`、`herdr_call` | 反射并调用原生 socket 方法 |
| 远程编排 | `herdr_inspect`、`herdr_since`、`herdr_prompt` | 适合聊天型客户端的一瞥 / 续读 / 投递提示 |
| 远程工作站 | `herdr_fs_*`、`herdr_exec` | **不是** herdr 能力 — 远程客户端本身没有磁盘 |

`HERDR_MCP_ALL_TOOLS=1` 会加上高级/废弃生命周期工具（`herdr_wait`、`herdr_reap`、session 等）。给 ChatGPT 时建议关掉以省上下文。

## 设计规则

1. **当前产品只保留一条正确路径** — 不为想象中的第二种客户端预留配置。
2. **变更** 限制在托管 git 根内；可选 `HERDR_MCP_READONLY` / `HERDR_MCP_WRITE_ROOTS`。
3. **投递不确定** — 传输失败后不要对非幂等 prompt 盲目重试；先用 inspect/since 核对。mutation 默认走 `herdr_prompt`（省略 `wait`）并带 `idempotency_key`；状态用 `herdr_since` / `herdr_inspect`。
4. **版本是缓存键** — 工具面或握手语义变了就 bump `src/version.ts` + `package.json`。

## 错误语义（`herdr_call` / `herdr_prompt`）

| `failure` / `failure_phase` | 含义 | 可否盲重试 |
|---|---|---|
| `herdr_transport` | 真连接/socket 问题 | 视方法；mutation 仍先核对 |
| `agent_status_wait_timeout` / `post_submission_status_wait` | 投递后等 agent 状态超时（常见于带 `wait` 的 `agent.prompt`） | **否** — 先 inspect/since |
| `herdr_error` | 其它 daemon 业务错误 | 视 `retryable` |

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
- 鉴权：OAuth JWT（connector）或静态 `HERDR_MCP_TOKEN`（Cursor/curl）
- 推送（扩展）：`GET /push/events` SSE — 与 MCP 同一 Bearer

## 相关文档

- [extension.md](./extension.md) — 扩展双主线总览
- [chatgpt-connector.md](./chatgpt-connector.md) — ChatGPT OAuth、schema、权限卡
- [extension-wake.md](./extension-wake.md) — 主线 A：进度回推（检查间隔 + 新摘要才发 + 可配置兜底）
- [extension-bridge.md](./extension-bridge.md) — 主线 B：JSON→MCP
