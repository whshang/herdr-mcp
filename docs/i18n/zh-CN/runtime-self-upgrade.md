# Runtime 升级

*通过 A/B 升级让本机 runtime 更新不打断远程开发。*

herdr-mcp 把公网入口和本机 runtime 拆成两个独立的发布平面。

对 ChatGPT 来说，公网 Edge、OAuth issuer 和 MCP URL 应该长期稳定；对工作站来说，herdr-mcp runtime 可以升级、验证、切换和回滚，而不必让 ChatGPT Connector 跟着重连。

```text
ChatGPT
  │ stable MCP/OAuth origin
  ▼
Cloudflare Edge
  │ persistent workstation WSS
  ▼
herdr-link
  │ active generation pointer
  ├───────────────┐
  ▼               ▼
runtime A       runtime B
127.0.0.1:8772 127.0.0.1:8773
```

这就是 Runtime A/B。

## 它解决什么问题

如果公网入口直接指向某一个本机 runtime 进程，那么一次普通升级可能同时影响：

- HTTP/MCP 连接；
- OAuth identity；
- 正在进行的工具调用；
- ChatGPT Connector；
- 回滚路径。

A/B 把这些问题分开。

一个 candidate runtime 先在新的 loopback endpoint 启动并验证，通过 gate 后才成为 active。旧 generation 在需要时继续 drain 已经进入它的请求，并保留为 rollback 目标。

## Runtime generation 是什么

一个 generation 是本机某一份可独立访问的 herdr-mcp runtime 实例，例如：

```text
generation A
  endpoint: http://127.0.0.1:8772/mcp
  version: current stable

generation B
  endpoint: http://127.0.0.1:8773/mcp
  version: candidate
```

`herdr-link` 持有 active-generation pointer。Edge 并不需要知道端口切换细节；它只和同一个 workstation link 通信。

因此：

```text
切换 runtime ≠ 切换 Connector
```

这是整个机制最重要的边界。

## DEV / PROD 是 generation 之上的 provenance 平面

v0.4.3+ 在同一套 generation 机制之上增加明确的源码开发平面，但**不会**因此多出第三套长期 runtime 环境：

```text
PROD
  published / verified installed binary
  固定 recovery source

DEV
  带 source provenance 的 repo/worktree build
  作为 managed generation 激活
```

日常源码 dogfood 使用：

```bash
herdr-mcp dev status
herdr-mcp dev sync
herdr-mcp dev rollback
```

`dev sync` 在激活 repo-built DEV generation 前先固定当前 PROD binary/checksum，而且只有 server/Link generation reconcile 完成后才接受切换。`dev rollback` 回到这个固定 PROD source，不重新编译源码，也不猜“哪个 previous generation 才是稳定版”。底层 `bin/herdr-runtime-generation ...` 仍可用于实现/UAT，但不再是维护者日常源码 dogfood 的主入口。

Runtime DEV/PROD 与浏览器扩展的 DEV/STANDALONE/STORE 身份模型无关。

## A/B 不是进程管理器

Generation manager 不负责“随便启动任何命令”。

推荐流程是：

1. 构建 candidate；
2. 独立启动 candidate runtime；
3. 注册 generation；
4. 运行 health / contract gate；
5. activate；
6. 观察；
7. 成功后再回收旧 generation。

把 process creation 和 traffic activation 分开，可以让“新程序能不能启动”和“是否应该接生产请求”成为两个不同问题。

## CLI

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation candidate-<id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate \
  --generation candidate-<id>

bin/herdr-runtime-generation rollback

bin/herdr-runtime-generation remove \
  --generation candidate-<id>
```

生命周期 mutation 禁止使用 `launchctl submit`。launchd 的 inferred job 可能在命令退出后继续 replay，导致 rollback/update 等破坏性操作重复执行。需要独立进程时使用受管 lifecycle 命令；确需 launchd one-shot job 时，必须使用显式 plist，并设置 `RunAtLoad=true`、`KeepAlive=false`。

`status` 是第一入口。任何升级/回滚前都应该先知道：

- desired active；
- actual active；
- previous generation；
- last known good；
- candidate health。

不要根据上一次发布日志猜当前 active generation。

## Activation gate 检查什么

Candidate 只有在真实 loopback endpoint 上通过验证后才允许切流。

核心 gate 包括：

1. endpoint 可达；
2. runtime health / discovery 可用；
3. 真实 `tools/list` 可用；
4. public tool contract 与当前 contract epoch 匹配；
5. 如果声明 expected runtime version，则版本一致；
6. 必要时连续多个 observation window 保持健康。

当前 production 的公共 Edge contract 是 **epoch 3 / 19 actions**，workstation Runtime Execution Contract 仍是 **epoch 2 / 18 tools**。具体 canonical hash 属于构建/发布事实，不应该作为一篇长期教程里的常量；实际激活以当前运行配置和冻结 contract 定义为准。

## 为什么 contract epoch 和 Runtime A/B 必须分开

A/B 适用于：

> **同一公开契约下，更换实现。**

例如：

- 修一个 fs bug；
- 改 snapshot fallback；
- 提升 exec 稳定性；
- 修 OAuth relay 内部实现但 public tool surface 不变。

如果 tools/list 本身发生不兼容变化，就是 contract migration：

```text
runtime implementation upgrade
    ≠
public MCP contract migration
```

后者会影响 ChatGPT tool snapshot、Edge identity、Link contract 以及新会话验收，需要单独的 epoch 迁移流程。

因此 `herdr-self-update` 不应该被用来偷偷跨 contract epoch。

## 激活发生时会怎样

可以把 active pointer 理解成一个本机路由开关：

```text
切换前
new request → A

activate B

切换后
new request → B
existing request on A → A 上完成/drain
```

理想情况下：

- persistent WSS 不断；
- Edge URL 不变；
- OAuth issuer 不变；
- ChatGPT Connector 不变；
- 已经投递到旧 generation 的请求不会被重复投递到新 generation。

这也是为什么 activation 和 drain 要有明确语义。

## Rollback

如果 B 激活后暴露问题，不需要重新部署公网入口。

```bash
bin/herdr-runtime-generation status
bin/herdr-runtime-generation rollback
```

Rollback 让新请求回到 previous / last-good generation。

回滚前同样要观察真实状态：如果某个 mutation 已经在 B 上发生，回滚 runtime 不会自动回滚 Git、文件、远端服务或 Agent 行为。

Runtime rollback 是**执行环境回滚**，不是业务副作用回滚。

## 本机控制状态

Generation manager 使用本机 control/status state 保存：

- generation specs；
- desired active；
- revision；
- observed active；
- previous / last-good；
- activation observations。

这些文件属于 workstation 控制状态，不是仓库源码，也不应该保存 bearer secret。

Candidate endpoint 限制在 loopback，是为了保持“generation 是本机 runtime”的安全边界，不把 A/B 变成任意远端转发器。

## Heartbeat 如何让 Edge 知道版本变化

workstation link 在 heartbeat 中报告当前 active runtime identity，例如 generation / version 等运行信息。

因此 Edge 可以观察：

```text
workstation online
active generation changed
runtime version changed
```

不需要因为一次本机 A/B 切换重建 WSS。

公网状态可能在下一次 heartbeat 才收敛，所以刚 activate 后短时间内看到旧 version 并不等于切换失败；以本机 generation status + 后续 heartbeat 为准。

## 推荐升级流程

```text
Inspect current state
  ↓
Build candidate
  ↓
Start candidate on second loopback endpoint
  ↓
Register generation
  ↓
Validate health + tools contract
  ↓
Activate
  ↓
Observe real tool calls / heartbeat
  ↓
Keep old generation during observation
  ↓
Remove old generation only after confidence
```

如果任一步骤失败：

- candidate 尚未 activate：修 candidate，不影响 production；
- candidate 已 activate：先判断是否 rollback；
- mutation delivery 不确定：先重新观察，不重复执行发布动作。

## `herdr-self-update` 适合做什么

`bin/herdr-self-update` 是受监督升级入口，它复用 generation 机制，而不是原地覆盖当前 active runtime。

适合：

- 同 contract epoch 的代码更新；
- 可以构建 candidate、跑 gate、切换和回滚的升级。

不适合：

- public tools contract 变化；
- Edge/OAuth identity 迁移；
- Custom Domain/DNS cutover；
- Herdr daemon 本体的任意跨边界升级。

这些是不同 release plane，应单独操作。

## 与 Edge 发布的关系

系统有两个主要 release plane：

```text
Public Edge plane
Worker / Durable Object / OAuth / public MCP relay

Local runtime plane
herdr-link / active runtime generation
```

它们应该尽量独立发布。

例如本机 `herdr_fs_*` bugfix 通常无需改 Worker；OAuth relay bugfix 通常也无需切本机 runtime generation。

分离 release plane 可以减少一次发布的爆炸半径。

## 安全规则

- candidate 必须使用 loopback endpoint；
- 当前 contract epoch 内，contract 不匹配的 candidate 不激活；
- 已投递请求不因切代而重复执行；
- 旧 generation drain 前不强杀；
- credential 不写进 generation control/status；
- 回滚 runtime 前后都重新观察 Git / Agent / 服务真实状态；
- contract migration、Edge deployment、Domain/DNS mutation 不混成一次普通 self-update。

## 验收标准

一次成功 A/B 升级至少证明：

- candidate 独立健康；
- 当前 public contract gate 通过；
- active generation 已改变；
- Edge heartbeat 收敛到新 runtime identity；
- 新请求进入新 generation；
- Connector/OAuth URL 没有变化；
- 至少一次真实 MCP tool call 通过；
- rollback target 仍然存在于观察窗口内。

这时才算“本机 runtime 升级完成”，而不是仅仅“新进程启动成功”。

相关内容：

- [Cloudflare Edge 部署](cloudflare-edge-deployment.md)
- [CLI 参考](cli-reference.md)
- [故障排查](troubleshooting.md)
- [架构](architecture.md)
