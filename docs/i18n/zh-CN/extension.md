# 浏览器扩展：连续工作、控制中心与本机桥接

herdr-mcp 浏览器扩展不是第二个 Agent runtime。它给已经可用的 herdr-mcp Connector 增加浏览器侧的长期工作能力：会话连续性、Side Panel 控制中心、workspace binding，以及当前回复结束后再发送的“排队”消息。

**先完成 runtime + ChatGPT Connector，再安装浏览器扩展。** 第一次把 ChatGPT 连到工作站并不需要扩展。

数据处理方式和权限用途见[浏览器扩展隐私政策](privacy.md)。

## 最终用户安装：只用 Chrome Web Store

最终用户不需要 git clone，不需要下载 extension zip，也不需要开启 Chrome Developer mode。

1. 打开 [Chrome Web Store](https://chromewebstore.google.com/)；
2. 搜索 `Herdr`，选择 Herdr 官方扩展；
3. 点击 **添加至 Chrome / Add to Chrome**；
4. 安装后在终端运行：

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

5. 打开 ChatGPT、z.ai、DeepSeek 或其它当前支持页面；
6. 点击浏览器工具栏里的 Herdr 图标，确认 **浏览器控制中心**直接在 Chrome Side Panel 打开；
7. 在控制中心确认“当前页面”识别到了正确 Project / conversation，再绑定 Herdr workspace。

> 当前扩展正在进入 Chrome Web Store 首次发布流程。在正式 listing 上线前，普通用户可以跳过扩展；不要把 `Load unpacked` 当作最终用户安装方式。

## 更新方式

通过 Chrome Web Store 安装后，扩展更新走 Chrome 的正常 Web Store 更新机制。普通用户不需要：

- 重新下载 `herdr-mcp-extension-*.zip`；
- 覆盖 `~/.config/herdr-mcp/extension`；
- 打开开发者模式；
- 每次版本升级手工点扩展 Reload。

扩展更新后，如果某个已经打开很久的 ChatGPT 页面仍运行旧 content script，刷新该网页即可让页面使用新版本。Chrome 扩展本身的版本更新与 Rust runtime 版本独立；纯 UI / DOM / browser compatibility 修复不要求发布新的 Rust runtime。

## 三个产品面

| 产品面 | 解决的问题 | 主要入口 |
|---|---|---|
| Continuity | 本地工作发生变化后，让正确的网页会话继续、恢复或接力 | 页面内 HUD |
| Control Center | 查看 workspace / pane / Agent，绑定当前网页并固定明确目标 | Chrome Side Panel |
| JSON → MCP bridge | 为没有原生 MCP Connector 的 Web AI 提供有界本机桥接 | z.ai / DeepSeek 页面内 |

ChatGPT composer 旁的 **“排队”** 表示“等当前回复结束后，再把这条用户意图作为下一轮发出去”，不会中断正在生成的回复。

## 入口分工

| 入口 | 用来做什么 |
|---|---|
| 工具栏图标 | 打开 Chrome Side Panel 浏览器控制中心 |
| HUD | 当前页面紧凑状态、Auto、手动继续、提取 Herdr 状态、LLM 判断 |
| Control Center | 页面识别、workspace binding、handoff、workspace / pane / Agent 观察、Pinned Target |
| Queue / 排队 | 当前回复结束后发送明确下一轮用户消息 |
| Options | 语言、continuity timing、可选 LLM judge 等低频配置 |

绑定 / 解绑与手动接力统一放在 Control Center；HUD 保持紧凑，不重复 Side Panel 的管理 UI。

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
2. 从 Chrome Web Store 安装扩展；
3. 运行 `herdr-mcp native-host install`；
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

## 开发者与商店发布

`Load unpacked`、本地 extension checkout、Chrome Web Store Developer Dashboard、包上传、Trusted Testers、商店素材和审核流程属于**维护者/扩展开发流程**，不属于最终用户安装文档。

维护者请看：

- `docs/_wip/browser-extension-development-and-store-release.md`
- `AGENTS.md` 中的 extension 验证与发布边界

正式商店发布后，最终用户文档只保留 Chrome Web Store 安装路径。

## 相关文档

- [浏览器控制中心](browser-control-center.md)
- [浏览器连续工作](browser-continuity.md)
- [自动继续 / 恢复](extension-wake.md)
- [浏览器桥接](extension-bridge.md)
- [安装](install.md)
