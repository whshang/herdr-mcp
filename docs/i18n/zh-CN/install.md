# 安装：从一台 Herdr 工作站到可用的 Web AI 开发环境

这篇文档负责把系统真正装起来。目标不是“某个本地进程能启动”，而是完成下面这条链：

```text
ChatGPT
  ↓ OAuth + MCP
Cloudflare Edge
  ↓ authenticated WSS
herdr-link / herdr-mcp
  ↓
Herdr + managed Git projects
```

如果只想快速体验，先看 [快速开始](quick-start.md)。如果想让本地 Coding Agent 代你完成绝大多数安装步骤，看 [Agent 辅助安装](agent-install.md)。

## 安装前确认

### 1. Herdr 已经可用

herdr-mcp 建立在 Herdr 上，不安装也不替代 Herdr。

```bash
herdr --version
herdr api schema >/dev/null
```

如果这里失败，先按 [Herdr 官方安装文档](https://herdr.dev/zh-cn/docs/install/) 修好 Herdr。

### 2. 明确你要连接哪类客户端

- **ChatGPT**：需要稳定公网 Edge + OAuth。
- **同机 Cursor / curl**：可以直接连接 `127.0.0.1`，不需要 Cloudflare。
- **z.ai / DeepSeek 浏览器桥接**：依赖浏览器扩展和 Native Messaging，本机链路即可。

本页主线以 ChatGPT 为例，因为它覆盖了完整安装链路。

运行本机 MCP runtime **不需要** Node.js。部署 Cloudflare Worker（`npx wrangler`）或从源码构建浏览器扩展时，仍可能临时用到 Node。

## 第一步：安装原生 runtime（主路径）

从 [GitHub Releases](https://github.com/whshang/herdr-mcp/releases) 下载当前平台的 `herdr-mcp` 二进制，放到 `PATH`（例如 `~/.local/bin/herdr-mcp`）并赋予可执行权限。然后执行 `herdr-mcp install`：安装器会在 `~/.config/herdr-mcp/runtime/` 下写入不可变 generation，并把 `~/.local/bin/herdr-mcp` 重定向到 `runtime/current/herdr-mcp`，使 PATH 入口不再依赖 git checkout。

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

优先使用以上顶层命令。**不要**把 `herdr-mcp service install` 写成普通用户安装主路径；`service ...` 仍是高级/内部接口。

## 第二步：先把本地 runtime 跑通

受管 runtime 默认监听 `127.0.0.1:8772`。二进制安装且本机服务健康后：

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

返回 `200` 或 `401` 都说明 HTTP 服务存在；`401` 表示它正在要求本机 bearer。

这一步还应该确认 Herdr socket 能被 runtime 访问。稍后真正连接后，`herdr_inspect` 应能返回当前 workspace / pane / managed roots。

日常升级：

```bash
herdr-mcp update apply
herdr-mcp update status
```

## 第三步：让 runtime 长期运行

在 macOS 上，安装完成后由原生二进制管理 LaunchAgent 生命周期。健康检查用 `herdr-mcp status` / `herdr-mcp doctor`，升级用 `herdr-mcp update ...`。不要把 launchd 指到 git checkout 或 `target/*/herdr-mcp`。

Linux / Windows 的服务封装目前更窄；把 Release 二进制放在 `PATH` 上，并按当前 Release 资产中的平台说明处理；若该平台尚未提供一键 service manager，就按文档给出的平台方式保活。

## 第四步：部署稳定公网 Edge

ChatGPT 不能访问你的 `127.0.0.1`。推荐架构是 Cloudflare Worker / Durable Object 作为稳定公网入口，工作站主动建立出站 WSS。

首次安装直接使用 `workers.dev`。它不要求购买域名，也避免在安装阶段把 DNS、证书和业务域名一起引入。

### 生成 Worker name

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
printf '%s\n' "$WORKER_NAME"
```

Worker name 是 DNS label。像 `MacBook.local` 这种主机名不要原样复制；helper 会做规范化。

### 准备 Wrangler 配置

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

按模板填入：

- Worker name；
- workstation identity；
- `OAUTH_ISSUER` / 公网 origin；
- 模板要求的其他部署值。

部署：

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

公网 origin 形如：

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev
```

MCP 端点：

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev/mcp
```

凭据最小权限见 [Cloudflare Edge Token](cloudflare-edge-token.md)；Worker / Durable Object / Link 细节见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## 第五步：验证工作站链路

Worker 部署成功只证明 Edge 存在。Edge 还必须能路由到你的工作站。

确认：

1. 本机 runtime 健康；
2. `herdr-link` 在跑；
3. workstation identity 与 Edge 配置一致；
4. Edge `/health` 报告工作站在线；
5. runtime generation/version 符合预期。

即使工作站离线，OAuth 也可能成功，所以要把公网 Edge 健康与工作站可达性当成不同层。

## 第六步：创建 ChatGPT Connector

在 ChatGPT 中：

1. 打开 Developer mode；
2. 创建自定义 MCP Connector；
3. 填入 `https://<worker>.<account>.workers.dev/mcp`；
4. 在浏览器完成 OAuth；
5. **新开一个会话**做验证。

不要把 `HERDR_MCP_TOKEN` 贴进 ChatGPT。公网 Connector 的认证边界是 OAuth。

ChatGPT 会缓存工具快照。服务器升级后旧会话仍可能暴露旧契约；见 [ChatGPT Connector](chatgpt-connector.md)。

## 第七步：做一次真实验证

从只读请求开始：

```text
检查当前 Herdr workspace 和项目状态。只读，不要修改。
```

预期：

1. `herdr_inspect` 成功；
2. 模型能看到真实 workspace / managed Git roots；
3. 可能加载一次 `herdr_skill`；
4. 能对真实项目状态使用 `herdr_git` 或 `herdr_fs_read`。

然后再试一次小的可逆编辑和测试命令。

当前生产公共契约是 **epoch 2 / 18 tools**。如果新会话仍只看到 17 个工具，先排查 Connector/工具快照缓存，而不是重装 runtime。

## 第八步：需要连续性时再装浏览器扩展

MCP 解决的是“ChatGPT 到达工作站”。如果你还希望本地 Agent 完成后浏览器能继续、恢复卡住的回复，或交接超长会话，再安装浏览器扩展。

见 [浏览器扩展](extension.md)。扩展使用 Native Messaging 与本机 IPC；不把 Herdr bearer 存进浏览器状态。

## 何时再加 Custom Domain

`workers.dev` 足以验证完整 Connector 路径。Custom Domain 适合：

- 你要在自己控制的域名下长期固定 OAuth issuer；
- 团队有集中域名治理；
- 你想把公网身份与 Cloudflare account 子域解耦。

先在 `workers.dev` 上跑通完整流程，再单独迁移稳定 origin。

## 本机客户端：绕过 Cloudflare

本机 Cursor / curl 可以直接连：

```text
http://127.0.0.1:8772/mcp
```

并使用本机静态 bearer。这条路径也便于把 runtime 故障与 Edge 故障分开。

## 贡献者说明：从本仓库构建

clone + `npm`/`cargo` 工作流仍留给开发 herdr-mcp 本身的人。那不是最终用户安装主路径，也不应被要求用来运行本机 MCP runtime。

## 什么叫“装好了”

一次完整的 ChatGPT 安装需要同时满足：

- Herdr socket 可用；
- herdr-mcp runtime 可用；
- Edge 已部署；
- 工作站链路在线；
- OAuth 成功；
- 新 ChatGPT 会话拿到 epoch-2 目录；
- `herdr_inspect` 能看到真实工作站；
- 至少一次真实的文件/Git/测试操作成功。

Worker 部署成功或 Connector 显示“connected”只是其中一层。

分层排障见 [故障排查](troubleshooting.md)：本机 runtime → Link → Edge → OAuth → MCP → ChatGPT 快照。
