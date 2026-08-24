# 本地 Agent 安装与 `workers.dev` 部署

这是一份给**本地 coding Agent**读取并执行的安装协议，不是给用户逐条复制命令的教程。目标是：用户只负责 Cloudflare 本人登录和创建 API Token；Agent 负责其余检查、clone、构建、本机服务、Cloudflare Worker、WSS Link 和验证。

> 当前约束：完整的后台服务自动安装路径以 macOS 为主；Node 服务本身支持 macOS / Linux / Windows。Linux / Windows 可以完成 clone、build 和 Cloudflare Edge 部署，但长期驻留的本机服务应接入用户已有的进程管理器，不要在本流程里擅自发明 systemd / Task Scheduler 配置。

## 0. Agent 执行契约

1. **直接执行，不把可自动完成的命令甩给用户。** 只有 Cloudflare 本人登录/API Token 创建，以及存在多个 Cloudflare Account 需要选择时才暂停。
2. **不破坏已有工作。** 如果目标目录已经存在且 Git dirty，禁止 `reset --hard`、`clean -fd` 或覆盖用户修改；改用现有安全状态，或 clone 到新的旁路目录。
3. **首次安装只用 `workers.dev`。** 不创建 Custom Domain、DNS 记录、Cloudflare Tunnel，也不修改已有 zone。
4. **Cloudflare Token 是高敏凭据。** 用户可能把它贴进本地 Agent 对话。收到后禁止回显、禁止写入仓库、`.env`、普通日志、截图或 shell history。优先通过 Agent 的进程环境注入；不得不落盘时，只允许 `0600` 临时文件，并在部署结束立即删除。
5. 每个 mutation 之后都先验证结果再继续；失败后先判断是否已经生效，禁止盲目重复部署、secret 写入或 LaunchAgent 安装。

## 1. 本机前置检查

Agent 先执行并记录非敏感结果：

```bash
git --version
node --version
npm --version
herdr --version
herdr api schema >/dev/null
```

要求：Node.js `>=20`；`herdr` 已安装且可以执行；默认 Herdr socket `~/.config/herdr/herdr.sock` 存在，或者用户已经通过 `HERDR_SOCKET_PATH` 配置其他 socket。

如果 Herdr 本体没有安装/运行，停止 herdr-mcp 部署并引导用户先完成 <https://herdr.dev> 的安装；不要用 herdr-mcp 代替 Herdr。

## 2. clone / 更新与构建

如果当前没有仓库：

```bash
git clone https://github.com/whshang/herdr-mcp.git ~/herdr-mcp
cd ~/herdr-mcp
npm ci
npm run build
```

如果已有仓库：先 `git status --short`。工作区 clean 时允许 `git fetch origin main` + `git pull --ff-only`；dirty 时保留原样，不做 destructive cleanup。

安装过程中不要修改 tracked 配置来放个人 Account、Token 或域名。

## 3. 生成本机安装身份（不要输出 secret）

Agent 在内存中生成：

- `HERDR_MCP_TOKEN`：本机 `127.0.0.1:8772` 的 bearer；
- `LINK_SHARED_SECRET`：工作站到 Cloudflare Edge 的 WSS 共享 secret；
- `WORKSTATION_ID`：从 hostname 派生、只含 `[A-Za-z0-9_.-]`、不超过 64 字符；
- `WORKER_NAME`：默认 `herdr-edge-<machine-slug>`，避免覆盖账号中已有的通用 `herdr-edge` Worker。

推荐使用 `openssl rand -hex 32` 生成随机 secret；不要把这些值放进最终摘要。

## 4. 只在这里暂停：让用户创建 Cloudflare API Token

如果 Agent 能打开浏览器，打开：<https://dash.cloudflare.com/profile/api-tokens>。否则把这个链接发给用户，并提示先登录 Cloudflare。

### 推荐 Token

最省步骤的做法是使用 Cloudflare 当前提供的 **Edit Cloudflare Workers** 模板，并把 Account scope 缩小到本次部署使用的那个 Account。Cloudflare 官方 Wrangler CI/CD 文档也使用这个模板。**不要额外增加 DNS Write。**

如果用户希望进一步收紧权限，可以创建 Custom Token，至少保留：

- Account → **Workers Scripts → Write/Edit**（目标 Account）；
- Account → **Account Settings → Read**（目标 Account）；
- User → **Memberships → Read**；
- User → **User Details → Read**。

Cloudflare UI 在不同页面可能把写权限显示为 `Write` 或 `Edit`。账号资源只选目标 Account。`workers.dev` 首次部署不需要 DNS 权限。

参考：

- <https://developers.cloudflare.com/fundamentals/api/get-started/create-token/>
- <https://developers.cloudflare.com/fundamentals/api/reference/template/>
- <https://developers.cloudflare.com/workers/ci-cd/external-cicd/github-actions/>

告诉用户：Token secret 只显示一次；创建后**只贴回当前本地 Agent 对话**。如果 Agent/宿主支持 secret input，应优先使用 secret input，而不是普通聊天文本。

## 5. 收到 Token 后自动完成 Cloudflare 预检

收到用户 Token 后，把它仅注入临时环境 `CLOUDFLARE_API_TOKEN`，不要把 secret 拼进命令字符串。

先验证 `GET https://api.cloudflare.com/client/v4/user/tokens/verify`，再在 `edge/cloudflare` 下执行 `npx wrangler whoami` 获取可用 Account。

- 只有一个 Account：自动使用它；
- 多个 Account：只向用户询问“使用哪个 Account 名称”，不要让用户再跑命令；
- Token 无权限或失效：停止 mutation，说明缺少的权限，让用户重建 Token。

选定后，把账号 ID 只放入当前部署进程的临时环境：

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

不要把个人 `account_id` 写入 tracked Wrangler 配置；后续 `wrangler deploy` / `wrangler secret put` 都继承这个临时环境。

拿到 `ACCOUNT_ID` 后，通过 Cloudflare API 读取当前账号的 Workers 子域：

```text
GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
```

这个接口接受 `Workers Scripts Read/Write`。如果已经存在 `subdomain`，**必须复用，禁止改名**。如果账号从未设置 `workers.dev` 子域，Agent 可以创建一个新的账号子域；只允许在“不存在旧值”时创建，候选名使用 `herdr-<account-id-short>`，冲突时追加随机后缀。绝不能覆盖一个已存在的 account subdomain。

仅在 GET 明确确认“当前没有子域”后，才允许创建：

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

创建后必须重新 GET 一次，确认返回值与候选名一致，再继续 Worker 部署。

API 参考：<https://developers.cloudflare.com/api/resources/workers/subresources/subdomains/>。

## 6. 生成本地 Wrangler 配置并创建 Worker

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

Agent 自动替换：

- `name = "<WORKER_NAME>"`；
- `DEFAULT_WORKSTATION_ID = "<WORKSTATION_ID>"`；
- `OAUTH_ISSUER = "https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev"`。

保持：

```toml
workers_dev = true
routes = []
```

第一次部署：

```bash
cd edge/cloudflare
npx wrangler deploy --config wrangler.user.toml
```

确认 Wrangler 返回的 Worker 名和 `workers.dev` URL 与配置一致。不要因为同名冲突覆盖未知 Worker；如果目标 Worker 在本次安装前已经存在且不能证明属于 herdr-mcp，换一个带机器名/随机后缀的新名称。

Worker 创建后，把 `LINK_SHARED_SECRET` 写入 **Cloudflare Worker secret**，不要写入 `wrangler.user.toml`：

```bash
printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
```

Cloudflare `Workers Scripts Write` 可以创建/更新 Worker deployment 和 `workers.dev` 暴露面。首次安装不需要 Zone/DNS mutation。

## 7. macOS：安装本地 MCP LaunchAgent

macOS 上使用 `deploy/dev.herdr-mcp.server.plist.example`，复制到 `~/Library/LaunchAgents/dev.herdr-mcp.server.plist`。Agent 只替换模板中的占位符：当前 `node` 绝对路径、repo 绝对路径、HOME、`HERDR_MCP_TOKEN`、Herdr socket，以及：

```text
HERDR_MCP_BASE_URL=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev
```

然后以 launchd 启动/重启并验证 `http://127.0.0.1:8772/mcp`。可把 CLI 链接到 PATH：

```bash
launchctl bootout "gui/$UID/dev.herdr-mcp.server" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$UID" "$HOME/Library/LaunchAgents/dev.herdr-mcp.server.plist"
launchctl enable "gui/$UID/dev.herdr-mcp.server"

mkdir -p ~/.local/bin
ln -sf "$PWD/bin/herdr-mcp" ~/.local/bin/herdr-mcp
```

不要在最终回复打印 `HERDR_MCP_TOKEN`。

## 8. macOS：安装持久 Herdr Link

把 `LINK_SHARED_SECRET` 存入 macOS Keychain，使用独立 service 名，例如 `herdr-edge-link-<WORKSTATION_ID>`。secret 必须通过环境变量传入，命令文本里只能出现变量名：

```bash
export HERDR_LINK_KEYCHAIN_SERVICE="herdr-edge-link-$WORKSTATION_ID"
security add-generic-password -U -a "$(id -un)" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET"
```

然后让 `bin/herdr-link` 使用：

```text
HERDR_EDGE_URL=wss://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/ws
HERDR_WORKSTATION_ID=<WORKSTATION_ID>
HERDR_LINK_KEYCHAIN_SERVICE=herdr-edge-link-<WORKSTATION_ID>
```

执行：

```bash
HERDR_EDGE_URL="wss://$WORKER_NAME.$ACCOUNT_SUBDOMAIN.workers.dev/ws" \
HERDR_WORKSTATION_ID="$WORKSTATION_ID" \
HERDR_LINK_KEYCHAIN_SERVICE="$HERDR_LINK_KEYCHAIN_SERVICE" \
bin/herdr-link install
```

脚本会从 PATH 自动解析 Node，可用 `HERDR_NODE_BIN` 显式覆盖。

## 9. 验证闭环

按顺序验证，任何一步失败先诊断再重试：

1. 本机 MCP：`server/discover` 可通过 `127.0.0.1:8772/mcp` 返回；
2. `herdr-mcp status` 显示进程和 Herdr socket 正常；
3. `bin/herdr-link status` 正常/在线；
4. `https://<WORKER>.<SUBDOMAIN>.workers.dev/health` 可访问，并能看到工作站 Link 已连接；
5. `https://<WORKER>.<SUBDOMAIN>.workers.dev/mcp` 是最终 MCP URL；
6. OAuth discovery 可访问；
7. 不创建 Custom Domain、DNS 或 Tunnel。

## 10. 清理 Cloudflare bootstrap Token

部署完成后：`unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID`，删除任何临时 credential 文件，不把 Token 复制到项目配置。如果只用于这次首次安装，可建议用户在 Cloudflare 中吊销它；如果后续需要自动发布，应把它存到专用 secret manager / CI secret，而不是仓库。

## 11. 最终给用户的结果

只报告非敏感信息：

```text
本地 MCP: healthy / error
Herdr Link: connected / error
Cloudflare Account: <name> (<id-short>)
Worker: <worker-name>
Origin: https://<worker>.<account-subdomain>.workers.dev
Health: https://<worker>.<account-subdomain>.workers.dev/health
MCP: https://<worker>.<account-subdomain>.workers.dev/mcp
```

随后引导用户在 ChatGPT 网页开启 Developer mode，创建自定义 MCP Connector，填上面的 `/mcp` URL，并通过 OAuth 完成连接。**不要把本机 `HERDR_MCP_TOKEN` 或 Cloudflare Token 填进 ChatGPT。**
