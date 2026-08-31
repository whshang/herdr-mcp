# 浏览器 JSON → MCP 桥：让没有原生 Connector 的 Web AI 调用本机工具

> **职责：** 实验性 JSON → MCP 兼容桥的高级参考。大多数用户不需要本页。

ChatGPT 可以通过自定义 MCP Connector 直接调用 herdr-mcp，但不是所有网页 AI 都提供同类能力。z.ai / DeepSeek 的网页会话可以很好地推理，却没有标准入口把本机 Herdr 工具注册进去。

JSON → MCP bridge 解决的就是这个兼容问题。

它不假装目标网站“原生支持 MCP”，也不把本机凭据交给页面 JavaScript。网页模型只负责输出受约束的 JSON 工具请求，真正的 MCP 调用由扩展和本机 trusted host 完成。

## 完整链路

```text
用户任务
  ↓
z.ai / DeepSeek Web model
  │ 输出受约束 JSON tool call
  ▼
content bridge
  ↓
extension service worker
  ↓ Chrome Native Messaging
native host
  ↓ Unix socket (0600)
herdr-mcp /mcp
  ↓
Herdr + files / Git / shell
  │
  └─ TOOL_RESULT 回填网页会话
```

整个工具执行仍发生在本机。Cloudflare Edge 不参与这条路径。

## 为什么不是直接让网页 JavaScript 请求 `127.0.0.1`

直接从网页脚本连接本机 MCP 会带来几类问题：

- 页面 origin 和浏览器权限模型限制；
- bearer 容易落进页面或扩展存储；
- 任意页面脚本可能试图复用本机高权限接口；
- 流式事件和 conversation identity 缺少统一控制层。

当前架构使用 Chrome Native Messaging，把浏览器侧能做的动作限制为明确的 request/stream 消息，再由 native host 通过权限为 `0600` 的 Unix socket 进入 runtime。

因此：

- 网页 JavaScript 看不到 Herdr bearer；
- extension service worker 也不需要长期保存 bearer；
- herdr-mcp runtime 仍是最终工具 schema、权限和 managed-root 闸门；
- local IPC 与公网 OAuth 是两套独立信任边界。

## 网页模型看到什么

Bridge 从本机实时 `tools/list` 获取工具 catalog，然后把必要的 typed schema 转成网页模型能够遵循的协议说明。

模型在需要调用工具时输出 JSON，例如：

```json
{"tool":"herdr_inspect","args":{}}
```

或：

```json
{"tool":"herdr_git","args":{"root":"/path/to/project","action":"status"}}
```

Bridge 解析后执行真实 MCP `tools/call`，再把 `TOOL_RESULT` 回填同一 conversation。网页模型根据结果决定下一步继续调用工具还是给用户正常答案。

## bounded tool loop

Bridge 不是把浏览器变成无限自治 Agent。每次工具循环都受状态、conversation identity 和调度边界约束。

一次逻辑流程是：

```text
assistant JSON calls
      ↓
validate
      ↓
execute MCP tools
      ↓
return TOOL_RESULT
      ↓
assistant reasons again
      ↓
JSON calls or normal answer
```

独立的同批调用可以并行；有依赖关系的步骤应继续串行。只有真实 `tools/call` 返回以后，网页模型才能把该工具视为成功。

## 结果为什么需要清洗

MCP result 可能包含：

- 很长的终端输出；
- image/binary 内容；
- structuredContent；
- 大段 base64 或其它网页模型不适合直接消费的字段。

Bridge 在回填前做长度限制和递归清洗，大型 binary/base64 字段会省略或摘要化。这不是改变工具事实，而是避免一轮结果把网页上下文淹没。

如果任务确实需要图片等富内容，优先让 Web planner选择适合的可见结果表达，而不是把原始二进制塞进文本 JSON。

## 中间协议消息为什么要折叠

JSON tool call / TOOL_RESULT 是机器协作记录，对人类阅读价值低，但长任务可能产生很多轮。

支持的站点会把这些内部消息折叠，让会话主线仍以“用户目标 → 最终解释/进展”为主。折叠只影响显示，不删除真实 conversation 内容。

## conversation identity

Bridge 必须知道“这次工具结果应该回到哪一个聊天”。

### z.ai

稳定 `/c/<chat_id>` URL 作为持久 conversation identity。根路径 `/` 是新聊天启动态，只能暂时保存启动期状态；第一次落成 `/c/<chat_id>` 后，临时 binding / Auto 偏好可以迁移一次。

之后从 `/c/A` 切到 `/c/B` 时，不会把 A 的 workspace binding 或自动化偏好误带到 B。

### DeepSeek

同样按稳定会话身份隔离 bridge state。页面 adapter 负责从当前站点路由/DOM 提取 identity，而不是把 tab id 当成长期会话 id。

## 页面刷新以后怎样继续未完成的 tool call

浏览器刷新不应该自动重跑所有历史 JSON。

恢复只在有充分上下文证据时进行：最后一条真实 conversation message 仍是 assistant 的 Herdr tool-call JSON，并且前文存在 bridge protocol context。这样才能判断“这是刚刚中断的工具步骤”，而不是用户打开了一段旧历史。

对于 mutation，恢复仍遵循 herdr-mcp 本身的 delivery/idempotency 规则。未知投递不能因为网页刷新就盲目执行第二次。

## 与浏览器 continuity 的关系

JSON → MCP 和 continuity 共用扩展与 Native Messaging transport，但解决不同问题。

| 能力 | 方向 | 目的 |
|---|---|---|
| JSON → MCP | 网页 → 本机 | 让没有原生 Connector 的 Web AI 调工具 |
| progress / settled | 本机 → 网页 | Agent 工作完成后推动会话继续 |
| recovery / handoff | 网页内部 | 恢复卡住的页面或切换长 conversation |

因此 z.ai 可以同时：

1. 用 JSON bridge 调 `herdr_fs_* / git / exec / prompt`；
2. 绑定同一个 Herdr workspace；
3. 在 Agent 长任务期间接收 progress / settled；
4. 必要时执行手动 handoff；`自动 开/关` 均可启动，目标会话继承源会话的 Auto 状态。

z.ai / DeepSeek 的会话 Auto 不意味着启用 ChatGPT 专属 stale-view 或自动 rollover。

## handoff 为什么必须绕过 JSON task wrapper

接力时旧会话需要生成摘要，新会话需要接收 seed。这些是**conversation control message**，不是“请调用 coding tools 完成一个业务任务”。

所以 z.ai handoff summary / seed 走 raw channel，明确绕过 JSON bridge。否则模型可能把“生成接力摘要”误包装成 Herdr coding task，形成错误递归。

## 安全边界

Bridge 的边界可以概括成：

- 只对明确支持的站点启用；
- 每次执行检查当前 site + conversation identity；
- tool catalog 来自本机真实 runtime，不维护另一份偷偷漂移的白名单；
- MCP 调用只经本机 trusted IPC；
- 浏览器不持有 Herdr bearer；
- 最终文件、Git、shell 权限仍由 herdr-mcp runtime gate 决定；
- 不通过 Cloudflare 把 extension 流量绕一圈公网；
- 不声称目标网站拥有官方 OAuth MCP 能力。

## 当前适用场景

它特别适合：

- 想使用 z.ai / DeepSeek Web 模型，但仍让它们操作自己的 Herdr 工作站；
- 不想再为每个网页站点实现一套本地开发 backend；
- 希望同一套 18-tool contract 和 managed-root 安全边界被多个 Web planner 复用。

如果目标客户端已经有可靠的原生 MCP Connector（例如 ChatGPT），优先使用原生 Connector；JSON bridge 是兼容层，不应该为了“统一”而替代更直接的标准路径。

## 验收

一条最小真实链路应该验证：

1. Bridge 能读取本机当前 `tools/list`；
2. 网页模型能生成合法 `herdr_inspect` JSON；
3. native host 成功执行 MCP tool；
4. `TOOL_RESULT` 回填当前 conversation，而不是其它 tab / chat；
5. 模型能根据结果继续第二个 tool call 或正常回答；
6. 页面刷新后不会重复执行已经完成的 mutation；
7. workspace binding / progress continuity 与 JSON tool loop 可以同时工作。

实现级 selector 和版本演进记录放在测试与 [CHANGELOG](../../../CHANGELOG.md)，本页只描述当前协议与安全边界。
