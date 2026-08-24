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
| `agent_working`（绑定 workspace 内任意 pane） | 武装；若检查间隔 >0 则启动定时器 |
| 进度 tick | 每 `progressTickSec` 检查；有新非空摘要或满 `progressFallbackSec` 才 `routeWake` |
| `agent_settled` 且同 space 仍有 working | **局部进展**模板唤醒（不是整轮收工） |
| `agent_settled` 且范围内无 working | **收工**模板唤醒一次，停 tick |
| 重连 `hello` | 可补一次错过的范围收工；若快照仍有 working → 续 tick |

**working 定时进度通报（主线 A）**：绑定会话在 agent `working` 期间，每隔 `progressTickSec` 秒**检查**一次是否要向网页提交进度提醒；`settled` 时仍按现逻辑唤醒一次并停止 tick。

**同一字段**：`progressTickSec` 也作为 ChatGPT 回合催促的冷却秒数（popup/options 只填一处）。

实发规则（避免空转刷屏）：
- `progressTickSec` 只决定**检查/催促**间隔（默认 60s），不是发送间隔；**填 0 = 关闭进度通报与回合催促**
- **首次**实发：有指纹变化的非空摘要 → `new_output`
- **已实发过**：距**上一次发送**未满 `progressFallbackSec`（默认 1200s / 20 分钟）→ **一律不发**（底线从最后一次发送起算，不是固定 cron）
- 满底线后：指纹有变 → `new_output`；否则 → `fallback`
- 实发基线写进绑定（`lastProgressSentAt` / `lastProgressOutput`），Service Worker 被杀后仍去重

默认值：`progressTickSec = 60`（进度检查 + 催促冷却；填 `0` = 全关）；`progressFallbackSec = 1200`（20 分钟兜底；填 `0` = 只在有新摘要时发）。均在 options 页可改。界面语言：en / 简体中文 / 日本語（首次跟系统，可选手动）。

## 安装与绑定

1. 加载 `extension/`
2. 选项填 `http://127.0.0.1:8772` 与 `herdr-mcp token`（扩展只连本机，用 `/push/events` 与 `/push/state`，不走 Cloudflare）
   - 这与公网 Worker 的 contract epoch 独立（当前为 epoch 2 / 18 tools）；扩展不读取 ChatGPT `tools/list`。
3. 打开目标对话（chatgpt / deepseek / z.ai / claude）
4. popup：**绑定**将要干活的 **workspace**（列表显示 herdr **label**，如 `novo (w5A)`；含仅开终端、无 agent 的窗格；space 内任意 agent 有进展都会回推）
5. 再派活

未绑定 = 无回推。旧「单 pane」绑定在重连后会按 pane 前缀升成 workspace。唤醒文案以 **workspace_label** 为主角，`{agent}` 只表示焦点窗格。

## ChatGPT 回合催促（小模型判定）

绑定会话 + 扩展 ≥ 0.1.20：

1. 内容脚本看 Stop 出现/消失，划定回合
2. Options 填了 Base URL + Key + Model 时：对用户/助手正文做一次 OpenAI 兼容 `chat/completions`
3. 回复落在「不发送关键词」→ 不催；否则若判定为继续 → **把小模型原文**灌进对话框并提交（提示词 / 不发送词在 Options 预填可见默认文案）
4. **不再使用**零工具 / 半途启发式；未配置小模型则本回合不催
5. 用户气泡若是上次催促句，**仍会**对助手新回复做判定（0.1.20 起）；冷却与进度检查共用 `progressTickSec`，**0 = 关闭催促**（默认 60s）
6. ChatGPT 页底部常驻状态条（≥0.1.22）：当前配置 + 最近一条判定；文案跟 Options 语言（en / 简中 / 日语）
7. herdr working/settled 唤醒仍独立存在

密钥只存本机，仓库默认留空。

## 多任务语义

| 事件 | 行为 |
|---|---|
| space 内任一 agent → working | 武装；启动进度 tick |
| 某一 pane settled，同 space 仍有 working | **局部进展**唤醒 |
| space 内全部停 working | **收工**唤醒 |
| 每次唤醒（局部/进度/收工） | 焦点窗格输出 + **同 workspace 全窗格一览**（agent / terminal title / 状态 / cwd）；有空闲窗格时附三选一：保留等待下一轮 / 总结收尾 / 回收新开 |
| working 期间新 output | 仍按指纹 + fallback 间隔发进度 |

默认模板占位符含 `{roster}` `{idle_hint}`；自定义模板未写时扩展也会强制附上。

## ChatGPT 权限卡

内容脚本 ≥ 0.1.3 在 chatgpt.com 常驻自动点「允许」。见 [chatgpt-connector.md](./chatgpt-connector.md)。

## 测试

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`
