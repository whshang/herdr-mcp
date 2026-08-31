# Browser Control Plane：本地窗口状态、Herdr 指令与 Active Agent Steering 设计

> Status: WIP / architecture proposal
>
> Date: 2026-08-27
>
> Related: GitHub Issue #57 `feat: browser steer bridge for active agents via herdr-mcp`
>
> Roadmap authority: `docs/herdr-architecture-roadmap.md`

## 1. 目标

Issue #57 不应该只实现一个 `Steer` 按钮。真正需要解决的是：**用户在浏览器里持续看见本地 Herdr 工作现场，并能明确地对某个本地窗口 / Agent 发送控制指令。**

目标体验：

```text
ChatGPT / Web 页面
        │
        ├─ 查看所有 Herdr workspace / tab / pane / agent 状态
        ├─ 固定一个明确 pane 作为控制目标
        ├─ 手动发送 Herdr 指令
        ├─ 给 Agent 发送 prompt / queue
        ├─ 给终端发送明确输入
        ├─ 在 provider 支持时 true steer 当前 turn
        └─ interrupt / focus / inspect 等显式控制
        │
        ▼
Browser Control Plane
        │ Chrome Native Messaging
        ▼
local herdr-mcp Rust runtime
        │
        ├─ Herdr native APIs
        └─ provider adapters（例如 Codex app-server turn/steer）
```

核心不是“浏览器自动操作 Herdr”，而是建立一个**本地、可观察、可定位、可确认、可恢复的远程控制面**。

---

## 2. 范围定义

本文中的“本地开的每一个窗口”指 **Herdr 已知的工作现场**：

```text
workspace
  └─ tab
      └─ pane
          ├─ terminal
          └─ agent
```

不尝试枚举 macOS/Windows 上所有普通 GUI 窗口。herdr-mcp 只控制 Herdr 管理的 workspace / tab / pane / agent。

浏览器必须能够看到：

- 所有 workspace；
- workspace 内的 tab；
- tab / workspace 下的 pane；
- pane 的 cwd / project；
- pane 是否存在 Agent；
- Agent 名称 / kind / working / idle / done / blocked；
- `started_at` / `last_activity_at`；
- terminal title / 当前 doing 摘要；
- 哪个 pane 当前 focused；
- 哪些 pane 正在执行长任务；
- 当前浏览器会话绑定了哪些 workspace；
- 当前显式控制目标是哪一个 pane。

浏览器看到的是**状态镜像**，权威状态始终属于 Herdr / local runtime。

---

## 3. 与当前 Roadmap 的关系

### 3.1 可以与当前 Rust / 性能路线一并推进

以下部分不需要等待所有 Rust 重构和性能优化完成，可以与当前路线并行：

| 能力 | Roadmap 对应 | 现在是否可做 | 说明 |
|---|---|---:|---|
| Browser Control Center UI | Browser Continuity / Continuity 2.0 | 是 | 纯 UI 与状态模型，可先做 |
| 全 workspace / pane / agent 状态镜像 | Streaming First / EventCache | 是 | 已有 `/push/state` + `/push/events` 基础 |
| 增加 tab 状态 | Continuity 2.0 | 是 | 当前 push state 缺 tab，需要补充 |
| 事件增量更新而非轮询全量 | Streaming First | 是 | 与下一性能片完全一致 |
| pane 显式选择 / pin | Continuity 2.0 safety | 是 | 不产生本地副作用 |
| stale target 检测模型 | Reliability 基础 identity | 是（先建模型） | 可以先定义 revision，不执行 mutation |
| 浏览器命令面板 / action schema | Product UX | 是 | 可以先做 UI、校验、dry-run |
| 只读 Herdr command | Rust local control API | 部分 | Rust 本地控制 route ready 后即可开放 |
| pane 输出 compact / expand | Result Optimization | 是 | 默认摘要，用户按需展开 raw tail |
| 长任务 started/progress/settled 展示 | Streaming First / Long Task Progress | 是，随 Streaming 落地 | 不需要等 Batch B |

这些工作不会增加第 19 个 MCP tool，也不改变 epoch 2 / 18 tools 公共 contract。

### 3.2 应等当前 Rust production ownership 到位后再实现

以下能力不要再在 Node runtime 上新增长期实现：

- 新的 extension mutation endpoint；
- 浏览器直接 `agent.prompt`；
- `pane.send_text` / `pane.send_input` / `pane.send_keys`；
- interrupt；
- provider steer adapter；
- durable pinned target / operation history。

原因：roadmap 已明确 Rust Native Messaging host、supervisor/updater/link 正在迁移。现在为这些能力另建 Node 控制 API，会制造第二套生命周期与安全语义。

**原则：UI/state 可以先做；新的 mutation owner 直接落 Rust。**

### 3.3 应等 Reliability Kernel 后再默认开放

这些属于有副作用操作：

```text
agent prompt
terminal input
send Enter / keys
interrupt
focus-changing control
true steer
```

它们需要复用 roadmap Phase 7 的：

```text
op_id
idempotency key
request hash
delivery phase
uncertain -> reconcile
runtime boot/generation identity
```

否则浏览器收到 timeout 时无法回答“刚才那句指令到底有没有发送”，容易重复 prompt / 重复 Enter / 重复 interrupt。

可以在 Reliability Kernel 前做开发/实验开关，但不能作为默认生产能力。

### 3.4 应等 Continuity 2.0 后再完成的部分

Continuity 2.0 负责把下面状态从浏览器临时 storage 提升为可恢复控制状态：

- conversation ↔ workspace binding；
- pinned control target；
- target revision；
- handoff 后 target 继承；
- extension service worker restart 后 rehydrate；
- browser rollover 后仍知道原来控制的是哪个 workspace / pane；
- progress / settled 与 checkpoint 结合。

Control Plane 的 UI 可以先出现，但真正跨浏览器重启 / 长对话接力稳定恢复，要复用 Continuity 2.0。

### 3.5 True Codex steer 应后置于什么

`turn/steer` 本身不依赖 Batch B、IndexBackend、IngressProfile 或更深 Project Context Cache。

它真正依赖：

1. Rust local control plane 已成为 production owner；
2. mutation reliability 基础完成；
3. 能稳定做：

```text
pane_id
  -> agent/provider identity
  -> Codex backend session
  -> threadId
  -> active turnId
```

4. `expectedTurnId` race 被正确处理。

因此 true steer 不需要等“所有性能优化结束”，但应该在 **Rust production ownership + Reliability Kernel 基础**之后落地。

---

## 4. 不需要等待的 Roadmap 项

以下 roadmap 工作不是 Browser Control Plane 的 blocker：

- Search IndexBackend；
- Batch B MCP batch contract；
- deeper Project Context Cache；
- IngressProfile；
- relay-vps / local-tunnel；
- model-aware result format。

Browser Control Plane 是 **local-only control surface**，不应该经过 Cloudflare Edge，也不应该依赖公网 ingress。

---

## 5. 重新设计后的整体架构

```text
┌──────────────────────────────────────────────────────┐
│ Browser                                               │
│                                                      │
│  ChatGPT page       Herdr Control Center Side Panel  │
│       │                         │                    │
│       │ selection/context      │ state + actions    │
│       └──────────────┬──────────┘                    │
│                      ▼                               │
│             Extension Service Worker                 │
│                      │                               │
│              Chrome Native Messaging                 │
└──────────────────────┼───────────────────────────────┘
                       ▼
              Rust Native Messaging Host
                       │
               mode-0600 Unix socket
                       ▼
┌──────────────────────────────────────────────────────┐
│ local herdr-mcp Rust runtime                         │
│                                                      │
│  BrowserStateMirror                                  │
│     ├─ authoritative snapshot                        │
│     └─ incremental event stream                      │
│                                                      │
│  BrowserControlService                               │
│     ├─ target validation                             │
│     ├─ action classification                         │
│     ├─ Reliability Kernel                            │
│     ├─ HerdrActionAdapter                            │
│     └─ ProviderAdapter                               │
│           └─ CodexSteerAdapter                       │
│                                                      │
└──────────────────────┬───────────────────────────────┘
                       ▼
                    Herdr
        workspace / tab / pane / agent APIs
```

浏览器 extension page / service worker 是控制 UI；**content script 不能直接持有本地执行权限**。

---

## 6. Browser Control Center UI

### 6.1 不再把复杂控制塞进 Popup

当前 popup 适合：

- 服务在线状态；
- workspace binding；
- 简单自动化开关。

新能力需要长期可见列表、输入框、状态变化，因此建议新增 Chrome Side Panel：

```text
Herdr Control Center
────────────────────────────────
● Local runtime healthy
● 4 workspaces · 11 panes · 3 working

▾ herdr-mcp (wBH)
  ▾ tab: 1
    ● wBH:p1  terminal       idle
       ~/Documents/herdr-mcp
    ● wBH:p2  utility        working
       git / tests

▾ novo (wBM)
  ▾ tab: 1
    ● wBM:p2  pi             working
       implementing release gate

────────────────────────────────
Pinned target
📌 wBM:p2 · pi · working

[Prompt] [Steer] [Herdr] [Terminal]

> Do not modify the database schema...

[Send]
```

Popup 保留为 quick status / bind；Side Panel 负责 control plane。

### 6.2 Pane row

每个 pane 至少展示：

```text
pane_id
workspace / tab
cwd / project
terminal title
agent name + kind
status
last activity
running duration
focused or not
pinned or not
```

状态颜色只用于辅助，不作为唯一语义：

- working；
- idle；
- done；
- blocked；
- terminal-only；
- unknown / stale。

### 6.3 默认 compact，按需展开

不能把所有 terminal output 持续复制进浏览器内存。

默认只展示：

```text
last activity
current doing/title
bounded tail summary
```

点击 pane 后才读取有限 raw tail。

这与 roadmap 的 Result Optimization 和 Browser Performance Budget 一致。

---

## 7. Browser State Mirror

### 7.1 当前基础

现有 `/push/state` 已包含：

```text
workspaces[]
panes[]
agents[]
server_time
```

现有 `/push/events` 已支持 agent working / output / settled 等实时事件。

当前缺口：

- `tabs[]` 尚未进入 push state；
- pane/project/agent 信息没有形成浏览器专用统一 view model；
- UI 主要展示 workspace 汇总，不展示所有 pane 的完整状态；
- binding 中 `pane` 是最近活跃 pane，不是显式控制 target。

### 7.2 目标状态模型

建议 Rust runtime 输出一个 browser-specific snapshot：

```json
{
  "boot_id": "...",
  "state_seq": 12345,
  "server_time": "...",
  "workspaces": [],
  "tabs": [],
  "panes": [],
  "agents": [],
  "tasks": [],
  "bindings": []
}
```

pane view：

```json
{
  "workspace_id": "wBM",
  "tab_id": "wBM:t1",
  "pane_id": "wBM:p2",
  "cwd": "/repo",
  "project_root": "/repo",
  "terminal_title": "pi - repo",
  "focused": false,
  "agent": {
    "name": "pi",
    "kind": "pi",
    "status": "working",
    "started_at": "...",
    "last_activity_at": "..."
  },
  "target_revision": "opaque-runtime-generated-value"
}
```

### 7.3 初始 snapshot + 增量 event

禁止 Side Panel 每秒重新请求全部 workspace。

```text
open side panel
  ↓
one authoritative snapshot
  ↓
subscribe push stream
  ↓
apply incremental events
  ↓
rare reconcile on reconnect / sequence gap
```

这项工作直接复用 Streaming First / EventCache 思路。

---

## 8. Target Pinning：控制安全的核心

当前 workspace binding 和 control target 必须分开。

```text
Conversation Binding
  = 这个网页 conversation 关联哪个 workspace

Pinned Control Target
  = 下一条人工控制命令明确发给哪个 pane / agent
```

不能因为 Herdr focus 变化就悄悄改变控制目标。

### 8.1 pinned target

```json
{
  "workspace_id": "wBM",
  "pane_id": "wBM:p2",
  "target_revision": "opaque...",
  "pinned_at": "..."
}
```

`target_revision` 由 runtime 生成，可结合：

- runtime `boot_id`；
- pane identity；
- agent session identity / start identity；
- provider identity；
- relevant state sequence。

浏览器不需要理解内部组成。

### 8.2 每个 mutation 前重新验证

如果：

- pane 已关闭；
- 原 Agent 退出后新 Agent 占用了 pane；
- runtime restart 导致 target revision 失效；
- provider session 已改变；

返回：

```text
stale_target
```

要求用户重新确认，不自动 redirect 到 focused pane。

---

## 9. 四类控制模式

不要把所有“发送指令”都叫 steer。

### 9.1 Prompt / Queue

含义：给目标 Agent 提交一条新 prompt。

对应 Herdr：优先 `agent.prompt`。

结果语义：

```text
submitted
queued
rejected
uncertain
```

如果 Agent provider 自己决定排队，这不是 true steer。

### 9.2 Steer

含义：修改**当前仍在执行的 turn**。

只有 provider 明确支持并确认当前 turn steer 后，UI 才显示可用。

Codex：

```text
pane_id
  ↓
resolve Codex session
  ↓
threadId + active turnId
  ↓
turn/steer(expectedTurnId)
```

只有成功调用 provider 的真实 mid-turn primitive 才返回：

```text
steered
```

不能把 `agent.prompt` / terminal text injection 标成 `steered`。

### 9.3 Herdr Command

给高级用户手动操作 Herdr，但不要求他们切回 terminal。

推荐两层 UI：

#### Curated actions

常用安全动作：

```text
Inspect
Focus pane
Read pane tail
Prompt agent
Wait/status
Create/split pane
Close pane
...
```

#### Advanced native method

从当前 Herdr live schema 动态生成 command palette：

```text
method: pane.focus
params: {...}
risk: read / mutation / unknown
```

执行前必须：

- schema 校验；
- target 明确；
- mutation 显示确认；
- 走 Reliability Kernel；
- unknown method fail closed。

浏览器 UI 不复制一份固定的 90+ Herdr API 列表。

### 9.4 Terminal Input

这是最底层、风险最高的 escape hatch。

明确区分：

```text
Send text        # 不自动 Enter
Send input       # 文本 + Enter / shell submission
Send keys        # Ctrl-C / arrows / shortcuts
```

它是 raw terminal control，不是 Agent prompt，也不是 steer。

默认折叠在 Advanced 区域。

---

## 10. Action API

### 10.1 不走公网 MCP

Browser Control Plane 的 mutation 不经过：

```text
ChatGPT -> Cloudflare -> /mcp
```

只允许：

```text
Chrome extension
  -> Native Messaging
  -> local Unix socket
  -> Rust runtime
```

页面 JS 永远拿不到 Herdr bearer。

### 10.2 建议本地接口

只读 snapshot 可以继续复用 `/push/state` / `/push/events`，后续升级为 browser state v2。

mutation 建议单独：

```text
POST /extension/control/action
```

**只有 trusted extension IPC 可访问。**

TCP / Cloudflare / 普通 bearer 请求即使命中 route 也必须拒绝。

request：

```json
{
  "op_id": "...",
  "idempotency_key": "...",
  "target": {
    "pane_id": "wBM:p2",
    "target_revision": "..."
  },
  "action": "agent_prompt",
  "args": {
    "text": "Do not change the database schema."
  }
}
```

response：

```json
{
  "ok": true,
  "outcome": "submitted",
  "delivery_phase": "observed",
  "target": "wBM:p2"
}
```

### 10.3 统一 outcome

建议：

```text
submitted
queued
steered
interrupted
focused
completed
no_active_turn
not_steerable
unsupported_provider
session_not_resolved
stale_target
rejected
uncertain
failed
```

UI 必须准确展示 outcome，不能把 best-effort injection 描述成 true steer。

---

## 11. Rust 内部组件

建议新增：

```text
BrowserStateService
BrowserControlService
TargetResolver
ActionClassifier
HerdrActionAdapter
ProviderRegistry
  └─ CodexSteerAdapter
```

### BrowserStateService

职责：

- 从 EventCache / live Herdr snapshot 构建 browser view；
- initial snapshot；
- incremental event；
- sequence gap / reconnect reconcile；
- 不创建第二份长期 workspace truth。

### TargetResolver

职责：

- pane_id -> current pane；
- pane -> current agent identity；
- target revision 校验；
- provider capability；
- stale target fail closed。

### ActionClassifier

```text
READ
MUTATION
TERMINAL_MUTATION
PROVIDER_STEER
UNKNOWN
```

### HerdrActionAdapter

将 curated / advanced Herdr command 映射到 live native methods。

### ProviderRegistry

只有需要 provider-specific 能力时进入，例如 Codex `turn/steer`。

Herdr 本身仍然是工作现场 owner；provider adapter 只是 plugin integration。

---

## 12. Codex True Steer

### 12.1 先做能力探测

```text
provider = codex?
backend session resolvable?
active turn exists?
turn id current?
```

UI capability：

```text
Steer: available
Steer: no active turn
Steer: unsupported
Steer: session unresolved
```

### 12.2 session mapping

最理想：现有 Herdr agent/session metadata 已足够得到稳定 backend identity。

如果 herdr-mcp 无法可靠完成：

```text
pane_id -> Codex thread/session
```

才向 Herdr upstream 提最小 feature request，例如只增加稳定 provider/session metadata。

不要为了 Issue #57 把 provider-specific `turn/steer` 塞进 Herdr core。

### 12.3 race

必须使用 `expectedTurnId`：

1. UI 看见 turn A working；
2. 用户输入 steer；
3. action 到达时 turn A 可能已经结束、turn B 已开始；
4. 不能把原本给 A 的约束错误送进 B。

此时返回：

```text
no_active_turn
或 stale_turn
```

由用户重新确认。

---

## 13. Interrupt

Interrupt 和 steer 分开。

优先顺序：

1. provider / Herdr 有明确 interrupt primitive -> 使用明确 primitive；
2. 对用户已显式固定且 `working` 的 Agent pane，可以提供窄化的 `停止 (Ctrl+C)` 操作，通过 `pane.send_keys(["C-c"])` 发送终端中断；
3. 普通 Terminal Input 仍保持独立、高风险、默认 Preview-only；
4. 不把 `Ctrl-C` 冒充 provider interrupt。

Ctrl+C 停止必须复用 target fencing，投递结果不确定时禁止自动重试。真正的 provider interrupt 仍按独立 capability 处理。

---

## 14. 与 Streaming First / Long Task Observability 的整合

Control Center 不应该只显示 Agent 的 `working`。

未来 Task Journal/phase event 到位后，同一个 pane 可以展示：

```text
Implementing
Validating
Waiting external CI
Deploying
Verifying
Completed
```

以及：

```text
started_at
elapsed
last_progress_at
latest milestone
```

浏览器看到的状态成为用户长任务反馈的直接 UI，而 Web 模型继续通过同一 progress/evidence 语义向用户汇报。

这就是 Issue #57 与 roadmap “持续执行 + 长任务及时反馈”的真正结合点。

---

## 15. 浏览器性能约束

Control Center 不能把“看见所有窗口”实现成高频轮询全部状态。

必须：

- 初始 snapshot 一次；
- SSE/event 增量更新；
- reconnect/sequence gap 才 reconcile；
- pane output 只保留 bounded tail；
- hidden Side Panel / tab 不做高频 DOM 工作；
- 不在扩展 JS heap 保存完整 terminal history；
- 列表很多时使用虚拟化或只渲染展开 workspace；
- UI 状态变化 debounce/coalesce。

目标：本地开几十个 pane 时，浏览器控制面本身不成为性能瓶颈。

---

## 16. 安全模型

### 强制边界

1. extension origin 由 Chromium Native Messaging manifest 固定；
2. Native Host -> mode-0600 local Unix socket；
3. content script / ChatGPT page 不持有 Herdr bearer；
4. `/extension/control/action` local IPC only；
5. mutation 必须显式 pane target；
6. stale target fail closed；
7. mutation timeout 不盲重试；
8. advanced native method 按 live schema 校验；
9. unknown action fail closed；
10. raw terminal control 单独标高风险。

### 页面选择文本

允许：

```text
选中 ChatGPT 中一段文字
 -> context menu: Send to pinned Herdr target
```

但页面只提供 text payload；真正 target、权限、发送动作都由 extension service worker + local runtime 决定。

不拦截、不修改 ChatGPT/OpenAI 网络请求。

---

## 17. 分阶段实施

### Phase A — 现在即可做：Read-only Control Center

与当前 Rust/Streaming 性能路线并行。

内容：

- Side Panel；
- workspace/tab/pane/agent tree；
- push state 增加 tabs；
- all-pane live status；
- compact output / on-demand tail；
- explicit pane pin；
- target revision 设计；
- command composer UI；
- action type / risk badge；
- 所有 mutation 先 disabled / dry-run。

验收：

- 浏览器能看到 Herdr 当前全部 workspace / tab / pane；
- Agent 状态与 Herdr 实际一致；
- pane 创建/关闭/状态变化实时更新；
- 不依赖固定短周期全量 polling；
- pin 不随 focus 改变；
- 大量 pane 下 UI 不明显拖慢浏览器。

### Phase B — Rust Native Host production owner 后：Read controls

内容：

- Rust browser state v2；
- local-only control route；
- Inspect / Focus / Read tail；
- live Herdr method discovery；
- Advanced command palette 的 read-only method。

验收：

- Node 不新增长期 control owner；
- TCP/public path 无法调用 browser control action；
- extension restart 后能重新 snapshot。

### Phase C — Reliability Kernel：Mutation controls

内容：

- agent prompt / queue；
- pane send text/input/keys；
- interrupt；
- curated mutation commands；
- `op_id` / idempotency / delivery phase / uncertain reconcile；
- stale target fencing。

验收：

- 注入 timeout 后不会重复发送；
- Agent/pane 变化后旧 target 无法误操作新 Agent；
- browser 显示真实 delivery outcome。

### Phase D — Continuity 2.0：Durable browser control state

内容：

- durable binding；
- durable pinned target；
- handoff target 继承；
- service worker/browser restart rehydrate；
- Task Journal / phase rendering；
- continuity chain 与 control state 对齐。

验收：

- 长对话接力后仍绑定相同 workspace；
- pinned target 只有 revision 仍匹配时才恢复；
- stale target 要求重新确认；
- progress history 有界且可恢复。

### Phase E — Provider adapters：Codex true steer

内容：

- Codex capability probe；
- pane -> backend session mapping；
- threadId + active turnId；
- `turn/steer(expectedTurnId)`；
- provider-specific interrupt（如存在）；
- exact outcome rendering。

验收：

- 正在运行的同一 Codex turn 收到 steer；
- 不 interrupt/restart；
- turn race 不串到下一 turn；
- `agent.prompt` fallback 永远显示 queued/submitted，不显示 steered。

---

## 18. Issue #57 重新映射

Issue #57 原提议可以保留，但作为 Browser Control Plane 的一个 action，而不是整个产品架构。

```text
Browser Control Plane
  ├─ Observe all local Herdr panes
  ├─ Pin target
  ├─ Herdr command
  ├─ Agent prompt / queue
  ├─ Terminal input
  ├─ Interrupt
  └─ Steer
       └─ Codex true turn/steer
```

这样用户价值从：

> “网页可以 steer Codex”

升级为：

> “网页是本地 Herdr 工作现场的实时控制台；steer 是其中一种精确控制动作。”

---

## 19. 与当前 Roadmap 的建议关系

不建议立刻把本文全部并入 `docs/herdr-architecture-roadmap.md`。

先作为 `_wip` 完成 Phase A / B 设计验证；一旦确定 Browser Control Plane 成为正式产品能力，再在 roadmap 的两个位置引用：

1. `Continuity 2.0`：Browser State / Binding / Control Surface；
2. `Reliability Kernel`：browser-originated mutation / target fencing / delivery evidence。

Codex adapter 属于 Product Completion / provider integration，不扩张 public MCP contract。

---

## 20. 推荐近期开发顺序

与当前 roadmap 不冲突的顺序：

```text
当前主线：Streaming First
        │
        ├───────────────┐
        │               │
        ▼               ▼
long-task progress   Phase A Control Center
                        read-only state/UI
        │               │
        └───────┬───────┘
                ▼
Rust Native Messaging production ownership
                ▼
Phase B read controls
                ▼
Reliability Kernel
                ▼
Phase C mutation controls
                ▼
Continuity 2.0 durable state
                ▼
Phase D continuity integration
                ▼
Codex session mapping / provider adapter
                ▼
Phase E true steer
```

**不要为了 Issue #57 打断当前 Rust production ownership 和 Streaming First 主线。**

Phase A 可以并行开发，因为它直接验证产品形态，而且主要是只读状态/UI；Phase C/E 属于后续 mutation/provider 能力。

---

## 21. Acceptance Criteria 总表

最终 Browser Control Plane 完成至少满足：

- [ ] 浏览器能实时看到所有 Herdr workspace / tab / pane / agent；
- [ ] terminal-only pane 不会因为没有 Agent 而消失；
- [ ] 状态来自 runtime push，不靠浏览器高频轮询；
- [ ] 用户可以明确 pin 一个 pane；
- [ ] focus 改变不会改变 pinned target；
- [ ] stale target 无法发送 mutation；
- [ ] 可以手动执行受校验的 Herdr command；
- [ ] 可以给 Agent prompt/queue；
- [ ] 可以显式发送 terminal text/input/keys；
- [ ] interrupt 和 steer 语义分开；
- [ ] true steer 只有 provider 确认后才显示 `steered`；
- [ ] mutation 有 `op_id` / delivery evidence，不盲重试；
- [ ] 所有 browser control mutation local-only；
- [ ] ChatGPT page/content script 无 Herdr bearer；
- [ ] 不拦截 OpenAI/ChatGPT network；
- [ ] service worker / browser restart 后可安全恢复控制状态；
- [ ] 长任务阶段可以在 Control Center 中及时展示；
- [ ] 不新增第 19 个 MCP tool；
- [ ] 不要求修改 Herdr core，除非 provider session identity 确实缺失，并且只提最小 upstream dependency。
