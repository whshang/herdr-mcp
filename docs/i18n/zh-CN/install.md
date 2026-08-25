# 安装：从一台 Herdr 工作站到可用的 Web AI 开发环境

这篇文档负责把系统真正装起来。目标不是“某个 Node 进程能启动”，而是完成下面这条链：

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

## 安装前确认三件事

### 1. Herdr 已经可用

herdr-mcp 建立在 Herdr 上，不安装也不替代 Herdr。

```bash
herdr --version
herdr api schema >/dev/null
```

如果这里失败，先按 [Herdr 官方安装文档](https://herdr.dev/zh-cn/docs/install/) 修好 Herdr。

### 2. Node.js 20+

```bash
node -v
```

### 3. 明确你要连接哪类客户端

- **ChatGPT**：需要稳定公网 Edge + OAuth。
- **同机 Cursor / curl**：可以直接连接 `127.0.0.1`，不需要 Cloudflare。
- **z.ai / DeepSeek 浏览器桥接**：依赖浏览器扩展和 Native Messaging，本机链路即可。

本页主线以 ChatGPT 为例，因为它覆盖了完整安装链路。

## 第一步：获取代码并构建

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npm run build
mkdir -p ~/.config/herdr-mcp
```

已有仓库时先检查 Git 状态，再安全更新；不要在有未知未提交修改的 checkout 上直接 `git reset --hard` 或覆盖文件。

## 第二步：先把本地 runtime 跑通

本地 runtime 默认监听 `127.0.0.1:8772`。静态 token 只用于本机客户端和管理，不应该交给 ChatGPT。

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
export HERDR_MCP_PORT=8772
node dist/server.js
```

另开一个终端检查：

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

返回 `200` 或 `401` 都说明 HTTP 服务存在；`401` 表示它正在要求本机 bearer。

这一步还应该确认 Herdr socket 能被 runtime 访问。稍后真正连接后，`herdr_inspect` 应能返回当前 workspace / pane / managed roots。

## 第三步：让 runtime 长期运行

### macOS

项目提供 LaunchAgent CLI：

```bash
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
herdr-mcp start
herdr-mcp status
herdr-mcp logs
```

需要自动检查 MCP 进程时：

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

watchdog 负责 herdr-mcp runtime 的基本存活检查。Herdr daemon 的控制面瞬时异常不会因此被反复重启。

### Linux / Windows

Node runtime 本身支持这些平台；当前仓库没有提供与 macOS LaunchAgent 等价的一键服务管理 CLI。用你熟悉的 systemd、容器或任务管理方式保持 `node dist/server.js` 常驻即可。

## 第四步：部署稳定公网 Edge

ChatGPT 不能访问你的 `127.0.0.1`。推荐架构是 Cloudflare Worker / Durable Object 作为稳定公网入口，工作站主动建立出站 WSS。

首次安装直接使用 `workers.dev`。它不要求购买域名，也避免在安装阶段把 DNS、证书和业务域名一起引入。

### 生成 Worker name

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
printf '%s\n' "$WORKER_NAME"
```

Worker name 是 DNS label，只能使用适合 `workers.dev` 的字符。`MacBook.local` 这样的 hostname 不能原样拿来当 Worker name，脚本会做规范化。

### 准备 Wrangler 配置

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

根据模板填写：

- Worker name；
- workstation identity；
- `OAUTH_ISSUER` / 公网 origin；
- 模板要求的其它部署参数。

然后部署：

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

得到的地址类似：

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev
```

MCP endpoint 是：

```text
https://herdr-edge-xxx.<account-subdomain>.workers.dev/mcp
```

Cloudflare API Token 的最小权限、创建和轮换方式单独见 [Cloudflare Edge Token](cloudflare-edge-token.md)。完整 Worker / DO / Link 配置见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## 第五步：确认 workstation link 在线

公网 Worker 能部署成功，只证明 Edge 存在。真正可用还需要 Edge 能找到你的工作站。

检查重点：

1. 本地 runtime 正常；
2. `herdr-link` 正在运行；
3. workstation identity 与 Edge 配置一致；
4. Edge `/health` 显示对应工作站在线；
5. runtime generation/version 是预期版本。

如果 Edge 正常但 workstation offline，ChatGPT 的 OAuth 甚至可能成功，工具调用仍会失败。把“公网入口”和“工作站在线”当成两层检查。

## 第六步：创建 ChatGPT Connector

在 ChatGPT 中：

1. 开启 Developer mode；
2. 创建自定义 MCP Connector；
3. 填入 `https://<worker>.<account>.workers.dev/mcp`；
4. 按页面流程完成 OAuth；
5. 创建一个**新对话**进行验收。

不要把 `HERDR_MCP_TOKEN` 粘进 ChatGPT。公网 Connector 的认证边界是 OAuth。

为什么连接成功后还要新开对话，见 [ChatGPT Connector](chatgpt-connector.md)：ChatGPT 会缓存工具快照，旧会话可能继续持有旧 contract。

## 第七步：做一次真实验收

建议从一个安全的只读任务开始：

```text
检查当前 Herdr 工作区和项目状态，只读，不做修改。
```

预期模型能够：

1. 调用 `herdr_inspect`；
2. 看到工作站的 workspace / managed Git roots；
3. 必要时调用一次 `herdr_skill`；
4. 用 `herdr_git` 或 `herdr_fs_read` 获取真实项目信息。

随后再测试一个可回滚的小修改和测试命令。

当前生产公共 contract 是 **epoch 2 / 18 tools**。如果新对话仍然看到 17 tools，优先检查 Connector/tool snapshot，而不是继续重装本地 runtime。

## 第八步：按需要安装浏览器扩展

MCP 解决“ChatGPT 主动操作工作站”。如果你希望本地 Agent 完成后网页能够继续、回复超时能够恢复、长对话能够自动接力，还需要浏览器扩展的反向通道。

扩展安装和作用域规则见 [浏览器扩展](extension.md)。它通过 Native Messaging 与本机 runtime 通信，浏览器侧不保存 Herdr bearer。

## 自定义域名什么时候再做

`workers.dev` 已经能完整跑通 Connector。Custom Domain 适合：

- 希望长期固定一个自己控制的 OAuth issuer；
- 团队有统一域名治理；
- 需要把公共入口和 Cloudflare account subdomain 解耦。

正确顺序是先在 `workers.dev` 验证全链路，再迁移稳定 origin。不要在首次安装时同时调试 DNS、OAuth、WSS 和 runtime。

## 本地客户端：不经过 Cloudflare

同一台机器上的 Cursor / curl 可以连接：

```text
http://127.0.0.1:8772/mcp
```

并使用本机静态 bearer。这条路径适合调试，也能帮助判断问题位于 runtime 还是公网 Edge。

## 安装完成的判断标准

一套完整的 ChatGPT 安装应该同时满足：

- Herdr socket 正常；
- herdr-mcp runtime 正常；
- Edge 已部署；
- workstation link 在线；
- OAuth 成功；
- 新 ChatGPT 对话能获取 epoch-2 工具 catalog；
- `herdr_inspect` 能看到真实工作现场；
- 一次文件/Git/测试操作能够完成。

只满足“Worker 部署成功”或“Connector 显示已连接”都还不算完整验收。

遇到问题按 [故障排查](troubleshooting.md) 从本地 → Link → Edge → OAuth → MCP → ChatGPT snapshot 逐层检查。
