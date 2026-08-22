# 浏览器扩展 — 产品总览（双主线）

读者：扩展作者与使用者。Chrome 显示名称 **herdr → Web wake**。`extension/` 同时承担两件事，共享 token 与 Options，**完成度不对等**：A 已可用，B 只做到 JSON 抽取。  
语言：产品 UI 为 en / 简体中文 / 日本語（与 herdr 一致）；本页以简体中文写。仓库根 README：[en](../README.md) / [zh](../README.zh.md) / [ja](../README.ja.md)。

| 主线 | 问题 | 方向 | 状态 | 首批站点 |
|---|---|---|---|---|
| **A. 进度回推** | 网页派活到 herdr 后，对话不再观察/继续 | herdr → 网页（写输入框并提交） | **已可用**（扩展 0.1.28） | chatgpt / deepseek / z.ai / claude |
| **B. JSON→MCP** | DeepSeek / z.ai 网页没有 MCP Connector | 网页 → 本机 `127.0.0.1:8772/mcp` | **未完成**（能抠 JSON，未调 MCP） | `chat.deepseek.com`、`chat.z.ai` |

共享：同一扩展、同一静态 token、同一 options。  
分文档：[extension-wake.md](./extension-wake.md)（A）、[extension-bridge.md](./extension-bridge.md)（B）。

```text
┌─────────────┐  MCP Connector / JSON桥   ┌──────────┐
│ 网页 AI      │ ───────────────────────► │ herdr-mcp│──► herdr
│ ChatGPT 等   │ ◄── A 进度/收工回推 ──── │  /push   │
│ DeepSeek 等  │ ── B SpeaksJSON→/mcp ──► │  /mcp    │
└─────────────┘                           └──────────┘
```

## A. 进度主动提醒（全站）

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
