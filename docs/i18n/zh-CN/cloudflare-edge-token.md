# Cloudflare Edge 凭据：最小权限、临时引导、可验证

部署 herdr-mcp Edge 需要 Cloudflare API 权限，但长期运行不应该依赖一个拥有整个账号权限的万能凭据。

这篇文档说明项目如何创建和使用 **最小权限的 Cloudflare Account API Token**，以及为什么凭据处理与 Edge 架构本身需要分开理解。

## 两类凭据不要混在一起

部署流程里可能出现两类 Cloudflare 凭据：

1. **bootstrap credential**：已有的较高权限凭据，只在创建目标最小权限 token 时临时使用；
2. **runtime/deployment credential**：herdr-mcp 后续部署或切换实际使用的最小权限 token。

目标是让第 1 类尽快退出流程，而不是把它长期写进项目配置。

## 项目需要的最小权限

当前 helper 为指定 zone / account 创建：

- **Workers Routes Write**：限定到目标 zone；
- **Workers Scripts Write**：限定到对应 account。

它不会为了方便默认申请全账号管理员权限。

如果首次部署只使用 `workers.dev`，是否需要 zone route 权限取决于你实际走的部署路径；本 helper 主要服务既有 Edge/cutover 工作流和 Custom Domain/route 场景。不要因为文档列出了权限，就给无关资源扩大作用域。

## 使用项目 helper

脚本：

```text
bin/herdr-cloudflare-token
```

查看参数：

```bash
bin/herdr-cloudflare-token --help
```

常用模式：

```bash
# 只解析 account/zone 并检查 bootstrap 权限，不创建新 token
bin/herdr-cloudflare-token --zone example.com --dry-run

# 创建并保存目标最小权限凭据
bin/herdr-cloudflare-token --zone example.com

# 只验证已经保存的凭据
bin/herdr-cloudflare-token --zone example.com --verify-only

# 明确轮换已经存在的本地凭据
bin/herdr-cloudflare-token --zone example.com --rotate
```

bootstrap credential 通过进程环境提供：

```bash
export CLOUDFLARE_API_TOKEN='<temporary-bootstrap-token>'
# 或 CF_API_TOKEN
```

不要把真实值写进仓库文档、commit、截图或聊天记录。

## 本地保存位置

helper 默认写入：

```text
~/.config/herdr-mcp/cloudflare-cutover.env
```

文件使用受限权限（**mode `0600`**），脚本不会把 token 值打印到标准输出。

保存内容还包含 account / zone identity，用于后续验证目标 token 是否真的能访问预期的 Workers Scripts / Routes API。

这个文件是**本机凭据状态**，不是项目配置，不应该加入 Git。

## 为什么先 dry-run

创建 token 本身也是 mutation。第一次接入一个 Cloudflare 账号时，建议先：

```bash
bin/herdr-cloudflare-token --zone <zone> --dry-run
```

它可以提前发现：

- zone 名称不唯一或不存在；
- bootstrap credential 没有足够权限；
- account / zone identity 解析异常；
- 目标权限组不可用。

先验证，再创建，可以避免不断产生废弃 token。

## 为什么不能看到失败就自动 rotate

如果本地已经存在凭据，脚本默认不会悄悄覆盖，而是要求显式 `--rotate`。

这是因为凭据轮换可能影响：

- 正在进行的 Edge 部署；
- CI / 本机脚本；
- 其它仍引用旧 token 的流程。

所以 rotation 是一个需要明确意图的 mutation。

## 验证什么

`--verify-only` 不只是检查“token 字符串存在”，而是确认保存的 credential 对预期 Cloudflare API 具备实际可用性。

理想验收包括：

- token active；
- account identity 正确；
- Workers Scripts 权限有效；
- 目标 zone 的 Workers Routes 权限有效。

部署失败时，先区分“凭据不可用”和“Worker/DO 配置错误”，不要把两类问题混在一起。

## ChatGPT 不需要这个 token

Cloudflare deployment credential 和 ChatGPT Connector OAuth 是完全不同的凭据层。

```text
Cloudflare API token
  用于：部署/维护 Edge

ChatGPT OAuth token
  用于：ChatGPT 访问已经部署好的 MCP Edge

HERDR_MCP_TOKEN
  用于：本机 curl / Cursor / legacy local compatibility
```

三者不要互相复制。

尤其不要把 Cloudflare API token 或 `HERDR_MCP_TOKEN` 填进 ChatGPT Connector UI。

## 凭据卫生

建议遵守：

- bootstrap token 只通过临时进程环境传递；
- 最小权限 token 只保存在本机受限文件或正式 Secret Store；
- 不放进 `wrangler.toml`、README、`.env` 示例或 Git；
- 不在 shell 输出中 `echo` 真值；
- 自动化日志只记录 credential 是否 ready，不记录 secret；
- 需要扩权时先判断是不是部署路径设计问题，而不是习惯性加管理员权限；
- rotation 后验证新 token，再清理旧 token。

## 与 Edge 部署的关系

第一次完整安装建议先用 `workers.dev` 把 Worker、workstation link、OAuth 和 MCP 跑通，再处理 Custom Domain / route。

Edge 架构和部署步骤见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)；最短安装路径见 [安装](install.md)。
