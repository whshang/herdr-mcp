# herdr-mcp

Expose [herdr](https://herdr.dev) as a lean MCP tool surface for ChatGPT Connector / Claude / other web LLMs. A web model orchestrates local herdr panes and agents — PI executes, Droid reviews — and the MCP server returns compact state/output summaries instead of filling the conversation with implementation transcripts.

## Positioning (third integration layer)

herdr 官方集成模式是「agent 在 pane 内、靠 `HERDR_ENV=1` 确认身份」。herdr-mcp 是反方向的**第三层**：pane 外的远程客户端（web LLM / 远程 orchestrator）经 MCP 通往本机 herdr socket——client 在远端，herdr-mcp 是它触达开发机的唯一通道。这层官方文档目前未覆盖；fs/exec 工具因此存在（通用 filesystem MCP 跑在客户端本地，够不到这台机器）。

三层边界：

| 层 | 能力 | 禁止 |
|---|---|---|
| 透传（herdr_call / herdr_methods） | 反射 `herdr api schema` 的 91 个方法 | 不自造状态推导 |
| 编排（task/wait/prompt/parallel） | workspace 生命周期 + agent 驱动 | prompt 只走 socket `agent.prompt` |
| 远程工作站（fs_read/fs_write/fs_edit/exec） | 只在 managed git root 内；并发/脏文件闸门 | 不做 headless exec（走可见 utility pane） |

异构 workspace（一个 workspace 多个 project root）是合法拓扑——herdr concepts 的 "one workspace per repo" 是建议不是硬约束；reap 闸门按此设计（dirty/multi-project 需显式 force_projects）。此点已提上游文档。

## Authorization (E)

```bash
HERDR_MCP_READONLY=1            # 所有变更操作拒绝（fs_write/fs_edit/exec + herdr_call 非幂等方法）
HERDR_MCP_WRITE_ROOTS=/a,/b     # csv 白名单；未列出的 root 只读。不设 = 允许全部 managed root
```

herdr-mcp 在 pane 外，绕过了 `HERDR_ENV=1` 的身份确认前提，默认对全部 workspace 有权限——上面两个开关是最低限度的授权边界。

## Quick Start (Endpoints)

| | URL |
|---|---|
| 公网（ChatGPT/Claude 用） | `https://xxxx.trycloudflare.com/mcp` |
| 本地（调试用） | `http://127.0.0.1:8772/mcp` |
| 插件推送（浏览器扩展，本地） | `http://127.0.0.1:8772/push/events` |

## Browser-extension push channel (`/push`)

方向相反的补充层: **herdr agent 干完活 → 浏览器插件唤醒网页 AI**。

插件本体在 `extension/`(MV3, z.ai/deepseek/claude.ai/chatgpt.com 四站适配器 +
绑定/恢复), 使用与安装说明见 [`extension/README.md`](extension/README.md)。

| Endpoint | 用途 |
|---|---|
| `GET /push/events` | SSE 长连接。事件: `hello`(权威 agent 快照, 连接即发)、`agent_working`、`agent_settled`(working→idle/done/blocked, 含 best-effort 输出片段)、`agent_output`、`agent_gone`; 15s keepalive 注释 |
| `GET /push/state` | 当前 agent 快照 JSON(background 重启对账用) |

- **鉴权复用** `HERDR_MCP_TOKEN`(同 `/mcp` 的 Bearer)。扩展从 background fetch 本地地址, 可带 Authorization 头(EventSource 不能带头)。
- **过滤**: `?agent=pi`(匹配 pane.agent 字段, 即 kind)或 `?pane=wH:p1`(最稳, pane_id 唯一)。
- **实现**: `src/push.ts` PushHub 挂在 `SnapshotCache` 的 `onEvent` 钩子上(复用既有 events.subscribe 长连接, 不另开 socket、不轮询); 状态转换检测在服务端, 扩展按自己绑定的 agent/pane 决定是否唤醒。
- **验证**: `node tests/push_sse.mjs`(auth/hello/keepalive)与 `node tests/push_sse.mjs --integration`(真实 herdr agent working→done 触发 agent_settled)。

**Auth**: 公网需要 Bearer Token。查看/复制 Token：

```bash
herdr-mcp token        # 打印并复制到剪贴板
herdr-mcp connector    # 打印完整接入信息（URL + Token + 各平台配置指引）
herdr-mcp              # 交互式菜单
```

## CLI 命令（跟 ctmc 类似）

```bash
herdr-mcp              # 交互菜单（状态/接入信息/启停/日志/复制）
herdr-mcp status       # 状态：进程/launchd/本地/公网/herdr socket
herdr-mcp connector    # 接入信息：URL + Token + ChatGPT/Claude 配置步骤
herdr-mcp start        # 启动 (launchd)
herdr-mcp stop         # 停止
herdr-mcp restart      # 重启（改代码/配置后用）
herdr-mcp logs [-f]    # 日志
herdr-mcp token        # 复制 Token 到剪贴板
herdr-mcp url          # 复制公网 URL 到剪贴板
```

## 各平台接入

### ChatGPT Connector（实测有效）
Settings → Connectors → Add new connector → 填 **MCP URL**：
`https://xxxx.trycloudflare.com/mcp` — 认证走 OAuth 自动注册（DCR），**不要填 API key**。
服务器实现 RFC 8414/9728/7591/7636/9207 的 OAuth 2.1 发现 + DCR + PKCE 流程（`/.well-known/oauth-authorization-server`、`/.well-known/openid-configuration`（RFC 8414 §5 OAuth 兼容文档，不声明 OIDC）、`/.well-known/oauth-protected-resource/mcp`；401 带 `WWW-Authenticate: Bearer resource_metadata=…`）。DCR 注册端点同时接受 `/oauth/register` 与 `/register`（ChatGPT 兼容 fallback）。

### Claude Connector
Settings → Connectors → Add → 认证选 OAuth → **Client ID / Secret 留空**（走 DCR 自动注册）→ 授权页自动完成

### OAuth 实现（`src/oauth.ts`）

- **发现**: RFC 8414 AS 元数据 + RFC 9728 受保护资源元数据，全部绝对 URL，`scopes_supported: ["mcp"]`；`openid-configuration` 返回同一 OAuth 文档（无 `openid` scope / userinfo / id_token 声明）。
- **授权码 + PKCE S256**: `code_challenge` 必填、`code_verifier` 必校验；code 一次性并绑定 client_id / redirect_uri / resource；回调带 RFC 9207 `iss`。
- **DCR 注册表 + 不透明令牌**: 客户端与 access/refresh token 原子持久化（tmp+rename，0600）到 `~/.config/herdr-mcp/oauth`（`HERDR_MCP_OAUTH_DIR` 可覆盖），重启不失效；refresh 轮换，旧 refresh 复用即拒。
- **兼容**: `HERDR_MCP_TOKEN` 静态 Bearer 依然有效（Claude 旧连接/curl 不受影响）；未配 token 时本地裸跑行为不变。
- **验证**: `node tests/oauth_flow.mjs`（发现/401 头/完整 DCR+PKCE/错误 verifier/code 复用/refresh 轮换/持久化重载/静态 token）。

### Claude / ChatGPT 都需要
配置后**开新对话**（旧对话不重读 instructions/tool descriptions）。

## Tools (7)

| Tool | 什么时候用 |
|---|---|
| `herdr_session` | **每个新对话的第一个调用**。传稳定任务 label，resume=true 恢复 workspace + handoff + agent 状态 |
| `herdr_inspect` | 看 herdr 连接 + 全部 workspaces(cwd)/tabs/panes/agents 状态 |
| `herdr_call` | 万能透传：`{method, params}` 调 herdr 任意 socket 方法（~150 个），不用记 150 个工具 |
| `herdr_wait` | 阻塞等 agent 到 idle/blocked/done。每次最多 90s（Cloudflare 安全），超时返回 `still_running` 再调一次 |
| `herdr_handoff` | **上下文快满时调**。保存 summary/pending/decisions，新对话用 herdr_session 恢复 |
| `herdr_parallel` | 一次调 N 个 agent 到 N 个 pane（PI 执行 + Droid 复核），真并行 |
| `herdr_reap` | 任务结束：收集各 agent 最终输出 + 关 workspace 回收 |

## 对话续接流程（核心场景）

ChatGPT 对话上下文满了 → 开新对话 → 新对话怎么继承旧工作？

**答案是稳定任务 label，不是对话 ID。**

```
┌─ 对话 A（快满）──────────────────────┐
│ herdr_session("fix-auth", cwd=...)   │
│ herdr_parallel([...])                │
│ herdr_handoff("fix-auth",            │
│   summary="根因已修，待集成测试",      │
│   pending=["集成测试","push"],        │
│   decisions=["保持Bearer不加OAuth"])  │
└──────────────────────────────────────┘
                 │ 开新对话
                 ▼
┌─ 对话 B（新开）──────────────────────┐
│ 第一条消息:                           │
│ "继续 herdr 任务 fix-auth，            │
│  先调 herdr_session 恢复"             │
│                                      │
│ herdr_session("fix-auth",resume=true)│
│ → 拿到 workspace + handoff + agents  │
│ → 继续只做 pending 项                 │
└──────────────────────────────────────┘
```

## 部署 / 运维

**当前已部署**：launchd `dev.herdr-mcp.server`（KeepAlive，开机自启）

```bash
# 配置文件（Token/端口/URL 都在这里）
open ~/Library/LaunchAgents/dev.herdr-mcp.server.plist

# 日志
herdr-mcp logs -f

# 修改代码后重新部署
cd ~/Documents/herdr-mcp && npx tsc && herdr-mcp restart
```

**换 Token**：编辑 plist 里的 `HERDR_MCP_TOKEN` → `herdr-mcp restart` → 各平台 Connector 重新配。

## Architecture

```
ChatGPT / Claude Connector
   │ HTTPS (Cloudflare tunnel, Bearer auth)
   ▼
herdr-mcp (Node.js, single process, port 8772)
   ├─ OAuth DCR endpoints (/.well-known, /oauth/*)
   └─ MCP Streamable HTTP (/mcp, Bearer auth)
   │ AF_UNIX newline-JSON (herdr protocol 19)
   ▼
herdr 0.8+ (workspaces / tabs / panes / agents / events)
```

## Project

```
src/
  herdr.ts    # Unix-socket client: call() + subscribe()
  server.ts   # MCP server: 7 tools + OAuth + Bearer auth (Express)
  session.ts  # durable label → workspace/handoff store
  wait.ts     # event-driven wait, 90s Cloudflare-safe chunks
bin/
  herdr-mcp   # CLI (status/start/stop/restart/logs/token/url)
dist/          # compiled JS (launchd runs this)
```

## Session Files

`~/.config/herdr-mcp/sessions/<label>.json` — 每个 task label 一个文件，存 workspace_id + handoff + agents。删掉即重置 session。
