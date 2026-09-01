# Cloudflare Edge

*给本地工作站一个稳定的公网入口。*

ChatGPT 在公网，本地 Herdr 工作站通常藏在 NAT、防火墙或公司网络之后。herdr-mcp 不要求你给开发机开放入站端口，而是让工作站主动建立一条出站连接到 Cloudflare Edge。

```text
ChatGPT
   │ HTTPS / OAuth / MCP
   ▼
Cloudflare Worker + Durable Object
   ▲
   │ authenticated WSS
   │
herdr-link
   │
   ▼
local herdr-mcp runtime
   │
   ▼
Herdr / Git / shell
```

这篇文档讲 Edge 为什么这样部署、第一次怎样跑通、什么时候需要 Custom Domain，以及旧 Tunnel 架构怎样安全迁移。

## 先记住三个原则

### 1. 新安装从 `workers.dev` 开始

不需要先买域名，也不需要先配置 DNS。

### 2. 工作站只建立出站连接

公网不能直接访问本机 `127.0.0.1:8772`。Edge 通过已经认证的 workstation link 把请求送回正确机器。

### 3. 公网 identity 要稳定

ChatGPT Connector、OAuth issuer 和 MCP resource 都依赖 public origin。第一次验证成功以后，不要把 Worker 名称或 Custom Domain 当成随手可改的临时变量。

## Edge 负责什么

Cloudflare 层主要承担：

- 稳定 HTTPS MCP endpoint；
- OAuth discovery / authorization / token；
- workstation identity 路由；
- 持久 WSS link 的连接管理；
- runtime online/offline 与 generation/version 状态；
- MCP request/response relay；
- 短时私有 R2 通用 artifact 中继（`/artifacts`，仅 Worker binding）。

Edge **不保存你的 Git 仓库**，也不代替本机 Herdr。代码、shell 和 Agent 仍在工作站执行。R2 桶只是临时通用 artifact 中继，不是素材库。

## 第一次部署：workers.dev

从用户配置模板开始：

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

`wrangler.user.toml` 是个人部署配置，不应提交包含 workstation identity 等本地部署信息的用户文件。

### 生成 Worker name

不要直接拿 `hostname` 当 Worker name。机器名里常有点号或其它不适合作为 DNS label 的字符。

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
printf '%s\n' "$WORKER_NAME"
```

`workers.dev` 的 Worker name 是一个 DNS label；Custom Domain 是完整域名，两者规则不同。

### 配置 public origin

首次部署完成后，Cloudflare 会给出类似：

```text
https://<worker>.<account-subdomain>.workers.dev
```

MCP endpoint：

```text
https://<worker>.<account-subdomain>.workers.dev/mcp
```

OAuth issuer / `HERDR_MCP_BASE_URL` 应使用同一个 origin：

```text
https://<worker>.<account-subdomain>.workers.dev
```

不要给 base URL 加 `/mcp`。

### 部署

先幂等创建私有 R2 中继桶，再部署 Worker。该桶只通过 Worker binding 访问，不要挂 public r2.dev 域名。

```bash
cd edge/cloudflare
node provision-r2.mjs --config wrangler.user.toml
npx wrangler deploy --config wrangler.user.toml
```

部署 Worker 成功只代表公网代码存在，下一步还要验证 workstation link。

## Workstation Link

`herdr-link` 在工作站主动向 Edge 建立认证 WSS。

```text
workstation ── outbound WSS ──► Edge
```

它负责：

- 证明 workstation identity；
- 保持持久连接；
- 接收发往该工作站的 MCP 请求；
- 把请求路由给当前 active runtime generation；
- heartbeat 上报 runtime generation/version。

因此 Edge 可以稳定存在，而本机 runtime 可以单独重启或 A/B 切代。

如果 OAuth 正常、Worker `/health` 也能打开，但工具调用报告 workstation offline，应该查 link，而不是重新安装 Connector。

v0.4.3+ 会先给最近在线过的 workstation 一小段纯内存 reconnect grace；如果 validated Link 没有回来，MCP 错误会明确带上 `retryable`、`delivery_state`、`retry_after_ms` 和有界只读 recovery policy，让 Agent 不需要猜“能不能重放”。本机 Link 自己负责 reconnect/backoff 与 prolonged-offline recycle；这条恢复链不依赖浏览器扩展状态，也不会为了请求恢复额外制造 request-led Durable Object write/alarm。精确的 replay 规则见 [故障排查](troubleshooting.md)。

## 第一次验收顺序

不要一上来就在 ChatGPT 里排所有问题。按层验证：

```text
1. local runtime
2. herdr-link
3. Edge health
4. OAuth metadata/token
5. public MCP initialize/tools/list
6. real herdr_inspect
7. ChatGPT new conversation
```

这能快速区分“Edge 代码部署失败”和“工作站没有接上”。

完整诊断见 [故障排查](troubleshooting.md)。

## 为什么不用 Cloudflare Tunnel 直连本机 MCP

早期最直觉的方案是：

```text
ChatGPT → Tunnel → local MCP
```

它能工作，但把公网 endpoint 和某个本机 runtime 进程绑得太紧：

- runtime restart 会直接影响公网连接；
- OAuth identity 和机器生命周期容易耦合；
- 多 workstation 路由困难；
- A/B runtime 切换不自然。

现在推荐：

```text
ChatGPT → stable Edge ← persistent link ← workstation
```

Cloudflare Tunnel 直连只保留为遗留迁移场景，不是新安装主路径。

## 什么时候使用 Custom Domain

`workers.dev` 已经能完整运行。如果你有自己的 Cloudflare zone，可以进一步绑定：

```text
https://herdr.example.com
```

Custom Domain 的价值主要是：

- public identity 更容易长期保持；
- OAuth issuer 归你自己的域名治理；
- 团队环境名称更清楚；
- 将来更换 Edge implementation 时，可以尽量保留外部 URL。

它不是 Herdr 技术前置条件。

## Custom Domain 操作

仓库提供独立控制器：

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

推荐流程：

```text
workers.dev 上先跑通
        ↓
preflight Custom Domain
        ↓
attach
        ↓
验证 health / OAuth / MCP / workstation
        ↓
稳定观察
```

**代码部署和域名切换分开做。** 一个新版本 Worker 是否正确，不应该依赖 DNS cutover 才能验证。

## 从旧 CNAME / Tunnel 迁移

只有已有旧架构时才需要这一节。

旧形态可能是：

```text
herdr.example.com
  ↓ CNAME
Cloudflare Tunnel
  ↓
local runtime
```

同一 hostname 上已有冲突 DNS 记录时，不能简单再 attach Worker Custom Domain。

安全迁移原则：

1. 新 Worker 在独立 `workers.dev` origin 已完全健康；
2. 记录旧 DNS / Tunnel rollback evidence；
3. 旧 Tunnel 在 cutover 期间继续在线；
4. 移除唯一冲突记录；
5. attach Worker Custom Domain；
6. 验证 public health、workstation、OAuth、当前 MCP contract 和一次真实只读 tool call；
7. 稳定后才退出旧 Tunnel；
8. 任一步失败，恢复旧入口。

仓库提供事务化 helper：

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

这里最大的安全要求仍然是：**不对投递状态未知的 DNS/domain mutation 盲重试，而是重新读取 Cloudflare 的真实状态。**

## Cloudflare API 凭据

部署 credential 与 ChatGPT OAuth 完全不同。

项目 helper：

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
```

目标是长期只保留最小权限。详见 [Cloudflare Edge 凭据](cloudflare-edge-token.md)。

旧 DNS cutover 如果确实需要 DNS Write，应使用单独、短生命周期的凭据，不要把 DNS 管理权限永久叠加到日常 Worker deployment token。

## GitHub Actions 与自动部署

仓库的 production Edge workflow 负责在 `main` 相关部署面变化后：

1. 安装依赖；
2. 构建并运行 Edge / contract regression；
3. 通过 production Environment gate；
4. 用 Wrangler 部署生产 Worker；
5. 做发布后 health 验证。

CI credential 应放在 GitHub Environment/Secrets 中，不写进仓库。

普通 Worker code deploy 不应该修改：

- Custom Domain；
- OAuth issuer；
- workstation identity；
- DNS；
- ChatGPT Connector URL。

这些边界保持独立，才能让“发布代码”和“切生产入口”分别回滚。

## Edge 与 Runtime A/B 是两层发布

```text
Public plane
Cloudflare Edge / OAuth / Connector URL

Local plane
herdr-link → runtime generation A/B
```

大部分 runtime 修复只需要在本机 generation 层发布，不应该修改 public Edge identity。

反过来，Edge relay/OAuth 实现更新也不意味着要同时重启本机 Herdr。

详见 [Runtime A/B](runtime-self-upgrade.md)。

## 安全边界

- 工作站没有公网入站端口；
- Link 使用认证 WSS；
- ChatGPT 使用 OAuth，不获取本机 static bearer；
- Cloudflare API secret 不进入 Git；
- OAuth signing material 不进入 Git；
- deployment credential 使用最小权限；
- Worker deployment 和 Custom Domain/DNS mutation 分开；
- Edge 只负责远程控制面，真实代码和执行留在工作站。

## 推荐部署选择

| 场景 | 推荐 |
|---|---|
| 第一次安装 / 自用 | `workers.dev` |
| 长期个人入口 | `workers.dev` 或稳定 Custom Domain |
| 团队正式环境 | Custom Domain + Environment secrets |
| 已有旧 Tunnel/CNAME | 先并行验证 Worker，再事务化 cutover |
| 只是本机 Cursor/curl | 不需要 Cloudflare Edge |

如果目标只是尽快让 ChatGPT 开始操作本机项目，请回到 [安装](install.md) 按最短路径跑通；本页主要用于理解和维护公网控制面。
