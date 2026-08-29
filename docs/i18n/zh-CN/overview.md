# 总览

## 从 Web AI 已经拥有的能力开始

ChatGPT 等 Web AI 的价值不只在模型能力，还在订阅产品提供的可用推理额度。实际使用中，这个额度与 API / 本地 Coding Agent 的计费和限额结构可能有显著差异；具体额度会随账号、模型和产品策略变化，因此 Herdr-MCP 不把任何固定倍率当作产品保证。

真正改变架构的是 MCP：只要 Web AI 支持通过 MCP 调用一个 HTTP 服务，网页里的强模型就不再只能聊天，它可以安全地连接到用户自己的执行环境。

最简单的方案是 Web AI → MCP → files / Git / shell。Herdr-MCP 选择继续向前一步：把 MCP 接到一个持续存在、可观察、可人工接管的 Herdr 工作现场。

> **MCP 让 Web AI 有了操作本机的双手；Herdr 给这些双手一个持续存在的工作现场；浏览器扩展再把本地变化接回 Web 对话。**

这也是为什么项目没有重新实现一套 Coding Agent。确定性小任务由 Web AI 直接调用工具；复杂或并行工作再委派 Pi、Grok 等本地 Agent。ChatGPT 对话可以结束或接力，而本地 workspace、PTY、进程、Agent 和 worktree 不必随之消失。

## 你得到什么

```text
你
│
▼
ChatGPT / Web AI
│  MCP + OAuth
▼
Cloudflare Edge
│  authenticated WSS
▼
herdr-link + herdr-mcp runtime
│
├─ 文件 / 搜索 / Patch / Git / Shell / 图片
├─ Herdr workspace / pane / agent 状态
└─ Pi / Grok / 其它本地 Agent

浏览器扩展 ── continuity / 排队 ──► Web 会话
         └── 实时 workspace / pane 状态 ──► Chrome Side Panel
```

日常使用时，ChatGPT 可以像本地 Coding Agent 一样直接检查项目和执行确定性操作。需要独立推理、并行开发或审查时，再把任务交给 Herdr 中的本地 Agent。Herdr 保留所有工作区和窗格，因此网页端始终能知道“机器上现在有什么、谁在做什么、做到哪里了”。

## 为什么以 Herdr 为底座

普通 filesystem/shell MCP 可以让模型执行命令，但很难表达一个长期运行的开发工作台。Herdr 原生提供 Socket API 和大量终端控制方法，同时维护 workspace、tab、pane、agent、session 等状态。已经打开的窗口、Agent 和工作目录都有稳定身份，远程模型可以检查、继续、干预和重新调度。

这带来几项关键能力：

- **持久工作区**：终端和 Agent 会话不依赖某一次网页请求存在。
- **可观察**：远程模型能看到 workspace、pane、Agent 状态和近期事件。
- **可调度**：可以把适合独立执行的任务交给 Pi、Grok 等 Agent，再读取进度和结果。
- **可恢复**：Herdr 服务和工作区承担长期状态；短暂断线或重启后可以重新发现当前工作现场。
- **少造一套终端系统**：herdr-mcp 通过 Herdr Socket API 使用已有能力，只补远程模型缺少的文件、Git、Shell 和公网认证层。

## Herdr 与 herdr-mcp 的边界

Herdr 本体负责本地开发工作台：workspace / tab / pane / agent / session、PTY、原生 CLI、Socket API 和 Agent 自动化。相关行为以 [Herdr 官方文档](https://herdr.dev/docs/) 为准。

herdr-mcp 负责远程连接和编排：

- 面向 ChatGPT 的 MCP 工具契约；
- Cloudflare Edge、OAuth 和持久 workstation link；
- 受管 Git 项目里的文件、Git、Shell、图片访问；
- 面向 Web planner 的状态摘要和 Agent 调度；
- 浏览器端的进度回推、超时恢复、自动继续和长对话接力；
- z.ai / DeepSeek 的本地 JSON → MCP bridge。

## 工具设计

生产公共契约当前为 **epoch 2 / 18 tools**。它没有把 Herdr 的大量 Socket API 方法逐个复制成 MCP 工具。

设计分成四类：

1. `herdr_inspect` / `herdr_since`：低成本了解当前工作现场。
2. `herdr_fs_*` / `herdr_git` / `herdr_exec*`：直接完成确定性本地操作。
3. `herdr_prompt`：需要独立推理或并行工作时调度本地 Agent。
4. `herdr_methods` / `herdr_call`：需要 Herdr 原生高级能力时动态发现并调用 Socket API。

`herdr_skill` 提供与当前 Herdr / herdr-mcp 运行方式匹配的操作策略。远程模型无需把 90 多个原生方法全部塞进工具列表和上下文。

## 安全边界

工作站主动向 Edge 建立认证连接，不需要开放公网入站端口。ChatGPT Connector 使用 OAuth；本机静态 bearer 不应该复制到 ChatGPT。文件类工具限制在 Herdr 已识别的 managed Git roots，并跳过常见敏感文件名；写权限还可以通过 `HERDR_MCP_READONLY` 和 `HERDR_MCP_WRITE_ROOTS` 收紧。

Shell 本身具备执行任意命令的能力，因此它代表的是“允许这个远程模型在该开发工作站执行代码”的信任边界。生产使用时应把 workstation、Cloudflare identity 和 ChatGPT account 都视为安全边界的一部分。

## 从哪里开始

- 第一次部署：[快速开始](quick-start.md)
- 让本地 Agent 代装：[Agent 安装](agent-install.md)
- 连接 ChatGPT：[ChatGPT Connector](chatgpt-connector.md)
- 理解完整链路：[架构](architecture.md)
- 学习日常工作方式：[最佳实践](best-practices.md)
- 配置浏览器工作层：[浏览器扩展](extension.md) 与 [浏览器控制中心](browser-control-center.md)
- 处理安装和运行故障：[故障排查](troubleshooting.md)
