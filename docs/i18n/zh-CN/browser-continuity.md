# 浏览器连续工作

*为什么 MCP 之后还需要一条回到 Web 对话的路。*

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

这也是浏览器扩展最核心的存在理由。ChatGPT 在一个工具回合里可以把任务派给 Agent，但助手回复发出后，Web Planner 不能继续在后台轮询；Herdr 也不能直接反向启动新的模型推理回合。因此扩展把本机状态变化当作“继续推理”的触发信号：有真实新输出时可以发送有界 progress；`working → idle/done/blocked` 只唤醒一次；浏览器或 runtime 暂时断开时，重连还能补发离线期间发生的 settle。收到 wake 后，Web Planner 必须重新读取实时 Herdr/Git 状态，审查 Agent 产出和 diff；独立 branch/worktree 的完成结果在确认后再 cherry-pick/合并，并运行当前任务需要的验收。阻塞、失败、超时类结果继续诊断；wake 本身不能作为“任务已完成”的证据。

这套行为也是 Rust/runtime 迭代必须保持的兼容契约。只要修改本地 runtime、Native Messaging 或事件传输，就要继续保证 browser binding、settle 单次唤醒、progress 去重、旧 source 拒绝、Native Messaging 信任边界、handoff/queue 迁移和扩展版本一致性这些回归项。

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
- [JSON → MCP bridge](extension.md)：为没有原生 MCP Connector 的 z.ai / DeepSeek 提供本机工具兼容路径。

它们共享 Native Messaging 和本机 IPC，但不能混成一个状态机：Continuity 决定网页会话如何持续；Control Center 展示本机事实并固定人工目标；JSON → MCP 负责协议适配。

## 设计原则

浏览器连续工作关注的是“保持人在回路中的连续性”。

它让 AI 可以长时间工作，也让人随时能看到状态、调整方向和接管，而不是把电脑变成一个无法观察的自动机器人。

## 连续工作实现与恢复细节

> **职责：** 浏览器连续工作状态机的高级参考。大多数用户只需要 [浏览器扩展](extension.md) 与 [浏览器控制中心](browser-control-center.md)。

这篇文档解释浏览器连续工作的核心状态机：当 Herdr 里的工作继续发生，而网页会话已经停下来时，扩展如何把正确的会话重新唤醒；当 ChatGPT 页面卡住或上下文过长时，又如何恢复或接力，同时避免重复执行 mutation。

先读 [浏览器连续工作](browser-continuity.md) 理解为什么需要反向通道；安装和 HUD 使用见 [浏览器扩展](extension.md)。

## 基本模型：绑定 Project / 会话到一个 workspace

扩展把网页作用域绑定到 Herdr **workspace**，不是某一个 Agent。普通站点以具体 conversation 为作用域；ChatGPT Project 则以稳定 `project_id` 为作用域。

```text
ChatGPT Project conversation
            │
            │ binding
            ▼
Herdr workspace
  ├─ pane: Pi implementation
  ├─ pane: tests
  ├─ pane: server
  └─ pane: Grok review
```

这样，网页端关注的是完整任务现场，而不是某个单独进程。`workspace_id` 是稳定身份；label 只用于显示，会从实时 catalog 修正。

0.1.59 起，ChatGPT 可以直接在 `https://chatgpt.com/g/<project>/project` 上建立 Project binding，不需要先有 conversation；具体激活的 `/c/<id>` 只记录为 `active_conv_key` 投递目标。`https://chatgpt.com/` 根首页还可以保存当前 tab 的 pending binding，等这个 tab 首次进入具体 Project/conversation 后再一次性迁移。根首页与 Project 首页只提供绑定/Auto 等作用域控制，不运行需要 conversation composer 的手动继续、LLM、恢复或 rollover。

## 状态回推：working、progress、settled

扩展持续观察本机 `/push/events`。同一个 workspace 的多个 pane 会合并成一个任务范围。

### working

一旦范围内出现 working Agent，扩展进入“任务仍在进行”状态，并开始按配置检查是否有值得回推的新摘要。

### progress

`progressTickSec` 决定**多久检查一次**，不是多久一定发送一次。

发送规则强调低噪音：

- 有新的非空摘要时才优先发送；
- 没有新内容时，不因为计时器到了就刷屏；
- 超过 `progressFallbackSec` 可以发送一次兜底状态；
- 上一次已发送摘要和时间会持久保存，Service Worker 重启后也能去重。

### settled

某个 pane settled，但同 workspace 还有其他 Agent working，属于**局部进展**；整个 workspace 不再有 working Agent 时，才是范围收工。

收工事件会停止 progress timer，并唤醒网页 planner 重新检查 Git、测试和 Agent 输出。

## 为什么回推不是“直接告诉模型已经成功”

Herdr 事件只证明本地状态变化，不证明任务满足验收标准。

扩展注入的继续消息应该促使 Web planner：

1. 重新读取 live state；
2. 检查具体输出 / Git / tests；
3. 决定继续实现、返工、提交还是收尾。

所以 continuity 是调度信号，不是业务结论。

## HUD 的人工控制

HUD 提供几类显式操作：

- **手动继续**：直接让当前网页会话继续下一轮；
- **herdr监控**：先读取绑定 workspace 的实时状态，再把结果带回网页；
- **LLM 分析**：用 Options 中配置的小模型判断当前回复是否仍有明显未完成工作；

HUD 提供 `继续 / 查 Herdr / LLM 判断` 三个页面级推进动作和“手动接力”。前三个动作在当前作用域 `自动 开` 时会锁定，避免和自动状态机同时推进同一会话；手动接力在安全门通过时仍可使用。workspace 绑定和本地 Herdr 控制留在 Side Panel。**手动接力只有一个 UI 入口：HUD**；transfer 期间源会话自动 wake 暂停，target 继承 source 的 Auto 状态。

## 排队：用户下一轮意图优先于自动继续

ChatGPT composer 旁的 **排队** 和 HUD 的“手动继续”不是同一种动作。

排队用于：当前 assistant 仍在回复，但用户已经知道下一轮想补充什么。点击后不会打断 live turn，而是把当前 composer 内容持久保存到这个 conversation 的队列。

当 turn settled 时，顺序是：

```text
当前 assistant turn 结束
       ↓
有排队内容？ ── yes ──► 合并并发送下一条用户消息
       │ no
       ▼
再考虑通用 LLM auto-continue / idle nudge
```

这条优先级很重要：**明确的用户下一轮意图优先于模型自己判断“要不要继续”。**

队列还遵守以下约束：

- 多条内容按加入顺序保留，并用空行合并；
- `turn-in-progress` 等阻塞不会 ACK 或丢弃内容；
- 只有确认成功发送的 batch 才删除；
- 队列数量、单条长度和总合并长度都有上限；
- 右键排队按钮清空当前 conversation 队列；
- composer 为空时点击可尝试重发尚未送达的 batch；
- handoff 确认 cutover 后，未发送队列迁移到目标 conversation，并保持顺序。

排队不直接执行 Herdr tool，也不改变 workspace binding；它只负责保存并交付**下一条用户消息**。

## 自动化作用域

### ChatGPT Project

Project 自动化需要两层许可：

1. Options 允许 ChatGPT Project 自动化；
2. 当前 Project HUD 为 `自动 开`。

Project 开关按稳定 `project_id` 共享，因此同一 Project 里的接力后会话可以继承自动化偏好。

### 普通 ChatGPT / z.ai / DeepSeek

支持的站点使用会话级 Auto，按 conversation identity 保存。

z.ai / DeepSeek 的 Auto 只负责 Herdr progress / settled 回推；ChatGPT 专属的 stale-view 恢复、权限卡处理、LLM 回合判断和自动 rollover 不会移植过去假装通用。

## ChatGPT 回复结束后的 LLM 判断

有些回复从页面上看已经“结束”，但语义上其实停在半途，比如：

- “接下来我会检查测试……”
- “还需要验证生产环境……”
- “下一步是查看 Git 状态……”

扩展可以使用一个单独配置的小模型，对最近用户/助手正文做轻量判断。

小模型只负责回答一个问题：**这轮是不是明显还需要继续？**

它不是第二 planner，也不决定代码修改方案。判断需要继续时，扩展将受控继续消息提交给当前 ChatGPT 会话。

未配置小模型时，这个自动判断不会偷偷降级为脆弱的关键词猜测；用户仍可手动继续或使用 herdr监控。

## 页面卡住：先判断发生了什么

浏览器恢复最重要的原则是：**不把“页面没动”自动解释成“服务器没有执行”。**

工具调用、用户消息或 assistant 回复可能已经在 ChatGPT 服务端推进，只是当前 DOM 没刷新。

因此恢复顺序是证据优先。

```text
页面长时间无进展
        │
        ▼
读取同源 conversation snapshot（best effort）
        │
        ├─ server ahead ───────► 安全 reload 同步视图
        │
        ├─ request not accepted ► bounded retry
        │
        ├─ server stalled ─────► 等待后安全 reload
        │
        └─ unknown ────────────► fail closed
```

拿不到可靠证据时，不因为超时本身就重发原任务。

## stale-view：服务端已经有更多内容，页面只显示半截

扩展会比较：

- 当前页面最后一条 assistant message；
- 同源 conversation snapshot 的 `current_node` / assistant message；
- message id、文本长度、状态和更新时间。

### server ahead

服务端 message 更晚，或者同一 message 的服务端文本明显更长，说明浏览器视图陈旧。

安全条件满足时只 reload 一次，目的是同步已经存在的服务端结果，不是重新提交任务。

### server stalled

服务端自己也显示 assistant 尚未完成，而且一段时间没有更新。扩展会更保守地等待，再允许一次 reload。

### synced

服务端和 DOM 一致且回合已结束，不执行恢复，交给正常 LLM 判断 / settled 流程。

### unknown

snapshot 接口失败、超时或结构不确定时 fail closed，不凭时间差猜测。

## ChatGPT 显式“消息发送超时”

如果页面明确出现 thread error / “消息发送超时，请重试”，扩展仍然先检查服务端 conversation state。

- `current_node` 已进入 assistant：说明用户请求实际上已经提交，点击“重试”可能重复工具工作；优先 reload 同步。
- `current_node` 仍停在 user：才允许点击 ChatGPT 自带 Retry 一次。
- 无法确认 delivery：宁可做一次安全视图同步，也不盲目创建第二个用户回合。

Retry 和 reload 都是有预算的。恢复预算用尽后进入明确的 `rollover_recommended` / failed 状态，而不是无限刷新。

## 回复流中断

如果 assistant 已经开始输出，但页面明确显示连接中断，占位文本本身不算“持续有进展”。

只有 assistant 正文增长或签名变化才更新真实进度时间。持续断线且页面安全时，可以 reload 一次重新同步既有服务端回合；不会重新提交原用户任务。

## 页面健康自恢复：卡顿、内存与 429

0.1.63 把原本只用于诊断的 UI-pressure meter 接入了一个有严格预算的页面健康恢复层。它不会直接删除 ChatGPT/React 管理的历史 DOM；直接删节点可能让 React 内部树、事件绑定、虚拟列表和真实 DOM 失配。需要回收整页运行时内存时，优先通过受控 reload 重建 document / React tree / JS heap。

自动恢复使用固定窗口、O(1) 的聚合信号：MutationObserver callback rate、采样 tick rate、timer drift、Long Task，以及 Chromium 可提供时的 JS heap 使用量。单次尖峰只记录，不刷新。

- 活跃回合只有在页面持续高压且一段时间没有 assistant 进展，并且同源 conversation snapshot 明确证明 `current_node` 已是 **finished assistant** 时，才允许把 stale streaming UI 视为渲染问题并刷新。
- critical heap 只在页面至少静止一段时间后才允许维护刷新；人工输入、tool 运行、权限卡、未确认 delivery 都会阻断刷新。
- 第一级最多一次 durable `location.reload()`；如果刷新后页面仍持续处于同一健康故障，第二级最多一次 sender-scoped `chrome.tabs.reload(tabId)`。background 只接受当前 `sender.tab`、同一 conversation、Auto 已开启且 durable pending 状态匹配的请求，并在导航前记录 executed-at，避免 MV3 worker 重启后形成 reload loop。
- 两级预算都耗尽后不继续刷新，转为 `rollover_recommended`，让长会话走受控接力。

HTTP 429 是单独的反向信号：**429 只退避，不触发 Retry 或 reload。** 可见错误或 Resource Timing 中的 429 会进入 `30s → 60s → 120s`（封顶）的 cooldown；退避期间自动恢复不制造新的页面/API/附件请求，从而避免把服务端限流放大成刷新风暴。

## 自动接力：为什么需要新的 conversation

一个长时间 Herdr 项目可能产生几十轮用户/助手消息、MCP tool payload、Project 指令和系统上下文。即使可见正文看起来还不夸张，真实上下文压力也可能已经很高。

ChatGPT 还会虚拟化旧 DOM，所以“当前页面只挂着 5 条消息”不代表对话真的很短。

扩展使用保守的压力估算：

- 可见用户/助手文本的近似 token；
- `[data-testid="conversation-turn-N"]` 的最大绝对 turn 序号；
- 持久化的只增不减 message-count floor；
- 为 Project/system/tool payload 等不可见上下文预留余量。

达到高压力只代表**可以考虑接力**，不代表立即切会话。自动接力还必须满足：

- 当前 ChatGPT Project `自动 开`；
- 已绑定 workspace；
- workspace 不在 working；
- 页面无 streaming / tool / 权限卡；
- 没有人工未发送草稿；
- 没有 delivery uncertainty；
- 没有另一条 handoff 正在进行。

## handoff 的 fail-closed 流程

```text
旧 Project conversation
        │
        │ 生成带 transfer-id 的紧凑 handoff packet
        ▼
同一 Project 新 conversation
        │
        │ 提交 seed
        ▼
确认新 conversation id + seed marker
        │
        └── 确认成功后才切换 Project active_conv_key
```

关键规则：**Project/workspace binding 与 `continuity_id` 始终不变，旧 active conversation 在 cutover 前始终是权威。** z.ai 仍是 conversation-scoped，只有 seed 确认后才迁移它的具体 binding。

打开一个新 tab 不算成功；尝试发送 seed 也不算成功。必须确认新的 conversation identity 和 seed marker 真正存在，ChatGPT 才切换 `active_conv_key`；会话级站点才迁移 binding。

如果 seed delivery 不确定，则保留旧 active target，并记录可恢复状态。后续显式“恢复接力”先探测目标 conversation：seed 已存在就完成 cutover，不存在才重新尝试。

## handoff packet 应该包含什么

接力摘要的目的不是复述整段聊天，而是让新 planner 能继续工作。

建议保留：

- 当前目标；
- 已完成工作；
- 关键决定；
- 未完成工作；
- 已知 workspace / path / branch / commit / task id；
- 安全约束；
- 推荐下一步。

不应该把摘要中的 runtime / Git 状态当成永久事实。新 conversation 开始 mutation 前仍需重新 inspect / Git check。

## 手动接力

手动接力从页面 **HUD 的“接力”**启动，Side Panel 不再复制这个会话操作。它适用于：

- 你知道这条 conversation 已经很长；
- 当前工作已经到自然边界；
- 想主动在状态还清晰时换到新会话。

当前支持已绑定的 ChatGPT Project，以及稳定 `/c/<chat_id>` 的 z.ai 会话。手动接力在当前作用域 `自动 开` 或 `自动 关` 时都可以启动；新目标会话继承源会话的 Auto 状态。ChatGPT 接力只切换 Project binding 的 active target；z.ai 才迁移会话级 binding。接力期间源会话的自动 wake 暂停，workspace 仍有 working Agent 时则拒绝开始，避免 settled/wake 与 cutover 竞争。

手动 ChatGPT Project 接力会先解析持久化 Continuity Journal，并直接用 continuity reference 在同一 Project 的新会话中恢复，不再向源会话发送接力摘要请求。binding 中的 continuity 元数据缺失或过期时，本机 runtime 会按当前 conversation identity 找回同一条 chain。确实没有可用 Journal 时，才允许 Options 中已配置的 OpenAI-compatible LLM 从只读、经过上限裁剪的 source transcript 生成经过校验的 `HERDR_HANDOFF_V1` packet；两条来源都不可用时直接失败，当前会话和页面地址保持不变。自动接力、历史兼容路径和 conversation-scoped 站点继续遵循各自现有契约。

z.ai 的 handoff 控制消息走 raw channel，不经过 JSON→MCP task wrapper，避免摘要请求被误解释成 coding task。

## 自动化为什么默认关闭

连续工作能力很强，但它会主动向网页提交消息、处理部分页面动作和切换 conversation。

因此新 Project / 新会话默认 `自动 关`。用户先在 Side Panel 确认当前页面 binding，在 HUD 观察网页 / Herdr 状态，再显式开启 Auto。

这是一个有意的设计选择：**先让状态可见，再让状态自动推进。**

## 验收一个 continuity 配置

建议用一条真实长任务验证：

1. 当前网页作用域绑定一个 Herdr workspace；ChatGPT Project 要分别确认稳定 Project binding 与当前 active conversation target；
2. 打开对应作用域 Auto；
3. 从网页派一个会持续一段时间的 Agent 任务；
4. 确认 working 后 HUD 状态变化；
5. 有真实新输出时收到 progress；
6. workspace settled 后网页重新继续；
7. 故意关闭/刷新页面后，binding 仍指向正确作用域，active target 仍是预期 conversation；
8. 需要时测试手动 handoff：确认新会话 seed 存在后，ChatGPT Project 才切换 active target；conversation-scoped 站点才迁移 binding。

测试命令和实现级细节保留在仓库测试与 [CHANGELOG](../../../CHANGELOG.md)，本页只描述当前产品行为。
