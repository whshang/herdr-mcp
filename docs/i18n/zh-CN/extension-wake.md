# 进度回推

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
- `progressTickSec` 只决定**间隔驱动的进度检查 / 自动 LLM 回合判断**（默认 60s），不是发送间隔；**填 0 只关闭这两类间隔驱动动作，不改变 Options 的全局运行模式或 Project HUD 自动开关**
- **首次**实发：有指纹变化的非空摘要 → `new_output`
- **已实发过**：距**上一次发送**未满 `progressFallbackSec`（默认 1200s / 20 分钟）→ **一律不发**（底线从最后一次发送起算，不是固定 cron）
- 满底线后：指纹有变 → `new_output`；否则 → `fallback`
- 实发基线写进绑定（`lastProgressSentAt` / `lastProgressOutput`），Service Worker 被杀后仍去重

默认值：`progressTickSec = 60`（进度检查 + 自动 LLM 判断冷却；填 `0` = 关闭这两项）；`progressFallbackSec = 1200`（20 分钟兜底；填 `0` = 只在有新摘要时发）。Options 选择**全局手动 / 项目自动**：全局手动时 Project HUD 不显示自动开关并阻止自动 mutation；项目自动时显示 Project 开关，但新 Project 默认 `自动 关`。均在 options 页可改。界面语言：en / 简体中文 / 日本語（首次跟系统，可选手动）。

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
2. Options 为项目自动、当前 Project `自动 开` 且已填 Base URL + Key + Model 时：对用户/助手正文做一次 OpenAI 兼容 `chat/completions`；否则可用底栏 **LLM 分析** 手动触发
3. 回复落在「不发送关键词」→ 不催；否则若判定为继续 → **把小模型原文**灌进对话框并提交（提示词 / 不发送词在 Options 预填可见默认文案）
4. **不再使用**零工具 / 半途启发式；未配置小模型则本回合不催
5. 用户气泡若是上次催促句，**仍会**对助手新回复做判定（0.1.20 起）；自动判断冷却与进度检查共用 `progressTickSec`，`0` 关闭自动 LLM 判断（默认 60s），不影响手动 **LLM 分析**
6. ChatGPT 页底部 HUD：运行状态、**手动继续 / herdr监控 / LLM 分析 / 手动接力**、可选的 Project **自动 开|关**、展开；只有 Options 为项目自动时才显示 Project 开关。`手动接力` 只在 ChatGPT Project 出现，绑定 workspace 后可用，即使 `自动 开` 也保留。高频动作都在底栏，展开浮层只放事件设置、会话绑定和高级选项。文案跟 Options 语言（en / 简中 / 日语）
7. herdr working/settled 唤醒仍独立存在

密钥只存本机，仓库默认留空。

## ChatGPT 页面陈旧 / 半截回复恢复（0.1.44）

Project `自动 开` 时，人工发送和扩展自动发送的用户消息都会进入会话健康状态机。最近回合约 30 秒没有新的页面进展后，扩展会 best-effort 请求当前 ChatGPT conversation 的同源 snapshot，沿 `current_node` 取最新 assistant message，并与 DOM 中最后一条 assistant message 比较：

- 服务端 message id 更晚，或同一 message 的服务端文本明显更长：判为 **server ahead**，安全时刷新一次页面；
- 服务端明确显示 assistant message 尚未完成且至少 60 秒没有推进：判为 **server stalled**；如果页面仍显示 streaming，再多等待 30 秒；
- 服务端和页面一致且已结束：判为 **synced**，不刷新；
- snapshot 请求失败/超时/结构变化：判为 **unknown**，fail-closed，不盲目刷新。

刷新前记录当前 assistant 指纹。刷新后如果内容变长或重新开始 streaming，就认为页面已经恢复；如果 10 秒后仍是完全相同的半截回复，并且编辑器、工具、权限卡都空闲，则只发送一次浏览器恢复激活消息，让 ChatGPT 重新读取当前会话，从实际停止处继续且不要重复已完成工作。该消息仍失败后才进入 recovery-exhausted rollover。

## 手动接力（0.1.46）

底部 HUD 的 **手动接力**允许用户主动提前换会话，不依赖 `自动 开/关`：

- 仅 ChatGPT Project 显示；当前 conversation 必须先绑定 Herdr workspace。
- 点击后调用已有 `h2w_handoff_start(trigger=manual)`，先让当前 ChatGPT 生成带 transfer-id 的紧凑 handoff packet。
- 绑定 workspace 仍在 `working` 时拒绝开始，避免 settled/wake 与 binding cutover 竞争。
- packet 完成后在同一个 Project 打开新 conversation 并提交 seed；只有确认新 conversation 和 seed 都真实存在后，才把 workspace binding 从旧 conversation 迁到新 conversation。
- 已有接力任务时按钮显示 **压缩中… / 接力中… / 恢复接力**，避免重复创建 transfer；`seed_uncertain` 时可用“恢复接力”继续 fail-closed 恢复。
- `自动 开` 时前三个手动推进按钮会锁定，但 **手动接力不会被自动模式锁掉**，因为它是用户主动控制对话生命周期的操作。

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

内容脚本会持续观察 chatgpt.com 的页面内权限卡，但只有 **Options 勾选“启用项目自动” + 当前 Project `自动 开`** 时才会点击明确的「允许」动作；权限卡自动处理已并入 Project 自动化，不再有独立开关。全局手动或 Project `自动 关` 时停止自动点击。浏览器原生权限条不在可点击范围。见 [chatgpt-connector.md](./chatgpt-connector.md)。

## 测试

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`
