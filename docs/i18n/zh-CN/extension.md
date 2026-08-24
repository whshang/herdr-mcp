# 浏览器扩展

读者：扩展作者与使用者。Chrome 显示名称 **herdr → Web wake**。`extension/` 保持一个清楚边界：负责把网页会话与本地 Herdr 工作持续连接起来；它不是新的 Agent runtime、记忆系统或调度平台。JSON→MCP 是独立的兼容支线。
语言：产品 UI 为 en / 简体中文 / 日本語（与 herdr 一致）；本页以简体中文写。仓库根 README：[en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md)。

| 主线 | 问题 | 方向 | 状态 | 首批站点 |
|---|---|---|---|---|
| **A. 网页连续工作** | 网页派活后会停住；回复可能超时；长对话最终需要换会话 | Herdr 状态观察 + 网页会话绑定 + 手动继续 + ChatGPT Project 自动化/恢复/接力 | **已可用**（当前 0.1.43 系列；全局运行模式 + Project 自动开关） | 绑定/观察：4 站；自动化/恢复/接力：ChatGPT Project |
| **B. JSON→MCP** | DeepSeek / z.ai 网页没有 MCP Connector | 网页 → 本机 `127.0.0.1:8772/mcp` | **未完成**（能抠 JSON，未调 MCP） | `chat.deepseek.com`、`chat.z.ai` |

共享：同一扩展、同一静态 token、同一 options。  
部署边界：扩展始终只访问本机 `127.0.0.1:8772` 的 `/push/*`（未来 B 才会访问本机 `/mcp`），不经过公网 Worker/Tunnel。因此 Cloudflare Edge、Custom Domain、contract epoch 的变更不要求扩展改 URL/OAuth。主线 A 与新版 server 兼容。ChatGPT 普通 `/c/<id>` 以 conversation id 标识；Project 会话以稳定的 `g-p-<resource-id> + conversation id` 标识，忽略 ChatGPT 可能追加的人类可读 Project slug。SPA 路由变化会自动重新注册，不要求刷新整页。

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
- chatgpt.com 页内权限卡可自动点「允许」，但必须同时满足 Options 为**项目自动**、当前 Project HUD 为 **`自动 开`**、且 Options 的「自动点击允许」已启用；全局手动或 Project `自动 关` 时只观察，不自动点击
- ChatGPT 回合结束后可用小模型判断是否继续（Options 配 Base URL + Key + Model；自动模式下执行；冷却与进度检查共用 `progressTickSec`）
- 回复长时间没有开始时可自动做恢复探测；探测仍失败且页面安全时最多做一次安全刷新，再失败可进入同一 Project 的接力
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
- 4 个站点继续共用 workspace 绑定、状态观察和 injector 基础能力；新的“项目自动”只对可识别稳定 `project_id` 的 ChatGPT Project 开放自动 mutation。普通 ChatGPT `/c/<id>`、Claude、DeepSeek、z.ai 在该模式下保持手动，不会因为全局选择“项目自动”而自动发送消息

### 全局运行模式与 Project HUD 自动开关

Options 只配置全局运行模式：

- **全局手动**：所有会话只允许手动推进。ChatGPT Project HUD 不显示自动开关，只显示三个手动按钮；自动进度/settled 唤醒、LLM 判断、恢复、自动接力和权限卡自动点击均停止，但状态观察和绑定继续。
- **项目自动**：只表示“允许 Project 使用自动化”，不会自动开启所有 Project。每个 ChatGPT Project 默认 `自动 关`，必须在该 Project 的 HUD 里显式打开。Project 开关以稳定 `project_id` 保存，因此同一 Project 的多个 conversation 以及接力后的新 conversation 共享同一设置。

在“项目自动”模式下，页面底部 HUD 顺序为：**运行状态 → 手动继续 → herdr监控 → LLM 分析 → 自动 开/自动 关 → 展开**；在“全局手动”模式下不显示自动开关。展开浮层只保留低频设置：**事件设置、会话绑定、高级选项**；浮层里不重复放手动按钮或自动化开关。

三个手动按钮在全局手动或当前 Project `自动 关` 时可用；当前 Project `自动 开` 时禁用，避免手动/自动重复推进：

- **手动继续**：直接向当前网页会话发送一次“继续”请求，不自动点击权限卡。
- **herdr监控**：先读取当前绑定 workspace 的 Herdr 窗口、pane、Agent 与运行状态，再把状态带回网页会话继续编排。
- **LLM 分析**：用 Options 里配置的小模型判断当前助手回复是否还需要继续；只有判断为继续时才提交继续消息。

Project `自动 开` 是当前 Project 的执行 gate。只有 Options 已选择“项目自动”时才显示。开启后允许自动执行：

1. Herdr `working` 期间按事件设置检查并回推进度；局部/全部 `settled` 后自动唤醒网页继续。
2. ChatGPT 回合结束后执行已配置的小模型判断，并在需要时继续。
3. 用户消息提交后约 30 秒仍没有可见回复时发送一次只读恢复探测；仍失败且编辑器、流式输出、工具、权限卡与投递状态都安全时，最多执行一次安全刷新。
4. 恢复探测 + 安全刷新仍失败时，在可接力的已绑定 ChatGPT Project 会话里进入同一 fail-closed handoff/cutover 流程。
5. 长对话上下文压力达到自动阈值时，在页面静止、无工具/流式输出、无不确定投递、workspace 已绑定且可接力的前提下自动开同一 Project 的新会话并迁移绑定。
6. 若 Options 的「自动点击允许」也开启，自动处理 ChatGPT 页面内明确的 Allow/允许权限卡；浏览器原生权限条仍不会自动点击。

Project `自动 关` 会阻止该 Project 的新自动 mutation；“全局手动”则在更高一层阻止所有 Project 的自动 mutation。两种情况下 **Herdr/workspace SSE、HUD 状态、会话绑定和实时 workspace catalog 仍持续观察**。从“全局手动”切回“项目自动”不会删除各 Project 已保存的开关偏好。

`workspace_id` 是绑定身份，`workspace_label` 只是展示缓存。HUD 和后续 wake 会优先用实时 workspace catalog 的 label；如果历史 binding 里同一个 `workspace_id` 保存了旧/错误名称，background 会自动修正持久化 label，避免浮层与底栏显示不同项目名。

### A2. 长对话压缩与接力（ChatGPT Project）

扩展从 0.1.39 起提供 fail-closed **接力 / Rollover**，当前 0.1.43 系列在同一个状态机上增加了保守的上下文压力自动触发、回复超时恢复触发和 Project 级自动化 gate。全局手动或当前 Project `自动 关` 时不会启动新的自动接力。

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

### 已有

- SpeaksJSON：从助手回复抠 `{"tool":"...","args":{}}`

### 缺口

- 调本地 MCP、结果回填、白名单与 Options

### 拟定（已定三阶段）

1. **协议**：解析 → `tools/call` → 回填（默认只读白名单）
2. **能力**：按需开 exec / 写文件 / prompt
3. **完整面**：对齐 ChatGPT 默认 18 工具（仍仅本地）

详见 [extension-bridge.md](./extension-bridge.md)。

## 不做

- 假装 DeepSeek「自带」OAuth connector
- 扩展默认走公网 Cloudflare MCP URL
- 用 B 线替代 A 线（ChatGPT 已有 connector，停住问题靠 A）

## 验收口诀

- **A**：`自动 开` 时，已绑定会话能从 Herdr working/settled、回合 LLM 判断、回复超时恢复和必要的 Project 接力继续工作；`自动 关` 时仍能实时看到状态，并可用「手动继续 / herdr监控 / LLM 分析」推进。
- **B**（尚未实现）：DeepSeek 输出一段 `herdr_inspect` JSON，扩展调本机并回填 `ok` 摘要。当前只能确认助手回复里出现了该 JSON。
