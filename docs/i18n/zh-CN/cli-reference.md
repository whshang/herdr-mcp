# herdr-mcp CLI：本机管理与运维命令

本页只记录 **herdr-mcp** 自己的命令面。Herdr 本体的 workspace、pane、agent、session CLI 请看官方 [Herdr CLI Reference](https://herdr.dev/docs/cli-reference/)。

herdr-mcp 的命令可以按用途理解，而不是按 `bin/` 文件名死记。

## 日常管理：`herdr-mcp`

macOS 上最常用的是项目的管理 CLI：

```bash
herdr-mcp status
herdr-mcp start
herdr-mcp stop
herdr-mcp restart
herdr-mcp logs
herdr-mcp logs -f
```

它管理本机 herdr-mcp runtime 的 LaunchAgent 生命周期。

典型开发循环：

```bash
npm run build
herdr-mcp restart
herdr-mcp status
```

如果只是临时开发，也可以直接：

```bash
npm run dev
# 或
node dist/server.js
```

此时不需要 LaunchAgent。

## Connector 信息

```bash
herdr-mcp connector
```

用于查看当前 Connector / 公网入口相关信息。

本机静态 bearer 和公网 ChatGPT OAuth 是两套边界。不要因为 CLI 能显示本地连接信息，就把 `HERDR_MCP_TOKEN` 复制进 ChatGPT。

完整接入见 [ChatGPT Connector](chatgpt-connector.md)。

## 日志与健康检查

```bash
herdr-mcp status
herdr-mcp logs
herdr-mcp logs -f
```

排障时最好先记录状态再重启。

建议组合：

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
herdr-mcp status
herdr-mcp logs
```

本机 HTTP 返回 `200` 或 `401` 都说明 runtime 在监听；连接失败才说明进程/端口层有问题。

## Watchdog

macOS 可以安装本机 watchdog：

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

它用于发现 herdr-mcp runtime 自身失联并按受控策略恢复，不把每一次 Herdr TaskGroup/ExceptionGroup 控制面毛刺都当成 daemon 必须重启。

因此 watchdog 是 runtime 可用性保护，不是“看到任何错误就全家桶重启”。

## UI 语言

```bash
herdr-mcp lang en
herdr-mcp lang zh
herdr-mcp lang ja
```

用于项目本机管理界面/相关 UI 语言设置。浏览器扩展也支持 en / 简体中文 / 日本語。

## 浏览器 Native Messaging Host

浏览器扩展主链路需要本机 Native Messaging host：

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

安装后：

```text
Chrome extension
  ↓ Native Messaging
herdr extension host
  ↓ local Unix socket
herdr-mcp runtime
```

浏览器不需要保存 Herdr bearer。

安装与 HUD 使用见 [浏览器扩展](extension.md)。

## Cloudflare Edge 凭据

### 最小权限凭据

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
bin/herdr-cloudflare-token --zone example.com --rotate
```

用于创建/验证 Cloudflare Edge 所需的最小权限 deployment credential。具体权限和安全边界见 [Cloudflare Edge 凭据](cloudflare-edge-token.md)。

### Custom Domain

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

用于把已验证 Worker 绑定到 Custom Domain。它和普通 Worker code deployment 是两个独立操作。

### 旧 Tunnel/CNAME 迁移

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

只用于遗留架构从 CNAME/Tunnel 迁到 Worker Custom Domain。新安装不需要走这条路径。

详见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## Runtime A/B

Runtime generation manager：

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation <id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation <id>
bin/herdr-runtime-generation rollback
bin/herdr-runtime-generation remove --generation <id>
```

它负责的是**同一 contract epoch 内**的本机 runtime generation 注册、切流和回滚。

核心原则：candidate 先运行、先健康检查、先验证 contract，再激活；不要先杀旧 runtime 再祈祷新 runtime 能起来。

详见 [Runtime A/B](runtime-self-upgrade.md)。

## Self Update

```bash
bin/herdr-self-update
```

受监督自升级复用 generation 机制。它适合在同一 public contract 下更新实现，不用于偷偷跨 contract epoch。

如果 public tool catalog 发生不兼容变化，应走显式 contract migration，并在新的 ChatGPT conversation 验证新的 tool snapshot。

## Workstation Link

```text
bin/herdr-link
```

`herdr-link` 是工作站到 Cloudflare Edge 的出站 WSS sidecar。正常情况下由系统服务管理，不需要日常手工运行。

它负责：

- workstation identity；
- 持久出站 WSS；
- 当前 active runtime 路由；
- runtime generation/version heartbeat。

它不是 MCP runtime 本身。

## 常用环境变量

| 变量 | 默认 | 用途 |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | 本机 runtime HTTP 端口 |
| `HERDR_MCP_TOKEN` | 空 | 本机 curl/Cursor bearer；不给 ChatGPT |
| `HERDR_MCP_BASE_URL` | 空 | 公网 OAuth/MCP origin，不带 `/mcp` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr Socket API |
| `HERDR_MCP_READONLY` | 关 | 禁止 mutation |
| `HERDR_MCP_WRITE_ROOTS` | managed roots | 缩小允许写的项目范围 |
| `HERDR_MCP_AGENT_ALLOW` | 默认 worker/auditor | 控制 inspect/since 展示哪些 Agent |
| `HERDR_MCP_ALL_TOOLS` | 关 | 打开高级/兼容工具；正常 ChatGPT 不需要 |
| `HERDR_SKILL_NETWORK` | 开 | `0` 时只使用 bundled skill |

## 命令怎么选

| 目标 | 命令 |
|---|---|
| 看本机 runtime 是否活着 | `herdr-mcp status` |
| 看错误 | `herdr-mcp logs -f` |
| 更新本地开发构建 | `npm run build && herdr-mcp restart` |
| 安装扩展本机桥 | `bin/herdr-extension-host install` |
| 部署前检查 Cloudflare 权限 | `bin/herdr-cloudflare-token ... --dry-run` |
| 看 A/B 当前状态 | `bin/herdr-runtime-generation status` |
| runtime 出问题回滚 | `bin/herdr-runtime-generation rollback` |
| Custom Domain 状态 | `bin/herdr-cloudflare-domain status` |

如果你发现自己需要大量直接调用 Herdr workspace/pane/agent 命令，请回到 Herdr 官方 CLI 文档；herdr-mcp 不重复维护那一套控制面。
