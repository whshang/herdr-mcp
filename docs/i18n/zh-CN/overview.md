# 总览

herdr-mcp 是 [Herdr](https://herdr.dev) 的远程 / Web 入口。Herdr 本体负责本机终端工作区、窗格、Agent、会话以及原生 CLI / Socket API；herdr-mcp 只补远程网页模型拿不到的那一层：紧凑的 MCP 工具面、给 ChatGPT 用的 Cloudflare Edge + OAuth、本机项目文件/Git/shell 的安全访问，以及负责进度、恢复、接力和 JSON→MCP 的浏览器扩展。

如果你还不熟悉 Herdr 本体，先读官方 [Herdr 文档](https://herdr.dev/docs/)，尤其是 [安装](https://herdr.dev/zh-cn/docs/install/)、[快速开始](https://herdr.dev/zh-cn/docs/quick-start/) 和 [核心概念](https://herdr.dev/zh-cn/docs/concepts/)。herdr-mcp 不再重复维护这些内容。

## herdr-mcp 增加了什么

```text
ChatGPT / z.ai / DeepSeek
        |
        | MCP 或浏览器 JSON bridge
        v
herdr-mcp 远程控制面
        |
        | Herdr socket + 受控工作站访问
        v
Herdr workspaces / panes / agents
```

- **ChatGPT Connector**：通过 Cloudflare Worker + 持久工作站 Link 提供稳定 MCP/OAuth 入口。
- **远程工作站工具**：在受管 Git 项目里定向读取/修改文件、查 Git、跑 shell、看图片，以及观察 workspace/pane/agent，而不是开放任意本地磁盘。
- **浏览器连续工作**：进度回推、回复恢复、手动/自动接力，以及按 Project 或单会话作用域的自动化。
- **z.ai / DeepSeek 兼容**：网页模型输出 JSON tool call，扩展通过 Chrome Native Messaging + 权限 `0600` 的 Unix socket 调本机 MCP；浏览器不保存 Herdr bearer。

## 先分清 Herdr 与 herdr-mcp

下面这些属于 **Herdr 本体**，应直接以官方文档为准：

- workspace / tab / pane / agent / session 概念；
- Herdr 二进制的安装与升级；
- 原生 Herdr CLI；
- Socket API methods / events；
- 在 Herdr 内运行和自动化 coding agent。

常用官方参考：[Agents](https://herdr.dev/docs/agents/)、[Agent automation](https://herdr.dev/docs/agent-automation/)、[CLI reference](https://herdr.dev/docs/cli-reference/)、[Socket API](https://herdr.dev/docs/socket-api/)、[Agent skill file](https://herdr.dev/docs/agent-skill/)。

下面这些才属于 **herdr-mcp**：连接 ChatGPT、Edge 部署、浏览器自动化、远程 planner 安全边界，以及本机 MCP runtime 的维护。

## 下一步

- 第一次安装：[快速开始](quick-start.md)。
- 主要使用 ChatGPT：[连接 ChatGPT](chatgpt-connector.md)。
- 主要使用 z.ai / DeepSeek：[JSON → MCP 桥接](extension-bridge.md)。
- 希望网页任务持续推进：[浏览器扩展](extension.md)。
- 遇到故障：[故障排查](troubleshooting.md)。
