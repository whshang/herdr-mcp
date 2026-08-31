# 设计思路：把最强的脑子和最真实的开发现场接起来

> **职责：** [总览](overview.md) 说明 herdr-mcp 是什么，[架构](architecture.md) 说明系统怎样工作；本章只回答**为什么选择这些设计原则**。

很多 AI 编程工具从“再做一个 Coding Agent”开始。herdr-mcp 选择了另一条路：**网页模型已经很会思考，本地开发环境已经很会执行，真正缺少的是一条可靠、可观察、能长期工作的连接。**

这里不重复安装步骤、组件拓扑或竞品清单，而是给出后续设计决策应该遵循的原则。

## Web ChatGPT 和 Codex CLI，各自缺什么

本地 Coding Agent 的优势很明显：就在仓库旁边，可以读文件、跑测试、改代码。它的工作环境天然连续。

Web ChatGPT 也有很强的优势：模型选择、推理能力、长上下文、Project 中积累的资料，以及随时从手机、平板或另一台电脑继续对话。问题在于，它隔着浏览器看不到你的开发机。

herdr-mcp 做的事情可以概括成：

```text
Web ChatGPT
+ 本机文件 / Git / Shell
+ Herdr 持久工作区
+ 本地 Agent 调度
+ 浏览器连续工作
≈ 一个跨设备、可远程观察的 Coding Agent 工作台
```

这里没有试图复制 Codex CLI 的 UI，也没有再实现一套 Agent runtime。重点是让 Web AI 获得它真正缺少的“手和眼睛”。

## 原则一：planner 留在网页

高层目标、任务拆分、取舍和验收由当前网页模型负责。

这样做有一个很实际的好处：你和模型看到的是同一条决策链。为什么改这个文件、为什么把任务交给另一个 Agent、测试为什么还不能算通过，都留在当前对话里。

本地 Agent 更适合成为独立执行者：

- 并行调查一个子问题；
- 实现边界明确的一块功能；
- 做第二视角代码审查；
- 运行需要较长自主推理的任务。

Web planner 不需要为了执行一条 `git status` 再找一个 Agent，也不需要让一个本地“大模型指挥官”转派给另一个 Agent。少一层转述，就少一层延迟、费用和语义损失。

## 原则二：确定性操作直接做

判断一个动作是否需要 Agent，可以问一句：**这里真的需要另一份推理吗？**

| 工作 | 默认方式 |
|---|---|
| 读文件、搜索代码 | `herdr_fs_*` |
| 看 Git 状态/diff/log | `herdr_git` |
| 精确修改 | `herdr_fs_patch/edit/write` |
| 跑 lint/test/build | `herdr_exec*` |
| 看已有工作现场 | `herdr_inspect` / `herdr_since` |
| 独立调查/并行实现/审查 | `herdr_prompt` |
| Herdr 高级原生动作 | `herdr_methods` + `herdr_call` |

这让系统更像一个工程师在用工具，而不是“每碰一下键盘都要召唤一个 Agent”。

## 原则三：运行状态属于 Herdr，不属于聊天记录

聊天记录很重要，但它只是认知上下文。真实运行状态必须从机器重新确认。

例如上一轮对话里写着“测试正在运行”，十分钟后你再回来时，正确动作是查看 session 或 Git/test 结果，而不是继续相信那句话。

Herdr 的 workspace、pane、cwd、Agent status 和事件流因此成为运行事实。对话接力摘要只负责告诉下一段对话“我们之前在做什么”，不能证明机器现在仍保持同一状态。

这也是 herdr-mcp 每次新工作阶段倾向先 `herdr_inspect` 的原因。

## 原则四：上下文要花在问题上，不要花在工具说明上

Herdr 有大量原生 Socket API。如果每个方法都注册成 MCP tool，模型每轮都要携带一大堆 schema。

herdr-mcp 把常用远程工作压缩为 18 个公共工具；低频 Herdr 原生能力通过动态 schema 发现。

可以把它理解成工具箱：常用螺丝刀放桌面上，整面工具墙仍然在隔壁，需要时再去拿。桌面保持清爽，能力没有消失。

## 原则五：长任务要允许人离开屏幕

“站起来蹬”的重点不是让系统永远疯狂自动点击，而是你不需要因为任务还没结束一直守在电脑前。

Herdr 保持本地工作现场；Edge 保持公网入口；浏览器扩展负责网页侧恢复和接力。于是一次任务可以经历：

```text
电脑前提出目标
  ↓
ChatGPT 修改代码
  ↓
派 Agent 跑长任务
  ↓
你离开电脑
  ↓
Agent 完成 / 状态变化
  ↓
浏览器扩展推动网页继续
  ↓
对话过长时生成 handoff 并接到新对话
  ↓
新对话重新检查 live state 后继续
```

自动化的价值来自连续性。任何高风险 mutation 仍然需要清晰的权限和状态判断。

## 原则六：失败时先问“动作发生了吗”

远程系统最危险的错误处理是：看到 timeout 就再执行一次。

读操作通常可以安全重试。写操作、prompt、部署、提交等 mutation 可能已经在远端发生，只是成功响应没有回来。

所以 herdr-mcp 很重视 delivery phase、idempotency 和重新观察。一个好的 planner 在错误后会检查 Git、Agent 状态、session 输出或远端资源，再决定下一步。

## 原则七：安全来自清晰边界，不来自“看起来安全”

herdr-mcp 没有把 shell 宣称成 sandbox。允许 Web AI 调 shell，就是允许它以当前工作站用户权限执行命令。

因此系统把可限制的部分明确限制：managed Git roots、readonly、write roots、dirty/busy gate、OAuth、workstation identity、本机 Unix socket；同时把 shell 的真实权限写清楚。

这种设计比一个名字叫 `safe=true`、实际仍能绕出去的开关更容易审计。

## 一个推荐的完整工作循环

```text
Observe → Understand → Act → Verify → Delegate when useful → Re-observe
```

### Observe

看 workspace、Git、相关文件和已有任务。确认 live state。

### Understand

用网页模型完成高层分析。信息不足就继续读取，不根据旧摘要猜运行事实。

### Act

确定性修改直接 patch；短命令直接 exec。

### Verify

运行最贴近变更的测试，再逐步扩大验证范围。检查 diff，确认没有顺手改坏无关内容。

### Delegate when useful

独立任务再交给 Agent。明确目标、范围、验收条件和工作目录，让它能自主完成一段有意义的工作。

### Re-observe

Agent 完成、连接恢复、对话接力或长时间等待后，重新看 live state。新的事实覆盖旧的文字描述。

## 什么时候这套方式特别有价值

- 你希望使用 Web ChatGPT 的强模型，同时直接操作自己的仓库；
- 开发任务持续数小时，包含多个 Agent 和终端；
- 你经常在电脑、手机和平板之间切换；
- 你希望看到 Agent 到底在干什么，并能随时改变方向；
- 你不希望为了远程开发把整个工作站 SSH/桌面环境直接暴露出去；
- 你希望公网 Connector 稳定，而本地 runtime 可以独立升级和回滚。

## 什么时候直接用本地 Coding Agent 更省事

一个十分钟能结束的小改动、你本人就在终端前、也不需要跨设备或多 Agent 编排时，本地 Codex/Claude Code/Pi 已经很好用。herdr-mcp 的价值随着任务持续时间、并行度、远程需求和网页模型优势一起增加。

这也是整个项目最重要的克制：**能由现有 Herdr 或现有工具解决的问题，就复用；只有 Web AI 真正缺少的能力，才进入 herdr-mcp。**

接下来可以阅读 [最佳实践](best-practices.md) 看具体工作流程，或者阅读 [能力基准与取舍](capability-benchmark.md) 了解为什么某些看起来很诱人的功能没有加入。
