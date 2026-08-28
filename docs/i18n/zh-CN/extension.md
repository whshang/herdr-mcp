# 浏览器扩展：连续工作、控制中心与本机桥接

herdr-mcp 浏览器扩展不是一个“自动点网页”的外挂，也不是第二个 Agent runtime。

它把网页会话、本机 Herdr 工作现场和 MCP runtime 连接成一条长期可观察、可恢复的浏览器工作流。

如果只用 MCP，主要方向是：

```text
Web AI → 本机工作站
```

安装浏览器扩展后，还多出一条本机到网页的连续性通道，以及一个直接观察本机 Herdr 现场的 Side Panel：

```text
本机 Herdr → 浏览器扩展 → Web 会话
                    ↘ Control Center
```

## 三个产品面

当前扩展可以按三个职责理解。

| 产品面 | 解决的问题 | 主要入口 |
|---|---|---|
| Continuity | 本地工作发生变化后，怎样让正确的网页会话继续、恢复或接力 | 页面内 HUD |
| Control Center | 本机有哪些 workspace / pane / Agent，哪个 pane 是明确人工目标 | Chrome Side Panel |
| JSON → MCP bridge | 没有原生 MCP Connector 的 Web AI 怎样调用本机 Herdr 工具 | z.ai / DeepSeek 页面内 |

另外，ChatGPT composer 旁的 **“排队”** 用来表达“等当前回复结束后，再把这条用户意图作为下一轮发出去”。它属于网页交互层，不会中断正在生成的回复。

## 入口怎么分工

扩展现在有几个不同入口，它们不应该互相替代：

| 入口 | 用来做什么 |
|---|---|
| 工具栏图标 | 直接打开 Chrome Side Panel 浏览器控制中心 |
| HUD | 当前页面的紧凑状态（网页 + Herdr + 绑定数量）、Auto、手动继续、提取 Herdr 状态、LLM 判断 |
| Control Center | 跟随当前标签页识别 Project / conversation，统一 binding / handoff，并查看 workspace / pane / Agent、pin 明确目标、执行只读 inspect / tail、预览未来控制动作 |
| Queue / 排队 | ChatGPT 正在回复时先保存补充要求，回合结束后优先作为下一条用户消息 |
| Options | 语言、continuity timing、可选 LLM judge 等低频配置 |

工具栏图标直接进入 Control Center。绑定 / 解绑与手动接力只在 Control Center 有一个 UI 路径；HUD 不再有抽屉，只保留高频状态、Auto 和三个预置会话动作；timing / LLM 等低频配置只在 Options。

## 安全架构

扩展不会把 Herdr bearer 放进网页脚本、service worker 或浏览器存储。

当前主路径：

```text
网页内容脚本 / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
本机 Host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

因此：

- 浏览器负责页面交互和可视化；
- Native host 负责受信任本机桥接；
- herdr-mcp runtime 继续负责工具、权限和 mutation 边界；
- Cloudflare Edge 不是浏览器扩展访问本机状态的必经路径。

公网 OAuth/MCP 与本机 Native Messaging 是不同的安全边界。

## 安装与第一次使用

### 终端用户主路径

不需要 clone 仓库。

1. 从发布当前 Rust binary 的同一 GitHub Release 下载：

```text
herdr-mcp-extension-<version>.zip
herdr-mcp-extension-<version>.zip.sha256
```

扩展 zip 是 Release asset；它不进入 binary updater 的 `release-manifest.json`，所以扩展与 Rust binary 可以共享 Release，而不混淆 binary updater 契约。

2. 校验 sidecar 后解压到稳定托管目录：

```bash
mkdir -p ~/.config/herdr-mcp/extension
unzip herdr-mcp-extension-<version>.zip -d ~/.config/herdr-mcp/extension
# manifest.json 必须直接位于这个目录下
```

3. 打开：

```text
chrome://extensions
```

开启开发者模式 → **加载已解压的扩展程序** → 选择：

```text
~/.config/herdr-mcp/extension
```

4. 安装 Native Messaging host：

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

迁移期仓库脚本仍提供：

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

5. 打开 ChatGPT、z.ai、DeepSeek 或其它当前支持页面。
6. 点击浏览器工具栏里的 Herdr 图标，确认 **浏览器控制中心**直接在 Chrome Side Panel 打开。
7. 在 **浏览器控制中心**确认“当前页面”识别到了正确 Project / conversation，然后直接在下方对应 workspace 行点击“绑定”；绑定状态和实时 workspace 状态在同一行显示。
8. 确认本机 runtime 在线、workspace / pane 状态实时可见；切换浏览器标签页时“当前页面”和 workspace 绑定高亮应自动切换，但明确固定的 Pinned Target 不会跟着改变。

### 开发者路径

开发者可以直接加载仓库里的：

```text
<repo>/extension/
```

但 unpacked extension 的身份与绝对加载路径有关，Native Messaging host 又会限制允许的 extension origin。因此开发时不要随意在多个 worktree 路径之间切换后继续沿用旧 native-host 注册。

终端用户主路径仍然是稳定托管目录，不是 clone 仓库。

## Binding、Pinned Target 和 Focus

浏览器产品现在有三个不同的“指向”概念。

### Workspace Binding

表示这个网页 Project / conversation 属于哪份本地工作上下文。

例如：

```text
ChatGPT Project → Herdr workspace wD7
```

binding 的对象是 workspace，不是某一个 agent。

真实开发现场通常是：

```text
workspace
 ├─ coding agent
 ├─ tests
 ├─ server
 └─ reviewer
```

### Pinned Target

只属于 Control Center。

表示下一条人工控制明确针对哪个 pane / Agent，例如：

```text
wD7:p2 / pi
```

Pinned Target 不会因为 Herdr focus 变化自动漂移。

### Herdr Focus

只是人此刻在 Herdr UI 中看的 pane。

它可以频繁变化，但不能偷偷替代 binding 或 pinned target。

详见 [浏览器控制中心](browser-control-center.md)。

## Continuity：让正确的网页会话继续

浏览器连续工作负责：

- workspace binding；
- working / progress / settled 回推；
- ChatGPT stale-view / disconnect / send-timeout 恢复；
- 页面健康有界自恢复；
- 长 conversation handoff / rollover；
- 按 Project 或 conversation scope 保存 Auto 状态。

### ChatGPT Project binding

ChatGPT Project 持久 binding 以稳定 `project_id` 为身份，而不是绑定某一个 conversation。

具体 `/c/<id>` 是当前投递目标 `active_conv_key`。只有真正激活对应 tab，才允许它成为新的 active target；后台打开 sibling conversation 不会偷走投递目标。

handoff 时 Project binding 与 `continuity_id` 不搬家，只有新 conversation 和 seed 都确认成功后才切换 active target。

### 自动化默认关闭

新作用域默认 `自动 关`。

开启 Auto 后，扩展才根据当前站点与作用域支持的能力执行 progress、settled、LLM continue、恢复、handoff 或页内权限卡处理。

自动化关闭不会删除 binding，也不会停止状态观察。

完整状态机见 [自动继续、恢复与接力](extension-wake.md)。

## Queue / 排队：不中断当前回复地补充下一轮要求

ChatGPT 正在生成时，用户经常会想到：

- “顺便跑一下 smoke test”；
- “这个先别发布”；
- “再检查一个边界条件”。

如果立即按发送，很容易打断当前 turn 或与正在发生的工具工作竞争。

扩展因此在 ChatGPT composer 的原生发送区域旁提供 **排队**。

行为是：

1. 在 composer 写好补充要求；
2. 点击“排队”；
3. 文本进入当前 conversation 的本地持久队列；
4. 当前 assistant turn 继续，不被打断；
5. turn settled 后，队列内容优先于通用 LLM auto-continue；
6. 多条内容按顺序用空行合并成一条下一轮用户消息；
7. 只有确认发送成功的 batch 才 ACK 删除；
8. turn-in-progress 等失败不会把队列静默丢掉。

其它约束：

- 队列有数量和长度上限，不是无限历史；
- 右键“排队”按钮可以清空当前 conversation 的队列；
- composer 为空时点击可以尝试重发仍在等待的 batch；
- handoff 成功后，未发送队列会迁移到目标 conversation，保持顺序。

排队表达的是**用户下一轮意图**，不是后台命令队列，也不会直接调用 Herdr mutation。

## Control Center：看清本机现场再决定下一步

点击 Herdr 工具栏图标会直接打开 Chrome Side Panel 里的 **浏览器控制中心**。

当前控制中心可以：

- 实时显示 workspace / pane lifecycle；
- 显示 Agent working / idle / done / blocked；
- 区分 terminal-only pane；
- 明确标记当前 Herdr focus；
- pin 一个明确 pane / agent target；
- 在 reconnect 后重新验证 target identity；
- target stale 时 fail closed；
- `查看状态`；
- `读取最近输出`（有界）；
- 对 **提示 Agent / 调整会话 / Herdr API / 终端输入**生成风险分类后的 Preview descriptor。

### 当前写操作仍是 Preview-only

页面会明确显示：

> 实时状态 · 控制操作仅预览

提示 Agent / 调整会话 / Herdr API / 终端输入当前都不会真正执行 mutation。

这是当前 Browser Control Plane Phase A 的边界，不是缺少一个 click handler。

详见 [浏览器控制中心](browser-control-center.md)。

## JSON → MCP bridge

z.ai、DeepSeek 等网页没有 ChatGPT 同类原生 MCP Connector 时，扩展可以提供受限兼容层：

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

- 获取本机 `tools/list`；
- 执行 `tools/call`；
- 回填 tool result；
- 在 assistant 最终返回普通答案前持续受控 tool loop；
- 折叠页面里的内部协议消息，减少视觉噪音。

它不是“伪装原生 MCP”，而是明确的网页协议适配层。

详见 [JSON → MCP bridge](extension-bridge.md)。

## 不同站点能力不同

不要把 ChatGPT 专属恢复能力假装成所有站点通用能力。

| 能力 | ChatGPT | z.ai / DeepSeek | Claude |
|---|---|---|---|
| workspace binding | 支持 | 支持 | 基础支持 |
| progress / settled | 支持 | 支持 | 依当前适配能力 |
| ChatGPT stale-view / send-timeout 恢复 | 支持 | 不适用 | 不适用 |
| Project-scoped binding / rollover | 支持 | 不适用 | 不适用 |
| conversation handoff | Project 支持 | 已落成 `/c/<id>` 支持 | 不适用 |
| Queue / 排队 | 支持 | 不适用 | 不适用 |
| JSON → MCP bridge | 不需要 | 支持 | 不需要同一路径 |
| Control Center | 浏览器级，共用本机 Herdr 状态 | 浏览器级 | 浏览器级 |

具体能力以当前 manifest、adapter 和测试为准。

## Options 与低频配置

Options 用于：

- en / zh / ja 界面语言；
- 本机 runtime 地址（兼容/诊断）；
- progress / fallback timing；
- settled / progress message template；
- 可选的 post-turn LLM judge；
- ChatGPT Project 自动化总 gate。

Options 中的小模型 API key 只保存在本机浏览器存储中；它不是 Herdr bearer，也不应该提交进仓库。

## 本机网络与浏览器权限

较新的 Chrome 可能把 loopback / 本机应用访问与普通 host 权限分开管理。

Native Messaging 是当前受信任主链路；某些诊断或兼容路径仍可能暴露 loopback 权限提示。

如果 Control Center / Options / HUD 显示本机不可达，先检查：

1. `herdr-mcp` runtime；
2. Native Messaging host；
3. 扩展是否重新加载；
4. 浏览器本机设备/loopback 权限。

不要把 `HERDR_MCP_TOKEN` 复制到扩展存储作为常规修复。

当前 GA 阶段 `host_permissions` 仍保留 `<all_urls>`。原因是：content-script 站点之外，可选 LLM judge 允许用户填写运行时未知的 OpenAI-compatible base URL，Options 也保留非默认本机 URL 的兼容能力。在可选权限 UX 产品化前，不应假装四个 content-script 域名已经覆盖所有主机需求。

这不是 Chrome Web Store 已上架声明。商店发布计划保留在维护者 WIP 文档中。

## 扩展不会做什么

扩展不会：

- 替代 ChatGPT / Web AI 推理；
- 替代 Herdr Agent；
- 因为 Side Panel 存在就开放任意 terminal mutation；
- 把 Herdr focus 当成 mutation target；
- 在 target stale 后猜一个新目标；
- 把本机高权限 bearer 暴露给网页；
- 为浏览器连续性开放公网 Herdr 控制端口；
- 绕过组织、浏览器或系统权限。

它负责的是**长期连接、可观察状态、明确控制边界和网页侧恢复**。

## 下一步读什么

- [浏览器连续工作：为什么 MCP 之后还需要一条回来的路](browser-continuity.md)
- [浏览器控制中心](browser-control-center.md)
- [自动继续、恢复与接力](extension-wake.md)
- [JSON → MCP bridge](extension-bridge.md)
- [故障排查](troubleshooting.md)
