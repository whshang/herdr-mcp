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

细节见 [浏览器连续工作](browser-continuity.md) 与 [自动继续 / 恢复](extension-wake.md)。

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
- [自动继续 / 恢复](extension-wake.md)
- [浏览器桥接](extension-bridge.md)
- [安装](install.md)
