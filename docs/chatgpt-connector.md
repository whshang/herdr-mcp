# ChatGPT Connector 经验备忘

读者：给 ChatGPT 接 herdr-mcp 的人，以及改 OAuth / Streamable HTTP 的代理。

相关入口：[README.md](../README.md)、`src/oauth.ts`、`src/server.ts`。

## 「已连接」到底指什么

ChatGPT 有两层：

1. **OAuth / 安装 connector** — 设置里能看到插件。
2. **工具 schema 注册** — `tools/list` 成功，且 ChatGPT 接受每一份 `inputSchema`。

可能出现：设置里已安装，但当前对话 **0 个工具**。常见原因是 (2) 失败，或旧对话还握着旧工具快照。重连后请 **开新对话**。反复装 2～3 次多半是缓存，不一定是服务挂了。

## 公网 URL

- ChatGPT 用的资源地址：`{HERDR_MCP_BASE_URL}/mcp`
- Issuer / OAuth 发现：`{HERDR_MCP_BASE_URL}`（环境变量不要带 `/mcp` 后缀）
- 默认免费公网：Cloudflare Quick Tunnel `*.trycloudflare.com`
- 改公网源后要重启 herdr-mcp，让 JWT 的 `iss` / `aud` 对齐

## 工具权限卡：「允许 ChatGPT 使用 herdr？」

这是 **ChatGPT 网页自己的审批 UI**，不是 herdr-mcp 服务端开关。

| 你想做的事 | 现实 |
|---|---|
| 服务端强制「全部自动允许」 | **做不到。** Connector 网页没有稳定的 `require_approval: never`；那是 Responses API 开发者参数，不是 chatgpt.com 设置项 |
| 点一次「Always allow」后永久生效 | 社区反馈不稳定，有时会丢会话 / 重走 OAuth |
| 少点几次「允许」 | 装本仓库浏览器扩展；在 **chatgpt.com** 标签页里对页面内权限卡片自动点「允许」（见下） |

扩展行为（`extension/`，内容脚本 ≥ 0.1.3）：

- 识别页面内「允许 / 拒绝」工具权限卡（含 `data-testid=tool-action-buttons`）
- **chatgpt.com 打开后常驻观察**（不再只绑在 herdr→网页「唤醒」的 90 秒窗口）
- 只点可见、可用、文案明确为允许类的按钮；有拒绝按钮同卡才点（fail-closed）
- **点不了**：浏览器原生权限条、非 DOM 的系统对话框

服务端可做的只是诚实标注（例如 `readOnlyHint`）；ChatGPT 目前常忽略，仍可能把只读工具当成写操作来问。

每次工具调用仍弹卡时：确认扩展已加载、当前标签是 `chatgpt.com`、内容脚本版本 ≥ 0.1.3（扩展重载后旧标签会自动刷新）。

## OAuth（CIMD）

ChatGPT 偏好 **Client ID Metadata Document**（`https://chatgpt.com/oauth/.../client.json`），不是经典 DCR 密钥。

必须通：

| 步骤 | 期望 |
|---|---|
| Protected-resource 元数据 | `/.well-known/oauth-protected-resource` 与 `.../mcp` |
| AS 元数据 | `/.well-known/oauth-authorization-server`（含 `/mcp` 下变体） |
| OpenID | `/.well-known/openid-configuration`（ChatGPT 会探；404 曾直接中断连接） |
| Authorize | PKCE 自动批准跳转 |
| Token | `authorization_code` + 可选 `private_key_jwt`；拉取 ChatGPT JWKS |
| Access token | JWT，`aud` = 资源 URL |

不要把静态 Bearer 贴进 ChatGPT connector UI。静态 token 给 Cursor / curl。

线上见过：`client_assertion` 拉 JWKS 超时 → token `400`；重试通常好。

## MCP 线路（UA `openai-mcp`）

| 规则 | 原因 |
|---|---|
| 完全 **无状态** — 不发 `Mcp-Session-Id` | 重启后陈旧 sid → 客户端 `-32600 Session terminated` |
| 忽略未知 sid | 同上 |
| OAuth 后 `server/discover` 必须成功 | 回 `-32601` 会卡在 `initialize` 前 |
| discover 列表：**SDK 版本在前**，保留 `2026-07-28` | 发现能完成；线上仍偏好 `2025-11-25` |
| 请求头 `Mcp-Protocol-Version: 2026-*` → 改写为 `2025-11-25`（`req.headers` **和** `rawHeaders`） | Hono 用 `rawHeaders` 建 Web Request；只改 headers 等于没改 → SDK `400 Unsupported protocol version` |
| `initialize` / `tools/list` → **SSE** | 全改 JSON（0.3.6）曾出现 OAuth OK、initialize OK、却不再跟 `tools/list` |
| `tools/call` → JSON 可以 | 隧道下大载荷更稳 |
| 一次性 transport 只在 `res` finish/close 时关 | `finally` 里抢关会和 SDK `_closed` 竞态 → `404/-32001` |
| 鉴权失败 JSON-RPC 不要用 `-32600` | ChatGPT 会显示成「Session terminated」 |

识别 ChatGPT：UA `openai-mcp`，或 OAuth JWT 的 `client_id` / `sub` 落在 `chatgpt.com`。

## ChatGPT 会整表丢弃的 schema

一个坏工具就能让 **整张工具表** 消失，connector 看起来仍已安装。

`inputSchema` 里避免：

- `propertyNames`
- `additionalProperties: {}`（空对象；Zod `z.record` 常长这样 — 要用布尔 `true`、有类型的 schema，或别用自由对象）
- `exclusiveMinimum`（Zod `.positive()` → 改 `.min(1)`）

`herdr_call.params` 对外标成 **string**（JSON 对象文本）。运行时仍用 preprocess 接受真对象。

改工具面或握手时 bump `SERVER_VERSION` / `package.json`，逼客户端重新 `tools/list`。

## 「TaskGroup」/ omp 挂了却说读不了文件

交叉验证（健康的 0.3.9+）：

| 工具 | 期望 |
|---|---|
| `herdr_fs_list` / `herdr_fs_read` / `herdr_fs_grep` | 托管 git 根下 `ok: true` |
| `herdr_exec` | 工作区 utility 窗格有退出码和输出 |
| `herdr_call` `agent.start` | 同一窗格二次启动可能 `error` — 这是 herdr，不是 fs |

上述都通，则 TaskGroup **不是** herdr-mcp 文件通道故障。常见情况：

1. 把「读项目」走成了 `herdr_prompt` / agent 工具，而不是 `herdr_fs_*`
2. 窗格 agent 崩了；日志是 `call=agent.*`，不是 `tool=herdr_fs_*`
3. 占用中的窗格又 `agent.start` 一次

访问日志在 `herdr_call` 上会带 `call=<method>`（只记方法名）。读内容优先 `herdr_fs_*` + `herdr_exec`。

## 排障清单

1. 连接时 `herdr-mcp logs -f`
2. 期望：authorize → token `200` →（可选 discover）→ initialize → `notifications/initialized` → **`tools/list`**
3. token `200` 但无 initialize：发现端 / token 形态 / 客户端中止
4. `tools/list` `200` 但对话 0 工具：schema 拒收或 **旧对话快照** → 开新对话
5. `/.well-known/mcp.json` 的 `version` 是否等于你以为在跑的构建
6. 「读不了文件」：确认失败调用是 `herdr_fs_*` / `herdr_exec`，不是 `herdr_call call=agent.*`
7. 权限卡不停：扩展是否在 chatgpt.com、内容脚本 ≥ 0.1.3

## ChatGPT 派活后对话停住

Connector 只解决「ChatGPT → herdr」。若工具很快返回「已提交」，而 agent 仍在窗格里跑，网页对话常不再自动 `herdr_since` / 继续。

闭环要靠浏览器扩展：绑定该 chatgpt 会话 ↔ 干活的 pane，agent **settled**（以及将来的进度 tick）时往输入框塞继续提示并提交。见 [extension-wake.md](./extension-wake.md)。

未绑定扩展时，这不是 MCP 故障，是缺回推环。
