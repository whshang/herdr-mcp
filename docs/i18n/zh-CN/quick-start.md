# 快速开始

目标：已经有 Herdr 的前提下，不先读维护者和 runtime 文档，就把一个网页客户端真正跑通。

## 1. 先确认 Herdr 本体

herdr-mcp 不安装也不替代 Herdr。如果还没装，请先按官方 [Herdr 安装](https://herdr.dev/zh-cn/docs/install/) 和 [快速开始](https://herdr.dev/zh-cn/docs/quick-start/) 完成，然后确认：

```bash
herdr --version
herdr api schema >/dev/null
```

如果还不熟悉 workspace / tab / pane / agent / session，先读官方 [核心概念](https://herdr.dev/zh-cn/docs/concepts/)。

## 2. 构建 herdr-mcp

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npm run build
```

如果让本地 coding Agent 安装，可以直接用 [Agent 辅助安装](agent-install.md)。Agent 一旦确定目标项目 root，应先读取该项目存在的 `AGENTS.md`、`CLAUDE.md`、`README.md`，再按安装协议执行，不能自行猜流程。

## 3. 只选一条客户端路径先跑通

### ChatGPT

ChatGPT 需要经过有认证的公网 Edge。继续看 [安装](install.md)，然后看 [连接 ChatGPT](chatgpt-connector.md)。首次部署默认使用 `workers.dev`，不需要自定义域名。

### z.ai / DeepSeek

这两个站点直接走本机浏览器 bridge，不需要伪造公网 MCP Connector。安装 Chrome 扩展 / Native host，把会话绑定到 workspace，再看 [JSON → MCP 桥接](extension-bridge.md)。

## 4. 理解“自动”的作用域

- ChatGPT **项目**自动化按 Project id 共享，并受 Options 里的项目自动化总开关控制。
- 普通 ChatGPT `/c/<id>`、z.ai、DeepSeek 使用**单会话 Auto**；即使项目自动化总开关关闭，它们仍可以从自己的 HUD 单独打开“自动”。
- 每个作用域默认都是 Auto 关，需要显式打开。

## 5. 验收真正的客户端边界

不要只看到“进程启动”就结束。至少从实际要用的客户端确认：

- 本机 runtime 可达；
- ChatGPT 能看到当前 epoch-2 的 18 个工具，或者 z.ai / DeepSeek bridge 能列出并调用本机 catalog；
- 浏览器扩展能观察绑定 workspace；
- 如果打开 Auto，一次真实 progress / settled 事件能回到对应会话。

下一步：完整手工流程见 [安装](install.md)；任一边界失败见 [故障排查](troubleshooting.md)。
