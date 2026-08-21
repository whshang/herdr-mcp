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
| `agent_working` | 只武装，不写网页 |
| `agent_settled` | 若此前 working → 唤醒一次 |
| 重连 `hello` | 可补一次错过的 settle |

缺口：working 期间无定时 tick（见 [extension.md](./extension.md) 拟定）。

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
