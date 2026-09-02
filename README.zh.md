# herdr-mcp

[English](README.md) · **简体中文** · [日本語](README.ja.md)

**让 ChatGPT 负责思考，让工作持续留在你的电脑上。**

Herdr-MCP 让 ChatGPT 等 Web AI 直接查看代码、使用 Git、运行命令与测试，并协调真实开发机上的 Coding Agent。[Herdr](https://herdr.dev/) 负责持续保存 workspace、终端、服务、仓库、worktree 和 Agent 状态，所以对话结束后，长期任务和开发现场仍然存在。

```text
ChatGPT / Web AI
       │ MCP + OAuth
       ▼
Cloudflare Edge
       │ 认证后的出站连接
       ▼
   herdr-mcp
   ├─ 文件 / Git / 命令
   ├─ Coding Agent
   └─ Herdr workspace / 终端 / events
              ▲
              └─ 可选 Chrome 扩展：连续工作 / 接力 / 控制中心
```

模型继续负责规划，真实状态留在你的电脑。小任务可以直接执行，大任务可以拆给不同 Agent、不同设备并行完成，同时保持可观察、可恢复、可人工接管。

**[文档站](https://whshang.github.io/herdr-mcp/zh-CN/)**

## 安装

### 推荐：给 Agent 一句话

```text
帮我安装并配置 Herdr 和 herdr-mcp，请完整按照 https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/agent-install.md 执行；使用当前 Stable GitHub Release，完成 Cloudflare 和 ChatGPT 配置，只在必须由我本人登录或授权时暂停，并在结束前验证整条连接真实可用。
```

Agent 会检查电脑环境、安装 Herdr 和 herdr-mcp、创建或连接 Cloudflare 公网入口、启动开发机连接、指导你完成 ChatGPT 授权，然后执行健康检查和真实 MCP 请求验证。只有账号登录、授权这类必须由你本人完成的操作才需要暂停。

### 手动安装

希望自己控制每一步时，使用[手动安装指引](docs/i18n/zh-CN/install.md)。

### ChatGPT 配置

需要时开启 Developer Mode，然后在 **设置 → Apps** 添加 `herdr` App/Connector 并完成 OAuth。

[ChatGPT 配置](docs/i18n/zh-CN/chatgpt-connector.md) · [OpenAI Developer Mode / MCP 文档](https://help.openai.com/en/articles/12584461)

### Cloudflare 配置

Cloudflare 提供稳定的公网 MCP/OAuth 入口，每台开发机主动向外建立认证连接，因此无需给每台电脑开放公网入站端口。

[Cloudflare 配置](docs/i18n/zh-CN/cloudflare-edge-deployment.md) · [Cloudflare Dashboard](https://dash.cloudflare.com/)

## 群控多台电脑

一个 Herdr Worker 和一个 ChatGPT 连接可以同时管理多台已加入的电脑。ChatGPT 可以通过 `herdr_devices` 查看设备列表和在线状态，并把任务明确路由到指定设备。

例如：

```text
列出我的 Herdr 设备。后端任务使用 macbook-main，独立测试任务使用 macbook-lab；两边工作区保持隔离，完成后交叉验证结果。
```

当有多台电脑都可以执行修改操作，而提示词没有指定目标时，Herdr 会返回 `device_ambiguous`，不会自行猜测。后续操作和重试会保持设备身份，每台电脑也使用独立凭据。

Web AI 也可以通过私有 workstation method 在已加入的电脑之间复制少量、非敏感的 UTF-8 文本，不增加新的 public MCP tool。源端读取带完整性摘要；目标写入仅允许 HOME 下的普通非符号链接文件，大小上限 256 KiB，覆盖必须显式指定，默认创建备份，并拒绝疑似敏感的路径或内容。二进制文件、目录同步和凭据传输不在此能力范围内。

### 把新电脑加入现有设备组

在已经授权的电脑上创建短期配对：

```bash
herdr-mcp worker pair
```

命令会给出配对地址和一次性的 6 位验证码。然后把下面一句发给新电脑上的 Coding Agent：

```text
把这台电脑加入我现有的 Herdr 设备组，请按照 https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/zh-CN/existing-worker-connect.md 执行；配对地址是 <pairing-address>，等 CLI 提示时再让我输入 6 位验证码，完成后验证这台设备已经在同一个 Worker 中在线。
```

新电脑加入现有 Worker 和 ChatGPT 连接，不会再创建一套 Worker，也不需要复制长期共享密钥。

[多设备使用指引](docs/i18n/zh-CN/existing-worker-connect.md)

## 使用建议

### 给 Web AI 明确的工作规则

开发任务可以使用这类默认提示词：

```text
修改前先检查实时 Herdr workspace 和 Git 状态。已有 dirty worktree 保持隔离。确定性的读取、Git 检查、patch 和有限命令直接执行；独立或耗时任务适合时交给可用 Coding Agent 并行完成。结束前检查最终 diff，并运行相关测试后再汇报。
```

高风险修改再补充目标、安全约束和验收标准；调查类任务明确要求只读。

### 至少安装一个 Coding Agent

Herdr-MCP 可以直接完成确定性的操作。长时间实现、大型重构、测试修复循环、独立模块并行时，本机 Coding Agent 会更高效。Herdr 会发现每台电脑上可用的 Agent，因此项目不依赖某一家 Agent。

常用组合：

| 工作类型 | 建议组合 |
| --- | --- |
| 调查、小改动、Git/测试检查 | Web AI → Herdr-MCP 直接工具 |
| 中等规模实现 | Web AI 规划 → 一个 Coding Agent 执行 → Web AI 验证 |
| 多个独立模块 | Web AI 拆分 → 隔离 Agent/worktree → 交叉检查 + 测试 |
| 多台电脑 | Web AI 指定设备 → 各设备独立执行 → 汇总验证 |
| 长时间无人值守 | 再加 Chrome 扩展负责连续工作和接力 |
| 人工接管 | 直接进入同一个 Herdr workspace/终端继续操作 |

避免多个 Agent 同时修改同一个 working tree。并行修改时优先使用隔离 worktree。

长测试和构建应使用 `herdr_exec_start`，后续通过 `herdr_exec_read(session_id, offset=next_offset)` 接力读取，不把终端滚屏当成完成证据。已经结束的 session 会在有界保留期内保存最终输出摘要与 exit evidence，即使 runtime 被替换也能恢复；仍在运行的进程在重启后不会被假定为已经安全接管。

## Chrome 扩展

核心 ChatGPT → MCP → 开发机连接不依赖浏览器扩展。需要长对话连续工作、排队下一轮消息、Browser Control Center 或支持的 ChatGPT artifact 捕获时再安装。

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [扩展说明](docs/i18n/zh-CN/extension.md) · [浏览器连续工作](docs/i18n/zh-CN/browser-continuity.md)

## 常见问题

### 为什么使用 Cloudflare？

ChatGPT 在公网运行，开发机通常位于 NAT、防火墙、动态网络或公司网络之后。Herdr-MCP 让开发机保持无公网入站端口，由每台电脑主动连接到稳定的 Cloudflare 入口。

Cloudflare 同时承担公网 MCP/OAuth 地址、设备路由、重连协调，以及多设备所需的少量共享状态。

### 能不能用端口映射、Tailscale 或其它内网穿透？

其它传输方式需要同时提供这些能力才可以替代：ChatGPT 可访问的公网 HTTPS MCP 地址、可信 TLS、认证/OAuth、安全的设备路由、可靠重连，以及明确的修改交付状态。

私网 IP 或仅 Tailscale 可见的地址无法直接被 ChatGPT 云端访问；裸端口映射会扩大暴露面；通用公网隧道可以发布地址，但 Herdr-MCP 当前已经在 Cloudflare 路径上实现并验证了路由、OAuth、多设备和恢复语义，因此这是正式支持的方案。

### 遇到 `workstation_offline` 怎么办？

它表示 Cloudflare 仍能回应 ChatGPT，但当时没有目标电脑的有效在线连接。短暂断线会先等待重连，电脑端也会持续自动恢复连接。

先运行：

```bash
herdr-mcp status
herdr-mcp doctor
```

涉及修改操作时，按错误返回的 delivery/retry 信息处理；交付状态不确定的操作不要直接重复。详细见[故障排查](docs/i18n/zh-CN/troubleshooting.md)。

### 账号额度在哪里看？

ChatGPT 模型额度属于你的 ChatGPT 套餐或 workspace。查看 ChatGPT 当前账号提供的 usage / model limit 信息；部分套餐显示的是重置时间窗口，并不会提供精确剩余额度。

Cloudflare 用量独立计算。可以在 **Workers & Pages → 对应 Worker → Analytics & Logs**，以及账号 Billing/Usage 页面查看 Worker、Durable Object 等资源使用量。Herdr-MCP 会限制空闲设备产生的协调写入，避免无意义消耗。

### 必须安装 Chrome 扩展吗？

不需要。核心连接可以独立使用。需要浏览器连续工作、接力、Browser Control Center 和支持的浏览器侧 artifact 捕获时再安装。

### Herdr-MCP 必须绑定某个 Coding Agent 吗？

不需要。确定性的工作可以直接执行，复杂任务可以交给目标电脑上任意兼容且可用的 Agent。

## 相关项目与致谢

Herdr-MCP 从多个开源项目中吸收了成熟思路：

- [Herdr](https://github.com/herdrdev/herdr) — 持久 workspace、终端和 Agent 环境。
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — 聚焦确定性 Coding MCP 工具。
- [MCPX](https://github.com/opentokenz/mcpx) — 持久远程 MCP Session 与恢复思路。
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — Remote Worker 与 managed jobs 架构。
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — Web planner / Codex executor 协作。
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — Codex harness + Web 模型推理。
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — 安全暴露 MCP 服务给 ChatGPT 的参考实现。

更多同类路线和架构取舍见[生态对比](docs/i18n/zh-CN/herdr-vs-ecosystem.md)。

## 许可证

Herdr-MCP 使用 **MIT License**。第三方项目的名称、商标、代码和文档继续遵循各自许可证与政策。
