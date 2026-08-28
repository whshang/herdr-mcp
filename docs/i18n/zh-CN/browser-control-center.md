# 浏览器控制中心：在 Chrome Side Panel 里观察真实 Herdr 现场

浏览器控制中心把 Herdr 的 workspace / pane / Agent 运行现场放进 Chrome Side Panel。

它解决的不是“让浏览器替你写代码”，而是一个更基础的问题：

> 当网页 AI、本地 Agent、测试进程和终端同时工作时，怎样始终知道**哪一个现场正在发生什么，以及下一条人工控制明确指向哪里**？

当前控制中心以**当前页面上下文 + 实时观察 + 明确目标 + 有界读取**为主。写操作仍保持 Preview-only，不会因为 UI 上出现“提示 Agent / 调整会话 / Herdr API / 终端输入”就绕过现有 mutation 安全边界。

## 它和浏览器连续工作有什么区别

浏览器扩展现在有两个容易混淆、但职责不同的操作面：

| 操作面 | 主要问题 | 入口 |
|---|---|---|
| HUD / Continuity | 当前网页在干嘛、Herdr 在干嘛，以及是否开启 Auto 或发送三个预置会话动作 | 支持的 Web AI 页面内 |
| Control Center | 当前标签页属于哪个 Project / conversation、绑了什么、本机现场怎样、人工明确目标是谁 | Chrome Side Panel |
| Options | timing / LLM / 语言等低频配置是什么 | Control Center 的“设置” |

HUD **不是第二个控制面板**：没有抽屉、workspace picker、binding 编辑、timing 表单，也没有 handoff 按钮；只显示网页状态、Herdr 状态、一个紧凑的绑定徽标（`🔗N`）、Auto 和三个预置手动会话动作。pane / Agent 明细只在 Control Center 展示。

Control Center 统一负责**当前页面身份、绑定 / 解绑、手动接力、本机详细状态和明确目标选择**。

两者共享同一条 Native Messaging / 本机 IPC 信任链，但不是同一个状态机。

## 怎么打开

点击浏览器工具栏里的 Herdr 扩展图标，Chrome 会直接打开 **浏览器控制中心** Side Panel；中间不再出现旧 Popup。

控制中心沿用扩展 Options 中的语言设置，当前支持：

- English；
- 简体中文；
- 日本語。

## “当前页面”跟随浏览器激活标签页

顶部 **当前页面** 卡片是浏览器上下文与本机状态之间的桥。它让既有 binding authority 读取 Chrome 当前激活 tab，并显示：

- 当前受支持站点；
- ChatGPT Project identity（如果有）；
- conversation identity（如果有）；
- 这个 Project / conversation 当前绑定了多少个 workspace；
- 始终可发现的 **手动接力**。当前页面不支持、尚未绑定、Herdr 仍在工作或还没进入具体会话时，按钮保留但禁用，并直接解释原因。

绑定 / 解绑不再在“当前页面”卡里复制一套 selector / chip。唯一入口就是下方 **workspace 行右侧的绑定 toggle**：状态和绑定在同一行完成识别与操作。

切换 Chrome 标签页或当前标签页导航后，这张卡会通过 tab activation / navigation event 自动刷新，不做固定频率轮询；相应 workspace 会排到本机 workspace 列表前部并高亮。

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
- 唯一的 **绑定 / ✓ 已绑定** toggle。

当前页面已经绑定的 workspace 会排在列表前部并高亮。点击 workspace 行主体只负责展开 / 收起 pane；点击右侧绑定 toggle 只负责 bind / unbind，两种操作不会互相触发。绑定 mutation 在 UI 内串行化，避免连续点击制造难以判断的中间状态。若已绑定 workspace 已关闭或暂时不在 runtime snapshot 中，仍保留一个“当前不可见”的绑定行，允许直接解绑，不会把失效绑定藏起来。

展开后继续展示 pane 级明细：

- pane id；
- Agent 名称，或 terminal-only；
- working / idle / done / blocked 等状态；
- 当前 Herdr focus 标记；
- cwd / project root；
- Agent 已运行时长；
- 最近活动时间；
- 最近一条有界摘要 / terminal title。

初次打开只展开有限数量的 workspace，避免大量项目同时存在时把面板撑成不可读的长列表。切换浏览器 tab 会更新绑定排序与高亮，但不会改变 Pinned Target。

## Pin 一个明确目标

点击某个 pane 行即可固定它。

固定后底部会显示：

```text
固定目标
wD7 / wD7:p2 / pi
运行中 · revision ...
```

Pinned Target 会持久保存在扩展本地状态中，并在新的 snapshot / reconnect 后重新验证。

### 为什么会变成“目标已失效”

以下情况会让旧 pin 进入 stale：

- pane 已关闭；
- 同一个 pane id 已经属于新的 Agent session；
- 目标 revision 发生了不能安全视为同一执行对象的变化。

stale 后控制中心不会猜测新目标。需要用户重新点击 pane 才能继续读取或预览操作。

## 当前真正会执行的操作

当前版本只有**只读动作**会立即执行。

### 查看状态

`查看状态` 展示当前 pane 的结构化状态，并把可能很长的输出裁成有界尾部。

它适合快速确认：

- 这个 pane 当前是谁；
- status 是什么；
- cwd / project root 在哪里；
- 最近输出是什么。

### 读取最近输出

`读取最近输出` 从本机 runtime 读取受限的 terminal tail。

当前请求有固定上限（40 行 / 4096 字符级别），不会因为一个运行数小时的 terminal 就把完整历史灌进 Side Panel。

## 提示 Agent / 调整会话 / Herdr API / 终端输入为什么只显示“操作预览”

控制中心已经把未来控制面的交互模型放进 UI，但**当前版本没有开启这些 mutation**。

页面会明确显示：

> 实时状态 · 控制操作仅预览

并将下面四种模式放在 `操作预览` 区：

| 模式 | 最终意图 | 当前行为 |
|---|---|---|
| 提示 Agent | 通过 Herdr `agent.prompt` 给 pinned Agent 发一条新任务或补充提示 | 只生成 descriptor，不发送 |
| 调整会话 | 对**已经运行中的** provider / Agent session 调整方向；未来还取决于 provider 是否支持 steer | 只生成 descriptor，不发送 |
| Herdr API | 指定准备调用的 Herdr control-plane method；未来真正执行前必须通过实时 method schema 与安全检查，不是任意 shell | 只生成 descriptor，不执行，也不声称已经校验 |
| 终端输入 | 向 pinned terminal pane 写 literal text / input / keys；这是风险最高的路径 | 只生成 descriptor，不写入 |

点击“生成预览”只会展示经过分类的 action descriptor，例如：

- action type；
- risk class；
- workspace / pane；
- target revision；
- args；
- `executable: false`；
- `execution_mode: dry_run`。

这不是“按钮坏了”，而是当前 Phase A 的产品安全边界。

## 为什么先做 Preview，而不是直接开放终端输入

浏览器控制面的风险不是“能不能把文本写进去”，而是：

- 目标是否仍然是用户刚才选择的对象；
- 请求失败时到底有没有送达；
- 重试会不会重复 mutation；
- Agent session 是否已经换代；
- provider steer 与普通 terminal input 是否需要不同确认；
- 浏览器 reload / service worker restart 后是否仍能判断 delivery phase。

在这些契约稳定之前，让 Side Panel 直接拥有任意 terminal 写入，会破坏 herdr-mcp 已有的 mutation / idempotency / recovery 纪律。

因此当前顺序是：

```text
先把状态看准
  ↓
再把目标固定
  ↓
再把动作和风险描述清楚
  ↓
最后才逐类开放可执行 mutation
```

## Runtime 或事件流断开时会看到什么

顶部运行状态区分两类问题：

- **本机运行时不可用**：当前没有可靠 runtime snapshot；
- **运行时正常 · 事件流正在重连**：已有状态可显示，但实时事件链正在恢复。

控制中心不会把这两种情况都显示成一个模糊的“离线”。

重连后会重新获取 snapshot，再继续增量事件。

## Control Center、HUD 和“排队”应该怎么一起用

推荐把三个操作面理解成不同层级：

```text
Control Center
  看本机真实 workspace / pane / Agent
  ↓
HUD
  管理当前网页 Project / conversation 的 binding 与 Auto
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
6. 长任务由浏览器 continuity engine 维护 progress / settled / recovery / automatic handoff；HUD 只保留状态、Auto 和三个预置动作，手动接力只在“当前页面”。

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

当前 Control Center 已具备：

- Chrome Side Panel 正式入口；
- 实时 workspace / pane lifecycle；
- Agent 状态展示；
- explicit pinned target；
- stale target fail-closed；
- 有界状态 / 输出读取；
- mutation risk classification；
- Preview-only action descriptor；
- en / zh / ja UI。

当前**不具备**：

- 从 Side Panel 真正发送 Prompt；
- provider steer；
- 任意 Herdr mutation；
- terminal text / input / key 写入；
- interrupt。

未来任何一项从 Preview 进入可执行状态，都必须单独通过可靠性、安全和真实浏览器 UAT，而不是因为 UI 已经存在就自动启用。

## 相关文档

- [浏览器扩展总览](extension.md)
- [浏览器连续工作](browser-continuity.md)
- [自动继续、恢复与接力](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [故障排查](troubleshooting.md)
