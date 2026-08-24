# 浏览器扩展

读者：扩展作者与使用者。Chrome 显示名称 **herdr → Web wake**。`extension/` 保持一个清楚边界：负责把网页会话与本地 Herdr 工作持续连接起来；它不是新的 Agent runtime、记忆系统或调度平台。JSON→MCP 是独立的兼容支线。
语言：产品 UI 为 en / 简体中文 / 日本語（与 herdr 一致）；本页以简体中文写。仓库根 README：[en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md)。

| 主线 | 问题 | 方向 | 状态 | 首批站点 |
|---|---|---|---|---|
| **A. 网页连续工作** | 网页派活后会停住；回复可能超时或只显示半截；长对话最终需要换会话 | Herdr 状态观察 + 网页会话绑定 + 手动继续 + 自动化 gate + 安全接力 | **已可用**（当前 0.1.48 系列） | 绑定/观察：4 站；ChatGPT Project 完整自动化；z.ai / DeepSeek 会话级进度自动化；手动接力：ChatGPT Project + z.ai `/c/<chat_id>` |
| **B. JSON→MCP** | DeepSeek / z.ai 网页没有 MCP Connector | 网页 → 扩展 service worker → 本机 `127.0.0.1:8772/mcp` | **已可用**（bounded `tools/list` / `tools/call` loop） | `chat.deepseek.com`、`chat.z.ai` |

共享：同一扩展、同一 localhost transport、同一短期凭据 broker、同一 options。
部署边界：扩展始终只访问本机 `127.0.0.1:8772` 的 `/push/*` 与 `/mcp`，不经过公网 Worker/Tunnel；当前安装通过 Chrome Native Messaging host 获取短期 bearer，长期 `HERDR_MCP_TOKEN` 不进入扩展存储，网页 JavaScript 也拿不到 bearer；可选兼容 token 仅用于旧版 fallback。因此 Cloudflare Edge、Custom Domain、contract epoch 的变更不要求扩展改 URL/OAuth。主线 A 与新版 server 兼容。ChatGPT 普通 `/c/<id>` 以 conversation id 标识；Project 会话以稳定的 `g-p-<resource-id> + conversation id` 标识，忽略 ChatGPT 可能追加的人类可读 Project slug。SPA 路由变化会自动重新注册，不要求刷新整页。

传输层只维持 **1 条全局 `/push/events` SSE**。所有 workspace 事件由 background 根据事件里的 `workspace` 字段分发到对应 binding；不能按 binding 各建一条 SSE，否则多个历史 binding 会耗尽浏览器对 `127.0.0.1:8772` 的 HTTP/1.1 连接池，导致 `/push/state` 与 `/push/mcp-activity` 永久排队。

Chrome 145+ 把本机回环访问拆成 `loopback-network`（界面显示为“设备上的应用”）权限。扩展仍声明 `http://127.0.0.1:8772/*` host permission，但部分 Chrome/Chromium profile 仍可能把 loopback 权限保留为“询问”。Options 的“测试连接”和页面 HUD 都使用 bounded 请求：若本机服务请求被权限门控，会明确提示进入“管理扩展程序 → 网站设置 → 设备上的应用 → 允许”，而不是无限加载。

分文档：[extension-wake.md](./extension-wake.md)（A）、[extension-bridge.md](./extension-bridge.md)（B）。

```text
┌─────────────┐  MCP Connector / JSON桥   ┌──────────┐
│ 网页 AI      │ ───────────────────────► │ herdr-mcp│──► herdr
│ ChatGPT 等   │ ◄── A 进度/收工回推 ──── │  /push   │
│ DeepSeek 等  │ ── B SpeaksJSON→/mcp ──► │  /mcp    │
└─────────────┘                           └──────────┘
```

## A. 网页连续工作

这一主线解决的不是“让插件替网页模型思考”，而是两个连续性问题：

1. **本地工作连续性**：Herdr workspace 的工作状态变化后，原网页对话能够被重新唤醒并继续编排。
2. **网页会话连续性**：ChatGPT Project 对话过长时，把当前工作状态压成一个小型接力包，在同一 Project 开新对话并迁移原 workspace 绑定。

### A1. 进度主动提醒（全站）

### 已有

- 绑定：popup 把「当前会话」绑到某个 herdr **workspace**（space 内多 agent 并行可回推）
- SSE：`/push/events?workspace=...`
- 策略：见过 `working` 之后；局部 settle 报进展，范围内全空闲才收工；`hello` 可补
- chatgpt.com 页内权限卡自动处理已并入 Project 自动化：Options 勾选**启用项目自动**且当前 Project HUD 为 **`自动 开`** 时才会自动点明确的「允许」；全局手动或 Project `自动 关` 时只观察，不自动点击
- ChatGPT 回合结束后可用小模型判断是否继续（Options 配 Base URL + Key + Model；自动模式下执行；冷却与进度检查共用 `progressTickSec`）
- 回复长时间没有开始时可自动做恢复探测；0.1.44 还会处理“回复已经开始但页面停在半截”的 stale-view：best-effort 比较 ChatGPT 同源 conversation snapshot 与页面最后 assistant message，只有确认服务端领先页面，或服务端未完成消息确实长期停滞，才进入刷新恢复
- ChatGPT Project 长对话按页面可见用户/助手文本做保守 token 估算；达到自动阈值且 workspace/页面均静止、安全条件满足时，复用同一 fail-closed handoff 状态机自动接力
- UI：en / 简体中文 / 日本語

### 缺口

1. ~~working 期间定时进度通报~~（已实现，见下）
2. 绑定摩擦：未绑定则回推为零
3. 无 agent 的 `herdr_exec` utility 不进 agent 状态机（可选后续；workspace 绑定也不覆盖 utility）
4. ~~ChatGPT 回合催促~~（扩展 ≥0.1.18：仅小模型判定；Options 预填提示词/不发送词；继续时提交模型原文）

### 已实现（主线 A 定时进度通报）

- Options：全局运行模式为**全局手动 / 项目自动**；`progressTickSec`（默认 `60` 秒：working 进度检查 + 自动 LLM 回合判断冷却；填 `0` 只关闭这两类**间隔驱动**动作，不改变全局模式或 Project 开关）、`progressFallbackSec`（默认 `1200` 秒无新摘要兜底）、进度模板与收工模板分开（`progressTemplate` / `wakeTemplate`）；界面语言 en / 简体中文 / 日本語
- 进度实发规则：检查点到了之后，仅当 herdr 侧摘要相对上次实发有**新的非空内容**才往网页灌一条；否则满 `progressFallbackSec` 才兜底一条，避免空转刷屏
- `working`：每 `progressTickSec` **检查**摘要；仅新非空内容或满 `progressFallbackSec` 才往绑定会话灌进度并提交；同一 convKey 只一个定时器，重复 `working` 不丢已有实发基线
- popup 列表按 **workspace** 展示（含仅终端、无 running agent 的窗格）；标题一行 + 窗格统计一行，不重复项目名、不列 `agent@pane`
- `settled`：先取消 tick 定时器，再按收工模板唤醒一次
- 4 个站点继续共用 workspace 绑定、状态观察和 injector 基础能力。Options 的“项目自动”现在是全局自动化许可：ChatGPT Project 用稳定 `project_id` 保存开关；z.ai / DeepSeek 用具体 conversation key 保存开关。z.ai / DeepSeek 的 `自动 开` 只开放 Herdr progress/settled 自动回推，不启用 ChatGPT 专属 stale-view、回合 LLM 判断或自动 rollover。

### 全局运行模式与 Project HUD 自动开关

Options 只配置全局运行模式：

- **全局手动**：所有会话只允许手动推进，HUD 不显示自动开关；状态观察、binding 和支持站点的手动操作继续存在。
- **项目自动**：表示“允许当前支持范围使用自动化”，但不会自动开启任何 Project/会话。ChatGPT Project 默认 `自动 关` 并以稳定 `project_id` 保存；z.ai / DeepSeek 也默认关闭，但按当前 conversation 保存。从 0.1.48 起，z.ai / DeepSeek 即使还没有绑定 workspace 也可以先切换并保存会话自动偏好；依赖 workspace 的 progress/settled 自动回推在绑定后生效。

在“项目自动”模式下，支持站点底部 HUD 顺序为：**运行状态 → 手动继续 → herdr监控 → LLM 分析 → 手动接力（若当前会话支持）→ 自动 开/自动 关 → 展开**；全局手动不显示自动开关。ChatGPT Project 与已落成 `/c/<chat_id>` 的 z.ai 会话可显示“手动接力”，z.ai 根页 `/` 与 DeepSeek 不显示。展开浮层只保留低频设置：**事件设置、会话绑定、高级选项**。

从 0.1.45 起，当前 Project 实际处于 **`自动 开`** 时，整条底部 HUD 使用浅绿色背景、绿色顶边与轻微绿色阴影作为常驻视觉提示；`自动 关` 或全局手动仍使用中性色。该绿色只表达“自动化已启用”，不会覆盖 `working / blocked / recovering / failed` 等运行状态的橙色或红色告警语义。深色模式使用低亮度绿色表面，避免长期常驻时刺眼。

四个人工操作在全局手动或当前作用域 `自动 关` 时按能力可用；一旦 `自动 开`，HUD 手动操作全部锁定，避免手动/自动重复推进：

- **手动继续**：直接向当前网页会话发送一次“继续”请求，不自动点击权限卡。
- **herdr监控**：先读取当前绑定 workspace 的 Herdr 窗口、pane、Agent 与运行状态，再把状态带回网页会话继续编排。
- **LLM 分析**：用 Options 里配置的小模型判断当前助手回复是否还需要继续；只有判断为继续时才提交继续消息。

**手动接力**支持已绑定 ChatGPT Project 与已落成 `/c/<chat_id>` 的 z.ai 会话，且必须先切到 `自动 关`。前端会锁定按钮，background 也会再次拒绝 `automation_enabled`，因此不能绕过 HUD 与自动流程并发接力。点击后仍复用同一 fail-closed handoff 状态机；z.ai 的 summary/seed 走 raw 通道绕过 JSON bridge。只有确认新会话 id 与 seed marker 后才迁移 workspace binding；workspace 仍在 `working` 时同样拒绝开始。已有 transfer 时按钮显示“压缩中… / 接力中… / 恢复接力”。

Project `自动 开` 是当前 Project 的执行 gate。只有 Options 已选择“项目自动”时才显示。开启后允许自动执行：

1. Herdr `working` 期间按事件设置检查并回推进度；局部/全部 `settled` 后自动唤醒网页继续。
2. ChatGPT 回合结束后执行已配置的小模型判断，并在需要时继续。
3. 用户消息提交后约 30 秒仍没有可见回复时发送一次只读恢复探测；对“已经有回复但 DOM 停在半截”的情况，会 best-effort 读取 ChatGPT 同源 conversation snapshot 的 `current_node / message id / status / update_time`，与页面最后一条 assistant message 对比。服务端明确领先页面时只刷新一次；服务端本身仍未完成则至少持续停滞 60 秒（页面仍声称 streaming 时更保守）才允许刷新。
4. stale-view 刷新后先重新比较内容：若页面已追上服务端或回复恢复增长，立即停止恢复；若仍是同一半截内容，则只发送一次“浏览器恢复”激活消息，要求重新读取当前会话、从实际停止处继续、不要重复已完成工作。若同源 snapshot 接口不可用，则该探测 fail-closed，不凭时间差盲刷新。
5. 恢复消息 + 安全刷新仍失败时，在可接力的已绑定 ChatGPT Project 会话里进入同一 fail-closed handoff/cutover 流程。
6. 长对话上下文压力达到自动阈值时，在页面静止、无工具/流式输出、无不确定投递、workspace 已绑定且可接力的前提下自动开同一 Project 的新会话并迁移绑定。
7. 自动处理 ChatGPT 页面内明确的 Allow/允许权限卡；该能力随当前 Project `自动 开` 一起启停，不再有独立开关。浏览器原生权限条仍不会自动点击。

作用域 `自动 关` 会阻止该 Project/会话的新自动 mutation；“全局手动”在更高一层阻止所有自动 mutation。两种情况下 **Herdr/workspace SSE、HUD 状态、会话绑定和实时 workspace catalog 仍持续观察**。从“全局手动”切回“项目自动”不会删除 ChatGPT Project 或 z.ai / DeepSeek conversation 已保存的开关偏好。

`workspace_id` 是绑定身份，`workspace_label` 只是展示缓存。HUD 和后续 wake 会优先用实时 workspace catalog 的 label；如果历史 binding 里同一个 `workspace_id` 保存了旧/错误名称，background 会自动修正持久化 label，避免浮层与底栏显示不同项目名。

### 页面新鲜度 / stale-view 恢复

0.1.44 把“ChatGPT 真没回复”和“服务端已经有更新、当前网页还停在旧 DOM”拆开判断。内容脚本同时记录人工发送和扩展自动发送的用户回合，并持续维护页面最后 assistant message 的 id、文本指纹与最近变化时间。进入恢复窗口后，它会 best-effort 请求当前 conversation 的同源 snapshot，沿 `current_node` 找到最新 assistant message，再比较 message id、文本、完成状态与更新时间。

- **server ahead**：服务端 message id 已更新，或同一消息的服务端文本明显长于页面 → 页面陈旧，安全条件满足时刷新一次。
- **server stalled**：服务端消息明确仍处于未完成状态，且至少 60 秒没有更新；如果页面仍显示 streaming，再多等 30 秒后才允许刷新。
- **synced**：服务端和页面一致且服务端已结束 → 不刷新，交给正常的 settled / LLM 判断处理。
- **unknown**：内部 snapshot 接口返回错误、超时或结构变化 → fail-closed，不因为拿不到证据就刷新。

刷新前会持久记录旧 assistant 指纹。重载后若内容变化或继续流式输出，恢复立即结束；若 10 秒后仍是同一半截内容且编辑器、工具、权限卡都空闲，则发送一次 `stale_view_activation_template`，要求从实际停止处继续且不重复完成过的 mutation。只有这一步也失败后，才进入原有 recovery-exhausted rollover。

### A2. 长对话压缩与接力（ChatGPT Project）

扩展从 0.1.39 起提供 fail-closed **接力 / Rollover**；0.1.43 加入 Project 级自动化 gate，0.1.44 接入 stale-view 恢复，0.1.46 把“手动接力”放到底部 HUD，0.1.47 将手动接力扩展到持久 z.ai chat 并要求先 `自动 关`。ChatGPT 的自动恢复/自动 rollover 仍是 Project 专属能力。

工作流：

```text
旧 Project 对话（仍绑定 workspace）
  -> 当前 ChatGPT 根据完整旧上下文生成带 transfer-id 的紧凑 handoff packet
  -> 扩展在同一个 ChatGPT Project 打开新的会话入口
  -> 把 handoff packet 作为新会话第一条用户消息提交
  -> 确认页面已经落到一个新的 /c/<conversation-id>，且该用户消息真的存在
  -> 才把 workspace binding 从旧 convKey 原子迁移到新 convKey
  -> 后续 Herdr wake 只发到新会话
```

这里的“压缩”是**语义接力**：新会话只接收当前工作所需的紧凑状态，因此不需要重新携带旧对话全部上下文。它不会、也不能修改 ChatGPT 产品内部的 context compaction 算法。

安全边界：

- 仅对**已经绑定 workspace 的 ChatGPT Project conversation**开放；普通聊天后续再决定是否支持。
- bound workspace 仍有 agent `working` 时拒绝接力，避免收工事件和切换目的地竞争。
- handoff 请求明确要求当前网页模型只总结，不继续实现、不调用工具；摘要只能使用旧对话已经建立的事实。
- 新会话接力消息明确要求任何 mutation 前重新验证 live Herdr/runtime/Git state，不能把摘要当成最新事实证明。
- **旧绑定始终是权威，直到新会话 seed 已确认。** 打开了新 tab、甚至尝试提交过 seed，都不足以切绑定。
- 若 seed 投递状态不确定，记录 `seed_uncertain`，旧绑定不动；用户显式点“恢复接力”后先探测目标消息，已存在则直接完成 cutover，不存在才明确重试。
- handoff packet 临时保存在 `chrome.storage.local`；成功 cutover 后立即从 transfer 记录清掉正文，只保留少量状态元数据用于恢复/诊断。
- workspace binding 从旧版的 24 小时自动过期改为显式持久绑定；当前只在用户解绑或成功接力时主动改变关系，不再因为时间流逝静默失效。

上下文压力使用页面能观察到的**用户/助手文本估算**，不是 ChatGPT 后端真实 token 计数，也不保存完整消息正文到压力记录。当前策略以约 `72k / 84k / 90k / 96k / 108k` 估算文本 token 依次进入 warning / prepare / recommend / auto-rollover / high-risk；只有达到 `96k` 以上并通过静止、安全、已绑定 Project、无活动 handoff 等闸门时才允许自动接力。消息数、回合数或会话年龄只能把状态提升到 warning，不会单独触发自动新会话。

## B. JSON→MCP（DeepSeek / z.ai）

### 已实现

- extension service worker 正常使用 Native Messaging 签发的短期 bearer 调本机 `/mcp` 的 `tools/list` / `tools/call`；网页脚本拿不到 bearer，可选静态 token 仅作为旧版兼容 fallback。
- z.ai / DeepSeek 内容脚本把普通用户任务转换为带 Herdr tool catalog 的协议回合，在 bounded round 内提取工具调用、执行并把结果回填给网页模型。
- 工具协议中间消息会折叠；handoff summary/seed 使用 raw 发送通道，不会被 JSON bridge 再包装。
- z.ai 1.1.88 使用 `.user-message` / `.markdown-prose`、真实 `#send-message-button` 和 `/c/<chat_id>` 作为稳定会话身份。

详见 [extension-bridge.md](./extension-bridge.md)。

## 不做

- 假装 DeepSeek「自带」OAuth connector
- 扩展默认走公网 Cloudflare MCP URL
- 用 B 线替代 A 线（ChatGPT 已有 connector，停住问题靠 A）

## 验收口诀

- **A**：ChatGPT Project `自动 开` 可执行完整 working/settled、LLM、stale-view、自动 rollover；z.ai / DeepSeek `自动 开` 只自动回推 working/settled。需要手动接力时先 `自动 关`；已绑定 ChatGPT Project 或持久 z.ai `/c/<chat_id>` 才可执行。
- **B**：DeepSeek / z.ai 普通用户任务可以通过 JSON bridge 驱动本机 Herdr MCP 工具，并在 bounded round 内把结果回填给网页模型。
