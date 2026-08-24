# Cloudflare Edge Token

Herdr 的 Cloudflare Edge 部署、Worker Route 或 Custom Domain 管理都不应长期复用一个“全能” Cloudflare Token。推荐为 Herdr 单独创建一个 Account API Token，只授予两项权限：

- 目标 Zone（例如 `agentforme.cc.cd`）：**Workers Routes Write**。
- 所属 Account：**Workers Scripts Write**。

这足以发布 Worker、管理 Worker Route，也足以调用 Cloudflare Workers Domains API attach/detach Custom Domain。正常运行不需要 DNS Edit、Tunnel Edit 或 Account Admin。只有“把已经占用同一 hostname 的旧 CNAME/Tunnel 迁走”这一类一次性迁移，才需要单独处理旧 DNS 记录；不要因此扩大日常 Token 权限。

## 一键脚本

仓库提供：

```bash
bin/herdr-cloudflare-token
```

脚本通过 Cloudflare 官方 API 动态解析 Account ID、Zone ID 和权限组 ID，不硬编码用户账户标识。生成的 Token **不会打印到终端**，而是原子写入本地：

```text
~/.config/herdr-mcp/cloudflare-cutover.env
```

文件权限固定为 `0600`。默认包含：

```text
CLOUDFLARE_API_TOKEN=<secret>
CLOUDFLARE_ACCOUNT_ID=<account id>
HERDR_CUTOVER_ZONE=<zone name>
HERDR_CUTOVER_ZONE_ID=<zone id>
HERDR_CUTOVER_PATTERN=herdr-mcp.<zone>/*
HERDR_CUTOVER_WORKER=herdr-edge-prod
HERDR_CUSTOM_DOMAIN=herdr-mcp.<zone>
HERDR_CUTOVER_PROBE_KEYCHAIN_SERVICE=herdr-edge-prod-mcp-bearer
```

不要把这个文件提交到 Git、贴到 Issue、聊天或 CI 日志。

## 首次准备：bootstrap credential

Cloudflare 不允许“无凭据的程序”凭空创建 API Token，因此第一次自动化必须有一个 bootstrap credential。推荐两种方式：

1. **已有 Cloudflare 自动化 Token**：它至少要能读取目标 Zone，并具有 `Account API Tokens` 管理权限。把它临时放到当前 shell 的 `CLOUDFLARE_API_TOKEN`。
2. **只有网页登录态**：在 Cloudflare Dashboard 的 `Account → Account API tokens` 创建一次 bootstrap Token；也可以让带有用户登录态的 `ego-browser` 代为操作。bootstrap Token 只用于创建 Herdr 专用 Token，后续 Herdr 日常运行使用脚本生成的专用 Token。

bootstrap Token 不会被脚本写入 Herdr 配置文件。

## 创建

```bash
export CLOUDFLARE_API_TOKEN='<bootstrap token>'
bin/herdr-cloudflare-token --zone example.com
unset CLOUDFLARE_API_TOKEN
```

成功输出只包含非敏感状态，例如：

```json
{"ok":true,"code":"credential_created","mode":"600","permissions":["Workers Routes Write","Workers Scripts Write"]}
```

脚本随后自动验证：

- Account API Token 是 Active；
- 可以读取目标 Zone 的 Workers Routes；
- 可以读取 Account 的 Workers Scripts。

## 建议先 dry-run

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
```

它会完成 Zone/Account/权限组解析与 bootstrap 权限检查，但不会创建 Token，也不会写文件。

## 验证已有本地凭据

```bash
bin/herdr-cloudflare-token --verify-only
```

这不会显示 Token，只报告 API、Routes、Scripts 三个验证结果。

## 幂等与轮换

如果默认本地文件已经存在且验证通过，再次执行脚本会返回 `credential_already_ready`，不会重复创建 Token。

确实要轮换时：

```bash
bin/herdr-cloudflare-token --zone example.com --rotate
```

`--rotate` 会创建一个新 Token，并在创建成功后原子替换本地文件。旧 Token 不会被脚本自动吊销，避免在并发部署或连接仍使用旧凭据时造成中断；确认新 Token 已稳定使用后，再从 Cloudflare Dashboard 吊销旧 Token。

## 手工 Dashboard 等价步骤

如需人工复核，配置应与脚本完全一致：

1. `Account → Account API tokens → Create Token`。
2. `Start from scratch`。
3. 第一条 Policy：
   - Resource scope：`Specified Domains`；
   - 只选择目标域名；
   - `Workers Routes → Edit`。
4. `Add policy`，第二条 Policy：
   - Resource scope：`Entire Account`；
   - `Workers Scripts → Edit`。
5. 不添加 DNS、Tunnel、Account Admin 权限。
6. Review 页面应只看到 `Workers Routes Write` 与 `Workers Scripts Write`。
7. 创建后 Token 只显示一次，立即存入本地安全文件；不要在终端或聊天中回显。

## 与部署 / 生产切换结合

新安装默认先使用 `workers.dev`，不需要自己的域名。已有稳定域名时再选择 Custom Domain。

创建好本地凭据后：

```bash
set -a
source ~/.config/herdr-mcp/cloudflare-cutover.env
set +a

# Custom Domain 只读状态与生产 candidate preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain preflight
```

`bin/herdr-cloudflare-domain` 只调用 Workers Domains API，不会自行删除 DNS 或停止 Tunnel。旧架构如果已有同名 CNAME，Cloudflare 会拒绝直接 attach；必须先记录完整 rollback evidence，再在受控切换中删除冲突 CNAME、attach Custom Domain、验证，然后才退出旧 Tunnel。失败时 detach Custom Domain 并恢复旧 CNAME。详见 [`cloudflare-edge-deployment.md`](cloudflare-edge-deployment.md)。
