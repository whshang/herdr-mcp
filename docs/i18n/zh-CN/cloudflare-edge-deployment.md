# Cloudflare Edge 部署：`workers.dev` 默认，自定义域名可选

Herdr 的 Cloudflare Edge 不要求用户拥有自己的域名。

推荐把部署分成两个层次：

1. **默认 / 开箱即用：`workers.dev`** —— 任何 Cloudflare Workers 用户都可以直接部署，不需要购买或托管域名。
2. **可选 / 长期稳定入口：Custom Domain** —— 已有自己的域名时，建议绑定一个稳定子域名，但这不是 Herdr 的运行前提。

Cloudflare 官方也把 Custom Domain 定义为“Worker 自己就是 origin”的场景；Herdr Edge 正属于这种架构。`workers.dev` 则非常适合首次安装、开发、自用和独立验证。

## 架构

### 默认：不需要自定义域名

```text
ChatGPT
   |
https://herdr-edge.<account>.workers.dev/mcp
   |
Cloudflare Worker + Durable Object
   ^
authenticated WSS
   |
herdr-link
   |
local Herdr runtime
```

这种模式下不需要：

- 自有域名；
- DNS 记录；
- Cloudflare Tunnel；
- 公网 VPS；
- 入站端口映射。

`herdr-link` 主动向 Cloudflare 建立 WSS 连接，本机保持不可从公网直接访问。

### 可选：稳定 Custom Domain

```text
ChatGPT
   |
https://herdr.example.com/mcp
   |
Cloudflare Worker Custom Domain
   |
Cloudflare Worker + Durable Object
   ^
WSS
   |
herdr-link
```

Custom Domain 的价值是**稳定命名和所有权**，而不是 Herdr 技术上的必需条件。以后即使 Edge 实现从 Cloudflare 迁移到其他平台，用户仍可保留同一个 MCP/OAuth URL。

## 默认部署：`workers.dev`

从通用模板开始：

```bash
cp edge/cloudflare/wrangler.user.example.toml edge/cloudflare/wrangler.user.toml
```

`wrangler.user.toml` 已被忽略，不会误提交个人 Worker 名、workstation ID 或
OAuth issuer。

Worker 配置保持：

```toml
name = "herdr-edge"
main = "src/index.ts"
workers_dev = true
routes = []
```

部署：

```bash
cd edge/cloudflare
npx wrangler deploy
```

部署完成后 Cloudflare 会提供：

```text
https://<worker-name>.<account-subdomain>.workers.dev
```

Herdr 的 MCP URL 即：

```text
https://<worker-name>.<account-subdomain>.workers.dev/mcp
```

如果没有自定义域名，OAuth issuer 也应使用同一个稳定 `workers.dev` origin，避免 MCP endpoint 与 OAuth identity 分裂：

```toml
[vars]
OAUTH_ISSUER = "https://<worker-name>.<account-subdomain>.workers.dev"
```

> Worker 名称或 Cloudflare account subdomain 变化会改变 URL。已经接入 ChatGPT 后，应把这个 origin 当成稳定 identity，不要随意改名。

## GitHub Actions 自动部署 production Edge

仓库内置：

```text
.github/workflows/cloudflare-edge.yml
```

当 `main` 上的 Edge / Relay / package 部署面发生变化时，workflow 会：

1. `npm ci`；
2. 跑完整 Edge/frozen-contract Gate；
3. 跑 root regression tests；
4. Gate 全绿后进入 GitHub `production` Environment；
5. 用 `cloudflare/wrangler-action@v4` + Wrangler major 4 部署 `wrangler.prod.toml`；
6. 对独立的 `workers.dev/health` 做发布后验证。

Environment secrets：

```text
CLOUDFLARE_ACCOUNT_ID
CLOUDFLARE_API_TOKEN
```

自动部署**不会**修改 Custom Domain、DNS、Tunnel 或 OAuth issuer。生产域名继续指向同一个 Worker service，因此普通代码发布不需要重连 ChatGPT Connector。

最小权限目标是 `Workers Scripts Write`。当前本项目 GitHub Environment 暂时复用了已有的 Herdr cutover Token（`Workers Scripts Write + Workers Routes Write`，没有 DNS/Tunnel/Admin 权限），可以正常部署但权限仍比理想值多一个 Routes Write；后续拿到可签发 Token 的 bootstrap credential 时应换成 scripts-only。

文档站点与 Edge 是两条独立 workflow：Pages 失败不会阻断 Worker，Worker 部署也不会携带 Pages 凭据。详见 [`automation.md`](automation.md)。

## 可选部署：Custom Domain

有自己的 Cloudflare zone 时，可以把例如：

```text
herdr.example.com
```

绑定到已经验证通过的 Worker。

Wrangler 支持：

```toml
[[routes]]
pattern = "herdr.example.com"
custom_domain = true
```

但 Herdr 推荐把**代码部署**和**生产域名切换**分开：

1. Worker 始终先通过 `workers.dev` 部署和验证；
2. 确认 `/health`、WSS Link、MCP tools/list、OAuth 都正常；
3. 最后才绑定 Custom Domain。

仓库提供独立控制器：

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

它调用 Cloudflare Workers Domains API，**不会删除或修改 DNS，不会停止 Tunnel**。

如果是从旧版 `CNAME -> Cloudflare Tunnel` 迁移，而不是新安装，再使用事务化
迁移器：

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

`run` 只应该执行一次。它把以下步骤作为一个事务处理：

1. 精确比对旧 CNAME 与本地 rollback evidence；
2. 确认生产 Worker candidate、OAuth identity 和旧 Tunnel 均健康；
3. 删除唯一冲突 CNAME；
4. attach Worker Custom Domain；
5. 验证 health、workstation、epoch/hash、17-tool catalog、OAuth/MCP identity 和一次只读 `herdr_inspect`；
6. 任一步失败时自动 detach Custom Domain，并恢复原 CNAME；
7. 对 DNS DELETE/POST 和 Custom Domain PUT/DELETE 的“服务端已提交但响应丢失”场景重新读取真实状态，不盲重试。

事务状态写到用户本地 `~/.config/herdr-mcp/`，权限 `0600`，不进入 Git。

对应环境：

```text
CLOUDFLARE_API_TOKEN
CLOUDFLARE_ACCOUNT_ID
HERDR_CUTOVER_ZONE
HERDR_CUTOVER_ZONE_ID
HERDR_CUSTOM_DOMAIN
HERDR_CUTOVER_WORKER
HERDR_CUTOVER_PROD_EDGE
HERDR_CUTOVER_WORKSTATION
```

macOS 上生产 smoke-test bearer 默认从 Keychain service：

```text
herdr-edge-prod-mcp-bearer
```

读取，不需要把 bearer 写入仓库或命令行。

## 从 Tunnel/CNAME 迁移到 Custom Domain

这是唯一需要特殊处理的场景。

Cloudflare 不允许在**已有 CNAME** 的同一 hostname 上直接创建 Worker Custom Domain。因此，如果旧架构是：

```text
herdr.example.com -> CNAME -> Cloudflare Tunnel
```

不能直接覆盖。

安全迁移顺序应是：

1. `workers.dev` 上的生产 Worker 完整通过 preflight；
2. 记录旧 DNS CNAME 的完整值和代理状态，作为 rollback evidence；
3. 保持旧 Tunnel 进程在线；
4. 删除冲突 CNAME；
5. 立即通过 Workers Domains API 把同一 hostname 绑定到已经验证的 Worker；
6. 对 Custom Domain 验证：
   - `/health`；
   - workstation online；
   - epoch-1 `tools/list`；
   - OAuth discovery / token；
   - 一次真实 MCP tool call；
7. 观察通过后，旧 Tunnel 再退出服务；
8. 如果第 5/6 步失败：detach Custom Domain，并恢复第 2 步记录的旧 CNAME；旧 Tunnel 因为一直在线，可以立即重新承接流量。

不要先关闭 Tunnel，也不要在没有 DNS rollback evidence 的情况下删除 CNAME。

### 一次性 DNS 凭据

正常 Herdr Edge 凭据不需要 DNS Edit。迁移旧 CNAME 时，推荐额外创建一个**一次性、仅目标 Zone 的 `DNS Write` Token**，保存在：

```text
~/.config/herdr-mcp/cloudflare-dns-cutover.env
```

仓库提供独立的一键工具，避免把 DNS 权限混进长期 Edge Token：

```bash
# 先用具有 Account API Token 管理权限的 bootstrap credential
export CLOUDFLARE_API_TOKEN='<bootstrap token>'

# 创建，仅给 example.com 这个 Zone 的 DNS Write
bin/herdr-cloudflare-dns-token --zone example.com

# 不回显 secret 的验证
bin/herdr-cloudflare-dns-token --verify-only

# rollback observation window 结束后吊销并删除本地 credential 文件
bin/herdr-cloudflare-dns-token --revoke
```

脚本动态解析 Account、Zone 和 `DNS Write` permission group，不硬编码用户账户 ID；
创建出来的 Token 不打印到终端，本地文件强制 `0600`。

它只在迁移和 rollback observation window 内保留；Custom Domain 稳定、旧 Tunnel
正式退出后再吊销。不要为了这次迁移给长期 Herdr Token 增加 DNS 权限。

## 为什么不开源项目强制 Custom Domain

强制用户提供域名会给安装流程增加与 Herdr 核心能力无关的门槛：域名购买、Cloudflare zone onboarding、DNS 和证书管理。

因此项目约定：

| 部署模式 | 支持级别 | 适合场景 |
| --- | --- | --- |
| `workers.dev` | **默认、完整支持** | 首次安装、自用、开发、测试、没有域名的用户 |
| Custom Domain | **推荐、可选** | 长期稳定入口、团队/正式环境、希望固定 OAuth/MCP identity |
| Cloudflare Tunnel 直连本机 MCP | **Legacy / 迁移兼容** | 从旧版本升级，不再作为新安装默认架构 |

## 安全边界

无论使用哪种域名模式，都保持以下边界：

- Cloudflare Edge 是公网入口；
- `herdr-link` 只建立出站 WSS；
- 本地 Herdr runtime 不直接暴露公网端口；
- Link secret、OAuth signing material、MCP bearer 不进入 Git；
- Token 使用最小权限，参见 [`cloudflare-edge-token.md`](cloudflare-edge-token.md)；
- Custom Domain 切换和 Worker code deployment 是两个独立操作，可以分别回滚。

## 相关脚本

```text
bin/herdr-cloudflare-token   # 创建/验证 Cloudflare 最小权限 Token
bin/herdr-cloudflare-domain  # Custom Domain attach/watch/detach
bin/herdr-edge-cutover       # Legacy Worker Route 迁移/回滚工具
bin/herdr-link               # workstation -> Edge WSS sidecar
```

新安装优先使用 `workers.dev`；已有稳定域名的用户再选择 Custom Domain。不要因为文档展示了自定义域名示例，就把它当成安装前置条件。
