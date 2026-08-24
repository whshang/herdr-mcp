# JSON → MCP 桥接

读者：在 DeepSeek / z.ai 网页（没有 ChatGPT 式 MCP Connector）中，通过浏览器扩展安全驱动本机 herdr-mcp 的用户。

扩展总览与连续工作主线见 [extension.md](./extension.md)，进度回推与接力见 [extension-wake.md](./extension-wake.md)。

## 目标

B 线与 A 线并列：`chat.deepseek.com` / `chat.z.ai` 中的普通用户任务可以使用本机 Herdr MCP，同时不把工作站 token 暴露给网页 JavaScript，也不让扩展流量绕到公网 Worker/Tunnel。

```text
网页用户任务
  -> extension content bridge 注入 Herdr 工具协议 / catalog
  -> 网页模型输出 {"tool":"...","args":{...}}
  -> extension service worker POST tools/call 到 127.0.0.1:8772/mcp
  -> TOOL_RESULT 回填同一网页会话
  -> 网页模型继续调用工具或正常回答
```

## 当前状态（0.1.51）

| 能力 | 状态 |
|---|---|
| 从本机 Herdr MCP 读取 `tools/list` | **已可用** |
| 把 typed tool catalog 注入网页模型协议 | **已可用** |
| 单次助手回复输出一个或多个 JSON 工具调用 | **已可用** |
| 逐轮受控执行 `tools/call` + 回填结果，直到正常答案 | **已可用** |
| 同批独立工具并行执行 | **已可用** |
| 工具结果清洗 / 大二进制省略 / 长度上限 | **已可用** |
| Herdr 凭据不进入网页 JavaScript 或 service worker | **已可用** — 当前版本使用 Native Messaging + 权限 `0600` 的 Unix IPC；旧版本 bearer 兼容仅保留在 native host/server 内部 |
| z.ai / DeepSeek 会话级 `自动 开/关`，控制 Herdr progress/settled 自动回推 | **已可用**（需全局允许自动化） |
| 已落成 z.ai `/c/<chat_id>` 的“手动接力” | **已可用**（必须 `自动 关`；接力控制消息绕过 JSON bridge） |
| 刷新/重载后的未完成工具 JSON 恢复 | **已可用** — 若最后一条真实会话消息仍是 assistant 的 Herdr tool-call JSON，且前文存在 bridge 上下文，会自动继续执行 |
| 长 JSON→MCP 链路 | **已可用** — 第 12 轮只作为调度让出点；只有 assistant 返回正常非工具答案才算完成 |

Bridge 使用本机实时 `tools/list` catalog，而不是再维护一份手写工具白名单。扩展仍会校验调用站点与 conversation；所有 MCP 流量只走 loopback，本机 Herdr server 仍是最终工具/权限边界。

## 协议

Content bridge 给网页模型一个 typed catalog，并要求工具调用回复每行只放一个 JSON 对象：

```json
{"tool":"herdr_inspect","args":{}}
```

互相独立的调用可以同一回复里并列并行执行；有依赖的步骤继续串行。只有 service worker 返回 `TOOL_RESULT` 后，网页模型才能把该工具视为成功。

站点支持时，bridge 的中间协议消息会折叠。工具结果递归清洗，大体积 binary/base64 字段会被省略，整批 TOOL_RESULT 在回填网页模型前也有长度上限。

0.1.50 会在历史加载时重新折叠内部协议消息，不再只在 bridge 正在运行时折叠。折叠条作为站点 message root 的外部兄弟节点，显隐的是整条原消息；展开 z.ai 消息时不会再占满 flex 行宽、把正文挤成细竖条。

## 安全边界

- MV3 service worker 通过 Chrome Native Messaging 发送受限 request/stream 消息；native host 再经 `~/.config/herdr-mcp/extension.sock`（权限 `0600`）访问 herdr-mcp。因此 service worker 和网页 JavaScript 都拿不到 Herdr bearer。旧版 bearer 兼容只留在 native host/server 内部。
- MCP 请求只发送到配置的本机 Herdr endpoint，默认 `http://127.0.0.1:8772/mcp`。
- Bridge / 自动化动作执行前会校验 site 与 conversation identity。
- 不假装 DeepSeek / z.ai 原生支持 OAuth MCP Connector。
- ChatGPT 仍按需要使用其 Connector；JSON bridge 只解决缺少原生 Connector 的网页站点。

## z.ai 1.1.88 兼容

当前 adapter 把 `/` 视为新聊天启动页，把 `/c/<chat_id>` 视为稳定的持久会话身份。当前 DOM / composer 信号使用 `.user-message`、`.markdown-prose`、`#send-message-button`，同时保留兼容兜底 selector。

同一 tab 在根页 `/` 上临时建立的 binding 或会话自动化偏好，会在首次落成 `/c/<chat_id>` 时迁移一次；之后用户从已有 `/c/A` 切换到 `/c/B` 时，不会把 workspace binding 或自动化偏好一起拖过去。

## 与连续工作 / 接力协作

A 线可以同时把 z.ai / DeepSeek conversation 绑定到 Herdr workspace 做 progress/done 回推，B 线负责本机 MCP 工具调用。会话级 `自动 开/关` 只控制自动 progress/settled 回推，不会开启 ChatGPT 专属 stale-view 恢复、回合结束 LLM 判断或自动 rollover。

持久 z.ai conversation 只有在 `自动 关` 时才能使用 **手动接力**。summary / seed 通过 bridge 的 raw 通道发送，因此接力控制文案不会被重新包装成 coding-agent task。只有新 z.ai chat 已形成新的 `/c/<chat_id>` 且 seed marker 得到确认后，workspace binding 才迁移过去。
