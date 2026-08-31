# 快速 Agent 安装：用户一句话，Agent 完整协议

本页是 herdr-mcp **面向最终用户的 GA 上手路径**，不是 `docs/_wip/` 里的维护者 UAT 剧本。

## 粘贴给本地 Coding Agent 的一句话

复制下面整块发给 Codex、Claude Code、Cursor、Pi、Cline 等能读 URL 并执行命令的 Agent:

```text
请帮我安装并配置 Herdr 和 herdr-mcp，请先完整阅读并严格按照这个指引执行：https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/quick-agent-install.md 。

herdr-mcp 本机 runtime 使用 GitHub Releases，不用 git clone。只在 Cloudflare 登录/创建 API Token，以及 ChatGPT 添加 herdr Connector/插件这两类需要我本人操作的步骤暂停并指导我，其余步骤请自动完成并验证。
```

Agent 应读完本文并执行。Cloudflare Token 暂停细节也可参考 [Agent 协助安装](agent-install.md)。

## 完成后用户得到什么

- Herdr + 原生 `herdr-mcp` runtime 已安装且健康
- 个人 Cloudflare Edge Worker (`workers.dev` 或自定义域名)
- 工作站到 Edge 的出站 Herdr Link
- ChatGPT 开发者模式 + 自定义 MCP Connector 指向 `/mcp`
- 可选浏览器扩展 (仅在 ChatGPT 手动连通之后)

## Agent 合同 (简版)

1. 能自动化的 shell 直接执行；只在 Cloudflare 交互登录/API Token、无法自动判断的 Account 选择，或 ChatGPT 添加 `herdr` Connector/OAuth 时暂停。
2. **本机 MCP runtime 必须从 GitHub Releases 安装**; 除非用户明确要求开发 herdr-mcp, 否则不要 `git clone` + `npm`/`cargo`。
3. 每个 mutation 后验证 (`herdr-mcp doctor`, Link status, Edge `/health`, 公网 `/mcp`)。
4. 秘密不得写入仓库、日志、截图或 shell history。

## 前置条件

先检查 Herdr：

```bash
herdr --version
herdr api schema >/dev/null
```

如果 Herdr 未安装，Agent 直接按 Herdr 官方 stable 安装方式安装，不把用户丢到另一篇文档自行研究：

```bash
# macOS / Linux
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows 使用 Herdr 官方 PowerShell 安装器：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

安装后重新运行 `herdr --version` 和 `herdr api schema >/dev/null`；仍失败才暂停并说明问题。herdr-mcp 不替代 Herdr，但本协议负责把 Herdr 一并装好。

**平台:** 第一版 GA 以 macOS Apple Silicon 为主。Node.js 仅临时用于 `npx wrangler` 部署 Edge, 不是本机 runtime 依赖。

## 步骤 1 — 安装原生 runtime

1. 从 <https://github.com/whshang/herdr-mcp/releases> 下载 `herdr-mcp` — 使用最新 stable 版本（当前快照为 [`v0.4.2`](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.2)；后续始终优先最新已发布 stable tag）
2. 放到 `PATH` (如 `~/.local/bin/herdr-mcp`) 并赋予可执行权限
3. 执行:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

## 步骤 2 — 选择公网 Edge URL 策略

部署 Edge 前先确定一个 canonical public origin，OAuth、MCP、Link WSS 后续都引用同一个入口:

```text
你是否已有可指向 Cloudflare 的自有域名，或当前网络对 workers.dev 不稳定?
  ├─ 是 → 从第一次部署就使用自定义域名
  │       示例 MCP URL: https://herdr-mcp.example.com/mcp
  │       见下文「自定义域名路径」
  └─ 否 → 使用 workers.dev 作为无需 DNS 的 bootstrap
            示例 MCP URL: https://herdr-edge-device.username.workers.dev/mcp
```

| 场景 | 推荐公网 origin | ChatGPT Connector URL |
|---|---|---|
| 有域名 + Cloudflare zone | 自定义域名 | `https://herdr-mcp.example.com/mcp` |
| 无域名 / 最快首次安装 | `workers.dev` | `https://herdr-edge-device.username.workers.dev/mcp` |
| `workers.dev` 被拦 (中国 SNI) | 自定义域名 **或** `workers.dev` + 代理 | 同上 |

自定义域名操作详见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md#何时使用自定义域名)。

## 步骤 3 — Cloudflare Token 暂停 (仅人工)

打开 <https://dash.cloudflare.com/profile/api-tokens>, 用 **Edit Cloudflare Workers** 模板限定单个 Account。默认 `workers.dev` bootstrap **不要**加 DNS Write。

仅以临时进程环境注入:

```bash
export CLOUDFLARE_API_TOKEN='...'
```

验证与 Account 选择见 [Agent 协助安装](agent-install.md) §4–§5。部署后 unset Token。

## 步骤 4 — 部署 Edge

在 Agent 内存生成 (禁止打印): `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, `WORKSTATION_ID`, 以及:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

默认路径保持 `workers_dev = true`, `routes = []`。记录:

```text
EDGE_ORIGIN=https://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev
HERDR_EDGE_URL=wss://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev/ws
MCP_URL=${EDGE_ORIGIN}/mcp
```

`LINK_SHARED_SECRET` 存为 Worker secret。细节见 [Agent 协助安装](agent-install.md) §6。

### 自定义域名路径 (用户有域名时)

仅当用户在 Cloudflare 上有可用域名:

1. 为 hostname 添加 Worker route (如 `herdr-mcp.example.com/*`)
2. 配置 DNS 指向 Worker
3. 在 `wrangler.user.toml` 设置 `OAUTH_ISSUER=https://herdr-mcp.example.com`
4. 重新部署, 记录 `MCP_URL=https://herdr-mcp.example.com/mcp`

issuer 与 Connector URL 必须同一 origin。确定最终 `EDGE_ORIGIN` 后立即写入 herdr-mcp 实例配置，后续生成或重建 LaunchAgent 时统一派生同一个 WSS 入口：

```bash
herdr-mcp config set-edge-origin "$EDGE_ORIGIN"
```

## 步骤 5 — 安装 Herdr Link (含网络/中国说明)

安装托管 Rust Link:

```bash
herdr-mcp link install
herdr-mcp link status
```

将 Link LaunchAgent 上的 `HERDR_EDGE_URL` 与 `HERDR_WORKSTATION_ID` 设为与 Worker 一致。

### Link 代理 (中国 workers.dev 或系统代理)

Link 以**出站 WSS** 连 Edge。若 ChatGPT 走本地代理但 Link 直连被 reset, 在 `link install` 前或 LaunchAgent 环境变量中配置:

| 变量 | 用途 |
|---|---|
| `HERDR_LINK_PROXY` | Link WSS 显式代理 (最高优先级) |
| `HTTPS_PROXY` / `https_proxy` | 标准 HTTPS 代理 (用于 `wss://`) |
| `HTTP_PROXY` / `http_proxy` | HTTP 代理回退 |
| `ALL_PROXY` / `all_proxy` | 最后尝试 (仅 HTTP/HTTPS scheme) |

示例:

```bash
export HERDR_LINK_PROXY=http://127.0.0.1:7890
# 或复用已有 https_proxy (ChatGPT 已能上网时)
herdr-mcp link install
```

macOS 上若未设置 env, Link 还会读取 `scutil --proxy` 系统代理。

**Agent 行为:**

1. 探测 `https_proxy` / `HERDR_LINK_PROXY` / 系统代理
2. 若 ChatGPT 可用但探测不到代理, 仍继续 (透明代理可能已生效)
3. 若配置代理后 `workers.dev` 仍不可达, 向用户给出**两条路**:
   - 设置 `HERDR_LINK_PROXY` (或系统 `https_proxy`) 后重试 Link
   - **或** 改用网络可达的自定义域名

无代理时 Link 直连 (默认行为不变)。

## 步骤 6 — 验证

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

`herdr-mcp doctor` 应显示 Link 与 Edge 层健康 (`edge-reachable`, `oauth-metadata`, `mcp-endpoint`; `401 auth=not-sent` 可接受)。

完成后 unset `CLOUDFLARE_API_TOKEN`。

## 步骤 7 — 连接 ChatGPT (实用步骤)

**先于**浏览器扩展完成本步。

1. ChatGPT → **设置** → **插件/Connectors** (名称因套餐而异)
2. 开启 **Developer mode** (开发者模式)
3. 浏览连接器 → 右上角 **+**
4. 名称填 `herdr` (或任意短名)
5. Connector URL 填你的部署地址:
   - `https://herdr-edge-device.username.workers.dev/mcp`, 或
   - `https://herdr-mcp.example.com/mcp`
6. 勾选 **I understand and wish to continue**
7. 完成浏览器 OAuth
8. 在对话中: 添加插件 **或** 先建 Project 再添加插件 (后者更适合后续扩展接力)
9. **新开**会话, 第一条提示:

```text
分析我的 herdr 里有哪些项目
```

成功标准: OAuth 完成、工具列表出现、`herdr_inspect` 返回真实工作站。

详见 [ChatGPT Connector](chatgpt-connector.md)。

## 步骤 8 — 可选 Chrome Web Store 浏览器扩展

仅在步骤 7 成功后再做。

最终用户只通过 Chrome Web Store 安装，不用本地开发版替代，也不要求机器上存在 herdr-mcp git checkout：

1. 打开 <https://chromewebstore.google.com/>；
2. 搜索 `Herdr`，选择 Herdr 官方扩展；
3. 点击 **添加至 Chrome / Add to Chrome**；
4. Chrome Web Store listing 尚未正式上线时，直接跳过本步骤，不要回退到本地开发版。

`herdr-mcp doctor` 健康后安装 Native Messaging:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

Chrome Web Store 安装后，扩展版本更新由 Chrome 的正常扩展更新机制负责；普通用户不需要本地扩展安装包。见 [浏览器扩展](extension.md) 与 [浏览器连续性](browser-continuity.md)。

## 给用户的最终报告

只回报非敏感事实:

- 已安装 runtime 版本/generation
- `herdr-mcp doctor` 摘要
- Link 状态 + `HERDR_EDGE_URL` 主机名
- Cloudflare Account (名称 + 缩短 ID)
- Worker 名与公网 origin
- ChatGPT 使用的 MCP URL (`/mcp`)
- 选择了代理还是自定义域名

不要包含 `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, Cloudflare API Token。

## 维护者 UAT (不是本页)

第二台机器维护者验证见 [干净机 UAT](clean-machine-uat.md) 与归档 [第二台 Mac GA UAT Agent 提示词](../../history/ga/second-mac-ga-uat-agent-prompt-zh-CN.md)。不要把 34 步 UAT 发给最终用户。
