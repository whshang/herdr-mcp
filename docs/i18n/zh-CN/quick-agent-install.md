# 快速 Agent 安装协议

> **执行角色：Agent。** 本页不是给用户逐条抄命令的教程，也不是需要再次包装成一段“发给某个 Coding Agent”的提示词。收到安装 herdr-mcp 的任务后，Agent 直接读取并执行本文。

这是普通工作站安装的精简执行协议。更完整的 Cloudflare 权限、秘密处理、Link 与故障边界见 [Agent 安装合同](agent-install.md)。维护者 UAT 不属于本协议。

## 执行约束

1. 先读完本页，再做 mutation；能确定执行的 shell / 文件 /状态检查直接完成。
2. 只在 Cloudflare 交互登录/API Token、无法安全推断的 Account 选择、ChatGPT Connector/OAuth 等确实需要用户本人操作时暂停。
3. 普通工作站的 PROD runtime 从正式 GitHub Release 安装；除非任务明确是开发 herdr-mcp，否则不要用 git checkout、`target/*` 或本地 build 代替。
4. 如果网络、登录状态或第三方服务不可用，停止并报告 blocker；不要自行搭代理、切网络节点、改系统代理或发明旁路。
5. 每个关键 mutation 后验证实际状态；投递结果不确定时先读取状态，禁止盲目重试。
6. 秘密不得进入仓库、普通日志、截图、shell history 或最终报告。

## 完成后用户得到什么

- Herdr + 原生 `herdr-mcp` runtime 已安装且健康
- 个人 Cloudflare Edge Worker (`workers.dev` 或自定义域名)
- 工作站到 Edge 的出站 Herdr Link
- ChatGPT 开发者模式 + 自定义 MCP Connector 指向 `/mcp`
- 可选浏览器扩展（基础 Connector 连通后，按 runtime 实际支持选择 STORE / STANDALONE / DEV）

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

部署 Edge 前先确定一个 canonical public origin，OAuth、MCP、Link WSS 后续都引用同一个入口。只有用户已经明确选择自定义域名，或现有安装配置/策略能证明该意图时，才从一开始走自定义域名；运行中一旦发现连通性失败，先停下询问用户，不自动改路径：

```text
用户是否已明确选择可指向 Cloudflare 的自定义域名?
  ├─ 是 → 从第一次部署就使用该自定义域名
  │       示例 MCP URL: https://herdr-mcp.example.com/mcp
  │       见下文「自定义域名路径」
  └─ 否 → 使用 workers.dev 作为无需 DNS 的 bootstrap
            示例 MCP URL: https://herdr-edge-device.username.workers.dev/mcp
```

| 场景 | 推荐公网 origin | ChatGPT Connector URL |
|---|---|---|
| 用户已明确选择自定义域名 + Cloudflare zone | 自定义域名 | `https://herdr-mcp.example.com/mcp` |
| 无域名 / 最快首次安装 | `workers.dev` | `https://herdr-edge-device.username.workers.dev/mcp` |
| `workers.dev` 不可达 | 停止并让用户选择现有代理路径或自定义域名 | 由用户确认后决定 |

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

### Link 网络路径

Link 以**出站 WSS** 连 Edge。它可以复用用户已经配置好的代理环境；Agent 不得为了完成安装而自行新建或修改代理。

| 变量 | 用途 |
|---|---|
| `HERDR_LINK_PROXY` | Link WSS 显式代理 (最高优先级) |
| `HTTPS_PROXY` / `https_proxy` | 标准 HTTPS 代理 (用于 `wss://`) |
| `HTTP_PROXY` / `http_proxy` | HTTP 代理回退 |
| `ALL_PROXY` / `all_proxy` | 最后尝试 (仅 HTTP/HTTPS scheme) |

macOS 上 Link 也会读取现有 `scutil --proxy` 系统代理。这些能力用于识别/复用现状，不代表 Agent 可以修改系统网络。

**Agent 行为：**先只读检查当前 `HERDR_LINK_PROXY` / proxy env / 系统代理并按现状执行；如果目标仍不可达，立即停止并报告 blocker，让用户明确选择是否调整代理、网络或改用自定义域名。未经用户明确指示不得继续网络 mutation。

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

## 步骤 8 — 选择可选浏览器扩展通道

仅在步骤 7 成功后再做。扩展通道与 Runtime DEV/PROD 是两套不同概念：

- **STORE**：普通用户默认路径；由 Chrome Web Store 提供固定身份和更新。
- **STANDALONE**：v0.4.3+ 的 GitHub / 手动安装路径；使用固定非 Store 身份，不依赖 unpacked 目录路径。
- **DEV**：仅用于开发 herdr-mcp/extension 源码；从 repo/worktree `extension/` Load unpacked，身份由路径派生。

先让已安装 runtime 报告其支持的 Native Host 通道。当前 stable v0.4.2 只有 Store/DEV；不要假装它支持 standalone。v0.4.3+ 如果明确提供 `native-host use standalone`，则按以下顺序选择：

1. 用户未指定且 Store 可用 → **STORE**；
2. Store 不可用或用户明确要求 GitHub/独立分发，且 runtime 支持 → **STANDALONE**；
3. 只有任务明确是源码开发时 → **DEV**。

Store 路径安装官方 Herdr 扩展；Standalone 路径只使用正式固定身份 package；DEV 不得作为普通用户 fallback。随后根据所选通道安装/同步 Native Messaging，并运行：

```bash
herdr-mcp native-host status
```

状态必须明确显示预期 active channel / extension identity，并且 Native Host runtime 与当前 runtime generation 一致。见 [浏览器扩展](extension.md) 与 [浏览器连续性](browser-continuity.md)。

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
