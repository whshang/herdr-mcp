# 自动继续、恢复与接力

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

正常情况下仍优先让当前网页主模型生成 handoff packet，因为它持有最完整的会话上下文。如果页面已经显示单次对话硬上限、接力 prompt 无法提交，或者主模型在有界等待后已停止生成但仍没有给出摘要，Herdr 会改用 Options 中已配置的 OpenAI-compatible LLM 兜底。兜底模型接收经过上限裁剪的 user/assistant 会话 transcript，仍必须输出同一个经过校验的 `HERDR_HANDOFF_V1` packet，之后继续复用原有 target / seed / binding / continuity commit 安全链。

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
