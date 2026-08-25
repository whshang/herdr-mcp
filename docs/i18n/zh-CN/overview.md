# 总览

herdr-mcp 让 ChatGPT 等 Web AI 直接参与本机软件开发：读取和修改代码、搜索仓库、查看 Git、运行命令、观察 Herdr 工作区，并在需要时调度本地 coding agent。数据和执行仍留在自己的机器上，公网侧只提供稳定、可认证的 MCP 入口。

它解决的是一个很具体的问题：网页模型推理能力强、上下文大，但天然看不到你的终端、仓库和正在运行的 Agent；本地 Coding Agent 能操作机器，却通常各自困在一个终端会话里。Herdr 提供持久工作区、真实 PTY、Agent 状态和 Socket API，herdr-mcp 再把这些能力压缩成适合远程模型使用的控制面。

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

浏览器扩展 ── 进度、恢复、自动继续、长对话接力 ──► Web 会话
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
- 配置浏览器连续工作：[浏览器扩展](extension.md)
- 处理安装和运行故障：[故障排查](troubleshooting.md)
