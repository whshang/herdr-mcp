# 浏览器连续工作：为什么 MCP 之后还需要一条回来的路

MCP 解决的是：

> Web AI 如何访问本地开发环境？

浏览器连续工作解决的是：

> 本地开发环境发生变化后，如何让网页会话知道？

这两个方向缺一不可。

## 单向 MCP 的限制

典型 MCP 流程：

```text
ChatGPT
   │
   │ tool call
   ▼
herdr-mcp
   │
   ▼
Herdr Agent
```

ChatGPT 发起请求，服务器返回结果。

但软件开发任务经常是异步的：

```text
10:00  ChatGPT 提交测试任务
10:01  Agent 开始运行
10:20  测试完成
```

10:20 时，原来的网页回合已经结束。MCP 本身不会主动打开一个新的 ChatGPT 回合。

## 两条连接组成完整系统

herdr-mcp 有两条方向相反的通道：

### 下行：能力通道

```text
ChatGPT
  ↓
MCP + OAuth
  ↓
Edge
  ↓
herdr-mcp
  ↓
本地工作站
```

负责：

- 文件读取；
- Patch；
- Git；
- Shell；
- Agent 调度。

### 上行：连续性通道

```text
Herdr
  ↓ events
runtime
  ↓ local IPC
Browser extension
  ↓
网页会话
```

负责：

- Agent working 进度；
- settled 收工通知；
- 页面恢复；
- 长对话接力；
- 自动继续。

## 扩展不是第二个 Agent

浏览器扩展不负责思考，不替代 ChatGPT，也不运行 coding agent。

它更像一个连接器：

- 知道当前哪个网页会话对应哪个 workspace；
- 接收本机状态变化；
- 在安全条件满足时推动网页继续。

真正的推理仍然由 Web AI 或本地 Agent 完成。

## 为什么不用公网回推

扩展和本机 runtime 在同一台机器上。

当前设计：

```text
Chrome
  ↓ Native Messaging
native host
  ↓ Unix socket 0600
herdr-mcp runtime
```

浏览器不保存 Herdr bearer，也不需要把扩展连接暴露到公网 Worker。

公网 Edge 服务的是 ChatGPT → workstation；本机 IPC 服务的是 browser → local runtime。

## 长任务工作流

推荐模式：

```text
提出目标
 ↓
ChatGPT 分析
 ↓
修改代码 / 派 Agent
 ↓
离开电脑
 ↓
Agent 完成
 ↓
浏览器收到状态
 ↓
继续当前工作
```

这让几个小时的软件任务可以保持连续，而不是要求人一直盯着终端。

## 自动化边界

自动继续不是无限循环点击。

系统会检查：

- workspace 是否绑定；
- Agent 是否真的发生状态变化；
- 是否存在未确认 mutation；
- 页面是否仍在生成；
- 是否满足 Project / conversation Auto 设置。

高风险动作保持显式边界。

## 两类扩展能力

### A. Web continuity

服务 ChatGPT、z.ai、DeepSeek 等网页会话：

- workspace binding；
- progress/settled 回推；
- stale response recovery；
- handoff / rollover。

### B. JSON → MCP bridge

服务没有原生 MCP Connector 的网页：

- 注入工具描述；
- 解析 JSON tool call；
- 调本机 MCP；
- 回填工具结果。

两者共享 Native Messaging 和本机 IPC，但解决的问题不同。

## 设计原则

浏览器连续工作关注的是“保持人在回路中的连续性”。

它让 AI 可以长时间工作，也让人随时能看到状态、调整方向和接管，而不是把电脑变成一个无法观察的自动机器人。
