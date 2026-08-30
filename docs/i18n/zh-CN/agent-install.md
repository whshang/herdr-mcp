# 本地 Agent 安装与 `workers.dev` 部署

> **职责：** 面向维护者/自动化的实现参考，供本地 coding Agent 执行。最终用户应从 [快速 Agent 安装](quick-agent-install.md) 或 [安装](install.md) 开始。

这是一份给**本地 coding Agent**读取并执行的安装协议，不是给用户逐条复制命令的教程。面向最终用户的一句话安装入口见 [快速 Agent 安装](quick-agent-install.md)。目标是：用户只负责 Cloudflare 本人登录和创建 API Token；Agent 负责环境检查、Release 二进制安装、Cloudflare Worker、出站 WSS Link 和验证。

> 当前约束：完整的后台服务自动安装路径以 **macOS Apple Silicon** 为第一正式平台。Windows 可有 Release artifact 作为 preview。不要发明未支持的 Linux lifecycle 包装。Edge 部署可临时使用 Node/`wrangler`；本机 MCP runtime **必须**来自 GitHub Releases，而不是 `git clone` + `npm ci`。

## 0. Agent 合同

1. 能自动化的 shell 步骤直接执行；只在 Cloudflare 交互登录 / API Token 创建，或多个 Account 选择时暂停。
2. 不破坏已有工作。禁止对无关 checkout 做 `reset --hard`、`clean -fd` 或覆盖用户修改。
3. 首次安装就确定一个 canonical public origin，并让 Worker OAuth、MCP、Link WSS 全部使用同一个入口。`workers.dev` 仍是无需 DNS 的 bootstrap 路径；用户已有自定义域名，或工作站网络对 `workers.dev` 不稳定（例如中国大陆）时，从第一次部署就使用 Custom Domain。没有用户明确意图时，不创建或修改 Custom Domain/DNS zone。
4. Cloudflare Token 是高敏凭据。禁止回显或写入仓库、`.env`、普通日志、截图、shell history。优先进程环境注入；若必须落临时文件，用 mode `0600` 并在部署后立刻删除。
5. 每个 mutation 后先验证再继续。出错时先判断 mutation 是否已经提交，再决定是否重试。
6. **不要**用 clone 本仓库或 `npm`/`cargo` 安装本机 MCP runtime，除非用户明确要求贡献者/从源码开发会话。

## 1. 前置条件

运行 `herdr --version` 与 `herdr api schema >/dev/null`。需要可用的 `herdr` 与 Herdr socket（默认 `~/.config/herdr/herdr.sock`，或显式 `HERDR_SOCKET_PATH`）。若 Herdr 本身未安装/未运行，停下来并引导用户到 <https://herdr.dev>；herdr-mcp 不替代 Herdr。

Node.js 只用于临时 Cloudflare Worker 引导（`npx wrangler`）和可选贡献者工具链，**不是**运行本机 MCP runtime 的依赖。

## 2. 从 GitHub Releases 安装原生 runtime（主路径）

1. 从 <https://github.com/whshang/herdr-mcp/releases> 下载当前 stable 平台二进制（当前已发布 stable runtime：`v0.4.1`；`v0.4.2` 已合入但尚未 tag/publish）。只有明确测试 preview channel 时才选择 prerelease 标签。
2. 放到 `PATH`（例如 `~/.local/bin/herdr-mcp`）并赋予可执行权限。
3. 执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update          # same as update check
```

`install` 会在 `~/.config/herdr-mcp/runtime/` 写入不可变 generation，并把 `~/.local/bin/herdr-mcp` 指到 `runtime/current/herdr-mcp`。优先使用以上顶层命令。不要把 `herdr-mcp service install` 写成普通安装主路径。

只有明确测试 prerelease build 时才使用 `update.channel = "preview"`。当前 stable runtime 使用默认 `stable` channel 即可。

## 3. 在内存中生成身份，不要打印秘密

在 Agent 内存中生成：`HERDR_MCP_TOKEN`、`LINK_SHARED_SECRET`，以及限制在 `[A-Za-z0-9_.-]`、最长 64 字符的 `WORKSTATION_ID`。有临时 Edge checkout 时，`WORKER_NAME` 只能通过仓库 helper 生成，Agent 不得自造 hostname slug：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

`WORKER_NAME` 与 `WORKSTATION_ID` 故意使用不同语法。helper 会把 hostname 小写，把 `[a-z0-9-]` 以外字符安全替换，压缩/修剪 `-`，并保证完整 Worker 名不超过 63 且匹配 `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`。例如 `MacBook.local` → `herdr-edge-macbook-local`。秘密用 `openssl rand -hex 32` 一类强随机；最终报告不得包含秘密。

## 4. 唯一需要人暂停：Cloudflare API Token

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

浏览器扩展 / Native Messaging 仍是可选项，不是第一条 ChatGPT 闭环的必需。若用户之后要连续性，只从 Chrome Web Store 安装官方 **Herdr** 扩展；Store listing 尚未上线时直接跳过，不改用本地开发版。`herdr-mcp doctor` 健康且商店扩展安装完成后：

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

详见 [浏览器扩展](extension.md) 与 [浏览器连续性](browser-continuity.md)。

## 8. macOS 持久 Herdr Link

把 `LINK_SHARED_SECRET` 存进 Keychain，服务名 `herdr-edge-link-<WORKSTATION_ID>`。命令文本只能引用环境变量，不能写字面秘密。优先使用已安装 `herdr-mcp` 二进制提供的托管 Link 安装路径（`herdr-mcp link ...` / 当前 stable 产品文档）。不要把生产 Link 所有权留在仓库 Bash 包装上。

在中国或 `workers.dev` 被 SNI 拦截时，Link WSS 需走系统/显式代理或改用自定义域名。代理变量优先级：`HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`；macOS 还会读取 `scutil --proxy`。完整决策树见 [快速 Agent 安装](quick-agent-install.md) §5。

## 9. 验证闭环

验证本机 `server/discover`、`herdr-mcp status`、`herdr-mcp doctor`、Link status、Worker `/health`、公网 `/mcp`、OAuth discovery，并确认未创建 Custom Domain/DNS/Tunnel。doctor 可在不发送 token 的前提下探测 Edge `/health`、OAuth metadata、`/mcp`；永远不要打印 token。

## 10. 清理 bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` 与 `CLOUDFLARE_ACCOUNT_ID`，删除临时凭据文件和不再需要的临时 Edge checkout。不要把 Token 拷进项目配置。若是一次性 Token，建议吊销；否则迁到专用密钥管理/CI secret。

## 11. 最终报告

只回报非敏感事实：已安装 runtime generation/version、本机 MCP 状态、Herdr Link 状态、Cloudflare Account 名 + 缩短 ID、Worker 名、`workers.dev` origin、`/health`、`/mcp`。

最后引导用户开启 ChatGPT Developer mode，用 `/mcp` 创建自定义 MCP Connector 并完成 OAuth。永远不要把本机 `HERDR_MCP_TOKEN` 或 Cloudflare Token 粘贴进 ChatGPT。

## 附录：仅开发者从源码

只有在用户明确要求开发 herdr-mcp 本身时，才允许 clone + `npm`/`cargo`。该路径不得作为普通工作站的 runtime 安装主路径。
