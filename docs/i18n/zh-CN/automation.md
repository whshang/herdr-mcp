# GitHub Pages、CI/CD 与自升级

herdr-mcp 有三条刻意分离的自动化平面。它们共享版本控制，但不共享凭据或故障域。

## 1. GitHub Pages

Workflow：`.github/workflows/pages.yml`

发布的站点：

```text
https://whshang.github.io/herdr-mcp/
```

Pages artifact 包含：

- `site/` — 静态产品/安装页；
- 每个被跟踪的 `docs/*.md` 页面（排除 `docs/_wip/`）渲染出的 HTML；
- `herdr-mcp-SKILL.md` — 从 `assets/herdr-mcp-SKILL.md` 拷贝的公网 remote-planner 策略；
- `release.json` — 当前包版本 + Git commit + docs/skill 位置。

仓库是公开的，Pages 使用仓库原生的 GitHub Pages 部署。`npm run build:site` 是 CI 与 Pages workflow 共用的唯一构建路径，所以文档改动不可能绕过用于发布的同一个静态站点构建。

这让 Pages 既是给人看的文档站，也是 `herdr_skill` 的无凭据更新源。

`herdr_skill` 默认使用 Pages skill URL，缓存它，并在 Pages/网络不可用时回退到 release 内置的 `assets/herdr-mcp-SKILL.md`。设置 `HERDR_SKILL_NETWORK=0` 可完全离线，或用 `HERDR_MCP_SKILL_URL` 覆盖策略 endpoint。

## 2. CI

Workflow：`.github/workflows/ci.yml`

每次推送到 `main` 与每个 pull request 都运行：

1. `npm ci`；
2. TypeScript 构建；
3. 文档站构建（`npm run build:site`）；
4. root 测试套件；
5. Edge/frozen-contract 套件；
6. 浏览器扩展冒烟；
7. shell 语法检查；
8. npm 包 dry-run；
9. `git diff --check`。

Root runtime 版本可以与 ChatGPT 公网 contract epoch 独立演进。当前 production 冻结 epoch 2 为 18 tools；epoch-1 兼容测试只用于证明历史 17-tool 回滚/旧会话 ABI 仍可精确复现。

## 3. Cloudflare Edge 生产部署

Workflow：`.github/workflows/cloudflare-edge.yml`

该 workflow 只对影响 Edge/Relay/package 部署面的 `main` 改动运行，或手动触发。它先跑 Edge/contract 与 root 回归闸门，然后用 `cloudflare/wrangler-action@v4` 与 Wrangler major 4 部署 `edge/cloudflare/wrangler.prod.toml`。

部署 job 使用 GitHub Environment：

```text
production
```

必需的 Environment secrets：

```text
CLOUDFLARE_ACCOUNT_ID
CLOUDFLARE_API_TOKEN
```

最小权限目标是目标 Account 的 **Workers Scripts Write only**。当前预置的 GitHub Environment secret 复用了已有的 Herdr cutover 凭据，它带 **Workers Scripts Write + Workers Routes Write**，仍然没有 DNS Write、Tunnel Edit 或 Account Admin。这足够部署，但在能签发 API token 的 bootstrap 凭据可用时应换成 scripts-only token。部署 workflow 刻意不改动 Custom Domain 所有权/路由。

部署后 workflow 检查独立的 workers.dev health endpoint。现有 Custom Domain 继续指向同一个 Worker service。

## 4. 本机 runtime 自升级

CLI：`herdr-self-update`

Runtime release 平面刻意与 Cloudflare 部署分开。升级本机 herdr-mcp 不得重启公网 Edge 或持久 `herdr-link`。

典型远程 planner 流程：

```bash
herdr-self-update status
herdr-self-update check
herdr-self-update apply --source remote --ref main
```

测试未提交的开发树：

```bash
herdr-self-update apply --source working-tree
```

`apply` 启动一个 detached supervisor，在当前 MCP runtime 被重启之前返回。Worker 在 `~/.config/herdr-mcp/` 下记录结构化进度，构建/测试一个隔离 release，启动 loopback candidate，用持久 generation manager 验证并激活它，从新 release 重载稳定 8772 runtime，提升新的 stable generation 并移除临时 candidate。

Updater 继承当前 contract profile。它**不会**自动改变 ChatGPT contract epoch、DNS、OAuth issuer、Custom Domain 或 Edge 部署。

升级后，从同一个远程 Connector 验证：

- `herdr_inspect` 报告新 runtime 版本；
- generation status 报告新的 stable generation；
- Edge `/status/<workstation>` 收敛到该 version/generation；
- 除非是有意的 contract 迁移，公网 contract epoch/hash 保持不变。

## 5. `herdr_skill` 职责

`herdr_skill` 不只是官方 Herdr 使用教程。它组合三层：

1. **herdr-mcp 项目策略** — 直接编辑/工具顺序、agent 派发偏好、mutation/幂等规则、浏览器边界与自维护流程；
2. **live runtime 上下文** — 运行版本、contract profile、generation/self-update 状态；
3. **与 release 匹配的原生 Herdr 参考** — `herdr --skill`，明确限定为 pane-local 参考，这样它的 `HERDR_ENV=1` 规则不会错误地拦住远程网页 planner。

项目策略对 ChatGPT/Web 用法有优先级。`herdr_methods` 仍是已安装原生 socket 方法名与 schema 的 live 权威。