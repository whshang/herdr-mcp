# 浏览器扩展 — 唤醒网页会话（不是 MCP）

读者：加载 `extension/` 的人，或以为 z.ai / DeepSeek「装了扩展就有 MCP 工具」的人。

## 它是什么

MV3 扩展 **herdr → 网页唤醒**：绑定的 herdr agent 进入 settled 后，把一段消息写入绑定网页会话的输入框并提交。

有适配器的站点：`chat.z.ai`、`chat.deepseek.com`、`claude.ai`、`chatgpt.com`。

SpeaksJSON（`content/webmcp/speaks-json.js`）在 z.ai / DeepSeek 上解析助手输出，用于投递确认，以及将来的 **页面 → herdr** 反向桥（见 [extension-bridge.md](./extension-bridge.md)）。它 **不会** 在这些站点里注册 herdr-mcp 工具表。

## 它不是什么

| 预期 | 现实 |
|---|---|
| 装扩展 → DeepSeek/z.ai 拥有和 ChatGPT Connector 一样的 11 个 MCP 工具 | **否。** 那些站点没有本项目里的 ChatGPT 式 MCP OAuth connector |
| 服务端 OAuth / schema 修复会自动作用到扩展 | **否。** MCP 握手 ≠ 扩展唤醒通路 |
| 扩展必须依赖公网 Cloudflare URL | **否。** 扩展只打本地 `http://127.0.0.1:8772`（`/push/events`），用静态 token |

要从 **ChatGPT** 调度 herdr：用 MCP connector（[chatgpt-connector.md](./chatgpt-connector.md)）。  
要在 herdr agent 收工后 **捅一下** z.ai / DeepSeek：用本扩展。

## ChatGPT 工具权限卡

chatgpt.com 上 Connector 每次调工具常弹出「允许 ChatGPT 使用 herdr？」。  
扩展在 **chatgpt 标签页常驻** 观察页面内 DOM 权限卡并自动点「允许」（内容脚本 ≥ 0.1.3）。详见 connector 文档「工具权限卡」一节。

这解决不了 OpenAI 平台「永远不再询问」——那是客户端策略；扩展只能点已经画在页面上的按钮。

## 安装

1. `chrome://extensions` → 加载已解压的扩展 → 选 `extension/`
2. 选项：URL `http://127.0.0.1:8772`，token 用 `herdr-mcp token`
3. 打开目标聊天标签；在 popup 里绑定 agent ↔ 会话
4. herdr-mcp 需在跑（LaunchAgent）

改过 content 脚本后：扩展管理页点一次「重新加载」；已打开的目标站标签会因版本握手自动刷新。

## 若要在 z.ai / DeepSeek 里用 MCP

需要其一：

1. 站点自己长出 MCP/connector（再复用 herdr-mcp OAuth），或  
2. 扩展桥：页面 tool-call JSON → `POST /mcp`（路线见 [extension-bridge.md](./extension-bridge.md)）

不要把唤醒当成 MCP。

## 测试

- 静态 / 单元：`node tests/manual/extension_smoke.mjs`
- 绑定逻辑：`node tests/manual/background_bind_test.mjs`
- 推送通路：`node tests/manual/push_sse.mjs`
