# Web AI × 本地开发：架构路线与 Herdr-MCP 的选择

这不是一篇竞品排名。这里研究的是同一个机会出现后，不同项目为什么选择了不同架构、各自做对了什么，以及什么场景最适合哪一种。

## 共同前提：Web AI 已经是一块可利用的推理资源

ChatGPT、Claude 等 Web AI 的订阅用户已经拥有很强的模型能力和一套独立于 API / 本地 Coding Agent 的额度体系。不同账号、模型和时期的具体限额会变化，因此本文不把任何固定倍率当作平台承诺；但对重度开发用户，一个现实机会已经出现：**网页订阅里的高质量推理能力可以很充裕，而 API 或本地 Coding Agent 属于另一套额度、计费和限制。**

MCP 改变了工程边界。只要 Web AI Host 能通过 MCP 调用 HTTP 服务，网页里的模型就可以按需读取项目、执行工具、调用本地 Agent，而不需要把整个仓库预先塞进对话。OpenAI Secure MCP Tunnel 又把私有 MCP 接入标准化为 outbound-only 模式，本机不必开放公网入站端口。

> **真正的问题不是“怎样再做一个 Coding Agent”，而是“怎样把用户已经购买的 Web AI 推理能力，变成可以安全、持续地操作真实计算环境的生产力”。**

## MECE：先回答四个正交问题

1. **对话入口在哪里？** Web Chat，还是 Codex / 本地 Agent？
2. **Web 模型拿到什么执行能力？** 直接工具、本地 Agent，还是远程设备路由？
3. **谁保存长期工作状态？** 简单 MCP Runtime、持久开发 Runtime、Coding Agent harness，还是 Herdr workspace？
4. **本机能否反向影响已经停下来的 Web 对话？** 没有、只通知人，还是存在浏览器侧通道？

由此可以把主流方案归纳为五条路线，而不是简单按“功能多少”排名：

| 路线 | 主入口 | 典型代表 | 最突出的价值 |
| --- | --- | --- | --- |
| A. Web AI → 直接 Coding MCP | Web Chat | coding-tools-mcp、DevSpace | 最短路径、安全、模型中立 |
| B. Web AI → 持久开发 Runtime | Web Chat | MCPX、gpt-webcodex | Session / Task / Artifact / 恢复与产品化 |
| C. Web AI → 单机 Worker / 多机 Hub | Web Chat | AgenticGPT | standalone-first、多设备故障隔离 |
| D. Codex → ChatGPT Web | Codex | codex-chatgpt-web、codex-with-chatgpt | 保留 Codex harness，消费 Web 模型能力 |
| E. Web AI → 持久工作现场 + 双向连续性 | Web Chat | Herdr-MCP | workspace / Agent 长期存在，Web 与本地闭环 |

## 各路线与代表项目：详细调研

## 1. coding-tools-mcp

**定位**：model-neutral coding runtime。

它提供固定的文件、搜索、结构化 patch、Git、PTY/exec 和 runtime 工具，通过 workspace confinement、permission mode、输出上限和原子 patch 把安全边界放在服务端。支持 stdio 和 Streamable HTTP，可接 Claude Desktop、Claude Code、Codex、Cursor、Cline、VS Code、Windsurf、Gemini CLI，也可以通过 Tunnel 给 ChatGPT/Grok 使用。

**使用流程**：启动一个绑定 workspace 的 MCP Server → 客户端连接 → 客户端模型直接调用工具。

**优势**：

- 工具 Runtime 职责单一；
- Client / Model 中立；
- workspace、权限、结果预算和 patch 原子性设计成熟；
- 适合被其它 Agent 产品嵌入，而不必重复造文件/Git/exec 工具。

**边界**：它不试图管理浏览器会话、长期 Agent workspace 或 workstation → Web 的反向连续性。

**对 Herdr 的启发**：继续保持确定性工具的稳定、紧凑、可验证；复杂生命周期应由 runtime/control plane 承担，不应靠 prompt 约定。

## 2. MCPX

**定位**：带持久 Remote Session 的本地 MCP Runtime / Gateway。

与简单的 Coding MCP 相比，MCPX 明确区分传输层 `Mcp-Session-Id` 和业务层 `remote_session_id`。Workspace、Edit、Execution Task、Plan、Operation、Artifact、Skill、上游 MCP 与 Observation 都围绕持久 Session 管理，并使用 SQLite 保存状态。

**使用流程**：注册 Workspace → Client 连接 Runtime → 创建/恢复 Remote Session → 在 Session 内读取、编辑、执行、计划、观察和恢复。

**优势**：

- 把“断开连接”和“业务会话结束”分开；
- SHA/Edit ID/Task ID/Artifact ID 等 identity 很明确；
- 长命令、计划、批量 Operation 和 Artifact 都有恢复语义；
- Skill 与上游 MCP 也进入统一 Runtime 边界。

**边界**：这是 MCP Runtime 自身的 session/orchestration 体系，并不提供 Herdr 式可见终端现场和 browser continuity。

**对 Herdr 的启发**：`conversation/session transport identity` 与 `continuity/work identity` 应严格分层；恢复不能靠从日志猜 ID。

## 3. DevSpace Local Artifacts

**定位**：面向 ChatGPT/Claude 的自托管本地 Workspace MCP，尤其强调附件和二进制 Artifact。

它的特色不是重新定义 Agent，而是解决“聊天里的文件怎样安全进入本地 workspace”。Windows 版本补齐 native file artifact 保存和 Base64 fallback，并保留 traversal、overwrite、symlink/junction、大小和完整性检查。

**使用流程**：初始化允许根目录 → 启动本地服务 → 通过本地 MCP 或 HTTPS Tunnel 接入 ChatGPT → `open_workspace` → 按需调用文件/Artifact 工具。

**优势**：

- 对 ChatGPT 原生附件能力边界解释得清楚；
- 文件输入失败时有明确 fallback；
- 适合文档、图片、构建产物等“聊天 ↔ 本地文件”工作流。

**对 Herdr 的启发**：Artifact 应成为有 identity、大小、hash、来源和权限边界的一等对象，避免把大文件塞进普通 tool result。

## 4. AgenticGPT

**定位**：Linux 远程 Agent/Worker Runtime，Standalone Secure MCP Tunnel 优先，可选集中 Rust Hub。

它提供三种运行方式：每台机器独立的 Secure MCP Tunnel、集中式 Hub + Local Agents、owner-only Local Unix MCP。核心抽象是 managed Job、path/command policy、confirmation、Skill、downstream MCP 和可选 tmux workspace。

**使用流程**：本机配置 Agent → 选择 Standalone/Hub/Local → ChatGPT 连接 → 调用 Agent tools → 长操作进入 managed Jobs。

**优势**：

- 多机部署边界明确；
- Standalone 模式故障域小，不强依赖中心服务器；
- Job、confirmation、bounded MCP result、取消语义成熟；
- Hub 模式适合统一入口、历史和通知。

**边界**：其产品中心是远程 Worker/Job；Herdr 更强调人可观察、可接管的长期开发现场以及任意 Agent 的并行编排。

**对 Herdr 的启发**：多设备场景应优先独立故障域；中心控制面只保存必要路由/状态，避免把每次 heartbeat 都变成中心依赖。

## 5. gpt-webcodex

**定位**：Windows 一体化 ChatGPT Coding 桌面助手，内置 Coding Tools MCP Runtime。

它把 OpenAI Tunnel、本地 Runtime、ChatGPT 接入向导、worktree 隔离、后台任务、heartbeat、恢复、通知、诊断与构建验证包装成普通用户可安装的桌面产品。

**使用流程**：安装桌面应用 → 选择 workspace → 建立 Tunnel/Connector → 在 ChatGPT 中工作 → 桌面中心负责 Runtime、Task、Worktree 和诊断。

**优势**：

- 产品化程度高；
- 页面、Tunnel、Runtime 分故障层；
- runtime/schema identity、worktree、后台 operation、heartbeat、恢复都进入 UI；
- Windows 用户几乎不需要理解底层 MCP。

**边界**：更像一个围绕 ChatGPT 的单机 Coding 产品；Herdr 需要同时容纳多个 workspace、pane、shell、server 和不同 Agent。

**对 Herdr 的启发**：安装、doctor、升级、Runtime identity 和恢复证据必须成为正式产品能力，而不是开发者 runbook。

## 6. codex-chatgpt-web

**定位**：让 Codex 原生客户端使用 ChatGPT Web 作为模型后端。

它保留 Codex 的任务入口和工具循环，本地 Responses-compatible server 把请求映射到 launcher 管理的浏览器/ChatGPT 页面。完整模式还允许 ChatGPT 通过 MCP 调用 Codex 暴露的外层工具。

**关键设计**：

- 每个任务拥有 browser tab lease；
- 并行任务分离 tab；
- tool authority 绑定到当前 turn；
- browser DOM 映射为流式响应；
- context/compaction、launcher restart、upgrade/drain 都有专门状态；
- DEV browser profile 与生产 profile 严格隔离。

**优势**：它深入处理了 browser surface 的现实问题：DOM 会变化、tab 会失活、流可能中断、浏览器健康不等于页面语义健康。

**风险**：把 Web UI 适配成模型 API 的兼容层，本身比普通 MCP 工具更容易受 DOM、页面生命周期和产品 UI 变化影响。

**对 Herdr 的启发**：Browser Lease、Page Epoch、semantic liveness、drain、DEV/production identity isolation 都非常值得吸收；无需复制完整 Responses API emulation。

## 7. codex-with-chatgpt

**定位**：ChatGPT 负责规划和审查，Codex 负责执行的双 Agent 协作桥。

其 MCP bridge 刻意保持 read-only，ChatGPT 不直接执行 shell 或写文件；执行权交给 Codex。双方用小型控制消息和状态机协作，较大的 diff、文件和日志由 ChatGPT 通过 MCP 主动读取。

**关键设计**：

- `PLAN → EXECUTING → EXECUTED → REVIEW` 的显式状态；
- control plane 只传递小消息，不传大块代码/日志；
- conversation handoff 保存目标、进度、当前状态、问题和下一步；
- 新对话重新通过 MCP 读取当前代码事实。

**优势**：职责清楚，handoff 不依赖把整个历史复制到新会话。

**边界**：双 Agent 是产品前提；对于只改一行配置、查 Git 状态等任务会多一层执行代理。

**对 Herdr 的启发**：handoff 应保存 canonical checkpoint/reference，新对话重新读取 live facts；控制面不要承载大块数据。

## 补充调研：值得观察的相近项目

## 8. chatgpt-workspace-mcp

路线非常纯粹：ChatGPT Web 通过 OpenAI Secure MCP Tunnel 操作经过批准的本地项目。它支持查看、搜索、修改、白名单任务，以及 Git 项目的本地 commit 和受控 push，并刻意不开放任意 Shell。

它代表一种重要取舍：**减少工具自由度，换取更容易理解的安全边界。** 对个人远程 coding，未必需要通用 shell 才能覆盖高频工作。

## 9. chatgpt-mcp

这是 Linux oriented 的 stateless MCP adapter。能力族默认 opt-in，文件、shell、service、browser、screen/input 等权限由本地配置决定。官方推荐路径也是 OpenAI Secure MCP Tunnel，并强调 direct executable + argv，避免隐式 `sh -c`。

它代表“Runtime 尽量薄、policy 尽量明确”的路线。与 MCPX 的持久 Session 相比，这是另一端的设计选择。

## 10. chatgpt-local-coder

这是偏直接的 ChatGPT Web → 本地 Coding MCP 路线，提供大量 file/shell/git/background process 工具，并支持 Secure MCP Tunnel 与 session recovery。

其优势是功能直接；值得注意的是，工具数量多和默认较宽的机器访问也意味着更大的 public surface 和权限审计成本。它适合作为“功能覆盖上限”的参考，而不适合作为 Herdr public tool catalog 的目标。

## 11. codex-from-chatgpt / codex-chatgpt-bridge

两者都属于 ChatGPT → Codex bridge。ChatGPT 的工具调用启动本地 Codex turn，Codex 才是实际 coding executor。

这类方案的优势是充分复用 Codex 自己的 sandbox、approval、tool loop 和 coding context；缺点是 Web planner 无法像 Herdr 一样直接在“确定性工具”和“委派 Agent”之间自由选择。

## 12. OpenAI tunnel-client

它不是 Coding Agent，而是这一生态里越来越重要的基础设施层：customer-run agent 从本机主动连接 OpenAI control plane，long-poll 接收针对 tunnel 的命令，转发给本地 MCP，再把结果返回。这样本地服务不需要开放公网入站端口。

对所有 ChatGPT → 本地 MCP 项目而言，Secure MCP Tunnel 正逐渐成为应单独评估的 transport，而不是和 Runtime、Agent、Browser Control Plane 混为一层。

## 关键维度横向比较

| 项目 | ChatGPT Web 入口 | 任意 MCP Client | 本地直接工具 | 固定 Coding Agent | 持久任务/会话 | Browser continuity | 多机/Hub | 产品重点 |
| --- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | --- |
| coding-tools-mcp | ✓ | ✓ | ✓ | — | PTY session | — | — | 安全 Coding Runtime |
| MCPX | ✓ | ✓ | ✓ | — | **强** | — | — | Remote Session / Runtime Gateway |
| DevSpace Local Artifacts | ✓ | ✓ | ✓ | — | 基础 | — | — | Workspace + Artifact |
| gpt-webcodex | ✓ | 主要面向 ChatGPT | ✓ | workflow 可选 | **强** | 页面连接管理 | — | Windows 一体化产品 |
| codex-chatgpt-web | 间接 | Codex 为主 | Codex tools | **Codex** | **强** | **核心能力** | — | Codex 使用 ChatGPT Web 模型 |
| codex-with-chatgpt | ✓ | MCP bridge | ChatGPT 只读 | **Codex** | handoff | 对话 handoff | — | 双 Agent 协作 |
| AgenticGPT | ✓ | ✓ | ✓ | 可替换 | **强** | — | **强** | Tunnel/Hub worker |
| Herdr + herdr-mcp | ✓ | MCP control plane | ✓ | **可替换/可不启动** | **workspace/PTY/agent** | **双向** | 规划中 | 持久 workstation control plane |

## 使用流程的本质差异

### 只想“让 ChatGPT 能改代码”

优先考虑 `coding-tools-mcp`、`chatgpt-workspace-mcp` 或类似薄 Runtime。它们部署和心智模型最简单。

### 想让多个 AI Client 共用同一套本地能力

`coding-tools-mcp` 和 `MCPX` 更自然。前者强调固定、安全、轻量的工具；后者强调跨连接恢复和 Runtime 内 orchestration。

### 想从手机/Web 长时间控制 Linux 机器

`AgenticGPT` 的 Standalone Tunnel 和 Hub 模式值得优先比较。它对 Job、policy、confirmation 和多机故障域的处理很完整。

### 想要 Windows 开箱即用产品

`gpt-webcodex` 的安装器、管理中心、通知、诊断和 worktree 体验更贴近普通桌面软件。

### 已经喜欢 Codex CLI，只想换用 ChatGPT Web 模型

`codex-chatgpt-web` 的方向最匹配。这里 Codex 仍是用户入口，ChatGPT Web 更像 inference surface。

### 希望 ChatGPT 做架构师，Codex 专职写代码

`codex-with-chatgpt` 或 ChatGPT → Codex bridge 更匹配。这是明确的双 Agent 工作流。

### 希望 Web 模型既能直接做小修改，也能编排多个 Agent，并在离开电脑后继续

这是 Herdr + herdr-mcp 最有区分度的场景。Web planner 可以直接调用 file/Git/exec，也可以把独立任务派给 Pi、OpenCode、Grok 等 worker；Herdr 保存真实 workspace/pane/PTY/Agent 现场；browser extension 再把本机事件接回具体 Web conversation。

## Herdr-MCP 为什么选择另一种组合

前面的项目已经证明 file/Git/exec、持久 Session、managed Job、桌面产品和 Codex-first browser bridge 都是成立的路线。Herdr-MCP 选择的用户入口不同：**用户本来就在 Web Chat 中思考和协作，本地工作现场需要比单个 conversation 活得更久。**

```text
Web AI / ChatGPT
      │ MCP
      ▼
herdr-mcp
  ├─ deterministic files / Git / exec
  ├─ identity / health / recovery
  └─ optional agent delegation
      │
      ▼
Herdr workstation
  ├─ workspace / pane / PTY
  ├─ process / agent lifecycle
  └─ event stream

optional return path: Herdr events → browser extension → Web conversation
```

Secure MCP Tunnel、Cloudflare Edge 或其它 transport 都可以是连接方式，不应成为 workstation canonical state。

## 最值得吸收的设计

### P0：持久 identity 与恢复

来自 MCPX、gpt-webcodex、codex-with-chatgpt：

- transport session 与业务 continuity identity 分离；
- Task/Edit/Operation/Handoff 都使用服务端生成的稳定 ID；
- 新 Web conversation 从 checkpoint 恢复，然后重新读取 live facts；
- runtime generation/schema/page epoch 都进入证据链。

### P0：Semantic Health

来自 gpt-webcodex、codex-chatgpt-web：

进程存在、socket 存在或 `/healthz` 200 都不足以证明系统可工作。READY 至少需要 runtime handshake、generation/schema 一致、真实 request/response probe，以及 browser surface 在需要时语义可用。

### P0：Browser Lease / Page Epoch

来自 codex-chatgpt-web：

每个受控 tab/conversation 应有显式 lease。reload、navigation、discard、extension reload、handoff、generation change 都应撤销旧 lease，并统一取消 observer、timer、listener 和 pending operation。

### P0：Control Plane / Data Plane 分离

来自 codex-with-chatgpt：

handoff、wake、agent message 只传状态、identity 和 evidence reference；文件、diff、日志和测试结果通过 Herdr data plane 按需读取。

### P1：Artifact 一等化

来自 DevSpace、MCPX：

构建报告、截图、测试报告、附件和大日志应拥有 artifact ID、hash、size、media type、source 和 bounded read，不应反复塞入模型上下文。

### P1：独立故障域与中心请求预算

来自 AgenticGPT：

多设备优先保证每台 workstation 独立运行；中心服务负责路由和必要协调。heartbeat、recent activity 和 analytics 必须有界，中心不可成为本机执行的单点依赖。

### P1：Release/UAT 是产品契约

来自 coding-tools-mcp、codex-chatgpt-web、gpt-webcodex：

协议测试之外，还应固定真实账号 UAT：Store/DEV extension、Tunnel、长对话、reload、runtime restart、generation upgrade、handoff、active-task drain、child-process cleanup。

## 哪些能力保持在各自最合适的层

调研也帮助明确了边界：稳定且紧凑的 public tool catalog 继续由 herdr-mcp 提供；简单 Git/文件操作不强制创建 Task；Coding Agent 保持为可替换 worker；Web DOM 只作为页面投影而不是本机事实源；Herdr 继续承担 terminal/workspace runtime；Tunnel/Edge 负责连接而不拥有长期工作 identity。

这不是为了和其它项目刻意不同，而是避免在 Herdr 已经提供成熟能力的层上重复建设。

## Herdr-MCP 的独特位置

从本轮项目看，单独的 file/Git/exec MCP 已经高度同质化；Secure MCP Tunnel 也正在把“从 ChatGPT 安全访问本机”逐步基础设施化。长期差异更可能来自以下组合：

- Web AI 保持主 planner；
- 本机拥有持久、可见、可人工接管的 workspace/PTY/Agent 现场；
- 小任务无需启动 Agent，复杂任务可并行委派任意 worker；
- browser ↔ workstation 是双向连续性，而非只有 MCP request 下行；
- runtime、browser、task、conversation 都有稳定 identity 和可验证恢复；
- transport、模型和 Agent 品牌均可替换。

这也是 Herdr-MCP 后续架构演进最值得守住的边界。

## 参考项目

本轮重点核查：

- https://github.com/xyTom/coding-tools-mcp
- https://github.com/opentokenz/mcpx
- https://github.com/cooky-dance/devspace-local-artifacts
- https://github.com/slhaf/AgenticGPT
- https://github.com/3169657175/gpt-webcodex
- https://github.com/miuuyy/codex-chatgpt-web
- https://github.com/XiaoDuoYa/codex-with-chatgpt

补充对照：

- https://github.com/dxawdc/chatgpt-workspace-mcp
- https://github.com/alexcodeplace/chatgpt-mcp
- https://github.com/posavr/chatgpt-local-coder
- https://github.com/joseanu/codex-from-chatgpt
- https://github.com/Dalomeve/codex-chatgpt-bridge
- https://github.com/openai/tunnel-client

本文只把这些项目作为架构和产品路线样本；涉及 ChatGPT plan、Developer Mode、Secure MCP Tunnel 等平台可用性时，应以 OpenAI 当前官方文档和 Workspace policy 为准。

## 为什么 Herdr-MCP 仍值得重新造这个轮子

真正不值得重造的是文件读取、Git、Shell、OAuth 和 HTTPS Tunnel。Herdr-MCP 要补的是这些能力组合后仍然存在的**长期工作连续性**：一个 ChatGPT turn 结束后，server、shell、Agent 和 worktree 仍在；某个 Agent 可以独立工作几十分钟；当前 conversation 可以达到上下文上限并接力；新的 conversation 重新读取同一个 live worksite，而不是重建现场。

如果从普通 MCP Runtime 开始补齐这些能力，最终需要逐渐增加长期 PTY、workspace identity、Agent lifecycle、event stream、人工 attach/takeover、并行现场和恢复逻辑。Herdr 已经原生提供这些能力，所以选择 Herdr 是复用一个真实开发工作台，而不是为了差异化再造一套任务系统。

Herdr-MCP 因此形成了一个特别实用的组合：确定性小任务由 Web AI 直接使用 file / Git / exec；复杂或并行工作再委派任意本地 Agent；workspace、pane、PTY、process、Agent 和 worktree 独立于单个 Web conversation 持续存在；人也可以随时进入同一个现场接管。

## 没有浏览器扩展行不行？

**行。** 没有扩展，ChatGPT → MCP → herdr-mcp → Herdr 已经可以完成文件、Git、Shell、图片、Agent、长命令、多 workspace 和多设备访问。浏览器扩展不应该成为第一次连接的硬依赖。

但 MCP 的主方向是 model turn → tool call → result。若 ChatGPT 启动一个本地 Agent 后当前 turn 已结束，二十分钟后 Agent 才完成，MCP Server 可以保存状态，却不能普遍假设自己有权让已经静止的 Web Chat 自动产生下一轮响应。MCP Runtime 也看不到 conversation URL、streaming 状态、SPA navigation、reload、tab discard、composer 状态、DOM 卡顿和 handoff 是否成功。

因此长期无人值守协作需要第二条 **local/page → Web** 通道。可选技术包括人工或桌面通知、OS Accessibility / AppleScript、Playwright / CDP、Electron 嵌入、Userscript 和 Browser Extension。

对 Codex-first 产品，embedded browser / Playwright 很合理，因为它们需要主动打开并驾驶 ChatGPT。对 Herdr-MCP，用户本来就在真实 ChatGPT 页面，因此 Browser Extension 的位置更合适：它直接使用现有登录态，只补 page observation、workspace binding、local event → Web wakeup、排队下一轮、reload recovery、长对话 handoff 和重新绑定，不需要重新拥有整个浏览器。

> **对第一次把 Web AI 接到本机，浏览器扩展是锦上添花；对无人值守长任务、页面恢复和跨 conversation 连续工作，它是闭环所需的第二条通道。**

## 最终如何选择

| 需求 | 更自然的选择 |
| --- | --- |
| 任意 MCP Client 只需要安全 file / Git / exec | coding-tools-mcp 类 Runtime |
| 需要完整持久 Session / Task / Artifact Runtime | MCPX 类 Runtime |
| Windows 普通用户需要开箱即用桌面产品 | gpt-webcodex |
| 多台 Linux Worker，需要独立故障域和可选集中 Hub | AgenticGPT |
| 已经以 Codex 为主要入口，只想利用 ChatGPT Web 模型 | codex-chatgpt-web / codex-with-chatgpt |
| 以 Web Chat 为主要开发入口，需要长期 workspace、并行 Agent、人工接管和 conversation continuity | **Herdr-MCP** |

Herdr-MCP 最适合的并不是“让 ChatGPT 改一个仓库”这么窄的场景，而是：**继续使用 Web AI 作为主要思考和协作入口，同时让它长期、可靠、可观察地操作真实开发现场；本地可以同时存在多个 workspace、终端、服务和任意 Coding Agent；人可以离开、回来、接管；conversation 可以接力；同一个入口还可以进一步发现和路由多台工作站。**

> **MCP 让 Web AI 有了操作本机的双手。Herdr 给这些双手一个持续存在的工作现场。浏览器扩展再把本地变化接回 Web 对话。**
