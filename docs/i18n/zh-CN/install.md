# 安装

让 ChatGPT（或其它网页模型）通过 herdr-mcp 与本地 Herdr 工作站打通。官方推荐的顺序：先安装并启动本地 MCP 服务器，再部署 Cloudflare Edge，最后连接 ChatGPT —— ChatGPT 实际访问的是 Edge，所以**先部署 Edge，再创建 Connector**。

完整的系统边界见 [架构](architecture.md)，Connector 契约详见 [连接 ChatGPT](chatgpt-connector.md)。

## 前置条件

- 已安装并运行 [herdr](https://herdr.dev)（服务器连接 Herdr 的 API socket，不扫描安装目录）。
- Node.js 20+（`node -v`）。
- 使用 ChatGPT 需要 Cloudflare Worker 端点（默认为 `workers.dev`，无需自有域名）。长期稳定的源站可选配自定义域名；直接的 `cloudflared` 暴露仅作为遗留迁移路径保留。

## 1. 下载并构建

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npx tsc
mkdir -p ~/.config/herdr-mcp
```

## 2. 启动本地 MCP 服务器

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
echo "token=$HERDR_MCP_TOKEN"   # 留给 Cursor / 本机管理
node dist/server.js
# 可选检查：curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

服务器默认监听 `127.0.0.1:8772`（可用 `HERDR_MCP_PORT` 覆盖）。上面的静态 token 仅供 Cursor / curl 使用——**绝不要把它粘给 ChatGPT**，ChatGPT 在 Edge 走 OAuth 认证。

### macOS：以 LaunchAgent 方式运行

```bash
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
herdr-mcp start     # LaunchAgent
herdr-mcp status
herdr-mcp logs [-f]
herdr-mcp watchdog install   # 每 120s 若 MCP 掉线自动重启
```

`npm` 的 `bin` 是 `dist/server.js`（服务器本身）而不是 bash CLI；macOS 上可按上面链接到 `~/.local/bin`。改代码后：`npx tsc && herdr-mcp restart`（或重启 `node dist/server.js` 进程）。

## 3. 部署 Cloudflare Edge

默认方案不需要自有域名——Worker 运行在你账号的 `workers.dev` 主机名下：

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
# 编辑 worker name、workstation id 与 OAUTH_ISSUER（填你的 workers.dev 源站）
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

得到一个稳定源站，例如：

```text
https://herdr-edge.<你的账号子域>.workers.dev/mcp
```

如果你有自己的 Cloudflare zone，自定义域名如 `herdr.example.com` **推荐但可选**——先在 `workers.dev` 上验证 Worker，再单独绑定。参见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md) 与 [Cloudflare Edge Token](cloudflare-edge-token.md)。

## 4. 连接 ChatGPT

1. 打开 ChatGPT 设置并开启 **Developer mode（开发者模式）**。
2. 创建自定义 MCP connector。
3. 填入 Edge MCP URL：`https://<worker>.<account>.workers.dev/mcp`（或自定义域名 + `/mcp`）。
4. 在浏览器里完成 OAuth；不要把本地 Herdr token 粘进 ChatGPT。
5. 连接后**新开一个会话**，让会话拿到全新的工具快照。

运行时发布可以在不修改 Connector 的情况下，在稳定的 Edge/Link 后面做代际切换——见 [Runtime A/B 自升级](runtime-self-upgrade.md)。

## 验证安装

- 本地：`curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/` 返回 `200` 或 `401`（`401` 表示服务在跑、在索要 token）。
- Edge：打开 Worker 的 `/health`，确认工作站 Link 已连接。
- ChatGPT：连接后的会话应列出 18 个工具（含 `herdr_skill`）。如果工具缺失，见 [故障排查](troubleshooting.md)——常见原因是会话缓存过期，而不是服务挂了。