# herdr-mcp

把 [herdr](https://herdr.dev) 暴露成 MCP，让 ChatGPT / Claude / Cursor 等远程客户端指挥本机 pane 与 agent。

English: [README.md](README.md).

## 地址

| 用途 | URL |
|---|---|
| 本机 MCP | `http://127.0.0.1:8772/mcp` |
| 公网 MCP（默认推荐） | `https://<subdomain>.trycloudflare.com/mcp` |
| 浏览器插件推送 | `http://127.0.0.1:8772/push/events` |

**给其他人的默认公网方案：** Cloudflare 免费 Quick Tunnel（`*.trycloudflare.com`），不必自备域名。把该 HTTPS 源站（不含 `/mcp`）写入 `HERDR_MCP_BASE_URL`，OAuth 发现才会和 ChatGPT/Claude 填的地址一致。

Connector 认证走 **OAuth（自动注册）**，不要填 API key。静态 Bearer 只给本机 curl / Cursor：`herdr-mcp token`。

## 接入

### 1. 公网（免费 Cloudflare）

```bash
# 终端 A — MCP 已在 :8772 运行
cloudflared tunnel --url http://127.0.0.1:8772
# → https://xxxx.trycloudflare.com

# LaunchAgent / 环境变量写入源站（不要带 /mcp）:
# HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
herdr-mcp restart
herdr-mcp connector   # 打印 …/mcp 给 ChatGPT / Claude
```

Quick Tunnel 每次重启 `cloudflared` 子域会变。要稳定主机名可用 Cloudflare 免费命名隧道，或自有域名——可选，不是默认前提。

### 2. ChatGPT / Claude

1. MCP URL：`https://xxxx.trycloudflare.com/mcp`（以 `herdr-mcp connector` 为准）
2. 选 OAuth，**不要**粘贴 Token
3. 配好后**开新对话**（旧对话会持有旧 tool snapshot）

ChatGPT 踩坑与硬性要求见 [docs/chatgpt-connector.md](docs/chatgpt-connector.md)（含「允许使用 herdr」权限卡说明）。

浏览器扩展（**主用途**：herdr 收工/进度回推 ChatGPT 网页，避免派活后对话停住）见 [docs/extension-wake.md](docs/extension-wake.md)。z.ai / DeepSeek 无 connector 时的 JSON→MCP 路线见 [docs/extension-bridge.md](docs/extension-bridge.md)。

### 3. Cursor（本机）

`~/.cursor/mcp.json` 只挂本地（同一配置里不要再挂公网，Cursor 会对相同工具面去重）：

```json
{
  "mcpServers": {
    "herdr-mcp-local": {
      "url": "http://127.0.0.1:8772/mcp",
      "headers": {
        "Authorization": "Bearer <执行 herdr-mcp token 后粘贴>"
      }
    }
  }
}
```

## 命令行

```bash
herdr-mcp              # 菜单
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart
herdr-mcp logs [-f]
herdr-mcp token | url
```

## 默认工具（为什么是这 11 个）

herdr 本体是一大套 Unix socket API（`herdr api schema`，约 90 个方法）。herdr-mcp **不会**把每个方法都做成 MCP 工具（占上下文、也和 herdr 重复），而是三层：

| 层 | MCP 工具 | 和 herdr 的关系 |
|---|---|---|
| 透传 | `herdr_methods`、`herdr_call` | 直接打到 **herdr 原生** socket。先 `herdr_methods` 查 schema，再用 `herdr_call` 调任意方法。 |
| 远程编排 | `herdr_inspect`、`herdr_since`、`herdr_prompt` | 给「只有用户发消息才跑」的网页客户端用的薄封装，基于 snapshot/events/`agent.prompt`，不是另造一套 herdr 能力。 |
| 远程工作站 | `herdr_fs_*`、`herdr_exec` | **不是** herdr 工具。MCP 客户端在远端，看不到你这台机器的磁盘；在 managed git root 里补读写与可见 shell。 |

| 工具 | 做什么 |
|---|---|
| `herdr_methods` | 列出当前 herdr socket 方法与参数 schema（反射缓存）。陌生调用前先查。 |
| `herdr_call` | 用 `{ method, params }` 调任意 herdr 方法（pane / workspace / agent 等），避免「一方法一工具」。 |
| `herdr_inspect` | 一次看清连接 + workspaces / tabs / panes / agents（cwd、状态）。通常第一个调用。 |
| `herdr_since` | 按 cursor 增量摘要，续聊时不用整包重拉状态。 |
| `herdr_prompt` | 经 socket `agent.prompt` 投递（默认 fire-and-forget；强烈建议 `idempotency_key`；状态用 `herdr_since` / `herdr_inspect`）。 |
| `herdr_fs_read` | 读本机 managed git 项目内的文件。 |
| `herdr_fs_list` | 列 managed root 下目录（跳过 `.git` / 疑似密钥名）。 |
| `herdr_fs_grep` | 在 managed root 内搜内容（优先 `rg`）。 |
| `herdr_fs_write` | 新建/覆盖文件（脏文件 / 忙碌闸门；`confirm_dirty` / `confirm_busy`）。 |
| `herdr_fs_edit` | 精确唯一字符串替换（闸门同 write）。 |
| `herdr_exec` | 在 workspace 可见的 `herdr-mcp:utility` pane 里跑 shell；同项目有 working agent 时默认拒绝（`confirm_busy` 可过）。无 secret-path 闸门。 |

可选：`HERDR_MCP_ALL_TOOLS=1` 打开高级/废弃生命周期工具。写操作限 managed git root；`HERDR_MCP_READONLY=1` / `HERDR_MCP_WRITE_ROOTS=/a,/b` 可再收紧。

## 浏览器插件

目录 `extension/`（MV3）。`chrome://extensions` 加载未打包扩展。

**两条对等主线**（见 [docs/extension.md](docs/extension.md)）：

1. **进度回推**：herdr working（新摘要 / 可配置兜底，默认 10 分钟）与 settled → 往绑定网页写提醒并提交（chatgpt / deepseek / z.ai / claude）
2. **JSON→MCP**：DeepSeek / z.ai 无 connector 时，助手 JSON → 本机 `/mcp`（路线见 [docs/extension-bridge.md](docs/extension-bridge.md)）

共享本地 `127.0.0.1:8772` 与静态 token。这不是给 DeepSeek「安装」ChatGPT 式 OAuth connector。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/extension.md](docs/extension.md) | 扩展双主线总览（中文） |
| [docs/architecture.md](docs/architecture.md) | herdr 与 MCP 分层 |
| [docs/chatgpt-connector.md](docs/chatgpt-connector.md) | ChatGPT OAuth / 传输 / schema |
| [docs/extension-wake.md](docs/extension-wake.md) | 主线 A：进度回推 |
| [docs/extension-bridge.md](docs/extension-bridge.md) | 主线 B：JSON→MCP |
| [tests/README.md](tests/README.md) | 默认测试 vs 手工脚本 |

过程笔记在 `docs/_wip/`（gitignore，不入库）。

## 运维

```bash
npx tsc && herdr-mcp restart
herdr-mcp logs -f
```

LaunchAgent：`dev.herdr-mcp.server`。会话文件：`~/.config/herdr-mcp/sessions/`。
