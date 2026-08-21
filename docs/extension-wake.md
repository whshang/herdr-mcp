# 浏览器扩展 — herdr 进度回推网页（主线 A）

读者：任意网页版 AI 派活到 herdr 后，对话停住、需要主动提醒继续的人。

总览与双主线：[extension.md](./extension.md)。JSON→MCP 见 [extension-bridge.md](./extension-bridge.md)。

## 要解决的问题

1. 网页经 MCP 或 JSON 桥把任务交给 herdr。
2. 工具很快返回「已提交」，**本轮对话结束**。
3. herdr 里 agent 仍在 `working`，或稍后才 settled。
4. 网页模型不再自动观察 → 任务像中断。

扩展主线 A：**进度定时通报 + 收工提醒**，写入绑定会话并提交。

## 当前实现

| 事件 | 行为 |
|---|---|
| `agent_working` | 武装；若检查间隔 >0 则启动定时器（到点只**检查**，不必然发） |
| 进度 tick | 每 `progressTickSec` 检查；有新非空摘要或满 `progressFallbackSec` 才 `routeWake` |
| `agent_settled` | 先停 tick，若此前 working → 收工模板唤醒一次 |
| 重连 `hello` | 可补一次错过的 settle；若快照仍 working → 续 tick |

**working 定时进度通报（主线 A）**：绑定会话在 agent `working` 期间，每隔 `progressTickSec` 秒**检查**一次是否要向网页提交进度提醒；`settled` 时仍按现逻辑唤醒一次并停止 tick。

实发规则（避免空转刷屏）：
- `progressTickSec` 只决定**检查**间隔（默认 60s），不是发送间隔
- **首次**实发：有指纹变化的非空摘要 → `new_output`
- **已实发过**：距**上一次发送**未满 `progressFallbackSec`（默认 600s）→ **一律不发**（底线从最后一次发送起算，不是固定 cron）
- 满底线后：指纹有变 → `new_output`；否则 → `fallback`
- 实发基线写进绑定（`lastProgressSentAt` / `lastProgressOutput`），Service Worker 被杀后仍去重

默认值：`progressTickSec = 60`（检查间隔；填 `0` = 关闭进度通报）；`progressFallbackSec = 600`（10 分钟兜底；填 `0` = 只在有新摘要时发）；进度模板 `progressTemplate` 默认 `herdr agent {agent} ({pane}) 仍在执行 ({status})。\n\n{output}\n\n请用 herdr_since 续看；能 fs/exec 就不要再开贵模型。网页继续编排，勿把规划交给本机 Claude/OMP。`。均在 options 页可改。

## 安装与绑定

1. 加载 `extension/`
2. 选项填 `http://127.0.0.1:8772` 与 `herdr-mcp token`
3. 打开目标对话（chatgpt / deepseek / z.ai / claude）
4. popup：**绑定**将要干活的 pane
5. 再派活

未绑定 = 无回推。

## ChatGPT 权限卡

内容脚本 ≥ 0.1.3 在 chatgpt.com 常驻自动点「允许」。见 [chatgpt-connector.md](./chatgpt-connector.md)。

## 测试

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`
