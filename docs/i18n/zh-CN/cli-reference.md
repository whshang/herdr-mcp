# CLI 参考

herdr-mcp 提供两套命令面：面向 macOS/LaunchAgent 的 `herdr-mcp` bash CLI，以及一组 Node `bin/` 维护工具。npm 的 `bin` 是 `dist/server.js`——那是服务器本体，不是 CLI。

## herdr-mcp（macOS CLI）

```bash
herdr-mcp              # 菜单
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart   # LaunchAgent
herdr-mcp logs [-f]
herdr-mcp token | url
herdr-mcp lang [en|zh|ja]   # 界面语言（首次跟系统；默认 en）
herdr-mcp watchdog install  # 每 120s：MCP 掉线自动重启；TaskGroup 仅记日志
herdr-mcp watchdog status
```

改代码后：`npx tsc && herdr-mcp restart`（或重启 `node dist/server.js` 进程）。

## bin/ 维护工具

| 命令 | 用途 |
|---|---|
| `bin/herdr-cloudflare-token` | 为 Herdr 创建最小权限的 Cloudflare Account API Token（zone 的 Workers Routes Write + account 的 Workers Scripts Write）。写入 `~/.config/herdr-mcp/cloudflare-cutover.env`（`0600`），绝不打印 token。见 [Cloudflare Edge Token](cloudflare-edge-token.md)。 |
| `bin/herdr-cloudflare-dns-token` | 签发仅用于一次性迁移路径的窄权限 DNS token，日常操作不需要。 |
| `bin/herdr-cloudflare-domain` | 通过 Cloudflare Workers Domains API 为已部署的 Worker 挂载/卸载自定义域名。见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。 |
| `bin/herdr-custom-domain-cutover` | 把遗留 CNAME/Tunnel 主机名一次性切换到 Worker 自定义域名。 |
| `bin/herdr-runtime-generation` | 管理 Runtime A/B 代际：`status`、`register --generation <id>`、`activate --generation <id>`、`rollback`、`remove --generation <id>`。见 [Runtime A/B 自升级](runtime-self-upgrade.md)。 |
| `bin/herdr-self-update` | 受监督的自升级，复用 runtime-generation 机制；拒绝跨契约代际迁移。 |
| `bin/herdr-link` | 工作站 → Edge 的 WSS 边车（LaunchAgent）。工作站只会建立出站的已认证连接。 |

## 环境变量

| 变量 | 默认值 | 含义 |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | 本地 Express 端口（绑定 `127.0.0.1`）。 |
| `HERDR_MCP_TOKEN` | — | 供 Cursor / curl 使用的静态 Bearer。绝不给 ChatGPT——它走 OAuth。 |
| `HERDR_MCP_BASE_URL` | — | 公共 Edge 源站；必须与 `OAUTH_ISSUER` 完全一致。 |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr 守护进程 socket。 |
| `HERDR_MCP_AGENT_ALLOW` | — | `*` 显示所有窗格；默认 `inspect`/`since` 会软隐藏 Claude/OMP/Codex。 |
| `HERDR_SKILL_NETWORK` | — | `0` 强制使用内置 skill 副本，不拉取上游 `SKILL.md`。 |

延伸阅读：[架构](architecture.md)（环境变量与闸门）、[Cloudflare Edge Token](cloudflare-edge-token.md)（token 工作流）、[Runtime A/B 自升级](runtime-self-upgrade.md)（代际生命周期）。