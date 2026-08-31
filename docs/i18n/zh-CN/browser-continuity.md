# 浏览器连续工作：为什么 MCP 之后还需要一条回来的路

MCP 解决的是：

> Web AI 如何访问本地开发环境？

浏览器连续工作解决的是：

> 本地开发环境发生变化后，如何让网页会话知道？

这两个方向缺一不可。

## 单向 MCP 的限制

典型 MCP 流程：

```text
ChatGPT
   │
   │ tool call
   ▼
herdr-mcp
   │
   ▼
Herdr Agent
```

ChatGPT 发起请求，服务器返回结果。

但软件开发任务经常是异步的：

```text
10:00  ChatGPT 提交测试任务
10:01  Agent 开始运行
10:20  测试完成
```

10:20 时，原来的网页回合已经结束。MCP 本身不会主动打开一个新的 ChatGPT 回合。

## 两条连接组成完整系统

herdr-mcp 有两条方向相反的通道：

### 下行：能力通道

```text
ChatGPT
  ↓
MCP + OAuth
  ↓
Edge
  ↓
herdr-mcp
  ↓
本地工作站
```

负责：

- 文件读取；
- Patch；
- Git；
- Shell；
- Agent 调度。

### 上行：连续性通道

```text
Herdr
  ↓ events
runtime
  ↓ local IPC
Browser extension
  ↓
网页会话
```

负责：

- Agent working 进度；
- settled 收工通知；
- 页面恢复；
- 长对话接力；
- 自动继续。

## 扩展不是第二个 Agent

浏览器扩展不负责思考，不替代 ChatGPT，也不运行 coding agent。

它更像一个连接器：

- 知道当前哪个网页会话对应哪个 workspace；
- 接收本机状态变化；
- 在安全条件满足时推动网页继续。

真正的推理仍然由 Web AI 或本地 Agent 完成。

ChatGPT Project 里，连续性的 binding 不再依赖某一个 conversation。workspace 直接绑定稳定 `project_id`，所以可以在 Project 首页先绑定；具体 `/c/<id>` 只作为当前 `active_conv_key`，决定 progress/continue 应投递到哪里。接力时 Project binding 与 continuity id 都不搬家，只在新 seed 确认后切换 active target。

从 0.4.2 开始，已绑定会话的 finalized user / assistant turn 会通过 Native Messaging 增量写入本机 Rust `state.db` 的 Continuity Journal。浏览器准备接力时会实时向 Rust 确认当前 `continuity_id` 仍存在；确认成功后，新会话只需要携带这个 ID，并通过现有 `herdr_call(method="continuity.resume", ...)` 恢复有界的最近工作上下文，再重新检查实时 Herdr、runtime 与 Git 状态。

用户**手动**在同一个已绑定 ChatGPT Project 里新开会话时，不需要记住或输入 `continuity_id`。新会话第一条已确认发送的用户消息（例如“继续”）会沿用 Project binding 写入同一条 continuity chain；Web planner 看到“继续 / 接着 / 恢复上次”等意图后，应先通过 `continuity.resolve` / `continuity.search` 搜索，而不是先要求用户提供内部 ID。只有 `conversation_id`、`project_id`、`workspace_id` 这类稳定身份把候选收敛为唯一链时才允许自动 `continuity.resume`。单纯文本匹配即使只剩一个候选也仍需要用户确认；“继续”本身只是触发搜索，不是选择证据。多个候选会返回有界的标题、workspace、更新时间和最近对话摘要供确认，系统禁止用“最近一次”或“最像”直接猜。

Rust journal 不可用或实时确认失败时，扩展继续使用既有 `HERDR_HANDOFF_V1` 与 bounded transcript 路径。

### 未来目标：Continuity 2.0

`v0.4.2` 解决的是“平时持续记录，窗口或会话失效后仍能恢复”。后续正式路线中的 Continuity 2.0 继续解决“同一个工作链持续几天、几百轮甚至更久以后，恢复上下文如何仍然保持很小”。它当前不属于 `v0.4.2`，也尚未绑定具体版本号；版本归属继续作为独立的 post-`v0.4.2` release planning 决策。

Continuity 2.0 会持续把较老 raw turns 压成 rolling semantic checkpoint，保留目标、已完成事项、关键决定、约束、活跃文件/分支/commit、待办、下一步和 literal anchors。恢复时优先使用“最新 checkpoint + 最近 raw tail”，而不是重新发送完整长会话。只有新 checkpoint 已验证可恢复后，系统才能回收更早的 raw body；页面内存、长 DOM 和主线程/render 压力也会与模型 context pressure 一起参与 rollover 决策。

实施顺序保持 `Reliability Kernel → Continuity 2.0`，详细技术设计见 [Rust Native Rearchitecture](../../history/architecture/rust-native-rearchitecture.md#phase-8continuity-20) 的 Phase 8 章节。

## 人工与自动控制

自动化按作用域管理。全局允许 ChatGPT Project 共享 Auto 后，每个 Project 仍从自己的 HUD 显式开启 / 关闭；普通 ChatGPT conversation、z.ai、DeepSeek 在支持时使用 conversation 级 Auto。所有新作用域默认都是 `自动 关`。

HUD 的三个预置推进动作是手动继续、提取 Herdr 状态、LLM 判断；开启 Auto 后它们会锁定，避免两个路径同时推进同一会话。**手动接力是例外，它只有一个 UI 入口：HUD**。支持的会话可以在 Auto 开 / 关时启动接力；transfer 期间源会话的自动 wake 暂停，目标继承源会话 Auto 状态。

需要人工接管时，可以先关闭 Auto，再从 HUD 手动继续 / 提取 Herdr 状态 / 运行轻量 LLM 判断；需要主动切换会话时，从 Control Center 的“当前页面”启动接力。

## 为什么不用公网回推

扩展和本机 runtime 在同一台机器上。

当前设计：

```text
Chrome
  ↓ Native Messaging
native host
  ↓ Unix socket 0600
herdr-mcp runtime
```

浏览器不保存 Herdr bearer，也不需要把扩展连接暴露到公网 Worker。

公网 Edge 服务的是 ChatGPT → workstation；本机 IPC 服务的是 browser → local runtime。

## 长任务工作流

推荐模式：

```text
提出目标
 ↓
ChatGPT 分析
 ↓
修改代码 / 派 Agent
 ↓
离开电脑
 ↓
Agent 完成
 ↓
浏览器收到状态
 ↓
继续当前工作
```

这让几个小时的软件任务可以保持连续，而不是要求人一直盯着终端。

## 自动化边界

自动继续不是无限循环点击。

系统会检查：

- workspace 是否绑定；
- Agent 是否真的发生状态变化；
- 是否存在未确认 mutation；
- 页面是否仍在生成；
- 是否满足 Project / conversation Auto 设置。

高风险动作保持显式边界。

## Continuity 只是扩展的一个产品面

本页只解释 **Web continuity**：workspace binding、progress / settled 回推、stale response recovery 和 handoff / rollover。

扩展现在还有两个职责不同的产品面：

- [浏览器控制中心](browser-control-center.md)：Chrome Side Panel 里的实时 workspace / pane / Agent 观察、Pinned Target 和只读操作；
- [JSON → MCP bridge](extension-bridge.md)：为没有原生 MCP Connector 的 z.ai / DeepSeek 提供本机工具兼容路径。

它们共享 Native Messaging 和本机 IPC，但不能混成一个状态机：Continuity 决定网页会话如何持续；Control Center 展示本机事实并固定人工目标；JSON → MCP 负责协议适配。

## 设计原则

浏览器连续工作关注的是“保持人在回路中的连续性”。

它让 AI 可以长时间工作，也让人随时能看到状态、调整方向和接管，而不是把电脑变成一个无法观察的自动机器人。
