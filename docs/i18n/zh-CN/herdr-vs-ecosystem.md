# 为什么选择 Herdr + herdr-mcp：与 tmux、cmux、ACP 及同类 MCP 的架构比较

这篇文档回答一个长期问题：当 Web AI 需要真正控制本地开发环境时，为什么 herdr-mcp 继续选择 Herdr 作为持久运行现场，而不是改成 tmux、cmux、ACP，或者直接复制 coding-tools-mcp、AgenticGPT、gpt-webcodex 的产品形态。

结论先行：**Herdr + herdr-mcp 当前最适合“Web AI 是 planner，本机是可持续、可观察、可接管的真实开发现场”这一目标。** 这个结论不意味着其它项目能力弱；它们分别在终端复用、桌面体验、Agent 协议、安全工具 Runtime、远程 Worker 和 Windows 产品化上有明显优势。关键在于系统边界是否匹配。

## 评判标准

herdr-mcp 的目标不是再做一个 Coding Agent。评判后端和同类方案时，重点看六件事：

1. Web AI 能否直接获得真实文件、Git、Shell 和运行状态；
2. 数小时任务中 workspace、PTY、Agent、进程能否持续存在；
3. Web 对话结束后，本机状态变化能否反向推动网页恢复工作；
4. mutation 超时后能否判断动作是否已经发生，避免重复执行；
5. 人是否可以随时看到现场、进入终端、修改方向；
6. 系统是否保持 Agent 中立，不把 Claude、Codex、Pi、OpenCode、Grok、Droid 等某一个 CLI 变成必要依赖。

这六项组合起来，才是 herdr-mcp 所说的“远程控制面”。

## Herdr、tmux、cmux、ACP 分别处在哪一层

| 方案 | 主要抽象 | 最强项 | 对 herdr-mcp 的主要缺口 |
| --- | --- | --- | --- |
| tmux | session / window / pane / PTY | 极成熟、轻量、SSH 友好 | 不理解 Agent 语义、项目关系、事件与恢复策略 |
| cmux | AI 增强桌面终端 / workspace | macOS UI、终端与浏览器组合、交互体验 | 更偏桌面产品，远程 Web 控制面与跨平台部署不是核心 |
| ACP | client ↔ coding agent 协议 | session、prompt、permission、结构化 Agent 通信 | 不负责真实 workstation、长期 PTY、Git/进程现场和 browser continuity |
| Herdr | 持久 workspace / pane / agent / event runtime | 长期开发现场、Agent 状态、人工接管、Socket API | 本身不负责公网 MCP/OAuth，也不直接提供 Web AI 的文件/Git 安全工具 |

### tmux：优秀的最低层，但抽象太低

tmux 非常适合长期 shell 和远程 SSH。若需求只是创建 pane、发送按键、抓取输出，tmux 已经足够。

但 Web AI 编排长期开发时还需要知道 pane 属于哪个项目和 workspace、里面是不是 Agent、Agent 当前 working/idle/blocked/done、哪些状态变化发生在上一个观察 cursor 之后，以及人接管之后如何重新观察。这些语义都可以在 tmux 上重新开发，但做完后会逐渐得到一个新的 Agent-aware runtime。Herdr 已经承担了这层职责。

### cmux：更好的桌面体验，但不是远程事实源

cmux 对本地 AI 编程体验很有吸引力：终端、workspace、通知、浏览器等界面组合得更紧密。对于主要坐在 Mac 前工作的用户，它可以比传统终端更顺手。

herdr-mcp 的核心问题却发生在“人不一定坐在开发机前”的场景：

```text
Web Chat / 手机 / 平板
        ↓
稳定公网入口
        ↓
私有工作站
        ↓
数小时存在的开发现场
```

这里首先需要稳定 runtime identity、远程观察、事件和恢复；GUI 是第二层问题。cmux 可以作为优秀的人类前端，但不适合因此替换 Herdr 作为 herdr-mcp 的运行事实层。

### ACP：适合 Agent 接入，不适合作为 workstation 控制面

ACP 很适合统一不同 coding agent 的交互：客户端创建 session、发送 prompt、处理 permission、接收结构化事件。长期看，ACP 可能是很好的 Agent compatibility layer。

但 herdr-mcp 还必须处理 workspace、repo/worktree、pane/PTY、process、Git state、long exec、runtime generation、browser conversation binding 与 handoff/wakeup。如果把 ACP 放到核心，就仍需另外建设 workstation runtime。更自然的边界是：Herdr 管现场；未来 Agent 若原生支持 ACP，可以由 Herdr 或 adapter 使用 ACP，而 herdr-mcp 公共控制面无需因此改写。

## 为什么 Herdr 更匹配 Web planner 模型

herdr-mcp 的设计原则是 **planner 留在 Web**。

```text
Web AI
  ├─ 理解需求
  ├─ 讨论方案
  ├─ 直接读文件 / Git / 执行测试
  ├─ 决定是否修改
  └─ 需要时委派本地 Agent
          ↓
       Herdr worker
```

这样临时小任务不需要先启动本地 Agent；调研和讨论也不必人为转换成“Agent task”。复杂开发才使用并行 worker。

这使系统保持很宽的任务分布：

- **临时小任务**：查一个配置、改一行、跑一次测试；
- **调研任务**：检查源码、运行实验、比较结果；
- **讨论任务**：基于 live Git/runtime 事实讨论架构，不发生 mutation；
- **复杂开发任务**：Web planner 自己做确定性部分，把独立调查、实现或审查交给多个本地 Agent。

若强制所有任务都经过一个本地 Coding Agent，Web Chat 会变成给另一个 planner 发需求的 UI，增加上下文、延迟和语义转述。

## herdr-mcp 与 coding-tools-mcp

coding-tools-mcp 是优秀的确定性 Coding Tools Runtime。它把 file/search/patch、Git、PTY/exec 做成固定 MCP 工具，并强调 workspace confinement、permission mode、结果压缩和可复现 benchmark。

最值得 herdr-mcp 学习的内容包括：稳定 public tool catalog、patch baseline 与原子 mutation、bounded output、上下文体积控制、deterministic dogfood benchmark、由服务端真正执行的安全边界。

两者边界不同：coding-tools-mcp 可以独立给任何 MCP 客户端“安全的编程双手”；herdr-mcp 除了这些双手，还要知道长期 Herdr 现场，并维持公网 Edge、workstation routing、runtime A/B 与浏览器反向连续性。

如果 herdr-mcp 只做 file/git/exec，那么 coding-tools-mcp 会是更简单的选择。Herdr 和 continuity 才构成额外价值。

## herdr-mcp 与 AgenticGPT

AgenticGPT 已覆盖很多远程 Worker 能力：Secure MCP Tunnel、可选集中 Hub、managed jobs、命令策略、path policy、confirmation、downstream MCP、tmux workspace 等。

它证明了“本机 Worker + Tunnel/Hub + Job”是一条有效路线，也提醒 herdr-mcp 不要重复建设通用 job platform。

herdr-mcp 的差异在于：Herdr workspace 是长期开发现场；Web planner 可以直接操作确定性工具，也可以自由选择是否派 Agent；公网 Connector 与本机 runtime generation 解耦；浏览器扩展提供 workstation → Web 的反向 continuity；重点是持续开发会话，而不是把所有执行都封装成 Job。

## herdr-mcp 与 gpt-webcodex

gpt-webcodex 把 Coding Tools MCP 包装成 Windows 一体化桌面产品，并加入 worktree 隔离、长任务、heartbeat、任务恢复、系统通知、Runtime/Schema identity 等能力。

它最值得吸收的是产品体验：安装后直接可用、Runtime 生命周期明确、长任务可恢复、worktree 降低主 checkout 污染风险、完成和失败有桌面通知。

它的默认路径更接近“一个 Windows 桌面 Coding Agent 管一个任务”。herdr-mcp 需要保持更通用：一台工作站可以同时有多个项目、多个 pane、普通 shell、开发服务器和任意 Agent，Web Chat 也可以只做讨论或调研而不创建正式任务。

因此 herdr-mcp 应吸收生命周期和证据设计，而不把所有交互都强制升级成 Task Center。

## 完整闭环为什么重要

Herdr 与 herdr-mcp 组合后的关键能力不是某一个 tool，而是两个方向的链路同时存在。

### 下行控制

```text
Web AI
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated routing
herdr-link
  ↓
Rust herdr-mcp runtime
  ↓
files / Git / exec / Herdr Socket API
```

### 上行连续性

```text
Herdr events
   ↓
herdr-mcp runtime
   ↓ local IPC / Native Messaging
browser extension
   ↓
当前网页会话
```

很多 MCP 工具只解决第一条链。对于十分钟任务已经够用；对于持续几个小时、人在中途离开的任务，第二条链决定了系统是否真正闭环。

## 为什么不把任务管理做得更重

研究 DSH、Luvus 等系统后，很容易产生“再加 Team、Task DAG、Lease、Mailbox、Workflow”的冲动。Herdr-MCP 当前刻意不这么做。

Web Chat 本身就是强 planner。herdr-mcp 更适合提供事实和轻量工作上下文，例如 `work_id`、scope、acceptance criteria、evidence、operation state。这些信息帮助恢复、观察和验收；它们不应该要求用户先创建 project/task/worker 才能执行一次简单操作。

> 任务语义可以帮助复杂工作，但不能成为所有工作的门槛。

这也使 Herdr-MCP 对 Agent 品牌保持中立。Claude、Codex、Pi、OpenCode、Grok、Droid 或以后出现的工具都只是 worker；没有它们时，Web AI 仍能依靠 files/Git/exec 完成大量工作。

## 哪些能力应该继续吸收

### 应该吸收

- coding-tools-mcp 的安全边界、bounded result、benchmark 方法；
- gpt-webcodex 的 Runtime identity、长任务恢复和产品安装体验；
- AgenticGPT 的 bounded jobs、confirmation 与 recovery 思路；
- Luvus 的任务 scope/evidence 概念，但保持为轻量 metadata；
- ACP 的 capability negotiation 思想，用于未来可选 Agent compatibility。

### 应该复用 Herdr

- workspace / pane / PTY；
- Agent lifecycle 与 Agent status；
- event stream；
- worktree 和原生高级动作；
- 人工 attach / focus / terminal inspection。

### 暂时不做

- 第二套 Agent runtime；
- 第二套 terminal multiplexer；
- 自建完整 Team/Task DAG/Lease 系统；
- 强制 ACP 成为内部控制协议；
- 绑定某一种 Coding Agent；
- 通用浏览器自动化框架。

## 当前推荐架构

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

职责保持单一：Web AI 是 planner；herdr-mcp 是安全远程控制面和连续性层；Herdr 是持久运行事实源；本地 Agent 是可替换 worker；浏览器扩展负责把本机事件重新接回网页，不承担推理。

## 什么时候应该选择别的方案

Herdr + herdr-mcp 并非所有场景都最优。

- 只需要本地终端复用：直接 tmux；
- 主要追求 Mac 桌面终端体验：优先 cmux；
- 只需要统一 editor 与 coding agent：优先 ACP；
- 只需要给一个 MCP 客户端安全 file/git/exec：coding-tools-mcp 更简单；
- 只需要 Linux 远程 Worker/Hub：AgenticGPT 值得优先评估；
- Windows 用户希望开箱即用的 ChatGPT Coding 桌面助手：gpt-webcodex 的产品形态更贴近。

Herdr + herdr-mcp 最适合的场景是：**希望继续使用最强 Web 模型作为主要思考者，同时让它长期、可靠、可观察地操作真实本机开发现场，并且人可以随时离开、回来和接管。**

## 对 Roadmap 的约束

后续架构升级优先：Rust 单二进制和跨平台产品化；mutation identity、delivery phase、idempotency 和 recovery；browser continuity 的可靠状态机；work context 与 evidence；安装、升级、回滚、doctor 和可观测性。

每增加一个新能力，都继续问：它是否让 Web AI 更可靠地控制真实开发现场？如果只是复制 Herdr、Agent runtime 或其它项目已有的能力，就优先复用而不是扩张 public surface。
