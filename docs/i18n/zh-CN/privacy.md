# 扩展隐私

*浏览器扩展的数据处理、权限与隐私政策。*

**生效日期：** 2026-08-28

本政策说明 Herdr 浏览器扩展如何处理用户数据，适用于 Herdr 项目通过 Chrome Web Store 分发的扩展，并应与[浏览器扩展产品文档](extension.md)一起阅读。

扩展的单一用途是：把受支持的 Web AI 对话连接到用户自己的本地 Herdr / herdr-mcp 工作站，让浏览器能够显示实时 workspace 状态、把对话绑定到 workspace、维持长任务和长对话连续性、排队下一轮用户消息，并通过 Chrome Side Panel 提供有界的恢复与控制界面。

## 扩展会处理哪些数据

为了实现上述用户可见功能，扩展在受支持的 Web AI 页面上可能处理：

- **网站内容与个人通信内容：** 为连续工作、排队消息、对话接力/恢复、可选 LLM 分析，以及下文所述的用户触发接力备用路径所需的对话文本和页面状态。
- **网页访问活动：** 当前受支持站点的 URL、由 URL 推导出的 conversation/project 标识，以及把当前页面与 Herdr workspace 对应起来所需的有限导航状态。扩展不会建立或出售通用浏览历史画像。
- **用户活动：** 回合生成/提交/结束/恢复时间、扩展开关状态，以及判断连续工作或恢复动作何时可以安全执行所需的有限交互状态。
- **认证信息：** 扩展不再保存或使用当前语义 Provider 的 API Key。升级时，扩展可能读取历史浏览器 Provider 配置，仅用于删除已经退役的浏览器凭证和权限。
- **本地 Herdr 状态：** 由本机 Herdr / herdr-mcp runtime 返回的 workspace、pane、agent、状态、输出尾部、绑定和固定目标等信息。

扩展不会为了自身功能主动请求或收集健康信息、金融/支付信息、精确位置，也不会建立广告画像。

## 数据存在哪里

扩展使用 `chrome.storage.local` 在用户自己的 Chrome Profile 中保存设置和连续性状态，包括：

- workspace / conversation 绑定；
- 已排队的下一轮消息；
- 自动化偏好、恢复预算与恢复状态；
- 固定的本地目标和语言设置；
- 本机 herdr-mcp endpoint 配置。

这些本地状态用于让 Manifest V3 service worker 或网页被 Chrome 挂起、刷新后能够安全恢复。发布者没有运行一个用于接收这些本地状态的扩展 Analytics / Telemetry 服务。

用户可以通过移除扩展或清除 Chrome 中对应的扩展/站点数据来删除这些本地数据。语义 Provider route 与凭证不存放在扩展配置中：单机值只放在 mode-`0600` 的 herdr-mcp `config.json`，全局 fallback 值放在已认证 Cloudflare Worker 的共享 route pool。

## 数据会发往哪里

扩展只会为了明确的用户可见功能与以下目标通信：

1. **同一台电脑上的本地 Herdr / herdr-mcp。** 通过 Native Messaging 向已安装的 native host 发送有界请求并读取实时 workspace 状态；这部分通信留在用户自己的电脑上。
2. **受支持与实验性的 Web AI 网站。** 扩展只在产品文档声明的浏览器页面上运行，用于观察当前对话状态并执行用户可见的连续工作/恢复交互。ChatGPT 是主要支持面，Claude 使用文档所述适配器；z.ai 与 DeepSeek 属于实验性集成，默认关闭，只有用户在 Herdr 设置中显式开启对应开关并授予 Chrome 对该精确站点的访问权限后才会注册其 content script。
3. **通过 herdr-mcp 配置的语义 route。** 扩展只把有界语义输入发送给本机 herdr-mcp Runtime。Runtime 可以把 typed evaluation 路由到用户配置的 TypeSafe System One、OpenRouter Decisions 或 Vercel Evaluation，也可以把有界 Goal Supervisor / handoff fallback 文本路由到用户配置的 OpenAI-compatible chat endpoint。普通 Auto 可包含有界的最新 user/assistant 回合；Goal 模式还可包含有界 objective/open TODO/runtime 摘要与近期 user/assistant 文本；handoff fallback 可包含有界的源会话 transcript（当前最多 70,000 字符，需要截断时保留早期任务背景和近期操作状态）。Planning、Work Memory 与 Parent orchestration boundary 也会通过同一 Runtime route pool 发送各自有界的结构化输入：planning 使用兼容 Agent 候选，attention 使用冻结的 child-task summary，validation 使用冻结的检查候选，optional closeout 使用有界近期 Agent 文本，cleanup triage 使用已确定计算出的 cleanup safety feature。语义结果始终只作辅助判断，不替代底层 task/Git/validation/cleanup 事实。Route 可以配置在本机，也可以配置在已注册的 Cloudflare Worker。使用 Worker route 时，Provider 凭证保留在 Edge，不返回扩展或 workstation，只返回有界语义/chat 结果。Provider endpoint 由用户选择，各 Provider 自身的隐私与数据保留条款适用。

扩展不会出售用户数据，不会把用户数据发送给广告网络，也不会为了无关画像、信用评估或放贷目的转移数据。

## 权限与远程代码

扩展申请的 Chrome 权限用于上述功能：

- `storage` — 保存本地设置和连续性状态；
- `scripting` — 在受支持的 Web AI 标签页因 MV3/页面刷新丢失脚本后恢复或重新注入**扩展包内自带**的 content script，并执行有界的浏览器连续性动作；
- `alarms` — 周期性唤醒 MV3 service worker，使 Chrome 挂起 worker 后能够恢复本地 Herdr 状态流和 timer；
- `nativeMessaging` — 连接本机安装的 herdr-mcp native host；
- `sidePanel` — 承载 Herdr Browser Control Center；
- host access — 常驻访问仅限文档声明的 ChatGPT/Claude 页面与本机 herdr-mcp endpoint。实验性的 z.ai/DeepSeek 只有用户显式开启对应集成后才申请 Chrome optional host permission。语义 Provider 由 Runtime/Edge 访问，扩展不直接连接这些 Provider，因此 Provider route 不需要 Chrome host permission；Herdr 不把 `<all_urls>` 作为常驻 host permission。

**扩展不使用远程可执行代码。** 所有可执行 JavaScript 都随扩展包发布；网络返回内容只按数据处理，不会被 `eval`、动态 import 或作为 JavaScript / Wasm 执行。

## Limited Use

通过 Chrome API 获得的信息仅按 Chrome Web Store User Data Policy（包括 Limited Use 要求）处理：

- 用户数据只用于提供或改进扩展的单一用途与用户可见功能；
- 除为这些用户可见功能所必要或政策允许的情况外，不出售或转移用户数据；
- 不把用户数据用于个性化、兴趣定向或重定向广告；
- 不把用户数据用于信用评估或放贷；
- 发布者不会允许人员读取用户的扩展数据，除非用户明确要求针对特定数据提供支持，或出于安全/法律合规所必需。

Chrome Web Store 政策参考：<https://developer.chrome.com/docs/webstore/user_data>

## 安全

扩展发起的公网连接在适用场景使用 HTTPS/WSS；浏览器扩展与同一台电脑上 native program 之间的 Native Messaging 保持本地。语义 Provider secret 只保存在 mode-`0600` 的单机 `config.json` 或已认证 Cloudflare Worker 的全局 route pool 中，扩展不把它们作为活动配置保存；这些秘密不会被有意写入项目仓库或发布者 Telemetry。

## 政策变更

如果扩展未来的行为发生会实质改变数据处理方式的变化，本政策和 Chrome Web Store 的 Privacy disclosures 会在该行为发布前同步更新。

## 联系与支持

项目主页：<https://whshang.github.io/herdr-mcp/>

支持与问题反馈：<https://github.com/whshang/herdr-mcp/issues>
