# 浏览器扩展：连续工作、控制中心与实验性本机桥接

herdr-mcp 浏览器扩展是已经可用的 MCP Connector 之上的**可选浏览器层**，不是第二个 Agent runtime，也不是第一次连接工作站的前置条件。

它只负责三类浏览器侧问题：

| 产品面 | 解决的问题 | 详细文档 |
| --- | --- | --- |
| Continuity | 本地工作完成后，如何让正确的网页会话恢复、继续或接力 | [浏览器连续工作](browser-continuity.md) |
| Control Center | 如何在 Chrome Side Panel 观察 workspace / pane / Agent，并管理 binding / pinned target | [浏览器控制中心](browser-control-center.md) |
| JSON → MCP bridge | 没有原生 MCP Connector 的 Web AI 如何通过有界 JSON 协议使用本机工具 | [JSON → MCP 桥](browser-json-mcp-bridge.md) |

ChatGPT composer 旁的“排队”属于浏览器交互：它等待当前回复结束，再把明确的下一轮用户意图发出去，不会中断正在生成的回复。

数据处理与权限用途见 [浏览器扩展隐私政策](privacy.md)。

## 安装身份：STORE / STANDALONE / DEV

扩展身份与 Runtime DEV/PROD 是两套独立通道：

| 通道 | 用途 | Chromium 身份 |
| --- | --- | --- |
| **STORE** | 普通用户默认 | Chrome Web Store 固定身份，由商店更新 |
| **STANDALONE** | v0.4.3+ GitHub / 手动独立分发 | 固定非 Store 身份，安装路径变化不改变 ID |
| **DEV** | 源码开发 | repo/worktree `extension/` Load unpacked，ID 由路径派生 |

stable v0.4.2 的 Native Host contract 只有 STORE/DEV；STANDALONE 需要实际支持该 contract 的 v0.4.3+ runtime。不要用路径派生 DEV build 冒充 standalone。

默认安装 [Herdr Chrome Web Store 官方扩展](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp)。只有 Store 不适用且 runtime 明确支持时才选 STANDALONE；DEV 只用于源码开发。

选择通道后验证：

```bash
herdr-mcp native-host status
```

要求 active channel、extension identity、Native Host 和当前 runtime generation 一致。STORE 由商店更新；STANDALONE 由正式独立 package 更新；DEV 由开发者显式 Reload。旧网页仍运行旧 content script 时，刷新页面即可。

## 入口与状态对象

| 概念 / 入口 | 唯一职责 |
| --- | --- |
| 工具栏图标 | 打开 Side Panel Control Center |
| HUD | 当前页面紧凑状态、Auto、手动继续/接力 |
| Control Center | workspace binding、Pinned Target、本地现场观察与人工控制 |
| Queue / 排队 | 当前回复结束后发送下一轮明确用户消息 |
| Workspace Binding | 当前 Project / conversation 属于哪个长期 workspace |
| Pinned Target | 下一条人工控制明确针对哪个 pane / Agent |
| Herdr Focus | 人此刻在 Herdr UI 里看的 pane；不能偷偷替代 binding 或 pinned target |

这些状态为什么必须分开、如何恢复和接力，由 [浏览器连续工作](browser-continuity.md) 与 [浏览器控制中心](browser-control-center.md) 分别作为 SSOT 说明，本页不再重复实现细节。

## 本机安全边界

扩展不会把 Herdr bearer 放进网页脚本、service worker 或浏览器存储：

```text
网页内容脚本 / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
本机 Host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

浏览器负责页面交互和可视化；Native Host 是受信任本机桥；runtime 继续负责工具 schema、managed-root、权限和 mutation 边界。公网 OAuth/MCP 与本机 Native Messaging 是两个独立信任边界。

## 第一次使用

1. 先确认 runtime + ChatGPT Connector 已经工作；
2. 选择 STORE / STANDALONE / DEV，并验证 `herdr-mcp native-host status`；
3. 打开受支持网页和 Side Panel；
4. 完成 workspace binding；
5. 保持 Auto 关闭，先核对状态、Pinned Target 与人工操作；
6. 只有需要长时间无人值守时再按作用域开启 Continuity 自动化。

z.ai / DeepSeek 的 JSON → MCP 属于实验性集成，默认关闭，需要在 Herdr 设置的实验性功能中显式开启。

## 发布与维护边界

STORE / STANDALONE / DEV 可以作为不同扩展身份共存，但受管 Native Messaging manifest 只有一个 active owner。Store 身份以 `contracts/browser-extension-store.json` 为机器可读 SSOT；v0.4.3 Standalone 身份以 `contracts/browser-extension-standalone.json` 为 SSOT；DEV 继续使用路径派生身份。

切换 `native-host use store` / `use standalone` / `use dev` 后刷新已经打开的受支持页面。扩展版本生命周期独立于 Rust runtime；只有新增 Native Host identity/channel contract 时才要求相应 runtime 能力。

维护者的详细发布流程见 `docs/_wip/browser-extension-development-and-store-release.md` 和 `AGENTS.md`。
