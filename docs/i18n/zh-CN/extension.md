# 浏览器扩展：连续工作、控制中心与本机桥接

herdr-mcp 浏览器扩展不是第二个 Agent runtime。它给已经可用的 herdr-mcp Connector 增加浏览器侧的长期工作能力：会话连续性、Side Panel 控制中心、workspace binding，以及当前回复结束后再发送的“排队”消息。

**先完成 runtime + ChatGPT Connector，再安装浏览器扩展。** 第一次把 ChatGPT 连到工作站并不需要扩展。

数据处理方式和权限用途见[浏览器扩展隐私政策](privacy.md)。

## 安装身份：STORE / STANDALONE / DEV

浏览器扩展的身份模型与 Runtime DEV/PROD 分开：

| 通道 | 用途 | Chromium 身份 |
| --- | --- | --- |
| **STORE** | 普通用户默认 | Chrome Web Store 固定身份，由商店更新 |
| **STANDALONE** | v0.4.3+ GitHub / 手动独立分发 | 固定非 Store 身份；安装路径变化不改变 ID |
| **DEV** | 源码开发 | repo/worktree `extension/` Load unpacked；ID 由路径派生 |

当前 stable v0.4.2 的 Native Host contract 只有 Store/DEV；v0.4.3 才加入 standalone。不要重打/移动 v0.4.2 tag，也不要用路径派生 DEV 版本冒充 standalone。

STORE 路径：安装 [Herdr Chrome Web Store 官方扩展](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp)。STANDALONE 路径：只使用 v0.4.3+ 正式发布的固定身份 package。DEV 路径：仅贡献者/扩展开发时打开 `chrome://extensions` → Developer mode → **Load unpacked** 并选择明确的 repo/worktree `extension/`。

安装/选择通道后运行：

```bash
herdr-mcp native-host status
```

要求 active channel / extension identity 与所选通道一致，并确认 Native Host runtime 与当前 runtime generation 一致。之后打开 ChatGPT 或其它当前支持页面；z.ai 与 DeepSeek 属于实验性集成，默认关闭，需要在 **Herdr 设置 → 实验性功能** 中分别开启并刷新页面。点击工具栏 Herdr 图标，确认控制中心在 Chrome Side Panel 打开，再核对当前 Project / conversation 与 workspace binding。

## 更新方式

- **STORE**：由 Chrome Web Store 正常更新。
- **STANDALONE**：由 GitHub/独立发布面提供新的固定身份 package；不会因为解压目录变化而变成新的浏览器身份。
- **DEV**：跟随所加载 repo/worktree 源码，由开发者显式 Reload。

扩展更新后，如果某个已经打开很久的 ChatGPT 页面仍运行旧 content script，刷新该网页即可让页面使用新版本。Chrome 扩展版本与 Rust runtime 版本独立；纯 UI / DOM / browser compatibility 修复不要求发布新的 Rust runtime，但新增 Native Host identity/channel contract 时必须使用实际支持该 contract 的 runtime。

## 三个产品面

| 产品面 | 解决的问题 | 主要入口 |
|---|---|---|
| Continuity | 本地工作发生变化后，让正确的网页会话继续、恢复或接力 | 页面内 HUD |
| Control Center | 查看 workspace / pane / Agent，绑定当前网页并固定明确目标 | Chrome Side Panel |
| JSON → MCP bridge | 为没有原生 MCP Connector 的 Web AI 提供实验性的有界本机桥接，默认关闭 | z.ai / DeepSeek 页面内 |

ChatGPT composer 旁的 **“排队”** 表示“等当前回复结束后，再把这条用户意图作为下一轮发出去”，不会中断正在生成的回复。

## 入口分工

| 入口 | 用来做什么 |
|---|---|
| 工具栏图标 | 打开 Chrome Side Panel 浏览器控制中心 |
| HUD | 当前页面紧凑状态、Auto、手动继续、提取 Herdr 状态、LLM 判断 |
| Control Center | 页面识别、workspace binding、handoff、workspace / pane / Agent 观察、Pinned Target |
| Queue / 排队 | 当前回复结束后发送明确下一轮用户消息 |
| Options | 语言、continuity timing、可选 LLM judge 等低频配置 |

绑定 / 解绑与本地 Herdr 控制统一放在 Control Center；手动接力放在紧凑 HUD，因为它直接作用于当前网页会话。HUD 仍不复制 Side Panel 的绑定或本地控制 UI。

## 安全架构

扩展不会把 Herdr bearer 放进网页脚本、service worker 或浏览器存储。

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
- Cloudflare Edge 不是浏览器扩展读取本机状态的必经路径；
- 公网 OAuth/MCP 与本机 Native Messaging 是不同安全边界。

## Binding、Pinned Target 和 Focus

### Workspace Binding

表示网页 Project / conversation 属于哪份本地工作上下文，例如：

```text
ChatGPT Project → Herdr workspace wD7
```

binding 的对象是 workspace，不是某一个 agent。

对于已绑定的 ChatGPT Project，用户手动新开会话后不需要复制内部 continuity ID；直接说“继续 / 接着上次”会先走本机 Continuity Journal 的搜索。只有稳定身份唯一命中时才自动恢复，存在歧义就要求用户确认。完整 search / confirm / resume 规则见 [浏览器连续工作](browser-continuity.md)。

### Pinned Target

只属于 Control Center，表示下一条人工控制明确针对哪个 pane / Agent，例如：

```text
wD7:p2 / pi
```

Pinned Target 不会因为 Herdr focus 变化自动漂移。

### Herdr Focus

只是人此刻在 Herdr UI 中看的 pane，可以频繁变化，但不能偷偷替代 binding 或 pinned target。

## 第一次使用

1. 先确认 `herdr-mcp doctor` 健康；
2. 按上面的 STORE / STANDALONE / DEV 规则选择并安装扩展；
3. 用 runtime 支持的 Native Host 命令激活所选通道，再确认 `herdr-mcp native-host status`；
4. 打开 ChatGPT Project / conversation；
5. 打开浏览器控制中心；
6. 从对应 workspace 行完成绑定；
7. 先保持 Auto 关闭，观察状态是否正确；
8. 熟悉后再开启自动连续工作。

## 自动连续工作

扩展会记录 conversation / Project 绑定、完成状态和有界恢复预算。目标是让长任务完成后网页会话能够恢复，而不是无限刷新或重复提交。

关键原则：

- 不因为页面没滚到底部而停止检测；
- 不在回复仍生成时强发下一条普通消息；
- 429 只退避，不触发重试/刷新风暴；
- 页面与服务端状态不一致时，优先安全 reload 同步已有结果，而不是重新执行原任务；
- 恢复预算耗尽后显式失败/建议接力，不无限循环。

细节见 [浏览器连续工作](browser-continuity.md) 与 [自动继续 / 恢复](browser-continuity.md)。

## 开发者、Standalone 与 Store 发布

DEV 源码加载、STANDALONE 固定身份 package、Chrome Web Store Developer Dashboard/审核属于三个不同发布路径。扩展版本生命周期独立于 Rust runtime，但 Native Host channel contract 必须与实际 runtime 能力匹配。

维护者请使用：

- `contracts/browser-extension-store.json` 作为 Chrome Web Store 身份的唯一机器可读 SSOT；Rust 只读取和校验这个 contract，不在源码里硬编码 Store ID；
- v0.4.3 的 `contracts/browser-extension-standalone.json` 作为 Standalone 固定身份 SSOT；打包时只注入公开 manifest key，DEV 源 `extension/manifest.json` 继续不带固定 `key`；
- `herdr-mcp native-host dev enable [PATH]` 登记并激活一个 unpacked Dev 身份（`PATH` 默认是 `./extension`）；
- `herdr-mcp native-host use store` / `herdr-mcp native-host use dev` 在不卸载另一份 Chrome 扩展的情况下切换唯一 active/default 浏览器 owner；
- `herdr-mcp native-host dev disable` 撤销 Dev 身份并让 Store 回到 active；
- `HERDR_EXTENSION_PATH=/path/to/unpacked/extension herdr-mcp native-host install` 只保留为旧维护者流程的兼容写法；
- `native-host dev enable` 只配置 Dev identity / Native Host trust / active owner，**不会**把 unpacked 扩展静默安装进正式 Chrome。Chrome 137+ branded build 已移除 `--load-extension`，139+ 又移除 `--disable-extensions-except`；因此 clone 后首次加载请打开 `chrome://extensions` → Developer mode → **Load unpacked** 并选择 `extension/`。自动化 smoke 使用 Chrome for Testing 或 Chromium；Chrome 146+ 的 CfT Native Messaging 目录为 `~/Library/Application Support/Google/ChromeForTesting/NativeMessagingHosts/`，`0.4.2` 会在该浏览器目录存在时作为 optional target 管理。
- `docs/_wip/browser-extension-development-and-store-release.md` 维护商店流程；
- `AGENTS.md` 中的 extension 验证与发布边界。

STORE / STANDALONE / DEV 可以作为不同 Chrome 扩展身份共存，但受管 Native Messaging manifest 始终只有一个精确的 `allowed_origins`：当前 active owner。非 active build 保持安装但进入 standby，不启动本地 shared stream，也不渲染 operational HUD。切换 active owner 后，Rust Native Host 继续通过受管 origin fence 撤销旧 build 已打开的 Native Messaging request/stream，避免旧 persistent connection 跨通道继续保有本地控制权。只有 DEV 是路径派生身份；移动 unpacked DEV 目录后必须重新登记。

切换 `native-host use store` / `use standalone` / `use dev` 后，请刷新已经打开的受支持 Web AI 页面。page-owner gate 在 content script 注入最前面决定页面归属；刷新后 newly-active build 接管页面，inactive sibling 在注册页面监听器或 HUD/Queue UI 前退出。`use standalone` 只在实际支持该命令的 v0.4.3+ runtime 上使用；v0.4.2 不伪装支持。

普通用户默认仍优先 STORE；STANDALONE 是正式的独立分发路径，不再被归类为开发版。

## 相关文档

- [浏览器控制中心](browser-control-center.md)
- [浏览器连续工作](browser-continuity.md)
- [自动继续 / 恢复](browser-continuity.md)
- [浏览器桥接](extension.md)
- [安装](install.md)

## 高级本地桥接架构

> **职责：** 实验性 JSON → MCP 兼容桥的高级参考。大多数用户不需要本页。

ChatGPT 可以通过自定义 MCP Connector 直接调用 herdr-mcp，但不是所有网页 AI 都提供同类能力。z.ai / DeepSeek 的网页会话可以很好地推理，却没有标准入口把本机 Herdr 工具注册进去。

JSON → MCP bridge 解决的就是这个兼容问题。

它不假装目标网站“原生支持 MCP”，也不把本机凭据交给页面 JavaScript。网页模型只负责输出受约束的 JSON 工具请求，真正的 MCP 调用由扩展和本机 trusted host 完成。

## 完整链路

```text
用户任务
  ↓
z.ai / DeepSeek Web model
  │ 输出受约束 JSON tool call
  ▼
content bridge
  ↓
extension service worker
  ↓ Chrome Native Messaging
native host
  ↓ Unix socket (0600)
herdr-mcp /mcp
  ↓
Herdr + files / Git / shell
  │
  └─ TOOL_RESULT 回填网页会话
```

整个工具执行仍发生在本机。Cloudflare Edge 不参与这条路径。

## 为什么不是直接让网页 JavaScript 请求 `127.0.0.1`

直接从网页脚本连接本机 MCP 会带来几类问题：

- 页面 origin 和浏览器权限模型限制；
- bearer 容易落进页面或扩展存储；
- 任意页面脚本可能试图复用本机高权限接口；
- 流式事件和 conversation identity 缺少统一控制层。

当前架构使用 Chrome Native Messaging，把浏览器侧能做的动作限制为明确的 request/stream 消息，再由 native host 通过权限为 `0600` 的 Unix socket 进入 runtime。

因此：

- 网页 JavaScript 看不到 Herdr bearer；
- extension service worker 也不需要长期保存 bearer；
- herdr-mcp runtime 仍是最终工具 schema、权限和 managed-root 闸门；
- local IPC 与公网 OAuth 是两套独立信任边界。

## 网页模型看到什么

Bridge 从本机实时 `tools/list` 获取工具 catalog，然后把必要的 typed schema 转成网页模型能够遵循的协议说明。

模型在需要调用工具时输出 JSON，例如：

```json
{"tool":"herdr_inspect","args":{}}
```

或：

```json
{"tool":"herdr_git","args":{"root":"/path/to/project","action":"status"}}
```

Bridge 解析后执行真实 MCP `tools/call`，再把 `TOOL_RESULT` 回填同一 conversation。网页模型根据结果决定下一步继续调用工具还是给用户正常答案。

## bounded tool loop

Bridge 不是把浏览器变成无限自治 Agent。每次工具循环都受状态、conversation identity 和调度边界约束。

一次逻辑流程是：

```text
assistant JSON calls
      ↓
validate
      ↓
execute MCP tools
      ↓
return TOOL_RESULT
      ↓
assistant reasons again
      ↓
JSON calls or normal answer
```

独立的同批调用可以并行；有依赖关系的步骤应继续串行。只有真实 `tools/call` 返回以后，网页模型才能把该工具视为成功。

## 结果为什么需要清洗

MCP result 可能包含：

- 很长的终端输出；
- image/binary 内容；
- structuredContent；
- 大段 base64 或其它网页模型不适合直接消费的字段。

Bridge 在回填前做长度限制和递归清洗，大型 binary/base64 字段会省略或摘要化。这不是改变工具事实，而是避免一轮结果把网页上下文淹没。

如果任务确实需要图片等富内容，优先让 Web planner选择适合的可见结果表达，而不是把原始二进制塞进文本 JSON。

## 中间协议消息为什么要折叠

JSON tool call / TOOL_RESULT 是机器协作记录，对人类阅读价值低，但长任务可能产生很多轮。

支持的站点会把这些内部消息折叠，让会话主线仍以“用户目标 → 最终解释/进展”为主。折叠只影响显示，不删除真实 conversation 内容。

## conversation identity

Bridge 必须知道“这次工具结果应该回到哪一个聊天”。

### z.ai

稳定 `/c/<chat_id>` URL 作为持久 conversation identity。根路径 `/` 是新聊天启动态，只能暂时保存启动期状态；第一次落成 `/c/<chat_id>` 后，临时 binding / Auto 偏好可以迁移一次。

之后从 `/c/A` 切到 `/c/B` 时，不会把 A 的 workspace binding 或自动化偏好误带到 B。

### DeepSeek

同样按稳定会话身份隔离 bridge state。页面 adapter 负责从当前站点路由/DOM 提取 identity，而不是把 tab id 当成长期会话 id。

## 页面刷新以后怎样继续未完成的 tool call

浏览器刷新不应该自动重跑所有历史 JSON。

恢复只在有充分上下文证据时进行：最后一条真实 conversation message 仍是 assistant 的 Herdr tool-call JSON，并且前文存在 bridge protocol context。这样才能判断“这是刚刚中断的工具步骤”，而不是用户打开了一段旧历史。

对于 mutation，恢复仍遵循 herdr-mcp 本身的 delivery/idempotency 规则。未知投递不能因为网页刷新就盲目执行第二次。

## 与浏览器 continuity 的关系

JSON → MCP 和 continuity 共用扩展与 Native Messaging transport，但解决不同问题。

| 能力 | 方向 | 目的 |
|---|---|---|
| JSON → MCP | 网页 → 本机 | 让没有原生 Connector 的 Web AI 调工具 |
| progress / settled | 本机 → 网页 | Agent 工作完成后推动会话继续 |
| recovery / handoff | 网页内部 | 恢复卡住的页面或切换长 conversation |

因此 z.ai 可以同时：

1. 用 JSON bridge 调 `herdr_fs_* / git / exec / prompt`；
2. 绑定同一个 Herdr workspace；
3. 在 Agent 长任务期间接收 progress / settled；
4. 必要时执行手动 handoff；`自动 开/关` 均可启动，目标会话继承源会话的 Auto 状态。

z.ai / DeepSeek 的会话 Auto 不意味着启用 ChatGPT 专属 stale-view 或自动 rollover。

## handoff 为什么必须绕过 JSON task wrapper

接力时旧会话需要生成摘要，新会话需要接收 seed。这些是**conversation control message**，不是“请调用 coding tools 完成一个业务任务”。

所以 z.ai handoff summary / seed 走 raw channel，明确绕过 JSON bridge。否则模型可能把“生成接力摘要”误包装成 Herdr coding task，形成错误递归。

## 安全边界

Bridge 的边界可以概括成：

- 只对明确支持的站点启用；
- 每次执行检查当前 site + conversation identity；
- tool catalog 来自本机真实 runtime，不维护另一份偷偷漂移的白名单；
- MCP 调用只经本机 trusted IPC；
- 浏览器不持有 Herdr bearer；
- 最终文件、Git、shell 权限仍由 herdr-mcp runtime gate 决定；
- 不通过 Cloudflare 把 extension 流量绕一圈公网；
- 不声称目标网站拥有官方 OAuth MCP 能力。

## 当前适用场景

它特别适合：

- 想使用 z.ai / DeepSeek Web 模型，但仍让它们操作自己的 Herdr 工作站；
- 不想再为每个网页站点实现一套本地开发 backend；
- 希望同一套 18-tool contract 和 managed-root 安全边界被多个 Web planner 复用。

如果目标客户端已经有可靠的原生 MCP Connector（例如 ChatGPT），优先使用原生 Connector；JSON bridge 是兼容层，不应该为了“统一”而替代更直接的标准路径。

## 验收

一条最小真实链路应该验证：

1. Bridge 能读取本机当前 `tools/list`；
2. 网页模型能生成合法 `herdr_inspect` JSON；
3. native host 成功执行 MCP tool；
4. `TOOL_RESULT` 回填当前 conversation，而不是其它 tab / chat；
5. 模型能根据结果继续第二个 tool call 或正常回答；
6. 页面刷新后不会重复执行已经完成的 mutation；
7. workspace binding / progress continuity 与 JSON tool loop 可以同时工作。

实现级 selector 和版本演进记录放在测试与 [CHANGELOG](../../../CHANGELOG.md)，本页只描述当前协议与安全边界。
