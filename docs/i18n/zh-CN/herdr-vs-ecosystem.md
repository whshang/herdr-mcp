# 生态对比

*为什么选择 Herdr + herdr-mcp、架构差异，以及哪些场景更适合其它方案。*

当 Web AI 需要真正控制本机开发机器时，现有方案有很多种形态。这篇文章解释 Herdr + herdr-mcp 在生态中的位置、为什么采用这套架构，以及什么时候选用其它工具才是更好的选择。这是一个系统边界决策，不是功能数量比拼。

结论先行：**Herdr + herdr-mcp 最适合“Web AI 是 planner，本机是可持续、可观察、可人工接管的真实开发现场”这一目标。** Web 模型直接完成确定性小任务，把更大的工作委派给可替换的本地 Agent，并让工作现场跨会话持续存在，从而用户可以随时离开、回来并接管。

## 比较的维度

让 ChatGPT 或 Codex 操作本地开发环境的项目，最大的差异来自三个正交问题：

1. **任务从哪里开始** — ChatGPT Web、Codex CLI、桌面应用，还是任意 MCP 客户端。
2. **谁负责规划** — Web 模型直接调用确定性工具，还是委派给本地 Coding Agent。
3. **本机保存什么长期状态** — 单次 MCP 请求，还是持久 session、task、PTY、Agent、浏览器会话和恢复证据。

因为组合方式比单个工具更重要，主流项目大致归为几类架构路线。

| 路线 | 典型代表 | 主入口 | 规划/执行 | 最适合 |
| --- | --- | --- | --- | --- |
| 通用 Coding MCP Runtime | coding-tools-mcp、MCPX、DevSpace Local Artifacts | 任意 MCP 客户端 / 模型 | 客户端模型直接调确定性工具 | 给已有 AI 客户端加安全本地编程工具 |
| Web → 工作站 | AgenticGPT、gpt-webcodex、chatgpt-workspace-mcp、chatgpt-local-coder | ChatGPT Web | Web planner → 本地 runtime/worker | 从 Web/手机操作一台开发机 |
| Web → Coding Agent | codex-from-chatgpt、codex-chatgpt-bridge | ChatGPT Web | Web → Codex → 本地仓库 | 让专用 CLI 成为 coding executor |
| Codex → ChatGPT Web | codex-chatgpt-web | Codex CLI | Codex 驱动，消费 Web 模型 | 保留 Codex harness，使用 Web 模型 |
| 双 Agent 协作 | codex-with-chatgpt | ChatGPT + Codex | Web planner/reviewer ↔ Codex executor | 明确的计划–执行–复核工作流 |
| 持久工作现场控制面 | Herdr + herdr-mcp | ChatGPT / 任意 MCP 客户端 | Web planner → 确定性工具或可替换 Agent | 长期工作站 + 浏览器连续性 |

## 架构路线

### Coding MCP Runtime：模型直接驱动工具

```text
ChatGPT / Claude / Grok / Cursor
              │ MCP
              ▼
      coding-tools-mcp / MCPX
              │
       files / Git / exec
```

Runtime 保持模型中立，客户端模型决定检查、修改和执行什么。coding-tools-mcp 强调服务端强制、稳定紧凑的 file/search/patch、Git、PTY/exec，并带 workspace confinement 和有界结果；MCPX 展示持久 Remote Session 如何给工具表面带来恢复语义。当全部问题是安全的 file/Git/exec 时，这是最简单的形态——也正因如此，Herdr-MCP 不重复实现这些工具。

### 远程工作站产品

```text
ChatGPT Web → Secure MCP Tunnel / HTTPS → 本地 runtime → workspace / process / tools
```

产品表面从工具扩展到安装、隧道管理、权限、后台工作、恢复和本地生命周期。AgenticGPT（Linux 远程 worker，带 managed Jobs 和可选 Hub）和 gpt-webcodex（打包的 Windows 产品）属于这一类。它们在故障域隔离和产品化上是很好的参考，但往往把所有操作都塞进 job/task 系统。

### Codex-first 桥

```text
ChatGPT Web → MCP → Codex bridge → Codex CLI → repository
```

codex-from-chatgpt 和 codex-chatgpt-bridge 复用 Codex 自己的沙箱和 Agent 循环，代价是让 Codex 成为强制的执行跳板。反向的 codex-chatgpt-web 保留 Codex 作为用户界面、只把背后的模型换成 ChatGPT Web——当你已经把 Codex 当作入口时很有价值。

### 双 Agent 协作

```text
ChatGPT planner/reviewer ↔ Codex executor
```

codex-with-chatgpt 把规划与执行明确分离：planner 走只读 MCP bridge，控制通道只传小状态。当显式双 Agent 循环正是目标时，它是个好模型。

## Herdr 与 tmux、cmux、ACP

这三者经常被提议为 Herdr 层的更简单替代。它们解决的问题不同。

| 方案 | 主要抽象 | 最强项 | 对 herdr-mcp 的主要缺口 |
| --- | --- | --- | --- |
| tmux | session / window / pane / PTY | 成熟、轻量、SSH 友好 | 不理解 Agent 语义、项目关系、事件与恢复 |
| cmux | AI 增强桌面终端 / workspace | macOS UI、本地交互体验 | 远程 Web 控制面与跨平台 runtime 不是核心 |
| ACP | client ↔ coding agent 协议 | session、prompt、permission、事件 | 不负责 workstation、PTY、Git/进程状态和浏览器连续性 |
| Herdr | 持久 workspace / pane / agent / event runtime | 长期现场、Agent 状态、人工接管、Socket API | 需要 herdr-mcp 提供公网 MCP/OAuth 与 Web 工具 |

- **tmux** 是优秀的最低层但抽象太低：长期 Web planner 还需要项目/workspace 身份、语义化 Agent 状态、增量事件、人工接管后的安全重观察，以及浏览器绑定。在 tmux 上重建这些会逐渐得到一个 Agent-aware runtime——Herdr 已经承担了这层。
- **cmux** 是出色的本地 macOS 前端，但 herdr-mcp 针对的是“用户可能不在开发机前”，机器要数小时保持可达、可观察、可恢复。Runtime 身份和事件语义优先于桌面呈现。
- **ACP** 是未来 client↔agent 通信的自然兼容层，但控制面仍然需要 workspace、repo/worktree、PTY、process、Git state、long exec、runtime generation 与 handoff。更清晰的边界是让 Herdr 管现场，未来通过可选 Agent adapter 使用 ACP。

## 为什么 Web-planner 模型让工作保持轻量

```text
Web AI
  ├─ 直接读文件 / Git / 执行测试
  ├─ 做确定性修改
  └─ 需要时委派本地 Agent
          ↓
       Herdr worker
```

临时小任务、调研和架构讨论保持轻量；复杂开发再组合多个本地 worker。若强制所有请求都经过另一个 Coding Agent，Web 模型会变成第二个 planner 的 UI，引入额外延迟和上下文转述。

## 完整闭环是差异点

大多数 Coding MCP server 只解决下行方向：

```text
Web AI → MCP/OAuth → Edge → outbound link → herdr-mcp → files / Git / exec / Herdr Socket API
```

这对短任务够用。但当工作持续数小时、用户离开屏幕时，你需要返回路径：

```text
Herdr events → herdr-mcp → 本地 IPC / Native Messaging → browser extension → Web conversation
```

浏览器扩展在首次安装时可选，但它对无人值守长任务、页面恢复和跨会话接力是闭环所需的第二条通道。没有它，标准 MCP 无法在本地 Agent 完成后让已经静止的 Web 会话自动开启新的一轮。

## 该吸收、复用和避免什么

生态里可长期迁移的经验包括：

- **身份与恢复优先。** 把传输会话与 continuity/work identity 分离；task、edit、operation、artifact、runtime generation、browser page epoch、handoff checkpoint 都用服务端生成的稳定 ID；绝不从日志猜 ID。
- **语义化健康，而不是绿色 `/healthz`。** READY 至少需要协议握手、generation/schema 一致、真实 request/response probe，以及需要浏览器控制时页面语义可用。
- **Browser Lease / Page Epoch。** 每个受控 tab/conversation 应有显式 lease；navigation、reload、discard、extension reload、handoff 或 generation change 都应撤销旧 lease 并取消 observer、timer、listener 和 pending 操作。
- **控制面/数据面分离。** handoff 和 wake 消息只传状态、identity 和 evidence 引用；文件、diff、日志、测试结果按需读取。
- **Artifact 一等化。** 构建报告、截图、测试报告、附件和大日志应带 id、hash、size、media type、source 和有界读取，而不是反复塞进模型上下文。
- **独立故障域。** 中心服务只路由和协调；每台 workstation 在中心故障时仍独立可用；heartbeat、activity 和 analytics 保持有界。

Herdr-MCP 复用 Herdr 处理 workspace/pane/PTY、Agent 生命周期与状态、event stream、worktree、原生高级操作和人工 attach/focus/inspect。主线之外不做的包括：第二套 Agent runtime、第二个 terminal multiplexer、完整 Team/Task DAG/Lease 系统、强制 ACP 内部化、绑定单一 Coding Agent 品牌，以及通用浏览器自动化框架。Task 语义（轻量 `work_id`、scope、acceptance criteria、evidence）可辅助复杂工作，但不应成为一次临时读取或命令的门槛。

## 推荐架构

```text
                    Web AI
                      │
                MCP + OAuth
                      │
                Stable Edge
                      │
               outbound WSS
                      │
              Rust herdr-mcp
             /        │        \
            /         │         \
       files/Git     exec       Herdr
                                │
                         workspace / pane
                         agent / event / PTY
                                │
                       Native Messaging
                                │
                        Browser continuity
```

职责保持单一：Web AI 是 planner；herdr-mcp 是安全远程控制面和连续性层；Herdr 是持久运行事实源；本地 Agent 是可替换 worker；浏览器扩展是返回通道而非推理系统。传输层（Secure MCP Tunnel、Cloudflare Edge 或其它）保持可替换，不拥有 canonical workstation state。

## 什么时候选择其它方案

- 只需要本地终端复用 → 直接用 tmux；
- 追求 Mac 桌面终端体验 → 优先 cmux；
- 需要 client↔coding-agent 互通 → 优先 ACP；
- 只需要独立的 file/Git/exec MCP → coding-tools-mcp 更简单；
- 需要 Linux 远程 Worker/Hub → 评估 AgenticGPT；
- 需要 Windows 开箱即用的 ChatGPT 桌面产品 → gpt-webcodex；
- 已经喜欢 Codex 界面、只换 Web 模型推理 → 看 codex-chatgpt-web。

Herdr + herdr-mcp 最适合的场景是：**继续使用最强 Web 模型作为主要思考者，同时让它长期、可靠、可观察地操作真实本机开发现场，并且人可以随时离开、回来和接管。**

## 参考项目

本轮重点核查的项目：

- https://github.com/xyTom/coding-tools-mcp
- https://github.com/opentokenz/mcpx
- https://github.com/cooky-dance/devspace-local-artifacts
- https://github.com/slhaf/AgenticGPT
- https://github.com/3169657175/gpt-webcodex
- https://github.com/miuuyy/codex-chatgpt-web
- https://github.com/XiaoDuoYa/codex-with-chatgpt
- https://github.com/dxawdc/chatgpt-workspace-mcp
- https://github.com/alexcodeplace/chatgpt-mcp
- https://github.com/posavr/chatgpt-local-coder
- https://github.com/joseanu/codex-from-chatgpt
- https://github.com/Dalomeve/codex-chatgpt-bridge
- https://github.com/openai/tunnel-client

这些项目演化很快，具体实现细节请对照各自最新发行版。涉及 ChatGPT plan、Developer Mode、Secure MCP Tunnel 等平台可用性时，以 OpenAI 当前官方文档和工作区 policy 为准。
