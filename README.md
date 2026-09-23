# herdr-mcp

**简体中文** · [English](README.en.md) · [日本語](README.ja.md)

让 ChatGPT 等 Web AI 在你自己的电脑上持续开发：读写文件、使用 Git、运行命令和测试、调用 Coding Agent，并在多台电脑之间调度任务。

**[文档站](https://whshang.github.io/herdr-mcp/zh-CN/)**

## 一句话安装

把下面一句话发给你电脑上的 Coding Agent：

```text
请按 https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/agent-install.md 安装并配置 Herdr 和 herdr-mcp；使用最新 Stable GitHub Release，自动完成所有可安全自动执行的步骤，只在 Cloudflare 登录/授权、macOS 完全磁盘访问和 ChatGPT OAuth/Connector 审批需要我操作时暂停。
```

Agent 会检查环境、安装 Herdr 与 herdr-mcp、部署 Worker、连接工作站并完成验证。

普通用户不需要 git clone、npm、Cargo、Wrangler，也不需要手动安装 Runtime。

## Cloudflare 配置

- Cloudflare Workers Free 足够使用，不需要绑卡。
- 安装 Agent 会执行 `herdr-mcp worker bootstrap`。
- 没有域名也可以直接使用 `workers.dev`。
- 已有合适域名时，可在 ChatGPT 授权前配置专用 Custom Domain。
- 最终给 ChatGPT 的 MCP 地址必须以 `/mcp` 结尾。

[Cloudflare 配置文档](docs/i18n/zh-CN/cloudflare-edge-deployment.md)

## ChatGPT 配置

1. 在 ChatGPT 插件设置中开启 **Developer mode**。
2. 打开 **插件 → 浏览插件**。
3. 添加 Herdr Connector，名称建议使用 **`herdr`**。
4. 填入完整 MCP 地址，例如 `https://herdr.example.com/mcp`。
5. 完成 OAuth。
6. 在 ChatGPT Project 中工作。
7. **每个新对话第一次需要手动选择或 @ `herdr`。**

建议在 ChatGPT Project 的项目指令里写清本地项目文件夹，例如：

```text
本地项目：/Users/you/Documents/my-project
```

安装浏览器扩展后，也可以从 Herdr Control Center 把设备、workspace 和本地文件夹映射同步到 Project 指令。执行前仍以实时 Herdr 状态为准。

[ChatGPT 配置](docs/i18n/zh-CN/chatgpt-connector.md)

## 授权配置

macOS 只在需要时授权一次稳定 broker：

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

只在 `permissions status` 返回 `needs_setup` 时执行 setup，然后在 **系统设置 → 隐私与安全性 → 完全磁盘访问权限** 中授权。

ChatGPT OAuth 首次连接时，授权页会给出精确的本地批准命令和 6 位验证码。按页面提示在自己的终端完成批准。不要把 Cloudflare Token、设备凭据或其他秘密发到聊天里。

[安装与授权](docs/i18n/zh-CN/agent-install.md) · [故障排查](docs/i18n/zh-CN/troubleshooting.md)

## 产品特点

- **状态留在电脑上。** workspace、终端、Git、worktree、Agent 和长任务不会因为 ChatGPT 对话结束而消失。
- **一套 ChatGPT 控多台电脑。** 一个 Worker 可以登记多台工作站，任务按明确设备路由。
- **多个账号可控同一台电脑。** 已授权的 Connector / WebChat 账号独立识别；浏览器控制按 provider、account 和 session 隔离。
- **支持多个 Coding Agent。** 小任务直接执行，大任务可分配给 Pi、Codex、Claude、Cline、OpenCode 等可用 Agent。
- **安全处理重试。** mutation 的 delivered / not-delivered / uncertain 状态明确，不会在结果不确定时盲目重放。
- **浏览器连续工作可选。** Chrome 扩展提供 Project 绑定、Control Center、排队下一轮、接力和 WebChat 控制。

## 常用方式

### 一个项目

在 ChatGPT Project 指令中保存项目路径。之后可以直接说：

```text
检查这个项目的 Git 状态，修复当前测试失败，只改这个项目，完成后跑相关测试。
```

Herdr 会从实时工作区确认设备、目录和 Git 状态。

### 群控多台电脑

```text
列出我的 Herdr 设备。macbook-main 做后端修改，linux-lab 跑独立测试。使用隔离 worktree，最后合并验证结果。
```

当多个设备都可能执行 mutation 且没有明确目标时，Herdr 会拒绝猜测。

新增电脑时，在已登记设备上运行：

```bash
herdr-mcp worker pair
```

然后在新电脑按返回的 pairing 命令连接。

### 多账号控一台电脑

同一台工作站可以服务多个已授权 ChatGPT / WebChat 账号。每个 Connector、账号和浏览器 session 都保持独立身份。

浏览器控制只会操作 Registry 中已经发现并授权的 session，不会猜账号或跨账号复用会话。

### 本地 Agent 与 ChatGPT 协作

本地 Coding Agent 也可以通过 herdr-mcp 创建、继续和接力受支持的 WebChat 会话，不需要自己再搭一套 Playwright / DOM 自动化。

[本地 Agent ↔ WebChat](docs/i18n/zh-CN/local-agent-webchat-control.md)

## 浏览器扩展（可选）

核心 ChatGPT → MCP → 工作站连接不依赖浏览器扩展。

需要 Project 绑定、Control Center、浏览器连续工作、排队下一轮或 WebChat 接力时，再安装官方扩展：

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [扩展说明](docs/i18n/zh-CN/extension.md) · [浏览器连续工作](docs/i18n/zh-CN/browser-continuity.md)

## 检查状态

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
herdr-mcp device list
```

macOS Apple Silicon、Linux x86_64 和 Linux ARM64 为 Production。Windows x86_64 / ARM64 当前为 Candidate。WSL 不在支持范围。

[平台支持](docs/i18n/zh-CN/platform-support-matrix.md) · [CLI 参考](docs/i18n/zh-CN/cli-reference.md) · [完整文档](https://whshang.github.io/herdr-mcp/zh-CN/)

## License

MIT
