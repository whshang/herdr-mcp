# 浏览器扩展 — herdr 进度回推网页（唤醒继续）

读者：用 ChatGPT Connector 派活到 herdr 后，发现对话停住、不再 `herdr_since` / 继续推进的人。

## 要解决的问题

典型断点（ChatGPT + MCP）：

1. ChatGPT 经 connector 把任务交给 herdr（常见是 `herdr_prompt` 派到窗格 agent，或长命令）。
2. 工具调用很快返回「已提交 / 已启动」，**本轮对话结束**。
3. herdr 里 agent 还在 `working`，或稍后才 `idle/done`。
4. 网页上的 ChatGPT **没有**主动再调工具去观察；也没有人往输入框塞「请继续看进度」。
5. 任务看起来中断或停住。

扩展存在的理由：**把 herdr 侧进度/收工信号写回绑定的网页会话并提交**，逼网页模型再观察或继续推进。  
这不是给 z.ai/DeepSeek「装 MCP」，也不是替代 connector。

参考对话形态：ChatGPT 已能指挥 herdr，但缺「干完/干到一半时回捅网页」的一环。

## 它做什么

MV3 扩展 **herdr → 网页唤醒**：

1. 在 popup 把 **当前网页会话** 绑到某个 **herdr pane/agent**
2. background 对该 pane 挂 `GET /push/events` SSE
3. 收到 `agent_working` / `agent_settled`（及 hello 错峰补发）后，按策略决定是否唤醒
4. 向绑定标签页输入框写入模板消息并提交（chatgpt / claude 走 MAIN world 插入）

有适配器的站点：`chatgpt.com`（主场景）、`chat.z.ai`、`chat.deepseek.com`、`claude.ai`。

## 当前策略（已实现）

| 事件 | 行为 |
|---|---|
| `agent_working` | 只武装状态，**不**往网页塞字 |
| `agent_settled`（idle/done/blocked） | 若此前见过 working → **唤醒一次**，带输出摘要 |
| 重连 `hello` | 若离线期间错过 settle → 补唤醒一次 |

默认模板大意：`agent (pane) 已完成 (status)` + 输出片段 + 「请基于以上结果继续」。

**缺口（相对「定时通报 / 执行中提醒」）：**

- 没有 `working` 期间的定时进度通报
- 必须手动绑定会话 ↔ pane；未绑定则 MCP 派活了也不会回推
- 纯 `herdr_exec` / 无 agent 的 utility 窗格不走 agent 状态机，不会触发 push（除非以后扩展事件源）

## 安装与绑定（ChatGPT 主路径）

1. `chrome://extensions` → 加载 `extension/`
2. 选项：`http://127.0.0.1:8772` + `herdr-mcp token`
3. 打开要用的 **chatgpt.com 对话**（例如已接好 connector 的那条）
4. 点扩展图标 → 选将要干活的 herdr agent/pane → **绑定**
5. 再让 ChatGPT 经 MCP `herdr_prompt`（或其它会让该 pane 进入 working 的路径）派活

未绑定 = 扩展不会往这个对话写字，任务仍会在 herdr 里跑完，但网页不会被捅醒。

改过 content 脚本后：扩展管理页「重新加载」；已开标签会因版本握手自动刷新。

## ChatGPT 工具权限卡

Connector 每次 tools/call 常弹「允许 ChatGPT 使用 herdr？」。  
内容脚本 ≥ 0.1.3 在 **chatgpt.com 常驻** 自动点页面内「允许」。详见 [chatgpt-connector.md](./chatgpt-connector.md)。

## 和别的文档的关系

| 文档 | 关系 |
|---|---|
| [chatgpt-connector.md](./chatgpt-connector.md) | MCP 怎么连上、工具怎么调 |
| 本文 | 调完之后如何 **回推网页继续** |
| [extension-bridge.md](./extension-bridge.md) | 另一条线：网页 JSON → 本地 MCP（z.ai/DeepSeek 无 connector 时）；**不是**本问题的主解法 |

## 测试

- `node tests/manual/extension_smoke.mjs`
- `node tests/manual/background_bind_test.mjs`
- `node tests/manual/push_sse.mjs`
