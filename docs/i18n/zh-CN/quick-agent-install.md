# 快速 Agent 安装：用户一句话，Agent 完整协议

本页是 herdr-mcp **面向最终用户的 GA 上手路径**，不是 `docs/_wip/` 里的维护者 UAT 剧本。

## 粘贴给本地 Coding Agent 的一句话

复制下面整块发给 Codex、Claude Code、Cursor、Pi、Cline 等能读 URL 并执行命令的 Agent:

```text
请帮我安装 herdr-mcp。完整协议请阅读并严格执行: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/quick-agent-install.md 。本机 runtime 用 GitHub Releases (不要 git clone)。只在 Cloudflare 登录/创建 API Token 时暂停。不要回显或提交任何秘密。
```

Agent 应读完本文并执行。Cloudflare Token 暂停细节也可参考 [Agent 协助安装](agent-install.md)。

## 完成后用户得到什么

- Herdr + 原生 `herdr-mcp` runtime 已安装且健康
- 个人 Cloudflare Edge Worker (`workers.dev` 或自定义域名)
- 工作站到 Edge 的出站 Herdr Link
- ChatGPT 开发者模式 + 自定义 MCP Connector 指向 `/mcp`
- 可选浏览器扩展 (仅在 ChatGPT 手动连通之后)

## Agent 合同 (简版)

1. 能自动化的 shell 直接执行; 只在 Cloudflare 交互登录/API Token 或多 Account 选择时暂停。
2. **本机 MCP runtime 必须从 GitHub Releases 安装**; 除非用户明确要求开发 herdr-mcp, 否则不要 `git clone` + `npm`/`cargo`。
3. 每个 mutation 后验证 (`herdr-mcp doctor`, Link status, Edge `/health`, 公网 `/mcp`)。
4. 秘密不得写入仓库、日志、截图或 shell history。

## 前置条件

```bash
herdr --version
herdr api schema >/dev/null
```

Herdr 未就绪则引导用户到 <https://herdr.dev>。herdr-mcp 不替代 Herdr。

**平台:** 第一版 GA 以 macOS Apple Silicon 为主。Node.js 仅临时用于 `npx wrangler` 部署 Edge, 不是本机 runtime 依赖。

## 步骤 1 — 安装原生 runtime

1. 从 <https://github.com/whshang/herdr-mcp/releases> 下载 `herdr-mcp` — 使用最新 stable 版本（[`v0.4.0`](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0) 或更新的 stable tag）
2. 放到 `PATH` (如 `~/.local/bin/herdr-mcp`) 并赋予可执行权限
3. 执行:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

## 步骤 2 — 选择公网 Edge URL 策略

部署 Edge 前先决定 ChatGPT 如何访问工作站:

```text
你是否有可指向 Cloudflare 的自有域名?
  ├─ 有 → 优先用自定义域名作为 Connector URL
  │       示例 MCP URL: https://herdr-mcp.example.com/mcp
  │       见下文「自定义域名路径」
  └─ 无 → 首次安装用 workers.dev
            示例 MCP URL: https://herdr-edge-device.username.workers.dev/mcp
            中国/受限网络还需配置 Link 代理 (见下文)
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

issuer 与 Connector URL 必须同一 origin。

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

## 步骤 8 — 可选浏览器扩展 (ChatGPT 手动可用之后)

仅在步骤 7 成功后再做。

git checkout 里的 `extension/` 常在隐藏目录旁, **Load unpacked** 时推荐复制到可见路径:

```bash
cp -R extension ~/Documents/herdr-mcp-extension
# 或: ln -s "$(pwd)/extension" ~/Documents/herdr-mcp-extension
```

Chrome 打开 `chrome://extensions` → 开发者模式 → **加载已解压的扩展程序** → 选 `~/Documents/herdr-mcp-extension`。

**备选:** 文件选择器按 **Cmd+Shift+.** 显示隐藏文件, 再选 checkout 里的 `extension/`。

`herdr-mcp doctor` 健康后安装 Native Messaging:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

见 [浏览器连续性](browser-continuity.md)。

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

第二台机器维护者验证见 [干净机 UAT](clean-machine-uat.md) 与内部 [第二台 Mac GA UAT Agent 提示词](../../_wip/zh-CN/second-mac-ga-uat-agent-prompt.md)。不要把 34 步 UAT 发给最终用户。
