# Agent 安装

*端到端 Agent 安装协议与 `workers.dev` 部署。*

> **执行角色：Agent。** 本页是 herdr-mcp 普通工作站安装的唯一权威执行合同，包含精简上线路径、安全边界、Cloudflare 部署、Link、浏览器扩展通道选择与验证。[安装参考](install.md) 只用于人工/运维查阅。

Agent 直接读取并执行本文，不需要用户再把本文包装成一段“发给某个 Coding Agent”的提示词。用户只承担 Cloudflare 本人登录/API Token、无法自动判断的 Account 选择、ChatGPT Connector/OAuth 等必须本人完成的授权步骤；Agent 负责环境检查、Release 二进制安装、Cloudflare Worker、出站 WSS Link、可选浏览器扩展通道选择与验证。

> 当前约束：完整的后台服务自动安装路径以 **macOS Apple Silicon** 为第一正式平台。Windows 可有 Release artifact 作为 preview。不要发明未支持的 Linux lifecycle 包装。Edge 部署可临时使用 Node/`wrangler`；本机 MCP runtime **必须**来自 GitHub Releases，而不是 `git clone` + `npm ci`。

## 0. Agent 合同

1. 能自动化的 shell 步骤直接执行；只在 Cloudflare 交互登录 / API Token 创建，或多个 Account 选择时暂停。
2. 不破坏已有工作。禁止对无关 checkout 做 `reset --hard`、`clean -fd` 或覆盖用户修改。
3. 首次安装就确定一个 canonical public origin，并让 Worker OAuth、MCP、Link WSS 全部使用同一个入口。`workers.dev` 仍是无需 DNS 的 bootstrap 路径。只有用户明确选择 Custom Domain，或现有安装策略/配置能证明这一意图时，才从第一次部署使用自定义域名。连通性失败是暂停点，不代表 Agent 获得创建或修改 Custom Domain/DNS zone 的权限。
4. Cloudflare Token 是高敏凭据。禁止回显或写入仓库、`.env`、普通日志、截图、shell history。优先进程环境注入；若必须落临时文件，用 mode `0600` 并在部署后立刻删除。
5. 每个 mutation 后先验证再继续。出错时先判断 mutation 是否已经提交，再决定是否重试。
6. **不要**用 clone 本仓库或 `npm`/`cargo` 安装本机 MCP runtime，除非用户明确要求贡献者/从源码开发会话。
7. 如果网络、登录状态或第三方服务不可用，停止并向用户报告 blocker；不要自行搭代理、切网络节点、修改系统代理或发明绕过路径。

## 1. 前置条件

运行 `herdr --version` 与 `herdr api schema >/dev/null`。需要可用的 `herdr` 与 Herdr socket（默认 `~/.config/herdr/herdr.sock`，或显式 `HERDR_SOCKET_PATH`）。若 Herdr 本身未安装/未运行，停下来并引导用户到 <https://herdr.dev>；herdr-mcp 不替代 Herdr。

如果 Herdr 缺失，Agent 直接安装官方稳定版：

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows 使用 `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"`。安装后重新检查健康状态。

Node.js 只用于临时 Cloudflare Worker 引导（`npx wrangler`）和可选贡献者工具链，**不是**运行本机 MCP runtime 的依赖。

规范公网 MCP URL 示例：`https://herdr-edge-device.username.workers.dev/mcp` 与 `https://herdr-mcp.example.com/mcp`。

## 2. 从 GitHub Releases 安装原生 runtime（主路径）

1. 从 <https://github.com/whshang/herdr-mcp/releases> 下载当前 stable 平台二进制，以 GitHub 标记的 `Latest` stable Release 为准。只有明确测试 preview channel 时才选择 prerelease 标签。
2. 放到 `PATH`（例如 `~/.local/bin/herdr-mcp`）并赋予可执行权限。
3. 执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update          # 下载并应用下一版 stable release
```

`install` 会在 `~/.config/herdr-mcp/runtime/` 写入不可变 generation，并把 `~/.local/bin/herdr-mcp` 指到 `runtime/current/herdr-mcp`。`herdr-mcp update` 是正常的一步升级入口；只有运维明确需要只读检查是否有新版本时才使用 `herdr-mcp update check`。优先使用以上顶层命令。不要把 `herdr-mcp service install` 写成普通安装主路径。

macOS v0.4.3+ 首次安装还会准备固定的 `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`。如果是交互式安装且尚未获得完全磁盘访问，系统设置会打开一次，由用户本人给这个 broker 授权。不要尝试用 `sudo` 代替这一步；`doctor` 若返回 `needs_setup`、`denied`、`unknown` 或 `timeout`，不得当作健康。应提示用户完成完全磁盘访问，然后重新执行 `herdr-mcp permissions verify` 和 `herdr-mcp doctor`。普通 runtime generation 更新会保留同一个 broker，不应再次要求授权。

只有明确测试 prerelease build 时才使用 `update.channel = "preview"`。当前 stable runtime 使用默认 `stable` channel 即可。

## 3. 在内存中生成身份，不要打印秘密

在 Agent 内存中生成：`HERDR_MCP_TOKEN`、`LINK_SHARED_SECRET`，以及限制在 `[A-Za-z0-9_.-]`、最长 64 字符的 `WORKSTATION_ID`。有临时 Edge checkout 时，`WORKER_NAME` 只能通过仓库 helper 生成，Agent 不得自造 hostname slug：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

`WORKER_NAME` 与 `WORKSTATION_ID` 故意使用不同语法。helper 会把 hostname 小写，把 `[a-z0-9-]` 以外字符安全替换，压缩/修剪 `-`，并保证完整 Worker 名不超过 63 且匹配 `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`。例如 `MacBook.local` → `herdr-edge-macbook-local`。秘密用 `openssl rand -hex 32` 一类强随机；最终报告不得包含秘密。

## 4. Cloudflare 授权暂停

有浏览器控制时打开 <https://dash.cloudflare.com/profile/api-tokens>；否则把该 URL 交给用户。

最简单支持路径是 Cloudflare 当前的 **Edit Cloudflare Workers** 模板，限定到本次安装使用的单个 Account，并额外授予 Account → **Workers R2 Storage → Edit**。**不要**加 DNS Write。R2 写权限用于在 Worker 部署前幂等创建私有 artifact 中继桶。

更紧的自定义 token 至少保留 Account → **Workers Scripts → Write/Edit**、Account → **Workers R2 Storage → Edit**、Account → **Account Settings → Read**、User → **Memberships → Read**、User → **User Details → Read**。`workers.dev` bootstrap 不需要 Zone/DNS 权限。

告知用户秘密只显示一次，并要求只粘贴到当前本地 Agent 会话；有专用密输通道时优先使用。

## 5. Token 到达后的 Cloudflare 预检

只以临时 `CLOUDFLARE_API_TOKEN` 注入，不要写成命令行字面量。验证 `GET https://api.cloudflare.com/client/v4/user/tokens/verify`，再对临时 Edge 工作目录运行 `npx wrangler whoami`。

- 一个 Account → 自动选择；
- 多个 Account → 只问要用哪个 Account 名；
- Token 无效/权限不足 → 停止 mutation 并说明缺什么权限。

选定后只把 account ID 放在当前部署进程环境：

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

不要把个人 `account_id` 写进被跟踪的 Wrangler 配置。后续 `wrangler deploy` / `wrangler secret put` 继承该临时环境。

有 `ACCOUNT_ID` 后请求 `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain`。复用已有 account subdomain，**永不改名**。Worker origin 永远是 `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`。

若尚无 subdomain，只在确认不存在时创建；用 `herdr-<short-account-id>`，冲突再加随机后缀。GET 明确无 subdomain 后才：

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

创建后再 GET，要求返回值匹配才继续部署。

## 6. 部署 Edge，不要求永久仓库 checkout

仅为 Edge 部署获取 Worker 源码（临时 shallow clone 或与 Release 相邻的文档包均可）。从已发布的 user example 生成被忽略的 `wrangler.user.toml`，设置 `name`、`DEFAULT_WORKSTATION_ID`、`OAUTH_ISSUER=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`。保持 `workers_dev = true` 与 `routes = []`。

先按 `wrangler.user.toml` 幂等创建私有 R2 桶（已存在视为成功），再部署 Worker。不要跳过 provision：绑定的桶不存在时 `wrangler deploy` 会失败。

```bash
node provision-r2.mjs --config wrangler.user.toml
npx wrangler deploy --config wrangler.user.toml
```

除非能证明拥有权，否则不要覆盖已有 Worker；改用机器相关/随机后缀名。然后把 WSS 共享秘密存为 Worker secret：

```bash
printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
```

不需要 Zone/DNS mutation。只为 Edge 部署用的临时 checkout **不得**成为 `herdr-mcp` 的生产 PATH。

## 7. macOS 本机 MCP 服务所有权

优先使用已安装的 Release 二进制路径：

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
```

不要重建指向仓库的 `~/.local/bin/herdr-mcp` bridge。不要把 LaunchAgent 指到 git checkout 或 `target/*/herdr-mcp`。

浏览器扩展 / Native Messaging 仍是可选项，不是第一条 ChatGPT 闭环的必需。扩展通道与 Runtime DEV/PROD 分开建模：**STORE / STANDALONE / DEV**。

- STORE：普通用户默认，Chrome Web Store 固定身份与更新。
- STANDALONE：v0.4.3+ 的 GitHub/手动固定身份 package；Store 不可用或用户明确要求独立分发时使用。
- DEV：仅源码开发，Load unpacked repo/worktree `extension/`，ID 路径派生。

Agent 必须先读取当前 runtime 实际支持的 `native-host` 命令；v0.4.2 只有 Store/DEV，不得虚构 standalone。STANDALONE 是独立于源码开发的分发通道，DEV 仍仅用于源码开发。支持该能力的 runtime 使用 `herdr-mcp native-host use standalone` 显式切换。选择并安装通道后执行：

```bash
herdr-mcp native-host status
```

状态应明确显示预期 active channel / extension identity，并确认 Native Host runtime 与当前 runtime generation 一致。详见 [浏览器扩展](extension.md) 与 [浏览器连续性](browser-continuity.md)。

## 8. macOS 持久 Herdr Link

把 `LINK_SHARED_SECRET` 存进 Keychain，服务名 `herdr-edge-link-<WORKSTATION_ID>`。命令文本只能引用环境变量，不能写字面秘密。优先使用已安装 `herdr-mcp` 二进制提供的托管 Link 安装路径（`herdr-mcp link ...` / 当前 stable 产品文档）。不要把生产 Link 所有权留在仓库 Bash 包装上。

Link 可以复用用户环境里**已经存在**的代理配置。识别优先级：`HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`；macOS 也会读取现有 `scutil --proxy` 状态。如果所选 origin 仍不可达，停止并询问用户；未经明确指示不得修改代理、网络节点、系统代理、DNS/自定义域名选择或其它网络设置。见 [本 Agent 安装协议](agent-install.md) §5。

## 9. 验证闭环

验证本机 `server/discover`、`herdr-mcp status`、`herdr-mcp doctor`、Link status、Worker `/health`、公网 `/mcp`、OAuth discovery，并确认未创建 Custom Domain/DNS/Tunnel。doctor 可在不发送 token 的前提下探测 Edge `/health`、OAuth metadata、`/mcp`；永远不要打印 token。

## 10. 清理 bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` 与 `CLOUDFLARE_ACCOUNT_ID`，删除临时凭据文件和不再需要的临时 Edge checkout。不要把 Token 拷进项目配置。若是一次性 Token，建议吊销；否则迁到专用密钥管理/CI secret。

## 11. 最终报告

只回报非敏感事实：已安装 runtime generation/version、本机 MCP 状态、Herdr Link 状态、Cloudflare Account 名 + 缩短 ID、Worker 名、`workers.dev` origin、`/health`、`/mcp`。

最后引导用户开启 ChatGPT Developer mode，用 `/mcp` 创建自定义 MCP Connector 并完成 OAuth。永远不要把本机 `HERDR_MCP_TOKEN` 或 Cloudflare Token 粘贴进 ChatGPT。

## 附录：仅开发者从源码

只有在用户明确要求开发 herdr-mcp 本身时，才允许 clone + `npm`/`cargo`。该路径不得作为普通工作站的 runtime 安装主路径。
