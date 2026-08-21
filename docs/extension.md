# 浏览器扩展 — 产品总览（双主线）

读者：扩展作者与使用者。`extension/` 同时承担两件对等的事，不互相替代。

| 主线 | 问题 | 方向 | 首批站点 |
|---|---|---|---|
| **A. 进度回推** | 网页派活到 herdr 后，对话不再观察/继续 | herdr → 网页（写输入框并提交） | chatgpt / deepseek / z.ai / claude（凡有适配器） |
| **B. JSON→MCP** | DeepSeek / z.ai 网页没有 MCP Connector | 网页 → 本机 `127.0.0.1:8772/mcp` | `chat.deepseek.com`、`chat.z.ai` |

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

- 绑定：popup 把「当前会话」绑到某个 herdr pane
- SSE：`/push/events?pane=...`
- 策略：见过 `working` 之后，`settled` 唤醒一次（hello 可补）
- chatgpt.com 权限卡常驻自动点「允许」

### 缺口

1. ~~working 期间定时进度通报~~（已实现，见下）
2. 绑定摩擦：未绑定则回推为零
3. 无 agent 的 `herdr_exec` utility 不进 agent 状态机（可选后续）

### 已实现（主线 A 定时进度通报）

- Options：`progressTickSec`（默认 `60` 秒检查一次；填 `0` = 关闭）、`progressFallbackSec`（默认 `600` 秒无新摘要兜底）、进度模板与收工模板分开（`progressTemplate` / `wakeTemplate`）
- 进度实发规则：检查点到了之后，仅当 herdr 侧摘要相对上次实发有**新的非空内容**才往网页灌一条；否则满 `progressFallbackSec` 才兜底一条，避免空转刷屏
- `working`：每 `progressTickSec` **检查**摘要；仅新非空内容或满 `progressFallbackSec` 才往绑定会话灌进度并提交；同一 convKey 只一个定时器，重复 `working` 不丢已有实发基线
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
3. **完整面**：对齐 ChatGPT 默认 17 工具（仍仅本地）

详见 [extension-bridge.md](./extension-bridge.md)。

## 不做

- 假装 DeepSeek「自带」OAuth connector
- 扩展默认走公网 Cloudflare MCP URL
- 用 B 线替代 A 线（ChatGPT 已有 connector，停住问题靠 A）

## 验收口诀

- **A**：ChatGPT（或任意已绑定站）`herdr_prompt` 后，working 中有进度戳、settled 后有继续提示，对话能自己往下走。
- **B**：DeepSeek 输出一段 `herdr_inspect` JSON，扩展调本机并回填 `ok` 摘要。
