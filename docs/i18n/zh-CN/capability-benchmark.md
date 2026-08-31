# 能力基准与设计取舍：什么该吸收，什么不该复制

这篇面向 Maintainer / Contributor。它不是“谁功能更多”的产品对比，而是一份长期 ADR：当我们看到 Herdr、coding-tools-mcp、其它 MCP bridge 或新的 Coding Agent 能力时，判断它应该进入 herdr-mcp、复用原生能力，还是明确不做。

核心问题只有一个：

> 这个能力是否让 **Web AI 更可靠地控制本机开发现场**，同时不复制 Herdr、Agent runtime 或已有系统已经做好的事情？

## 先定义 herdr-mcp 自己的边界

herdr-mcp 不是：

- 第二个 Herdr；
- 第二个 Coding Agent；
- 通用远程 shell 产品；
- workflow / recipe DSL；
- 浏览器自动化框架。

它是 Web AI 与本机 Herdr/workstation 之间的控制面，所以最值得一等支持的是：

1. Web 模型原本拿不到的本机能力；
2. 跨长任务、跨会话仍然可靠的运行状态；
3. 公网到私有工作站之间稳定、安全的传输；
4. mutation 的可观察性、幂等和失败语义。

## 一条判断公式

看到一个新功能时，可以按四问过滤：

```text
Web AI 缺吗？
  ↓ yes
Herdr 已原生提供吗？
  ↓ yes → 透传/发现，不复制
  ↓ no
现有 fs/git/exec 原语能表达吗？
  ↓ yes → 复用已有工具
  ↓ no
新增专用能力是否显著提升可靠性？
  ↓ yes → 考虑进入 public surface
```

这套过滤器比“某个上游项目有，所以我们也要有”更重要。

## 取舍一：固定 MCP 工具面，而不是映射整个 Herdr API

Herdr 原生 Socket API 很丰富，而且会持续演进。

如果把每个 `workspace.*`、`pane.*`、`agent.*` 方法都注册成 MCP tool，会产生两个问题：

- 每轮会话携带大量 schema；
- Herdr 一升级，public MCP ABI 就跟着被动变化。

因此 herdr-mcp 采用两层模型：

```text
高频远程工作
  → 专用 MCP tools

低频 Herdr 原生能力
  → herdr_methods + herdr_call
```

当前 production contract 是 **epoch 2 / 18 tools**。以后 catalog 变化也必须显式进入新的 contract epoch，不能由一次 runtime 重构顺手改变。

## 取舍二：文件 / Git / Shell 必须是一等能力

这些不是 Herdr 的职责，但恰恰是 Web AI 最缺的东西。

因此 herdr-mcp 提供：

- `herdr_fs_read/list/grep/image`；
- `herdr_fs_edit/write/patch`；
- `herdr_git`；
- `herdr_exec`；
- `herdr_exec_start/read/kill`。

这类操作通常是确定性的，不需要额外启动本地 Agent。

设计目标不是让 Agent “代替终端”，而是让 Web planner 像工程师一样直接使用终端和仓库事实。

## 取舍三：长命令拥有自己的生命周期

HTTP/MCP request 和实际命令生命周期不是一回事。

一个 build、测试或本地服务可能运行数分钟甚至数小时。把它绑死在一次同步 tool call 上会产生 timeout、重复执行和无法取消的问题。

因此：

```text
短命令
  → herdr_exec

长命令
  → herdr_exec_start
        ↓
     read / kill
```

这种 handle 模型值得吸收，因为它解决的是 Web AI 的真实远程执行问题，而不是复制一个 Agent API。

## 取舍四：Git 事实保持确定性

`git status`、`git diff`、`git log` 不需要再让一个模型解释后执行。

`herdr_git` 的价值是：

- 直接；
- 可验证；
- 不依赖 Agent；
- mutation 后能作为完成证据。

暂时没有为每个 Git 子命令新增专用 tool。低频 `show`、`blame` 等可以在安全边界内通过 exec 完成。只有当某个操作频繁到值得稳定 schema 时才考虑升格。

## 取舍五：Mutation 的失败语义比“自动重试”更重要

远程开发最危险的不是一次失败，而是**动作已经发生、客户端却以为没发生**。

因此 herdr-mcp 倾向吸收：

- delivery state；
- idempotency key；
- post-submit wait 与 transport failure 分离；
- uncertain delivery 后先重新观察。

而不是吸收“失败就自动 retry”的简单策略。

这个原则同时作用于：

- Agent prompt；
- shell command；
- Runtime activation；
- Cloudflare/DNS mutation；
- browser handoff。

## 取舍六：Shell 不伪装成 sandbox

有些工具会把命令分为 safe / dangerous，再让系统看起来像一个强隔离环境。

herdr-mcp 不宣称 shell 已被 sandbox。

真实边界是：

- `herdr_fs_*` 受 managed root / secret-path / write gate 限制；
- `herdr_exec` 则拥有当前工作站用户可以执行的 shell 权限。

如果未来需要容器级隔离，应作为独立安全架构设计，而不是在一个参数里假装已经解决。

## 取舍七：项目指令不自动重复扫描

本地 Coding Agent 通常已经有自己的 `AGENTS.md`、项目规则、skill 或 runtime instruction 机制。

herdr-mcp 不再额外扫描并注入一套“项目指令汇总”，避免：

- 重复上下文；
- 指令优先级冲突；
- Web planner 和本地 Agent 看到不同规则。

远程 planner 自身的操作策略由 `herdr_skill` 提供；具体项目规则由实际项目和 Agent runtime 管理。

## 取舍八：浏览器扩展只补 MCP 缺失的方向

标准 MCP 是请求驱动：

```text
Web AI → workstation
```

它无法让一个已结束的浏览器 turn 因为本地 Agent 后来完成而自己重新开始。

所以 extension 增加：

```text
workstation → browser conversation
```

这里值得吸收的是：

- workspace binding；
- progress / settled；
- evidence-first recovery；
- fail-closed handoff；
- 没有原生 MCP 的网页站点 JSON→MCP bridge。

不值得扩张成通用网页自动化平台。

## 取舍九：公网入口与本机 Runtime 解耦

公网 Connector URL 应稳定，本机 runtime 应可升级。

因此：

- Cloudflare Edge 管 OAuth / public MCP / workstation routing；
- `herdr-link` 维持出站 WSS；
- Runtime A/B 管本机 generation。

这比“Tunnel 直接打到某一个 Node 进程”复杂一点，但换来了：

- Runtime 升级不改 Connector；
- 回滚不改公网 URL；
- workstation 生命周期与 OAuth identity 分离。

## 取舍十：Local Agent 是 worker，不是第二个 planner

任何本地 Agent 都可能成为有用 worker：Pi、Cline、OpenCode、DSH 或未来的新工具。

选择标准不是品牌，而是：

- 能否无头/自动化运行；
- 是否有稳定的状态或输出边界；
- 是否能限制工作范围；
- 是否容易验证结果；
- 超时后能否判断 mutation 是否已经发生。

herdr-mcp 不再为每种 Agent 建一个专门 MCP tool。Herdr-native Agent 优先走 `herdr_prompt`；非 Herdr CLI 在必要时通过长 exec session 使用。

详见 [Worker 备选](worker-fallbacks.md)。

## 当前能力矩阵

| 能力 | 决策 | 原因 |
|---|---|---|
| Herdr workspace/pane/agent 原生 API | 动态透传 | 避免复制 90+ API |
| 文件 read/search/patch | 一等 MCP | Web AI 原本不可达 |
| Git status/diff/log | 一等 MCP | 高频确定性事实 |
| 长命令 session | 一等 MCP | 生命周期跨 tool call |
| 图片读取 | 一等 MCP | Web AI 需要真实像素上下文 |
| Agent prompt | 薄封装 | 需要 delivery/idempotency 语义 |
| recipe/workflow DSL | 不做 | Web AI 已是 planner |
| 第二套 Agent registry | 不做 | Herdr 已负责 |
| 自动扫描项目 instructions | 不做 | 避免和 Agent runtime 重复 |
| shell “安全等级”伪 sandbox | 不做 | 安全边界必须真实 |
| Browser progress / recovery / handoff | 做 | 补足请求式 MCP 的时间连续性 |
| JSON→MCP bridge | 有限做 | 只为无原生 MCP Connector 的站点兼容 |
| Runtime A/B | 做 | 稳定 Connector 与本机升级解耦 |
| Custom Domain | 可选 | 稳定命名，不是核心能力前置 |

## 新增能力的准入规则

准备新增一个 MCP tool 或一个自动化模块前，至少回答：

1. 现有 `herdr_call` 能不能表达？
2. 现有 fs/git/exec 能不能表达？
3. 为什么需要稳定 public schema？
4. 会不会增加每轮上下文负担？
5. mutation 怎么判断“是否已经发生”？
6. 失败怎么恢复？
7. 能不能真实测试，而不是只写一个 wrapper？
8. 是否正在复制 Herdr / Agent / Cloudflare 已有能力？

没有清楚答案，就先不扩张。

## 维护这篇文档的方法

这不是历史实验日志。

当上游项目或 Herdr 出现新能力时，更新的是**取舍结论**；具体版本、冒烟日期、某次 UAT 和一次性 bug 证据应进入 CHANGELOG、issue 或实验记录。

这样 capability benchmark 才能长期回答一个有价值的问题：

> herdr-mcp 为什么长成现在这样，以及下一项能力是否真的值得进入它的边界。
