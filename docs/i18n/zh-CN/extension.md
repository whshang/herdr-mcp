# 浏览器扩展

读者：扩展作者与使用者。Chrome 显示名称 **herdr → Web wake**。`extension/` 保持一个清楚边界：负责把网页会话与本地 Herdr 工作持续连接起来；它不是新的 Agent runtime、记忆系统或调度平台。JSON→MCP 是独立的兼容支线。
语言：产品 UI 为 en / 简体中文 / 日本語（与 herdr 一致）；本页以简体中文写。仓库根 README：[en](../../../README.md) / [zh](../../../README.zh.md) / [ja](../../../README.ja.md)。

| 主线 | 问题 | 方向 | 状态 | 首批站点 |
|---|---|---|---|---|
| **A. 网页连续工作** | 网页派活后会停住；长对话最终需要换会话 | Herdr 状态回推 + 网页会话绑定 + ChatGPT Project 接力 | **已可用**（扩展 0.1.39；接力先手动触发） | 回推：4 站；接力：ChatGPT Project |
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
- chatgpt.com 权限卡常驻自动点「允许」
- ChatGPT 回合结束后可选小模型催促（Options 配 Base URL + Key + Model；`idleNudgeEnabled`；冷却与进度检查共用 `progressTickSec`）
- UI：en / 简体中文 / 日本語

### 缺口

1. ~~working 期间定时进度通报~~（已实现，见下）
2. 绑定摩擦：未绑定则回推为零
3. 无 agent 的 `herdr_exec` utility 不进 agent 状态机（可选后续；workspace 绑定也不覆盖 utility）
4. ~~ChatGPT 回合催促~~（扩展 ≥0.1.18：仅小模型判定；Options 预填提示词/不发送词；继续时提交模型原文）

### 已实现（主线 A 定时进度通报）

- Options：`progressTickSec`（默认 `60` 秒：working 进度检查 + 回合催促冷却；填 `0` = 全关）、`progressFallbackSec`（默认 `1200` 秒无新摘要兜底）、进度模板与收工模板分开（`progressTemplate` / `wakeTemplate`）；界面语言 en / 简体中文 / 日本語
- 进度实发规则：检查点到了之后，仅当 herdr 侧摘要相对上次实发有**新的非空内容**才往网页灌一条；否则满 `progressFallbackSec` 才兜底一条，避免空转刷屏
- `working`：每 `progressTickSec` **检查**摘要；仅新非空内容或满 `progressFallbackSec` 才往绑定会话灌进度并提交；同一 convKey 只一个定时器，重复 `working` 不丢已有实发基线
- popup 列表按 **workspace** 展示（含仅终端、无 running agent 的窗格）；标题一行 + 窗格统计一行，不重复项目名、不列 `agent@pane`
- `settled`：先取消 tick 定时器，再按收工模板唤醒一次
- 全站同一套；站点差异只在 injector 写入/发送

### A2. 长对话压缩与接力（ChatGPT Project）

扩展 0.1.39 增加页面 HUD 的 **接力 / Rollover**。第一版刻意使用显式触发，不根据一个猜测的 token 阈值自动切会话。

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

当前没有自动 token 计数或自动新开对话策略。先证明显式接力稳定，再考虑只做“建议接力”的阈值提示；即使以后支持自动触发，也应复用同一个 fail-closed transfer state machine，而不是另造一套路径。

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

- **A**：ChatGPT（或任意已绑定站）`herdr_prompt` 后，working 中有进度戳、settled 后有继续提示，对话能自己往下走。
- **B**（尚未实现）：DeepSeek 输出一段 `herdr_inspect` JSON，扩展调本机并回填 `ok` 摘要。当前只能确认助手回复里出现了该 JSON。
