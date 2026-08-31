# 总览

herdr-mcp 把 **Web AI 的推理能力** 接到一台**持续存在、可观察、可人工接管的本地开发机**。

最短的产品模型只有三层：

```text
ChatGPT / Web AI
      │ MCP + OAuth
      ▼
Cloudflare Edge
      │ authenticated workstation link
      ▼
herdr-mcp + Herdr workstation
      ├─ files / Git / shell / images
      ├─ workspace / pane / agent state
      └─ optional local workers
```

浏览器扩展是可选的第四层：它负责把本机进度重新接回正确的网页会话，并提供 Chrome Side Panel 控制中心；标准 MCP 本身并不依赖它。

## 你真正得到什么

- Web AI 可以直接读取和修改真实 Git 项目，而不是只生成代码片段；
- workspace、PTY、进程和 Agent 生命周期属于 Herdr，不属于某一次聊天；
- 确定性小任务由 Web planner 直接做，复杂或并行任务再委派可替换的本地 worker；
- 工作站主动连向公网 Edge，不需要给开发机开放入站端口；
- mutation、managed root、OAuth、Native Messaging 和浏览器连续工作都有明确边界。

这不是“再做一个 Coding Agent”。Herdr 是持久工作现场，herdr-mcp 是 Web planner 面向这个现场的远程控制面。

## Herdr 与 herdr-mcp 的职责

**Herdr** 负责 workspace / tab / pane / agent / session、PTY、原生 CLI、Socket API 和本地 Agent 生命周期。相关行为以 [Herdr 官方文档](https://herdr.dev/docs/) 为准。

**herdr-mcp** 负责：

- 面向 ChatGPT / Web AI 的 MCP 工具契约；
- Cloudflare Edge、OAuth 与 workstation link；
- managed Git root 内的文件、Git、Shell 和图片能力；
- 面向 Web planner 的状态摘要、mutation 语义和 Agent 调度；
- 可选的浏览器 Continuity、Control Center 与实验性 JSON → MCP bridge。

为什么选择这种分工、而不是 tmux/cmux/ACP 或其它 coding MCP，见 [生态与架构比较](herdr-vs-ecosystem.md)。更深入的设计原则见 [设计思路](design-philosophy.md)，完整技术链路见 [架构](architecture.md)。

## 从哪里开始

- **让 Agent 直接完成安装**：[Agent 安装](agent-install.md)
- **人工理解安装与运维阶段**：[安装](install.md)
- **安装后跑第一次真实任务**：[快速开始](quick-start.md)
- **连接 ChatGPT**：[ChatGPT Connector](chatgpt-connector.md)
- **学习日常工作方式**：[最佳实践](best-practices.md)
- **需要长时间 Web 连续工作**：[浏览器扩展](extension.md)
- **出现异常**：[故障排查](troubleshooting.md)
