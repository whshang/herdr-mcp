# herdr-mcp

**让 ChatGPT 等 Web AI 通过 Herdr 直接参与一台真实本地开发工作站的控制面。**

网页模型擅长理解目标、跨步骤规划和做工程决策，但浏览器本身看不到你的本地文件、Git、Shell、长任务和 Herdr workspace。herdr-mcp 把这两端接起来，同时避免把工作站直接暴露在公网。

**文档站：** https://whshang.github.io/herdr-mcp/ · **源码：** https://github.com/whshang/herdr-mcp

语言：[English](README.md) · **简体中文** · [日本語](README.ja.md)

## 它解决什么

herdr-mcp 给 Web planner 补上五类能力：

- **持续存在的本地现场**：Herdr workspace、pane、Agent 生命周期；
- **确定性的工作站工具**：文件、Git、图片、Shell；
- **可控委派**：只有确实需要独立推理时才派本地 Herdr worker；
- **稳定的远程入口**：Cloudflare Edge 上的 OAuth/MCP + 工作站主动出站 Link；
- **浏览器连续工作**：本地进度可以回到网页会话，超长对话可以安全接力到新会话。

整体关系：

```text
用户
  ↓
ChatGPT / Web AI
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ 已认证路由
herdr-link
  ↓
本机 herdr-mcp runtime
  ↓
Herdr workspace
  ├─ files / Git / shell
  └─ local agents

Herdr progress
  ↓
浏览器扩展
  ↓
网页会话继续
```

## 它不是什么

herdr-mcp **不是**第二个 Herdr，也不是第二套 Agent runtime，更不会把 Herdr 的每一个 Socket API 方法都做成 MCP tool。

Web 模型负责目标和决策；Herdr 负责持续工作现场；本地 Agent 是 worker；Git 和 runtime 状态才是事实来源。

## 为什么公开工具只有很小一组

Herdr 原生 Socket API 很丰富，但 Web 模型不应该在每轮对话里携带几十上百个工具 schema。

因此公开契约分两层：

```text
高频远程工作
  → herdr_inspect / herdr_since / herdr_fs_* / herdr_git / herdr_exec* / herdr_prompt

Herdr 原生长尾能力
  → herdr_methods → herdr_call
```

当前生产公开契约：**epoch 2 / 18 tools**，其中包含只读 `herdr_skill`。

## 最短安装路径

前置：

- 已安装并运行 [Herdr](https://herdr.dev)；
- 如果要从 ChatGPT 公网连接，需要一个 Cloudflare 账号。

**本机 MCP runtime** 是原生二进制，运行它**不需要** Node.js / npm。Node 仍可用于 Cloudflare Edge 部署、浏览器扩展工具链，以及从本仓库做贡献者构建。

### 安装原生 runtime（主路径）

1. 从 [GitHub Releases](https://github.com/whshang/herdr-mcp/releases) 下载当前平台的 `herdr-mcp` 二进制（产品仍处 alpha 时会出现 prerelease 标签）。
2. 放到 `PATH` 上（例如 `~/.local/bin/herdr-mcp`）并赋予可执行权限。
3. 先验证二进制：

```bash
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

安装后的日常生命周期：

```bash
herdr-mcp update apply
herdr-mcp update status
```

优先使用以上顶层命令。**不要**把 `herdr-mcp service install` 当成普通用户安装主路径；`service ...` 仍是高级/内部接口。

加 Edge 之前先确认 Herdr：

```bash
herdr --version
herdr api schema >/dev/null
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

接 ChatGPT 时，推荐先把 Cloudflare Worker 部署到默认的 `workers.dev`，启动 `herdr-link`，然后在 ChatGPT 添加公网 `/mcp` 地址并完成 OAuth。

**不要**把 `HERDR_MCP_TOKEN` 或 Cloudflare API Token 填进 ChatGPT。

### 让本地 Coding Agent 自动安装

如果你已经在使用 Codex、Claude Code、Pi、DSH、Cline 等能读文件和执行命令的本地 Agent，可以直接把下面这段交给它，不要让 Agent 根据 README 自己猜部署步骤：

```text
请帮我安装并部署 herdr-mcp。先读取唯一权威安装协议：
https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/agent-install.md

严格按文档完成。首次安装不要创建 Custom Domain、DNS 记录或 Tunnel，只使用 workers.dev。不要回显或提交任何 Token。每个 mutation 后先验证状态再继续。
```

该 Edge 流程生成 Cloudflare-safe Worker 名时使用仓库内的确定性 helper：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

### 贡献者从源码构建（可选）

只有在开发 herdr-mcp 本身时才需要 clone 本仓库。源码构建仍可能使用 Node 工具链处理站点/扩展/Edge 包；那不是最终用户运行 MCP runtime 的主路径。

完整流程：[快速开始](docs/i18n/zh-CN/quick-start.md) · [安装](docs/i18n/zh-CN/install.md) · [ChatGPT Connector](docs/i18n/zh-CN/chatgpt-connector.md)

## 第一个真实任务

Connector 配好后，建议新开一个会话，从只读开始：

```text
检查当前 Herdr workspace 和 Git 状态。只读，不要修改。
```

理想流程：

```text
herdr_inspect
  ↓
herdr_skill
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
  ↓
回答
```

这比“设置页显示已连接”更能证明链路真正打通到了本机。

随后再尝试一次小修改 + 测试 + diff。只有任务确实需要独立分析时，才派本地 Agent。

## 浏览器连续工作

MCP 本质上是请求驱动：

```text
Web AI → workstation
```

但一个本地 Agent 可能在浏览器回合结束后继续工作很久。可选的 MV3 扩展补上反向通道：

```text
workstation → Web conversation
```

扩展支持：

- workspace binding；
- progress / settled 回推；
- evidence-first 页面恢复；
- 长 conversation handoff；
- 为 z.ai / DeepSeek 等没有同类原生 Connector 的网页提供受限 JSON→MCP bridge。

安装本机 Native Messaging host：

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

然后在 Chrome/Chromium 中以“加载已解压扩展程序”的方式加载 `extension/`。

详见 [浏览器连续工作](docs/i18n/zh-CN/browser-continuity.md) 和 [浏览器扩展](docs/i18n/zh-CN/extension.md)。

## 安全边界

herdr-mcp 明确区分不同权限面：

- 本机 runtime 只监听 loopback；
- 工作站主动建立**出站**认证 WSS 到 Edge；
- ChatGPT 公网访问使用 OAuth；
- 浏览器连续性使用 Native Messaging + 权限 `0600` 的本机 Unix Socket；
- `herdr_fs_*` 受 managed Git root、写入和 secret-path gate 约束；
- `herdr_exec` 是更强的 Shell 能力，**不是 sandbox**；
- mutation 投递不确定时先检查实际状态，不盲目重复执行。

详见 [架构](docs/i18n/zh-CN/architecture.md) 和 [最佳实践](docs/i18n/zh-CN/best-practices.md)。

## Runtime 升级不需要换 Connector

公网 Edge 和本机 runtime 是两个发布平面：

```text
稳定 Edge / OAuth / MCP URL
        ↓
persistent herdr-link
        ↓
runtime generation A / B
```

只要公开 contract epoch 保持兼容，本机新 generation 可以独立验证、切流和回滚，不需要修改 ChatGPT Connector URL。

详见 [Runtime A/B](docs/i18n/zh-CN/runtime-self-upgrade.md)。

## 文档地图

从这里开始：

- [总览](docs/i18n/zh-CN/overview.md)
- [设计思路](docs/i18n/zh-CN/design-philosophy.md)
- [快速开始](docs/i18n/zh-CN/quick-start.md)
- [安装](docs/i18n/zh-CN/install.md)
- [ChatGPT Connector](docs/i18n/zh-CN/chatgpt-connector.md)

日常使用与运维：

- [浏览器连续工作](docs/i18n/zh-CN/browser-continuity.md)
- [浏览器扩展](docs/i18n/zh-CN/extension.md)
- [架构](docs/i18n/zh-CN/architecture.md)
- [最佳实践](docs/i18n/zh-CN/best-practices.md)
- [CLI 参考](docs/i18n/zh-CN/cli-reference.md)
- [Cloudflare Edge 部署](docs/i18n/zh-CN/cloudflare-edge-deployment.md)
- [Runtime A/B](docs/i18n/zh-CN/runtime-self-upgrade.md)
- [故障排查](docs/i18n/zh-CN/troubleshooting.md)

维护者参考：

- [自动化](docs/i18n/zh-CN/automation.md)
- [能力基准与设计取舍](docs/i18n/zh-CN/capability-benchmark.md)
- [为什么选择 Herdr + herdr-mcp](docs/i18n/zh-CN/herdr-vs-ecosystem.md)
- [Worker 备选](docs/i18n/zh-CN/worker-fallbacks.md)
- [本地 Agent 安装协议](docs/i18n/zh-CN/agent-install.md)

## 开发检查

```bash
npm run build
npm test
npm run test:edge
npm run build:site
git diff --check
```

正式文档采用双语逻辑页模型；新增正式章节时，`docs/i18n/en` 与 `docs/i18n/zh-CN` 必须同时存在对应 slug。
