# 自动化

*让文档、Edge 与本机 Runtime 独立部署。*

这篇面向维护者，回答的是 **CI/CD 怎样执行**，不是重新定义 release model。

长期 release 边界只有一份 SSOT：[`docs/release-model.md`](../../release-model.md)。其中 Runtime、Browser Extension、Contract Compatibility 是版本与兼容性平面；本页讨论的是日常自动化会触达的几个**部署面**：

```text
文档发布            公网 Edge 部署           本机 Runtime 激活
GitHub Pages         Cloudflare Worker        runtime generation A/B
     │                      │                        │
人类/Agent 文档         OAuth / MCP / WSS         fs / Git / shell / Herdr
```

这些动作共享同一个 Git 仓库，但不应共享凭据、回滚边界或故障域。Browser Extension 和 contract migration 也有各自独立流程，本页只说明它们如何与 CI 协作。

## 为什么自动化必须解耦

一次普通修复不应该顺手改变 OAuth identity、Cloudflare routing、本机 runtime generation、浏览器扩展身份和 ChatGPT tool contract。

执行原则是：**只触发本次任务真正需要的部署动作，并保持其它 release/compatibility plane 不变。**

例如：

- 修文档 → 只发布 Pages；
- 修 Edge relay → 只部署 Worker；
- 修 runtime implementation → 只验证并切换本机 generation；
- 改 public tool catalog → 走独立 contract compatibility migration；
- 只改扩展 UI → 走扩展自己的发布路径，不顺带发布 Runtime。

## 1. GitHub Pages：人类文档和 Agent 策略

Workflow：

```text
.github/workflows/pages.yml
```

站点：

```text
https://whshang.github.io/herdr-mcp/
```

唯一站点构建入口：

```bash
npm run build:site
```

构建器读取 `docs/i18n/en` 与 `docs/i18n/zh-CN` 的逻辑文档集合，生成双语 HTML、导航、搜索索引和首页资源。

Pages 还发布 Agent 可读的项目 skill 和 release metadata，因此它同时承担两种角色：

```text
Human docs
  └─ HTML 文档站

Remote planner policy
  └─ herdr-mcp-SKILL.md
```

`herdr_skill` 可以读取上游项目策略；网络不可用时回退到 release 内置副本。设置 `HERDR_SKILL_NETWORK=0` 可强制完全离线。

### 文档发布 gate

文档修改至少要通过：

```bash
npm run build:site
git diff --check
```

站点构建同时检查双语 slug 是否完整，因此新增正式章节不能只创建中文或只创建英文版本。

## 2. CI：证明一个 commit 没破坏其它平面

主 CI：

```text
.github/workflows/ci.yml
```

CI 的目的不是“部署一切”，而是给各个平面提供共享证据。

典型 gate 包括：

- dependency install；
- TypeScript build；
- 文档站 build；
- root runtime tests；
- Edge / frozen-contract tests；
- 浏览器扩展 smoke；
- shell syntax；
- package dry-run；
- `git diff --check`。

### Contract 测试为什么独立重要

Runtime implementation 可以频繁变化，但 ChatGPT 看到的 public MCP catalog 不应该悄悄变化。

当前公共 Edge contract 仍是 **epoch 3 / 19 actions**（自 v0.4.3 引入），workstation Runtime Execution Contract 仍是 **epoch 2 / 18 tools**。新增的 `herdr_devices` 只在 Edge 执行，不转发到 workstation。兼容测试可以保留历史 epoch 作为 rollback evidence，但普通 runtime commit 不应该因为“顺手改了 schema”就改变任一 contract。

### GitLab CI 与其它无人值守 MCP 调用方

不要把 `HERDR_MCP_TOKEN`、`STATIC_MCP_BEARER_SECRET` 或一个全局共享 access token 塞进 CI。共享密钥无法区分具体流水线，也无法针对单个项目独立监控、轮换和 revoke。

应该在任意一台已登记设备上创建独立的 **Automation Client**：

```bash
herdr-mcp automation create --name "gitlab:group/project:prod" --device <device-id-or-unique-name>
herdr-mcp automation list
herdr-mcp automation rotate <svc_client_id> --confirm
herdr-mcp automation revoke <svc_client_id> --confirm
```

每个真正独立的信任边界使用一个 client，通常至少按 GitLab project + environment 拆分。每个 Automation Client 必须绑定到一台已登记设备；`--device` 可以传不可变 `device_id` 或唯一设备名，Worker 会保存解析后的不可变 id。`create` 只显示一次长期 `client_secret`；`rotate` 只显示一次替换后的新 secret；Worker 只保存 verifier。把以下值存入 GitLab masked/protected variables：

```text
HERDR_MCP_URL
HERDR_MCP_CLIENT_ID
HERDR_MCP_CLIENT_SECRET
```

每个 job 开始时，用 `client_credentials` 在 Worker 的 `/oauth/token` 换取短期 MCP access token。access token 最长一小时，不发 refresh token。Worker 只在换 token 时更新 `last_token_issued_at_ms` 和 `token_issue_count`，不会为了监控而给每次 MCP 请求增加 Durable Object 写放大。

Automation Client 只有普通 MCP 权限，并且只作用于绑定设备。调用省略 `device` 时 Worker 自动路由到绑定设备；显式选择或引用其它设备会 fail closed。它没有 fleet-admin 权限，不能 pair/revoke Device、approve/revoke Connector、继续创建 Automation Client，也不能发现其它设备。revoke 以不可变 `client_id` 为对象，同时阻止后续换 token，并在 Worker 鉴权时立即使已经签出的 access token 失效。

Automation 的 list/rotate/revoke 都属于已登记 Device/operator 控制面；已批准 WebChat Connector 不获得这些管理方法。清单永远不返回长期 secret。默认不要把 `client_secret` 放进聊天记录，创建和 rotate 应在已登记设备的 CLI 上完成，再直接写入 CI secret manager。

## 3. Cloudflare Edge：稳定公网入口

Workflow：

```text
.github/workflows/cloudflare-edge.yml
```

Edge workflow 只负责公网控制面，例如：

- Worker / Durable Object；
- OAuth；
- MCP relay；
- workstation routing；
- post-deploy health。

生产部署凭据放在 GitHub Environment / Secrets，不进入仓库。

### Edge workflow 不应该顺手做什么

普通 Worker code deployment 不应该自动：

- 改 Custom Domain；
- 改 DNS；
- 停旧 Tunnel；
- 改 OAuth issuer；
- 改 workstation identity；
- 改本机 runtime generation。

Domain/DNS cutover 是另一类 mutation，必须独立验证和独立回滚。

详见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md) 与 [Cloudflare Edge 凭据](cloudflare-edge-token.md)。

## 4. 本机 Runtime：A/B，而不是原地覆盖

本机 runtime 发布使用 generation 机制：

```text
stable runtime A
       │
       ├─ candidate B 启动
       ├─ health / contract gate
       ├─ activate B
       └─ 保留 A 作为 rollback target
```

常用入口：

```bash
bin/herdr-runtime-generation status
bin/herdr-self-update status
bin/herdr-self-update check
```

`herdr-self-update` 的目标是把“构建 candidate → 验证 → activate → 观察”自动化，而不是把当前运行目录直接覆盖掉。

它继承当前 contract profile，不负责：

- contract epoch migration；
- Edge deployment；
- OAuth issuer 迁移；
- DNS / Custom Domain mutation。

详见 [Runtime A/B](runtime-self-upgrade.md)。

## 5. Contract epoch：第四种、但不是日常发布平面

公开 MCP tool surface 变化会影响 ChatGPT conversation snapshot，因此不能和普通 runtime upgrade 混在一起。

Contract migration 至少涉及：

```text
local runtime contract
  ↓
workstation link identity
  ↓
public Edge contract
  ↓
new ChatGPT conversation snapshot
```

这类变更频率应远低于普通代码发布，并需要明确 migration/rollback evidence。

## 6. 浏览器扩展如何进入发布链

扩展属于客户端连续性层。它和 runtime 共用仓库版本，但不应该因此共享安全边界。

发布/验收重点包括：

- manifest / JavaScript syntax；
- Native Messaging host compatibility；
- binding state；
- Auto gate；
- progress / settled；
- recovery / handoff；
- JSON→MCP bridge。

真实 UI 行为仍需要浏览器 UAT；Node smoke 只能证明状态机和 adapter 的一部分逻辑。

## 7. `herdr_skill` 是运行策略，不是发布脚本

`herdr_skill` 组合：

1. herdr-mcp 项目策略；
2. 当前 runtime / contract / generation 上下文；
3. 与 release 匹配的原生 Herdr guidance。

它告诉 Web planner **应该怎样使用当前环境**，而不是代替 CI、部署脚本或 runtime manager。

`herdr_methods` 仍是当前安装的 Herdr Socket API schema 权威。

## 推荐的发布判断

在执行任何发布前先问：

| 变化 | 应该动哪个平面 |
|---|---|
| 文档、导航、教程 | Pages |
| Worker/OAuth/relay | Edge |
| 本机工具实现 | Runtime A/B |
| 浏览器 continuity | Extension + runtime compatibility validation |
| tool catalog/schema ABI | Contract epoch migration |
| Custom Domain/DNS | 独立 domain cutover |

如果答案是“全部都要一起动”，应该再次检查是不是把几个本可独立验证的问题捆在了一起。

## 一次发布什么时候算完成

完成不等于 workflow 变绿。

根据发布平面，还要有相应的真实证据：

- Pages：目标页面实际生成、链接可达；
- Edge：health + workstation + OAuth/MCP；
- Runtime：active generation + real tool call + rollback target；
- Extension：真实目标站点上的 binding/Auto/recovery smoke；
- Contract：新会话拿到预期 tools snapshot。

自动化的价值不是少看几个日志，而是把每类变更的**验证边界和回滚边界固定下来**。
