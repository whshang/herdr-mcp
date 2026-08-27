# 浏览器扩展：让网页 AI 保持连续工作

浏览器扩展是 herdr-mcp 的连续性层。

MCP 解决：

```text
ChatGPT / Web AI → 本机 Herdr
```

扩展解决：

```text
本机 Herdr → 网页会话
```

两者组合后，网页 AI 才能适合运行几十分钟甚至数小时的开发任务。

扩展名称：**herdr → Web wake**。

它不是新的 Agent runtime，也不是第二套调度平台。它负责网页会话、本地工作区和 MCP runtime 之间的持续连接。

## 两条能力主线

扩展包含两个方向不同的能力。

| 主线 | 解决的问题 | 典型场景 |
|---|---|---|
| A. Browser continuity | 网页任务结束后不知道本机发生了什么 | ChatGPT 派 Agent 后等待完成、自动恢复、长对话接力 |
| B. JSON → MCP bridge | 网站没有原生 MCP Connector | DeepSeek / z.ai 调用本机 Herdr 工具 |

两条主线共享本机安全传输，但目标不同。

## 安全架构

扩展不会把 Herdr bearer 放进网页脚本、service worker 或浏览器存储。

当前路径：

```text
网页内容脚本
    ↓
Chrome Extension Service Worker
    ↓ Native Messaging
本机 Host
    ↓ Unix socket (0600)
herdr-mcp runtime
```

因此：

- 浏览器负责页面交互；
- Native host 负责受信任本机桥接；
- herdr-mcp 继续负责权限和工具边界。

扩展通信不经过 Cloudflare Worker。公网 OAuth 和本机可信 IPC 是两条不同安全边界。

## 安装与第一次使用

主路径（不需要 clone 仓库）：

1. 从发布该 Rust binary 的同一 GitHub Release 下载 `herdr-mcp-extension-<version>.zip`（以及对应的 `.sha256` sidecar）。该 zip 只作为 Release asset；**不会**写入 `release-manifest.json`，因此 binary updater 契约保持不变。
2. 校验 sidecar 后解压到托管目录：

```bash
mkdir -p ~/.config/herdr-mcp/extension
unzip herdr-mcp-extension-<version>.zip -d ~/.config/herdr-mcp/extension
# manifest.json 等文件必须直接位于该目录下
```

3. 在 Chrome 打开 `chrome://extensions` → 打开开发者模式 → **Load unpacked** → 选择 `~/.config/herdr-mcp/extension`。
4. 安装 Native Messaging host（优先解析托管路径，或使用 `HERDR_EXTENSION_PATH`）：

```bash
herdr-mcp native-host install
# 迁移期等价命令：
# bin/herdr-extension-host install
```

5. 打开 ChatGPT、z.ai、DeepSeek 等支持站点。
6. 在 popup/HUD 中绑定一个 Herdr workspace。ChatGPT 可以直接在根首页、Project 首页或具体 conversation 上绑定；Project 不需要先创建 `/c/<id>`。

开发者仍可用仓库内 `extension/` 做本地调试，但这不是终端用户主路径。不要把 clone 本仓库当成安装方式。

绑定单位是 workspace，不是单个 agent。

原因：真实开发通常包含多个 pane：

```text
workspace
 ├─ coding agent
 ├─ test
 ├─ server
 └─ review agent
```

整个工作现场才是连续性的对象。

对于 ChatGPT Project，持久 binding 以稳定 `project_id` 为身份，而不是绑定某一个 conversation id。因此可以在 Project 首页先完成绑定。当前真正激活的 Project `/c/<id>` 只是 progress/continue 的**投递目标**（`active_conv_key`）；后台打开 sibling conversation 不会迁移 binding，只有实际激活对应 tab 才切换 delivery target。在 `https://chatgpt.com/` 根首页建立的 binding 则是当前 tab 的 provisional 状态，等该 tab 第一次进入具体 Project 或 conversation 后再一次性归位。

## HUD 操作

底部 HUD 提供当前作用域的状态和操作：

- 工作状态；
- 手动继续；
- herdr 状态监控；
- LLM 分析；
- 手动接力（支持范围内的会话）；
- 自动开关。

自动化默认关闭。

开启后，扩展才会根据作用域规则执行自动 progress、settled 唤醒、恢复或接力。

## A：网页连续工作

### Progress

当 Herdr workspace 中 Agent 工作时，扩展观察状态变化。

它不会固定频率刷消息，而是：

- 检查是否有新的有效进展；
- 有变化才发送；
- 长时间无变化时按 fallback 策略提醒。

这样可以避免一个长任务产生大量无意义消息。

### Settled

当 workspace 全部工作完成，扩展发送一次收工提醒，让网页模型重新观察结果。

注意：settled 只是“本地工作结束”的信号，不代表下一步一定自动修改代码。最终决策仍由 Web planner 完成。

### Recovery

浏览器页面可能出现：

- 回复半截；
- 连接中断；
- 服务端已经生成新消息，但 DOM 没刷新；
- 用户发送超时提示。

恢复流程优先确认事实：

```text
发现异常
 ↓
检查服务端状态
 ↓
确认是否已经推进
 ↓
同步页面
 ↓
必要时才恢复
```

不会看到错误就立即重复发送任务，因为工具调用可能已经发生。

### Handoff / Rollover

长对话最终会遇到上下文压力。

扩展支持语义接力：

```text
旧对话
 ↓
生成紧凑 handoff packet
 ↓
新会话
 ↓
确认 seed 成功
 ↓
切换 active delivery target
```

对 ChatGPT Project 来说，Project/workspace binding 与 `continuity_id` 在接力期间始终不变。旧 `active_conv_key` 一直是权威，直到新 conversation 与 seed 都确认成功；此后只切换投递目标。z.ai 等会话级站点仍在确认后迁移具体 conversation binding。接力摘要记录的是历史工作状态，不是实时运行证明。新会话仍需要重新检查 Herdr、Git 和运行环境。

## B：JSON → MCP bridge

DeepSeek、z.ai 等网页没有 ChatGPT 原生 MCP Connector 时，扩展提供兼容层：

```text
网页模型
 ↓ JSON tool call
扩展
 ↓ Native Messaging
本机 MCP
 ↓
Herdr tools
```

它可以：

- 获取本机 tools/list；
- 执行 tools/call；
- 将结果回填网页模型；
- 在有限轮次内完成工具调用循环。

这不是伪造原生 MCP，而是网页侧协议适配。

## 自动化边界

不同站点能力不同：

| 能力 | ChatGPT | z.ai / DeepSeek |
|---|---|---|
| workspace binding | 支持 | 支持 |
| progress / settled | 支持 | 支持 |
| ChatGPT 专属 stale-view 恢复 | 支持 | 不适用 |
| ChatGPT Project rollover | 支持 | 不适用 |
| JSON bridge | 不需要 | 使用 |

自动化仍然受：

- 用户开启状态；
- workspace binding；
- 页面状态；
- 安全 gate；

共同约束。

## 本机网络与 host 权限

较新的 Chrome 可能把 loopback / 本机应用访问与普通 host 权限分开管控。若 Options 连接测试或 HUD 提示本机访问被拦截，请在扩展站点设置中允许访问本机应用。

扩展使用有界连接尝试，并报告该状态，而不是一直转圈。

GA 阶段 `host_permissions` 仍保留 `<all_urls>`。收窄到四个 content_script 源站加 loopback 足以覆盖 ChatGPT / Claude / z.ai / DeepSeek 的 scripting 与已绑定 tab 的 reload 恢复，但可选的 LLM judge 会从 service worker 请求用户配置的 OpenAI 兼容 base URL，Options 也允许非默认的 `herdrMcpUrl`。这些主机在安装时未知，因此在可选权限 UX 就绪前继续保留 `<all_urls>`。这不是 Chrome Web Store 上架声明。

## 不做什么

扩展不会：

- 替代 ChatGPT 推理；
- 替代 Herdr Agent；
- 暴露公网 MCP；
- 存储高权限 token；
- 绕过浏览器或组织权限。

它只负责让一个长期运行的 AI 开发流程保持连接。

详细行为：

- [连续工作、恢复和接力](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [Browser continuity design](browser-continuity.md)
