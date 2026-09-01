# 浏览器控制中心

*在 Chrome Side Panel 里观察真实 Herdr 现场。*

浏览器控制中心把 Herdr 的 workspace / pane / Agent 运行现场放进 Chrome Side Panel。

它解决的不是“让浏览器替你写代码”，而是一个更基础的问题：

> 当网页 AI、本地 Agent、测试进程和终端同时工作时，怎样始终知道**哪一个现场正在发生什么，以及下一条人工控制明确指向哪里**？

当前控制中心采用紧凑布局：顶部只保留 Herdr 连接/运行状态和少量全局操作；受支持的当前页面只显示一条 `ChatGPT · 已绑定 N` 上下文；workspace / pane 列表是主体。`发送指令`调用 Herdr `agent.prompt`；只有 runtime 明确声明支持原生 steer 时才显示`调整当前任务`；正在运行的 Agent 还提供独立的`停止任务`；普通终端 pane 提供有 target fencing 的`运行命令`，通过 `pane.send_input + Enter` 真实执行。

## 它和浏览器连续工作有什么区别

浏览器扩展现在有两个容易混淆、但职责不同的操作面：

| 操作面 | 主要问题 | 入口 |
|---|---|---|
| HUD / Continuity | 当前网页在干嘛、Herdr 在干嘛，以及是否开启 Auto 或发送三个预置会话动作 | 支持的 Web AI 页面内 |
| Control Center | 当前标签页属于哪个 Project / conversation、绑了什么、本机现场怎样、人工明确目标是谁 | Chrome Side Panel |
| Options | timing / LLM / 语言等低频配置是什么 | Control Center 的“设置” |

HUD **不是第二个控制面板**：没有抽屉、workspace picker、binding 编辑、timing 表单，也没有本地 Herdr mutation 控件；它显示网页状态、Herdr 状态、一个紧凑的绑定徽标（`🔗N`）、Auto、三个预置推进动作和“手动接力”，因为这些操作都直接作用于当前网页会话。pane / Agent 明细只在 Control Center 展示。

Control Center 统一负责**当前页面身份、绑定 / 解绑、本机详细状态和明确的本地目标选择**；手动接力由页面 HUD 负责。

两者共享同一条 Native Messaging / 本机 IPC 信任链，但不是同一个状态机。

## 怎么打开

点击浏览器工具栏里的 Herdr 扩展图标，Chrome 会直接打开 **浏览器控制中心** Side Panel；中间不再出现旧 Popup。

控制中心沿用扩展 Options 中的语言设置，当前支持：

- English；
- 简体中文；
- 日本語。

## 当前页面上下文保持一行

控制中心仍读取 Chrome 当前激活 tab，但不再显示一张大卡片。受支持页面只显示一条紧凑上下文，例如 `ChatGPT · 已绑定 3`。内部仍保留：

- 当前受支持站点；
- ChatGPT Project identity（如果有）；
- conversation identity（如果有）；
- 这个 Project / conversation 当前绑定了多少个 workspace；

绑定 / 解绑不再在“当前页面”卡里复制一套 selector / chip。唯一入口就是下方 **workspace 行右侧的绑定 toggle**：状态和绑定在同一行完成识别与操作。

切换 Chrome 标签页或当前标签页导航后，这条上下文会通过 tab activation / navigation event 自动刷新，不做固定频率轮询；相应 workspace 会排到本机 workspace 列表前部并高亮。

但它**不会自动改变 Pinned Target**。当前页面 binding 回答“这个网页上下文属于哪个本机 workspace”；Pinned Target 回答“未来人工控制明确针对哪个 pane”。

## 实时状态从哪里来

控制中心不是固定频率轮询网页 DOM。

当前数据路径：

```text
Herdr workspace / pane / agent state
        ↓
herdr-mcp Rust runtime
        ↓ local IPC / push events
Extension service worker
        ↓ one snapshot + incremental events
Chrome Side Panel
```

打开或重连时先拿一次权威 snapshot，然后消费增量事件，例如：

- `workspace_upsert` / `workspace_removed`；
- `pane_upsert` / `pane_removed`；
- Agent working / settled 状态变化。

Side Panel 隐藏时会减少无意义 DOM 工作；重新可见或事件流重连时再做 reconciliation。

因此面板上新增/关闭 pane 应该直接出现/消失，而不是依赖“每隔几秒刷新一次”。

## Workspace Binding、Pinned Target 和 Herdr Focus 不是一回事

这是控制中心最重要的交互约束。

| 概念 | 含义 | 会不会因为 Herdr focus 变化自动改变 |
|---|---|---|
| Workspace Binding | 一个网页 Project / conversation 属于哪份本地工作上下文 | 不会 |
| Pinned Target | Control Center 下一条人工控制明确针对哪个 pane / Agent | **不会** |
| Herdr Focus | 人当前在 Herdr 桌面里看的是哪个 pane | 会 |

例如：ChatGPT Project 可以绑定 `wD7`，但你在 Control Center 里明确 pin `wD7:p2`。

即使随后人在 Herdr 中点击了 `wD7:p3`，控制中心也不能偷偷把目标改成 `p3`。

这避免了一个高风险问题：**用户以为下一条命令发给 A，实际因为焦点变化发给了 B。**

## Workspace 状态和当前页面绑定是一张列表

控制中心不再把“workspace 状态”和“当前页面绑定”拆成两套 UI。每个 workspace 行同时显示：

- workspace label / id；
- workspace 聚合状态点；
- pane 数量与 working 数量；
- 当前页面是否绑定到这个 workspace；
- 唯一的 **绑定 / 已绑定** toggle。

当前页面已经绑定的 workspace 会排在列表前部并高亮。点击 workspace 行主体只负责展开 / 收起 pane；点击右侧绑定 toggle 只负责 bind / unbind，两种操作不会互相触发。绑定 mutation 在 UI 内串行化，避免连续点击制造难以判断的中间状态。

binding 还会携带一个与 ChatGPT Project 身份完全独立的本地项目身份。Git workspace 由 runtime 根据 Git common-dir 元数据派生，因此主 checkout 和 linked worktree 会归为同一个本地项目；非 Git workspace 则退化为 canonical 本地目录。一个 workspace 手动绑定后，后续新开的 Herdr workspace 只要本地项目身份相同，就会自动继承当前网页作用域。ChatGPT Project 中仍然只继承该 Project；普通 `/c/<id>` 对话中只继承当前 conversation，不会串到另一个普通对话。

精确的 `workspace_removed` 生命周期事件会立即删除已关闭 workspace 的绑定；MV3 service worker 休眠期间如果漏掉事件，非空的权威 workspace catalog 会做补偿 reconciliation。空的或瞬时不完整的 catalog 不会被解释为“所有 workspace 都已关闭”。关闭过的历史 workspace 也不会再被合成为“离线绑定”行。对自动继承组中的任一 workspace 执行解绑，会把当前网页作用域下的这一组一起解绑，避免下一次 reconciliation 又立即自动绑定回来。

展开后继续展示 pane 级明细：

- pane id；
- Agent 名称，或 terminal-only；
- working / idle / done / blocked 等状态；
- 当前 Herdr focus 标记；
- cwd / project root；
- Agent 已运行时长；
- 最近活动时间；
- 最近一条有界摘要 / terminal title。

初次打开只展开有限数量的 workspace，避免大量项目同时存在时把面板撑成不可读的长列表。切换浏览器 tab 会更新绑定排序与高亮，但不会偷偷切换你正在操作的窗格。

## 在窗格里直接展开操作

点击某个 pane 行，会直接在这个 pane 下方展开它自己的操作区；再次点击同一个 pane，或点击“收起”，即可关闭。

用户不需要先理解“固定目标”再去面板底部找操作。内部仍会保存并校验 pane identity，确保发送指令或停止任务时不会误操作已经被替换的 Agent session。

### 为什么会变成“目标已失效”

以下情况会让旧 pin 进入 stale：

- pane 已关闭；
- 同一个 pane id 已经属于新的 Agent session；
- 目标 revision 发生了不能安全视为同一执行对象的变化。

stale 后控制中心不会猜测新目标。需要用户重新点击 pane 才能继续操作。

## 当前真正会执行的操作

每个 pane 的展开区会根据它是 Agent 还是普通终端，只显示真正适用的操作：

| 模式 | 当前行为 | 投递语义 |
|---|---|---|
| 详情 | 执行有界只读 | 只读 |
| 最近输出 | 执行有界 terminal tail 读取 | 只读 |
| 发送指令 | **真实执行**：走 extension-only 的本地可信 action route，并复用 Herdr `agent.prompt` 可靠性内核 | 返回 `submitted` / `queued` / `rejected` / `uncertain` / `failed`，可用时带 operation id / evidence |
| 调整当前任务 | 仅当 runtime 明确声明当前 provider 的 native steer 可用时显示 | 在不中断当前 active turn 的前提下调整正在执行的任务；不会退化成 Agent Prompt |
| 停止任务 | 仅对正在运行的 Agent 可用；确认后向该 pane 发送 `Ctrl+C` | 用于停止当前 CLI turn/process；不冒充 provider interrupt |
| Herdr API | 仅预览 | 不从这个 UI 执行任意 Herdr mutation |
| 运行命令 | **真实执行**：仅普通终端 pane 显示，通过 `pane.send_input` 写入命令并发送 `Enter` | mutation 前重新校验 `target_revision`；投递不确定时不自动重试 |

### 详情

`详情`展示结构化 pane 状态，同时对可能很大的最近输出做上限裁剪。

### 最近输出

`最近输出`让本地 runtime 返回有界 terminal tail（大致 40 行 / 4096 字符），即使终端运行数小时也不会把无限历史灌进 Side Panel。

### 发送指令：可靠 Agent Prompt，不是终端注入

`发送指令`始终作用于当前展开操作区所属的 pane，数据路径只有：

```text
Side Panel
  → extension service worker
  → Chrome Native Messaging
  → mode-0600 herdr-mcp Unix socket
  → POST /extension/control/action
  → 已有的持久化 agent.prompt operation
```

这个 HTTP route 在普通 TCP 上固定不可用：即使持有常规 herdr-mcp bearer 也会收到 `403`。因此浏览器控制 mutation 不会演变成公网/网页可直接调用的工作站控制 API。

每次 action 都携带 runtime 生成的 `target_revision`。Rust 在 mutation 前重新读取 live pane；如果 pane 已消失、pane 后面的 Agent/session 已替换，或 runtime generation 已变化，直接返回 `stale_target`，不会提交 mutation。

Prompt 还复用 `agent.prompt` 已有的持久化 idempotency/op record，而不是自己造浏览器重试逻辑。Side Panel 为一次用户动作生成 idempotency key，并明确展示 `uncertain`。结果不确定时，先检查 live state，不要盲目重发。

### 调整当前任务：绝不把 Prompt 冒充 true steer

`调整当前任务`比 Prompt 更严格：只有 runtime 的 `control_capabilities.steer.available` 为 true 时 UI 才显示它。它**不会**在 steer 失败时偷偷退化成 `agent.prompt`，再把结果写成“已 steer”。

对于 Codex，provider-native 同一 active turn 的 `turn/steer` 至少需要把所选 Herdr pane 权威映射到 app-server control endpoint、`threadId` 和当前 `expectedTurnId`。当前 Herdr pane/session metadata 并没有这些关联。因此目前 UI 通常不会显示“调整当前任务”。

仅看到 `~/.codex/ipc/ipc.sock` 文件不能证明可 steer：socket 可能已经 stale、可能属于别的 client/session，而且它本身既不标识目标 thread，也不提供 expected active turn。只有这些身份能够端到端证明时，才会开放真正的 provider steer。

`停止任务`是另一条更窄的本地控制路径。它只对处于 `working` 状态的 Agent 显示为可用，用户确认后通过 `pane.send_keys(["C-c"])` 发送终端中断键。它不声明 provider-level interrupt，也不会在投递结果不确定后自动重试；再次停止前先检查目标状态。

### 运行命令：只对普通终端开放

普通 terminal-only pane 展开后会显示`运行命令`。它不是任意 Herdr API，也不会绕过目标选择：Side Panel 先记录该 pane 的 `target_revision`，Rust 在真正写入前重新读取 live pane，只有目标仍是同一个普通终端时才调用：

```text
pane.send_input({ pane_id, text, keys: ["Enter"] })
```

如果 pane 已关闭、被替换或变成 Agent pane，直接返回 `stale_target` / `rejected`。网络或 IPC 结果不确定时返回 `uncertain`，UI 不会自动重发命令。该路径已用隔离终端做真实 UAT，验证输入与 `Enter` 会在目标 pane 中实际执行。

这正是 Issue #57 原始需求最容易混淆的地方：**“已排队/已 Prompt”与“同一 active turn 已 steer”是不同 outcome，绝不能互相冒充。**

## Reliability Kernel：内存、请求压力、超时恢复与刷新死循环

Side Panel 不是孤立功能。之前规划的页面性能/自愈能力已经是同一 Browser Control Plane 可靠性底座的一部分：

- **Side Panel 没有固定轮询循环。** 首次只取一次 snapshot，之后消费 workspace/pane 增量事件。
- **全局只保留一条共享 Herdr event stream。** 不会每个 binding 各开一条网络流。
- **状态请求去重。** 并发 freshness 请求会合并，不会放大 `/push/state` 请求量。
- **MutationObserver / render 合并调度。** DOM burst 被折叠成有界 UI 工作，不会每个 mutation 都触发一轮重计算/动作。
- **隐藏页面暂停昂贵 UI 工作。** 重新可见时再 reconcile。
- **输出严格有界。** terminal/output tail 会裁剪，长时间运行不会让浏览器无限保留历史。
- **UI 压力 / heap 信号。** 浏览器可提供时，会结合 mutation rate、timer drift、JS heap pressure 判断页面健康。
- **429 只做 backoff。** 遇到限流扩大网络退避，不因为 429 制造刷新风暴。
- **回复超时/断流走 evidence-first 恢复。** 先核对 same-origin/server state，再决定是否重试请求或只同步视图。
- **强制刷新是最后且有界的恢复手段。** sender-scoped、受 Auto gate 控制、导航前持久化、带 durable cooldown/budget；并发刷新请求只能选出一个 winner。
- **不会刷新死循环。** cooldown 内重复请求直接拒绝；Auto 关闭的会话不能由 background 强刷。

这些机制由 continuity 与 Browser Control Plane 共用；新 action route 没有增加另一套 polling、heartbeat 或盲重试 daemon。

## 为什么任意 Herdr API 仍只做 Preview

浏览器控制最难的不是“把字节写进终端”，而是保证：目标是不是原目标、失败是否已投递、重试会不会重复 mutation、pane 后面的 session 是否已换，以及 MV3 reload 后还能否判断 delivery phase。

Prompt 通过 Rust target fencing + `agent.prompt` idempotency 满足这些契约；终端路径现在只开放一个窄化的 `terminal_input -> pane.send_input + Enter` 动作，并复用同一 target fencing、禁止 uncertain 自动重试。任意 Herdr 方法的作用面仍然过广，因此继续 fail-closed / Preview-only。

## Runtime 或事件流断开时会看到什么

顶部运行状态区分两类问题：

- **本机运行时不可用**：当前没有可靠 runtime snapshot；
- **运行时正常 · 事件流正在重连**：已有状态可显示，但实时事件链正在恢复。

控制中心不会把这两种情况都显示成一个模糊的“离线”。

重连后会重新获取 snapshot，再继续增量事件。

## Control Center、HUD 和“排队”应该怎么一起用

推荐把三个操作面理解成不同层级：

```text
HUD
  当前网页会话：状态、Auto、继续 / 查 Herdr / LLM 判断
  ↓
Control Center · 当前页面 + 工作区
  当前页面 ↔ workspace 绑定，同时看 workspace / pane / Agent 实时状态
  ↓
Control Center · 本地 Herdr 目标
  固定明确 pane，执行本地读取与操作预览
  ↓
排队（ChatGPT composer）
  在当前回复不中断的前提下追加下一轮用户意图
```

典型工作流：

1. 打开 Control Center，确认真实 workspace 和 running pane；
2. 必要时 pin 某个 pane，读取最近输出；
3. 回到 ChatGPT，让 Web planner 继续通过 MCP / Herdr 工具做实际控制；
4. ChatGPT 正在回复时如果想到补充要求，使用“排队”而不是中断当前回合；
5. 当前回复结束后，排队内容优先成为下一条用户消息；
6. 长任务由浏览器 continuity engine 维护 progress / settled / recovery / automatic handoff；HUD 保留页面级状态、Auto、三个预置推进动作和手动接力，绑定与本地 Herdr 控制留在 Side Panel。

## 本机安全模型

Control Center 沿用浏览器扩展的本机信任路径：

```text
Side Panel
   ↓ Extension service worker
Chrome Native Messaging host
   ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

它不会：

- 把 Herdr bearer 暴露给网页；
- 因为打开 Side Panel 就开放公网端口；
- 把任意网页升级成无限制 Shell；
- 用当前 Herdr focus 代替明确 target identity；
- 在 stale target 上继续 mutation。

## 当前产品边界

Control Center 当前包括：

- Chrome Side Panel 一级入口；
- 无固定轮询的 live workspace / pane lifecycle；
- Agent 状态展示；
- 显式 pinned target；
- runtime-authoritative `target_revision` 与 stale-target fail-closed；
- 有界状态 / 输出读取；
- 可执行的可信 `提示 Agent`，复用持久化幂等与 outcome evidence；
- 可执行的 provider `调整会话`请求，并如实返回 capability outcome，**绝不拿 Prompt 冒充 steer**；
- 仅预览的任意 Herdr API / raw terminal control；
- 共用的内存/请求压力/超时/刷新循环防护；
- en / zh / ja UI。

Codex true same-turn steer 仍严格依赖可验证的 pane → app-server endpoint → `threadId` → active `expectedTurnId` 映射。在这个 primitive 出现前，`session_not_resolved` 才是正确结果，而不是隐藏 fallback。

## 相关文档

- [浏览器扩展总览](extension.md)
- [浏览器连续工作](browser-continuity.md)
- [自动继续、恢复与接力](browser-continuity.md)
- [JSON → MCP bridge](extension.md)
- [故障排查](troubleshooting.md)
