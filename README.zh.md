# herdr-mcp

**把你已经拥有的 ChatGPT / Web AI 推理额度，变成一个持续存在的本地开发环境。**

Web AI 订阅往往提供比 API 或本地 Coding Agent 工作流更充裕的可用推理额度，而 MCP 又让网页模型第一次可以用标准方式调用你自己的机器。Herdr-MCP 从这个机会出发，不要求你为了远程开发再购买一套模型能力。

**MCP 让 Web AI 有了操作本机的双手；Herdr 给这些双手一个持续存在的工作现场；可选浏览器扩展再把本地变化接回 Web 对话。**

ChatGPT 可以直接读改代码、操作 Git、执行命令和测试，也可以把长任务交给本地 Coding Agent。Herdr 让 workspace、终端、进程和 Agent 独立于任何单个 ChatGPT 对话持续存在，因此并行工作和长对话接力都不需要重建本地现场。

**文档：** https://whshang.github.io/herdr-mcp/ · **源码：** https://github.com/whshang/herdr-mcp

Languages: [English](README.md) · **简体中文** · [日本語](README.ja.md)

## Agent-first 安装

正式安装入口是一份**直接写给执行 Agent 的协议**：

- [快速 Agent 安装](docs/i18n/zh-CN/quick-agent-install.md) — 精简的端到端执行协议；
- [Agent 安装](docs/i18n/zh-CN/agent-install.md) — Cloudflare、Link、安全与验证的完整执行合同。

执行 Agent 应自行读取协议、直接完成确定性的检查和 mutation，只在确实需要用户本人授权或选择时暂停。普通工作站的 PROD runtime 使用 GitHub Releases 正式发布产物，不从 git checkout 安装。如果网络、登录状态或第三方服务不可用，Agent 应停止并报告 blocker，不自行搭代理、切网络或发明绕过方案。

本机 herdr-mcp runtime 是原生二进制；普通用户运行它**不需要 Node.js / npm**。Node 只可能由 Agent 在 Cloudflare / Wrangler bootstrap 时临时使用。

安装协议覆盖 Herdr、最新 stable herdr-mcp、Cloudflare Edge、工作站 Link、ChatGPT Connector/OAuth、`herdr-mcp doctor` 与真实 MCP smoke。

需要人工/运维参考时再看 [安装参考](docs/i18n/zh-CN/install.md)。

## 安装完成后，先做这一件事

在一个新的 ChatGPT 会话里启用 `herdr` Connector，然后发送：

```text
分析我的 Herdr 里有哪些项目。只读，不要修改。
```

正常情况下，ChatGPT 会通过 `herdr_inspect` 看到真实 workspace / pane / Agent，并能够继续读取 Git 和项目文件。

### v0.4.2 的图片与视觉开发能力

v0.4.2 把同一套工作站安全边界扩展到视觉与文件导入：`herdr_fs_image` 可以让 ChatGPT 直接读取 managed project 内的 PNG/JPEG/GIF/WebP；内置规划策略让 artifact 走最短安全路径——managed 本地文件直接用 `herdr_fs_*` 工具，安全签名 HTTPS URL 直接用 `herdr-mcp artifact import --signed-url` 导入，可直接消费的 MCP/Connector 文件引用直接消费，其余跨边界传输才使用私有、短生命周期的 Cloudflare R2 通用 artifact 中继。Rust runtime 在写入仓库前完成 HTTPS/SSRF、大小、MIME/文件签名、摘要、managed-root、dirty-file 和 busy-agent 校验。公共 MCP catalog 仍保持 18 个工具。

浏览器扩展**不是通用文件/artifact 中继**。v0.4.2 只增加了一条窄化的 ChatGPT Web 图片 source-capture 路径：ChatGPT cookie 与短生命周期 bearer 始终留在浏览器内存，扩展只解析当前会话已完成 assistant turn 的 `image_asset_pointer` / `file_id`，从允许的 `chatgpt.com` HTTPS 下载端点获取图片，并只把校验后的图片字节与非秘密元数据交给本机 Native Host。cookie、bearer、Authorization header 和下载 URL 都不会跨越该边界。其它 artifact 路由仍由 runtime + 直接导入 + 私有 R2 fallback 负责。

## 浏览器扩展：可选，基础 Connector 可用后再装

浏览器扩展用于长期会话连续工作、Side Panel 控制中心、workspace binding 和“排队”下一轮消息。它**不是**第一次连通 ChatGPT 的前置条件。

在 v0.4.2 source candidate 中，用户手动在同一个已绑定 ChatGPT Project 里新开会话后，直接说 **“继续” / “接着上次”** 即可，不需要提供内部 `continuity_id`。Herdr 会先按稳定的 conversation / Project / workspace 身份搜索本机 Continuity Journal；只有稳定身份本身唯一命中一条 active chain 时才自动恢复，否则只展示有界候选证据让用户确认。系统不会因为“最近一次”或“文字最像”就直接猜。详见 [浏览器连续工作](docs/i18n/zh-CN/browser-continuity.md)。

浏览器扩展的分发身份与 Runtime DEV/PROD 是两套不同概念：

| 扩展通道 | 用途 | 身份 / 更新来源 |
| --- | --- | --- |
| **STORE** | 普通用户默认安装 | 固定 Chrome Web Store 身份 + 商店更新 |
| **STANDALONE** | 不依赖 Store 的 GitHub / 手动安装 | 固定非 Store 身份 + 确定性安装包；v0.4.3+ 成为正式能力 |
| **DEV** | 扩展源码开发 | repo/worktree 的 unpacked 目录；ID 由路径派生 |

当前 stable v0.4.2 的 Native Host 只有 Store/DEV ownership；v0.4.3 才加入固定身份的 **STANDALONE**，不会为了补这个能力重打或移动 v0.4.2 tag。Agent 不得把 GitHub/手动 standalone 包称为 dev，也不得把路径派生的 DEV build 当普通用户 fallback。

当前 stable 的 Store 路径仍是：先完成 runtime + Connector 验证，Store 可用时安装 [Herdr Chrome Web Store 插件](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp)，再执行 `herdr-mcp native-host install` 和 `herdr-mcp native-host status`。v0.4.3+ 中，如果 runtime 明确支持 standalone，且 Store 不可用或用户主动选择独立分发，Agent 可以改走 STANDALONE。

详见 [浏览器扩展](docs/i18n/zh-CN/extension.md) 和 [浏览器控制中心](docs/i18n/zh-CN/browser-control-center.md)。

## 当前支持边界

- `herdr-mcp` stable：`v0.4.2`；
- 公共 MCP contract：epoch 2 / 18 tools；
- 完整 clean-machine 证据最充分的平台：macOS Apple Silicon；
- Windows x64 Release binary 已提供，但 Windows 端到端 UAT 仍在继续；
- Linux runtime 暂不作为当前 stable 的正式支持面承诺。

## 本机 runtime CLI

日常生命周期可以继续交给 Coding Agent；稳定的顶层用户命令是：

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status
herdr-mcp rollback
herdr-mcp uninstall
```

`herdr-mcp service ...` 属于**高级 / 内部**服务控制，不是普通安装主路径。`0.4.1+` 可运行 `herdr-mcp scan --json` 刷新“当前客户端实际可启动的 Herdr Agent”证据清单；详细语义见 [CLI Reference](docs/i18n/zh-CN/cli-reference.md#agent-能力发现scan)。

## 需要更多信息时再看

- [快速 Agent 安装协议](docs/i18n/zh-CN/quick-agent-install.md) — 推荐入口，给 Coding Agent 读；
- [手动安装](docs/i18n/zh-CN/install.md) — 想自己一步步配置时看；
- [ChatGPT Connector](docs/i18n/zh-CN/chatgpt-connector.md) — OAuth / MCP 连接；
- [浏览器扩展](docs/i18n/zh-CN/extension.md) — STORE / STANDALONE / DEV 身份与连续工作；
- [浏览器扩展隐私](docs/i18n/zh-CN/privacy.md) — 扩展的数据处理与 Limited Use；
- [故障排查](docs/i18n/zh-CN/troubleshooting.md) — `doctor`、Link、Edge、OAuth；
- [架构](docs/i18n/zh-CN/architecture.md) — 想理解 Runtime / Edge / Link / Extension 边界时再看。

维护者、发布、UAT、CI、Runtime A/B 和历史 GA 证据保留在详细文档中，但不属于第一次安装主路径。
