# Herdr Multi-Device Worker Control Plane

> 状态：WIP / 规划稿，仅定义产品、协议、安装与运维方向，本稿不包含实现变更。
>
> 基线日期：2026-08-28。

## 1. 决策摘要

Herdr 的多设备形态采用：

```text
ChatGPT / CLI / Worker Web Console
              │
              ▼
      one Herdr Connector
              │
              ▼
   one Cloudflare Worker
              │
       Device Control Plane
              │
    ┌─────────┼─────────┐
    ▼         ▼         ▼
 Device A   Device B   Device C
 herdr-mcp  herdr-mcp  herdr-mcp
 Rust       Rust       Rust
```

核心决定：

1. 用户侧继续使用 **Herdr Connector**；普通多设备场景不要求为每台设备创建一个 ChatGPT Connector。
2. 一个 Herdr Cloudflare Worker 可以登记和控制多台设备。
3. 对外产品术语使用 **device / 设备**；`workstation`、`WorkstationDO`、routing target 保留为内部实现术语。
4. 所有本机生命周期和控制命令统一以 `herdr-mcp` 开头；不引入第二套用户命令入口。
5. 新设备配置公网控制面时，必须明确选择：
   - 创建新的 Herdr Worker；
   - 连接已有 Herdr Worker；
   - 仅本机使用，不配置 Worker。
6. Worker 提供受保护的 Web Control Console，用于查看设备、连接健康、调度状态、最近调用和管理动作。
7. 同一套设备管理能力同时暴露给：
   - 设备上的安装 Agent；
   - `herdr-mcp` CLI；
   - Worker Web Console；
   - ChatGPT 自然语言 + MCP tools。
8. **连接状态、调度状态、授权状态分离**。暂停调度时设备仍保持在线并继续健康上报。
9. 实时在线状态由现有 WebSocket + heartbeat 自动派生，不通过高频写 Device Registry 维护。
10. 最近调用历史只记录必要元数据，默认不保存工具参数、文件内容、命令输出或模型对话文本。

---

## 2. 当前基线

本规划基于当前仓库和已安装 Rust runtime 的真实状态。

### 2.1 本机 runtime / CLI

当前生产 runtime 已为 Rust，已安装版本为 `0.4.0-alpha.16`。

当前普通生命周期入口：

```text
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update ...
herdr-mcp rollback
herdr-mcp uninstall
```

当前高级 Link 入口：

```text
herdr-mcp link status
herdr-mcp link run
herdr-mcp link install
herdr-mcp link uninstall
herdr-mcp link cutover ...
herdr-mcp link seal ...
herdr-mcp link migrate-runtime-control ...
```

因此后续 Worker / device 命令必须继续收敛在 `herdr-mcp` 命令树中。

### 2.2 当前公网 MCP

当前 public contract：

```text
contract epoch 2
18 tools
```

当前 Worker 已具备：

```text
GET /health
GET /info
GET /status/:workstationId
GET /ws/:workstationId
GET/POST /mcp
/.well-known/*
```

`/mcp` 当前通过以下顺序解析 workstation：

```text
x-herdr-workstation
?workstation=<id>
DEFAULT_WORKSTATION_ID
fallback dev-ws1
```

Worker 已经通过：

```text
WORKSTATION_DO.idFromName(workstationId)
```

按 workstation 建立独立 Durable Object。

`WorkstationDO` 已维护：

- WebSocket presence；
- `online` / stale 判断；
- heartbeat / `lastSeen`；
- runtime version；
- runtime generation；
- runtime health；
- active requests；
- request lifecycle / mutation safety 状态。

一个 workstation 只允许一个 active link，新连接会 supersede 旧连接。

### 2.3 当前需要改变的单设备假设

当前仍存在明显的单设备假设：

- `DEFAULT_WORKSTATION_ID`；
- Rust Link 默认 workstation id；
- Worker 没有可枚举的 Device Registry；
- ChatGPT 无法通过 MCP contract 显式表达设备选择；
- public contract 与 workstation execution contract 当前使用同一份 contract identity；
- Link authentication 仍以 shared secret 为主；
- 安装文档默认“部署一个 Worker + 配一个 workstation identity”；
- 没有正式的 Worker Web Control Console；
- 没有统一的设备管理审计事件。

本规划应逐步移除这些产品层的单设备假设，同时保留兼容期。

---

## 3. 产品术语

### 3.1 Herdr Worker

用户界面和 CLI 中使用 **Herdr Worker**。

它指：

> 由用户 Cloudflare Account 承载的 Herdr 公网控制面，包含 MCP endpoint、OAuth、设备注册、设备路由、状态聚合和 Web Control Console。

实现文档仍可使用 `Edge` 描述 Worker 内部层。

### 3.2 Device

`device` 是用户可见的远程计算设备。

```text
device_id   immutable stable id
name        mutable human-readable alias
```

示例：

```text
device_id = dev_01J...A8F2
name      = macbook-main
```

设备名允许重复，所有安全和路由判断使用 `device_id`。

### 3.3 Workstation

`workstation_id` 暂时保留为 relay / Durable Object 内部协议字段。

第一阶段允许：

```text
device_id <-> workstation_id
```

一一映射。

不要求为第一版多设备能力立即重写现有 relay protocol 的全部 `workstation_id` 字段。

### 3.4 Instance

已有 `HERDR_MCP_INSTANCE` 继续表示同一物理设备上的本机 runtime instance，例如：

```text
default
uat
experimental
```

未来可形成：

```text
Device
  ├─ default instance
  ├─ uat instance
  └─ experimental instance
```

第一版 fleet UI 以 device 为主。多 instance 远程选择可作为后续扩展，不阻塞多设备主线。

---

## 4. 为什么采用一个 Worker 管多设备

推荐主路径：

```text
one user / one control plane
        │
        ├─ one Worker
        ├─ one Connector
        └─ N devices
```

主要收益：

- 用户只配置一次 ChatGPT Connector；
- OAuth 和公网入口稳定；
- 新设备只需要 enroll，不需要重新配置 ChatGPT；
- ChatGPT 可以在一轮任务中跨设备工作；
- Worker 能统一做设备健康、调度和权限判断；
- 可以集中看到调用历史和设备状态；
- Worker 与 runtime 发布仍保持分层；
- 一台设备离线不会改变其他设备的公网入口。

保留“一个设备一个 Worker”的能力，但它属于隔离部署方案，不作为普通安装主路径。

典型适用场景：

- MacBook + Mac mini；
- 开发机 + Linux build server；
- 办公设备 + 家庭常在线设备；
- macOS + Windows + Linux 跨平台复现；
- 项目分别存在于不同设备；
- 一台设备专门运行长任务或 GPU / Docker workload。

---

## 5. Worker 与 Device 的生命周期

生命周期拆成：

```text
Worker lifecycle
create -> active -> rotate/update -> retire

Device identity lifecycle
enroll -> active -> suspended -> active
                   └──────────-> revoked

Device connection lifecycle
offline -> connecting -> online -> reconnecting -> offline

Device scheduling lifecycle
enabled -> draining -> paused -> enabled
```

这三类 device state 不合并成一个 `status` 字段。

建议聚合模型：

```json
{
  "device_id": "dev_01J...A8F2",
  "name": "macbook-main",
  "authorization": "active",
  "connection": "online",
  "scheduling": "enabled",
  "health": "healthy"
}
```

### 5.1 Connection

```text
online
reconnecting
offline
stale
```

来源：WebSocket + heartbeat + TTL。

### 5.2 Scheduling

```text
enabled
draining
paused
```

`paused` 的定义：

- 保持 Link 在线；
- 继续 heartbeat；
- 继续 runtime health / version 上报；
- Worker 不再向该设备发起新的普通工具调用；
- Worker Control Console 和 `herdr_devices` 仍能读取它的状态；
- 已经提交的请求按照现有 delivery / idempotency 规则完成，不盲目取消 mutation。

默认 pause 流程：

```text
pause requested
     │
     ├─ immediately reject new workload admission
     │
     ├─ active_requests > 0 -> draining
     │
     └─ active_requests == 0 -> paused
```

`pause` 需要带可审计、可自动恢复的上下文，而不是只保存一个布尔状态：

```json
{
  "scheduling": "paused",
  "paused_at": "...",
  "paused_by": "...",
  "pause_reason": "maintenance",
  "pause_expires_at": "..."
}
```

支持：

- pause 1 hour；
- pause until tomorrow；
- pause indefinitely；
- 到期自动恢复为 `enabled`；
- 自动恢复必须产生一条 admin/audit event；
- `draining` 期间保留原 `pause_reason` 和 expiry，不因进程重启丢失。

### 5.3 Authorization

```text
active
suspended
revoked
```

`suspended`：

- 临时禁止设备建立 Link；
- 当前 Link 被关闭；
- 后续 reconnect 被拒绝；
- 可由 owner resume。

`revoked`：

- 永久撤销当前 device credential；
- 关闭当前 Link；
- 不允许使用原 identity reconnect；
- 保留最小审计记录；
- 设备若再次加入，需要重新 enroll。

因此：

```text
pause scheduling != suspend access != revoke device
```

Web Console 不提供一个语义模糊的“下线”按钮。

---

## 6. 新设备安装：必须选择新 Worker 或已有 Worker

普通用户的第一条本机生命周期命令仍保持：

```text
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

不要改变 `herdr-mcp install` 当前稳定语义来偷偷部署 Cloudflare Worker。

新增一个明确的公网 onboarding 命令组，建议产品入口：

```text
herdr-mcp worker setup
```

交互流程：

```text
Herdr local runtime is healthy.

How should this device connect to ChatGPT?

[1] Create a new Herdr Worker
[2] Connect to an existing Herdr Worker
[3] Local only
```

这一步既适合人工 CLI，也适合安装 Agent 根据明确上下文选择非交互参数。

### 6.1 创建新 Worker

用户选择：

```text
Create a new Herdr Worker
```

流程：

1. 确认本机 `herdr-mcp doctor` 健康；
2. 确认 Cloudflare 登录 / bootstrap credential；
3. 选择 Cloudflare Account；
4. 创建稳定 Worker identity；
5. 部署 Worker；
6. 初始化 Worker owner credential；
7. 初始化 Device Registry；
8. enroll 当前设备；
9. 安装 / 启动 persistent Link；
10. 验证 `/health`、device presence、OAuth、`/mcp`；
11. 输出：
    - Worker name；
    - Worker origin；
    - Web Control Console URL；
    - ChatGPT MCP URL；
    - 当前 device name / id；
12. Cloudflare bootstrap token 离开常驻配置。

Cloudflare API Token 只属于 **创建 / 更新 Worker** 的部署权限，不成为设备运行时 credential。

### 6.2 连接已有 Worker

用户选择：

```text
Connect to an existing Herdr Worker
```

新设备不应该要求 Cloudflare API Token。

推荐输入方式按优先级：

1. enrollment code；
2. Worker URL + enrollment code；
3. 本机已有可信配置中发现的 Worker；
4. 高级用户显式提供 Worker origin。

推荐命令：

```text
herdr-mcp worker connect --code <one-time-code>
```

或：

```text
herdr-mcp worker connect \
  --url https://<worker>.<account>.workers.dev \
  --code <one-time-code>
```

Enrollment code：

- single-use；
- short TTL；
- scope 仅允许 `device:enroll`；
- 绑定具体 Worker；
- 默认只能注册一台设备；
- 使用后立即失效；
- 不包含 Cloudflare deployment credential；
- 不等价于 device credential；
- 不写入仓库或普通日志。

设备本地生成 keypair / installation identity，Worker 完成最终 device identity 签发和登记。

### 6.3 Local only

用户明确选择 Local only 时：

- 不配置 Cloudflare；
- 不创建 Worker；
- 不安装 Herdr Link；
- 本机 MCP / Native Messaging 能力保持可用；
- 以后可执行 `herdr-mcp worker setup` 再接公网。

---

## 7. 安装 Agent 合同

现有 `agent-install.md` 的目标从：

```text
每次默认部署一个 Worker
```

调整为：

```text
先判断：create new / connect existing / local only
```

Agent 不应因为能访问 Cloudflare 就自动创建第二个 Worker。

### 7.1 Agent 可自动判断的情况

以下情况可以直接选择已有 Worker：

- 用户明确给出 Worker URL；
- 用户明确给出 enrollment code；
- 本机存在可信 Herdr Worker binding；
- 用户说“把这台机器加入我现有 Herdr”。

以下情况可以创建新 Worker：

- 用户明确要求新建；
- 当前没有任何 binding，用户明确选择新建；
- 用户明确要求隔离环境 / 独立控制面。

无法判断时，Agent 只需给出三项选择，不展开技术问答。

### 7.2 Agent 非交互接口

未来 Agent 应优先调用确定性命令：

```text
herdr-mcp worker create ...
herdr-mcp worker connect ...
herdr-mcp worker status
herdr-mcp device status --self
```

`worker setup` 负责人工引导；`create/connect` 负责 Agent / automation。

### 7.3 安装完成报告

Agent 只报告非敏感信息：

- runtime version / generation；
- Worker name / origin；
- Worker control URL；
- device name / shortened id；
- connection / scheduling / health；
- ChatGPT MCP URL；
- OAuth / MCP smoke status。

不得报告：

- device private key；
- enrollment secret；
- Cloudflare API Token；
- owner admin key；
- MCP/local runtime token。

---

## 8. `herdr-mcp` CLI 规划

以下仅为命令合同草案，不代表当前已实现。

### 8.1 Worker

```text
herdr-mcp worker setup
herdr-mcp worker create
herdr-mcp worker connect
herdr-mcp worker status
herdr-mcp worker doctor
herdr-mcp worker dashboard
herdr-mcp worker disconnect
```

语义：

- `setup`：人工 onboarding；
- `create`：新建并初始化 Worker；
- `connect`：把当前设备 enroll 到已有 Worker；
- `status`：查看当前设备绑定的 Worker；
- `doctor`：验证 Worker / OAuth / link / registry；
- `dashboard`：获取一次性 Web Control Console 登录并打开浏览器；
- `disconnect`：解除当前设备的本地 Worker binding；如需要 revoke，必须显式指定。

### 8.2 Devices

建议：

```text
herdr-mcp devices list
herdr-mcp device status <device>
herdr-mcp device rename <device> <name>
herdr-mcp device pause <device>
herdr-mcp device resume <device>
herdr-mcp device suspend <device>
herdr-mcp device unsuspend <device>
herdr-mcp device revoke <device>
herdr-mcp device enrollment create
```

其中：

- `devices list` 为只读；
- `pause/resume` 只改变 scheduling；
- `suspend/unsuspend` 改变 connection authorization；
- `revoke` 是高风险不可逆 credential 撤销动作；
- `enrollment create` 生成一次性新设备加入码。

所有远程 mutation 都需要：

- request id；
- actor identity；
- idempotency key；
- explicit device id；
- audit event；
- delivery / mutation outcome。

CLI 可以接受 device name，但解析成功后必须固定成 `device_id` 再执行 mutation。

---

## 9. Worker Web Control Console

建议入口：

```text
https://<worker-origin>/control/
```

相关 API：

```text
/api/control/summary
/api/control/devices
/api/control/devices/:id
/api/control/activity
/api/control/worker
/api/control/security
```

`/control/` 与 `/api/control/*` 必须受 owner authentication 保护。

`/health`、OAuth discovery、`/mcp`、`/ws/*` 的现有职责保持独立。

### 9.1 Overview

首页至少显示：

```text
Devices
  total             4
  online            3
  offline           1
  scheduling paused 1
  degraded          0

Calls (recent)
  success           126
  failed            4
  delivery unknown  0

Worker
  edge version      ...
  MCP contract      ...
  OAuth             healthy
```

### 9.2 Devices 页面

列表字段：

- device name；
- shortened device id；
- OS / arch；
- authorization；
- connection；
- scheduling；
- health；
- runtime version；
- runtime generation；
- contract compatibility；
- last seen；
- active requests；
- last invocation；
- enrolled at。

支持：

- rename；
- pause scheduling；
- resume scheduling；
- suspend access；
- resume access；
- revoke；
- generate replacement enrollment flow。

高风险动作必须显示明确结果，不能用 UI 乐观状态代替 Worker 权威状态。

### 9.3 Device detail

设备详情页：

```text
Identity
Connection
Scheduling
Runtime
Recent calls
Recent admin events
Credential / enrollment metadata
```

默认不展示本机绝对路径、命令参数和工具结果正文。

### 9.4 Activity

最近调用表格至少包含：

```text
time
device
tool
operation class
source
status
latency
request_id
delivery_state
error_code
```

示例：

```text
09:12:03  macbook-main  herdr_git     read      chatgpt  ok     183ms
09:11:44  build-linux   herdr_exec    mutation  chatgpt  ok    3.21s
09:10:02  build-linux   device.pause  admin     web      ok      47ms
```

默认不记录：

- tool arguments；
- shell command 文本；
- 文件内容；
- tool result 正文；
- ChatGPT conversation 文本；
- secret / token。

### 9.5 Worker 页面

显示：

- Worker identity；
- origin；
- deployment version；
- public MCP contract epoch；
- runtime execution contract compatibility；
- OAuth health；
- device registry health；
- pending / active request counts；
- recent Worker errors；
- retention configuration；
- update availability（后续）。

### 9.6 Add device

Control Console 提供：

```text
Add device
```

生成：

```text
one-time enrollment code
expires in N minutes
```

并显示可复制命令：

```text
herdr-mcp worker connect --code <code>
```

不展示 Cloudflare API Token 或 shared link secret。

Enrollment 第一版支持两种审批模式：

```text
code possession = auto approve
owner approval required
```

严格模式下，新设备先进入 `pending approval`，Control Console 至少展示：

- requested device name；
- OS / arch；
- public key fingerprint；
- requested_at；
- enrollment issuer / source；
- enrollment code expiry；
- approve / reject。

在 owner approve 前不签发正式 device credential，也不允许成为 routable device。

### 9.7 Tasks / Operations

Web Console 必须把**当前运行态**和**历史 Activity**分开。

```text
Tasks       = what is happening now / what is waiting
Activity    = what already happened
```

建议一级导航增加：

```text
Tasks
  Running
  Queued
  Needs input
  Waiting permission
  Draining
  Delivery unknown
  Failed
  Completed (recent only)
```

一个 ChatGPT MCP invocation 可能启动更长生命周期的本地操作，因此标识符不能全部压成一个 `request_id`：

```text
request_id         one public MCP invocation
operation_id       one logical routed operation
exec_session_id    long-running shell session when present
agent_session_id   local agent session when present
```

这些 ID 应可相互关联，但不要为了 Web UI 人为改变现有 runtime correctness contract。

### 9.8 Queue / dispatch latency

为了能解释“为什么发送后很久才开始执行”，一次 operation 应聚合以下阶段耗时：

```text
routing_ms
queue_wait_ms
dispatch_ms
execution_ms
total_ms
```

Recent Activity 可以保留单次 compact 数值；Analytics 开启后才计算长期：

```text
p50 / p95 / p99
```

不要为每个阶段分别写一条 Durable Object history record；一次完成的 invocation 最多形成一条 compact telemetry record。

### 9.9 Overview：异常优先，不是资产统计优先

首页排序应优先回答“哪里需要处理”和“为什么任务慢”：

```text
Needs attention
Recent failures
Offline / stale
Scheduling paused
Contract incompatible
Needs input / waiting permission
Queue wait p95
```

`total devices`、OS 分布等资产统计放在第二层。

### 9.10 Device capacity / busy state

设备在线不等于可立即调度。Device card / detail 应逐渐提供：

```text
running_operations
queued_operations
execution_limit
available_slots
utilization
active_agents
blocked_agents
```

第一版不做复杂 load balancer，但这些字段可以帮助 ChatGPT 与用户判断：

- 设备是否已经满载；
- 为什么任务还在排队；
- 哪台机器有可用执行槽位；
- 是否存在 Agent blocked / needs input。

### 9.11 Activity 拆成三类事件流

不要把所有历史都塞进一张无语义的日志表。

```text
Executions
  MCP/tool/task/agent execution outcomes

Device Events
  online/offline/reconnect/health/version/contract transitions

Audit
  enroll/approve/rename/pause/resume/suspend/revoke/credential rotate
```

三类事件可以共享底层 bounded storage，但 API 和 UI 语义分开。

### 9.12 Current / Upcoming / History

Device detail 与 Tasks 页面都应区分：

```text
Current
Upcoming
History
```

`Upcoming` 用于展示仍未完成但已经存在的动作，例如：

- queued operation；
- waiting approval；
- drain in progress；
- pending runtime update；
- pending credential rotation。

这类状态不能错误地进入“已完成 Activity”。

### 9.13 Version drift / compatibility

Control Console 不只显示裸版本号，还应派生：

```text
current
behind
ahead
incompatible
update_available
```

Device detail 至少展示：

```text
Worker public contract
Runtime execution contract
Runtime version
Runtime generation
Required minimum
Compatibility
```

Overview 可直接提示：

```text
2 devices need update
1 device contract incompatible
```

### 9.14 Web Console 不是通用 RMM

Herdr 不应复制 MeshCentral / Tactical RMM 一类产品的完整远程运维面。

第一版明确不在 Web Console 重新实现：

- remote desktop；
- full file browser；
- generic browser terminal；
- service manager；
- patch management；
- software inventory platform。

Web Console 聚焦：

```text
visibility
routing
scheduling
approval
diagnostics
audit
```

复杂机器操作继续交给 ChatGPT、`herdr-mcp` CLI 和本地 Agent。

---

## 10. Web Control Console Authentication

Web Console 不能直接复用当前 public `/health` 语义，也不能公开设备列表。

第一版建议 owner-only，不急于引入团队 RBAC。

### 10.1 Owner credential

创建 Worker 时生成独立 owner administration credential：

```text
Worker owner credential
    != Cloudflare API Token
    != ChatGPT OAuth access token
    != device link credential
    != local MCP token
```

创建者设备将 owner credential 安全保存到 Keychain / platform secure store。

### 10.2 Dashboard login

推荐：

```text
herdr-mcp worker dashboard
```

流程：

1. CLI 使用 owner credential 请求短 TTL、single-use browser login nonce；
2. 打开 `/control/login?...`；
3. Worker exchange nonce；
4. 浏览器收到 `Secure` + `HttpOnly` + `SameSite` session cookie；
5. nonce 立即失效。

避免把长期 owner secret 放进 URL。

### 10.3 ChatGPT admin scope

ChatGPT 的设备读取与设备 mutation 使用独立 OAuth scopes，例如：

```text
herdr:devices.read
herdr:devices.manage
```

普通 tool execution 与 fleet administration 应可以分别授权。

`revoke` 等高风险操作可要求更强 scope / explicit confirmation contract。

---

## 11. Device Registry

新增一个可枚举的 Worker-side registry。

推荐权威长期字段：

```json
{
  "device_id": "dev_...",
  "name": "macbook-main",
  "credential_id": "cred_...",
  "authorization": "active",
  "scheduling": "enabled",
  "created_at": "...",
  "updated_at": "...",
  "revoked_at": null,
  "metadata": {
    "os": "macOS",
    "arch": "arm64",
    "labels": ["personal", "build"],
    "capabilities": ["git", "docker", "browser"]
  }
}
```

Device Registry 不承担 heartbeat 热路径。

长期状态变化才写 Registry：

- enroll；
- rename；
- scheduling pause / resume；
- suspend / unsuspend；
- credential rotate；
- revoke。

### 11.1 Labels

Labels 是用户和路由器都能理解的轻量设备分组，不引入第一版团队组织/RBAC。

建议区分：

```text
manual labels
  personal
  work
  laptop
  server
  always-on
  build
  gpu

derived labels
  os:macos
  os:linux
  arch:arm64
```

用户可说“在 always-on 设备上执行”，但 label 不是安全 identity，也不能覆盖显式 `device_id` 授权检查。

### 11.2 Capabilities

设备调度不能只看 OS / arch。应把与执行决策相关的能力提升为一等字段，例如：

```text
git
docker
browser
gui
gpu
agent:<kind>
```

现有 Relay `hello.capabilities` 已经存在，因此实现时优先扩展/规范这一条能力链，而不是在 Device Registry 另造互不一致的 capability 系统。

Registry 保存的是稳定/声明型摘要；WorkstationDO 可以保存当前 Link 报告的实时 capability snapshot。两者冲突时，运行时可达性判断以当前 authenticated Link 为准。

---

## 12. 实时状态继续由 WorkstationDO 负责

现有 `WorkstationDO` 已经适合承担 per-device realtime state：

```text
WebSocket
heartbeat
lastSeen
runtime version
generation
health
active requests
request lifecycle
```

`herdr_devices` / Web Console 聚合时：

```text
Device Registry
   │
   ├─ dev_A -> WorkstationDO(A) /internal/status
   ├─ dev_B -> WorkstationDO(B) /internal/status
   └─ dev_C -> WorkstationDO(C) /internal/status
```

这样：

- 设备状态会自动更新；
- 设备休眠后自动显示 offline / stale；
- Link reconnect 后自动回 online；
- scheduling paused 设备仍能显示 runtime health；
- 不需要每次 heartbeat 都写全局 Device Registry。

### 12.1 当前实现审计：已经节省持久化写入，但 heartbeat 仍会唤醒 DO

截至 2026-08-28 再次复核 `origin/main=3523dad`（`v0.4.0-alpha.17`），现有 Edge 已经实现了一部分 DO 配额保护；用户当前本机已安装 runtime 仍为 `0.4.0-alpha.16`，因此这里把“最新源码状态”和“当前已安装生产 runtime”明确区分：

- `WorkstationDO` 使用 `state.acceptWebSocket()`，属于 hibernatable WebSocket 路径；
- `HEARTBEAT_PERSIST_THROTTLE_MS = 300_000`，普通 heartbeat 的 `last_seen` 最多每 5 分钟持久化一次；
- runtime version / generation / health 等摘要发生变化时会立即持久化，而不是等待下一次 5 分钟 checkpoint；
- known-safe read request 使用 `ephemeralReads` 只在内存关联，不进入 pending / completed / idempotency Durable Object storage；
- DO 初始化路径明确保持 read-only，存储写配额耗尽不能在业务逻辑运行前阻断 inspect / fs-read 等安全读取；
- 历史遗留 safe-read rows 不会为了清理而主动消耗稀缺 write quota。

这意味着当前实现已经避免了“每个 heartbeat 都写一次 SQLite-backed Durable Object storage”的最危险模式。

但当前仍未达到最终的 heartbeat 成本目标：

- Rust production Link daemon 当前使用约 15 秒的应用层 Relay heartbeat；
- 每个 heartbeat 仍进入 `webSocketMessage()` / `handleHeartbeat()`；
- Edge 每次处理 heartbeat 后还会回复一个 `status` frame；
- 当前没有使用 `setWebSocketAutoResponse()` 把纯 transport liveness ping/pong 下沉给 Cloudflare；
- 因此即使绝大多数 heartbeat 不产生 storage write，仍会产生 DO JS wakeup / message processing 开销。

结论：

```text
Durable storage write protection    PARTIALLY IMPLEMENTED / GOOD BASELINE
Heartbeat JS wakeup protection      NOT YET COMPLETE
Quota-aware adaptive shedding       NOT YET IMPLEMENTED
```

未来必须区分两层：

```text
Transport liveness
  WebSocket protocol ping/pong or Cloudflare auto-response
  no Durable storage write
  preferably no JS wakeup

Herdr runtime health
  event-driven or low-frequency application frame
  only runtime/health changes need immediate persistence
```

不要把固定频率 application heartbeat 作为长期唯一在线判定来源。

### 12.2 DO quota 是 correctness resource

Durable Objects 免费/受限额度必须优先保护正常 MCP execution、authorization、routing 和 mutation delivery correctness，telemetry 与 heartbeat 永远不能反向挤占主链路。

架构硬约束：

```text
heartbeat durable writes     = 0 in steady state, except bounded recovery checkpoint
status poll durable writes   = 0
Web Console refresh writes   = 0
safe read request writes     = 0 whenever correctness allows
telemetry                    = bounded + shed-able
mutation correctness state   = protected
```

这里的“heartbeat durable writes = 0 in steady state”允许当前 5 分钟 recovery checkpoint 作为兼容期基线，但正式多设备版本应继续评估是否可以进一步改为 connection lifecycle / meaningful state-change driven checkpoint。

### 12.3 DO Budget Guard

Worker 应内置逻辑预算和 circuit breaker，而不是等 Cloudflare quota error 后才退化。

推荐软预算：

```text
Correctness / MCP traffic      reserved ~70%
Control-plane mutations        reserved ~15%
Telemetry / recent activity    max ~10%
Maintenance                    max ~5%
Heartbeat persistence          target ~0%
```

这些比例是 Herdr 内部保护目标，不代表 Cloudflare 提供真实 reservation。

保护状态：

```text
NORMAL
CONSERVE
CRITICAL
```

`CONSERVE`：

- successful-call activity 降采样；
- 停止非关键 snapshot persistence；
- failures / admin audit 继续保留；
- MCP execution 不受影响。

`CRITICAL`：

- 暂停非必要 telemetry persistence；
- 仅保留 correctness ledger、authorization/device mutation、必要 failure audit；
- 绝不为了保住 Dashboard 数据而拒绝正常 MCP 请求。

Web Console Worker 页面应显示：

```text
Durable Objects protection
heartbeat storage writes     0 / bounded checkpoint
heartbeat JS wakeups         current mode
recent activity writes       today
admin mutation writes        today
telemetry mode               normal / conserve / critical
quota protection             NORMAL / CONSERVE / CRITICAL
```

如果 Cloudflare 无法提供精确的实时 account quota consumed 数字，Herdr 也应至少维护自身可控写路径的计数器与估算值，并明确标记为 estimated，而不是伪装成 Cloudflare authoritative usage。

### 12.4 产品分层：Core / Recent Activity / Analytics

多设备控制、近期排障信息和长期分析必须明确分层，避免为了监控能力强制所有用户承担额外 Cloudflare 存储、数据库和写入成本。

#### Core — 永远开启

Core 是多设备能力本身，属于正确性主链路，不允许依赖可选 telemetry 后端。

包含：

```text
devices
online / offline
health
routing
enrollment
pause / resume
suspend / revoke
authorization
scheduling
request delivery correctness
```

默认实现依赖：

```text
Cloudflare Worker
WorkstationDO × N
DeviceRegistryDO
```

Core 必须满足：

- 永远开启；
- 不依赖 D1；
- 不依赖 Analytics Engine；
- 不依赖 Queues；
- telemetry 关闭或失败时仍可完整工作；
- quota pressure 下优先保障 Core。

#### Recent Activity — 默认开启、严格有界

Recent Activity 用于短期排障和解释，不属于 correctness ledger。

包含：

```text
recent calls
recent failures
admin audit
routing reason
recent device events
```

MVP 默认：

```text
enabled by default
bounded retention
7 days OR 500 records, whichever comes first
```

允许配置：

```text
normal
failures only
disabled except required audit
```

Recent Activity 必须满足：

- heartbeat 不写入 Activity；
- status poll 不写入 Activity；
- Web Console refresh 不写入 Activity；
- 一个完成 invocation 最多形成一条 compact record；
- 不保存完整 tool arguments、shell command、文件内容、result body 或 ChatGPT conversation；
- 可以清除；
- quota pressure 时可以降采样或关闭 successful-call history；
- 关闭后不影响 MCP execution、routing、authorization 或 mutation delivery correctness；
- MVP 不要求 D1。

#### Analytics — 明确选配

Analytics 用于长期趋势、容量分析和性能统计，不属于多设备功能的必经路径。

典型能力：

```text
p50 / p95 / p99 latency
queue wait trend
dispatch latency trend
device utilization
failure rate trend
reconnect trend
30 / 90 day reporting
```

默认状态：

```text
disabled
```

用户明确开启后，才评估使用：

```text
Cloudflare Analytics Engine
D1
Queues
```

推荐边界：

- Analytics Engine：高基数时序指标和 percentile/trend；
- D1：需要复杂搜索、长期 audit、跨设备查询时再引入；
- Queues：规模增长后用于异步 telemetry ingestion / retry，不进入 MVP 主链路；
- 任一可选模块不可成为 `herdr_devices`、设备路由或正常 MCP invocation 的依赖。

因此第一版正式依赖关系应保持：

```text
Required:
  Worker + Durable Objects

Optional:
  Analytics Engine
  D1
  Queues
```

Web Console 可以展示：

```text
Recent Activity     Enabled / bounded
Historical Analytics  Not enabled

Enable historical analytics ->
```

用户不开启 Analytics 时，Overview、Devices、Tasks、Recent Activity、Enrollment 和 Worker diagnostics 仍应完整可用。

---

## 13. 最近调用历史与存储原则

最近调用属于 telemetry，不应和 mutation delivery ledger 混为一个数据结构。

建议事件：

```json
{
  "at": "...",
  "request_id": "req_...",
  "device_id": "dev_...",
  "tool": "herdr_exec",
  "op_class": "mutation",
  "source": "chatgpt",
  "status": "ok",
  "latency_ms": 3210,
  "delivery_state": "delivered",
  "error_code": null
}
```

### 13.1 默认 retention

MVP 建议：

- bounded recent history；
- 默认 7 天或最近 500～1000 条，取先达到的边界；
- admin events 可保留更久；
- 用户可以清除 recent activity；
- 不因清理 telemetry 删除 mutation correctness ledger。

### 13.2 写入预算

历史系统不得重现 heartbeat 导致的 Durable Object 高频写入问题。

规则：

- heartbeat 不进入 activity history；
- status poll 不进入 activity history；
- 一个完成的 MCP invocation 最多形成一条 compact history record；
- admin mutation 最多形成一条 audit record；
- retention cleanup 批量执行；
- UI refresh 只读；
- telemetry 写失败不能破坏工具调用 correctness。

MVP 可以用专用 `ControlPlaneDO` / bounded storage；如果调用量增长，再把 telemetry sink 迁移到更适合分析查询的后端。Web Console API 不绑定具体存储实现。

---

## 14. ChatGPT MCP：`herdr_devices`

用户可见的设备发现工具命名：

```text
herdr_devices
```

不使用 `herdr_targets` 作为公开产品术语。

建议输出：

```json
{
  "devices": [
    {
      "device_id": "dev_...A8F2",
      "name": "macbook-main",
      "authorization": "active",
      "connection": "online",
      "scheduling": "enabled",
      "health": "healthy",
      "os": "macOS",
      "arch": "arm64",
      "runtime_version": "0.4.0-alpha.16",
      "last_seen_ago_ms": 1200
    }
  ]
}
```

该工具由 Edge 直接执行，不转发给某一台 workstation。

### 14.1 ChatGPT device management

建议增加单一 mutation tool：

```text
herdr_device
```

示例动作：

```text
status
rename
pause
resume
suspend
unsuspend
revoke
create_enrollment
```

读取设备列表使用 `herdr_devices`；修改单个设备使用 `herdr_device`。

避免为每个动作创建一个新的 MCP tool。

### 14.2 自然语言示例

```text
“看看我现在有哪些 Herdr 设备在线。”
-> herdr_devices

“暂停 build-linux 的任务调度，但保持它在线。”
-> herdr_device(action=pause, device=dev_...)

“恢复 Mac mini 调度。”
-> herdr_device(action=resume, device=dev_...)

“给我生成一个把新 MacBook 加进来的注册码。”
-> herdr_device(action=create_enrollment)
```

---

## 15. ChatGPT 如何选择正确设备

设备路由必须是 request-scoped，不能依赖一个 Worker 全局 `current_device`。

原因：

```text
Conversation A -> MacBook
Conversation B -> Linux
```

两条会话可以并发使用同一个 Connector。

### 15.1 路由优先级

建议：

1. 用户本轮明确指定 device；
2. opaque ref 已包含 `device_id`；
3. 当前 workspace / pane ref 已绑定 device；
4. project inventory 只在一个 routable device 上命中；
5. 只有一个 routable device；
6. 仍有多个候选 -> `device_ambiguous`。

Mutation 在歧义状态必须 fail closed。

### 15.2 Device-aware refs

当前本地：

```text
workspace_id = wBH
pane_id      = wBH:p2
```

多设备公网返回应逐渐加入：

```json
{
  "device_id": "dev_...",
  "workspace_id": "wBH",
  "pane_id": "wBH:p2"
}
```

或提供 opaque ref：

```text
device_ref / workspace_ref / pane_ref
```

后续调用使用 opaque ref 时自动恢复 device routing。

不要要求模型从路径字符串猜设备。

### 15.3 Routing explainability

每次自动设备选择都应能解释“为什么是这台设备”。

推荐在 operation/activity metadata 中记录：

```text
selected_device
routing_reason
candidates
excluded_reason
```

稳定 reason code 建议：

```text
explicit_device
opaque_ref
workspace_binding
unique_project_match
capability_match
single_available_device
legacy_default_device
ambiguous
```

示例：

```text
Selected: build-linux
Reason: unique_project_match

macbook-main   excluded: project_not_found
build-linux    selected
mac-mini       excluded: scheduling_paused
```

这部分是多设备安全与可排障性的核心能力：用户必须能够追溯“ChatGPT 为什么去了这台机器”。

---

## 16. Public MCP contract 与 Runtime execution contract 要分层

这是多设备设计里最重要的协议调整之一。

当前 Edge public contract 和 workstation runtime contract 基本等同，因此 `tools/list`、link contract hash 和本地执行工具共享同一身份。

多设备以后 Edge 需要增加：

- `herdr_devices`；
- `herdr_device`；
- device routing metadata；
- Edge-only administration semantics。

这些能力不应该全部进入每台本机 runtime 的执行器。

因此建议明确两层：

```text
Public Edge Contract
  ChatGPT-visible
  device-aware
  includes Edge-local tools

Runtime Execution Contract
  workstation-visible
  local 18-tool execution semantics
  no fleet administration responsibility
```

Edge 做：

```text
public tool call
   │
   ├─ extract device routing field
   ├─ authorize device
   ├─ check scheduling
   ├─ Edge-local tool? -> execute on Edge
   │
   └─ workstation tool
        ├─ translate public args -> runtime args
        └─ forward to WorkstationDO(device_id)
```

这样可以避免为了 `herdr_devices` 强迫每台 Rust runtime 实现一个“列出所有设备”的伪本地工具。

### 16.1 Contract migration

建议下一版 public contract 使用新的 epoch。

兼容原则：

- old Connector / old conversation 可在兼容窗口继续使用 legacy default device；
- new conversation 获得 device-aware public contract；
- old runtime execution contract 可在受控兼容窗口继续连接；
- Edge translation 明确记录 public contract identity 与 runtime execution contract identity；
- 后续移除 `DEFAULT_WORKSTATION_ID` 前必须有真实多设备 UAT。

---

## 17. Worker 路由与暂停调度

正式路由前必须经过：

```text
OAuth principal
   │
   ▼
Device Registry
   │
   ├─ exists?
   ├─ authorization active?
   ├─ scheduling enabled?
   └─ principal allowed?
   │
   ▼
WorkstationDO(device_id)
```

错误码建议统一使用 device 术语：

```text
device_not_found
device_ambiguous
device_offline
device_reconnecting
device_paused
device_suspended
device_revoked
device_unhealthy
device_contract_incompatible
```

现有 relay 内部可以继续使用 `workstation_*` code；Edge 在 public boundary 做稳定映射。

---

## 18. Device credential：从 shared secret 演进

当前 link authentication 的 shared secret 不适合作为正式多设备长期边界。

目标：每台设备独立 credential。

```text
Worker
  ├─ dev_A -> credential A
  ├─ dev_B -> credential B
  └─ dev_C -> credential C
```

收益：

- 单设备 credential 泄漏不允许冒充其他 device id；
- 可以单独 revoke；
- 可以单独 rotate；
- Worker 可以知道 credential 与 device 的绑定；
- 新设备加入不需要复制全局 `LINK_SHARED_SECRET`。

建议 enrollment 生成设备 keypair，Worker 保存 public identity / credential metadata，本机 private material 放 Keychain / platform secure store。

共享 secret 仅保留迁移兼容期。

---

## 19. 管理入口必须共享一套 Worker API

四个入口不分别实现业务逻辑：

```text
Installation Agent
herdr-mcp CLI
Worker Web Console
ChatGPT MCP
        │
        ▼
Device Administration Service
        │
        ▼
Registry / WorkstationDO / Audit
```

### 19.1 共同语义

同一个 `pause`：

- CLI pause；
- Web Console Pause；
- ChatGPT “暂停这台设备调度”；

都必须调用同一个 administration contract。

同一个 `revoke` 也必须有相同：

- authorization check；
- idempotency；
- audit；
- active link close；
- reconnect rejection；
- final state verification。

避免出现 Web UI 一套状态、CLI 一套状态、ChatGPT 一套状态。

---

## 20. Worker Control Plane 数据模型

建议逻辑组件：

```text
Cloudflare Worker
│
├─ MCP Router
├─ OAuth
├─ Device Admin API
├─ Web Control Console
│
├─ DeviceRegistryDO
│   └─ durable identity / desired state
│
├─ WorkstationDO(device_id) × N
│   └─ realtime connection / requests
│
└─ ControlPlaneHistory
    └─ bounded invocation + admin metadata
```

### 20.1 DeviceRegistryDO

权威：

- registered devices；
- names；
- authorization；
- scheduling desired state；
- credential metadata；
- enrollment records；
- revoke state。

### 20.2 WorkstationDO

权威：

- current Link；
- runtime status；
- heartbeat；
- request lifecycle；
- delivery state。

### 20.3 ControlPlaneHistory

非 correctness 权威：

- recent call metadata；
- recent admin events；
- dashboard aggregation。

Telemetry loss 可以使 Activity 少一条，但不能使 mutation 被重复发送或设备权限状态出错。

---

## 21. Worker identity 与已有 Worker 发现

Worker 本身需要稳定 identity：

```text
worker_id
worker_name
origin
owner identity
created_at
public contract identity
```

建议 Worker 提供一个无敏感信息的 metadata endpoint：

```text
/.well-known/herdr-worker
```

可包含：

```json
{
  "product": "herdr-mcp",
  "worker_id": "wrk_...",
  "worker_name": "herdr-main",
  "control_path": "/control/",
  "mcp_path": "/mcp",
  "enrollment_supported": true
}
```

安装器可以验证用户给出的 URL 确实属于兼容 Herdr Worker。

它不能泄露 devices、OAuth clients、tokens 或 owner 信息。

### 21.1 不自动扫描用户所有 Worker 后直接复用

即使安装 Agent 拥有 Cloudflare API 权限，也不应凭名字相似就连接某个 Worker。

自动发现只用于形成候选列表，最终复用必须依赖：

- stable `worker_id`；
- Herdr metadata；
- 用户已有 binding；
- enrollment authorization。

---

## 22. Connector 关系

推荐：

```text
one Worker -> one stable Connector URL
```

例如：

```text
https://herdr-main.example.workers.dev/mcp
```

设备增删不改变 Connector URL。

用户新加一台设备：

```text
Worker unchanged
OAuth unchanged
Connector unchanged
MCP URL unchanged
```

只改变 Device Registry 和 Link presence。

这也是多设备能力最直接的产品价值。

---

## 23. 安全原则

1. Worker deployment credential 与运行时 credential 分离。
2. 每台设备独立 credential。
3. owner admin credential 与 device credential 分离。
4. ChatGPT OAuth scope 区分 tool execution 与 device administration。
5. Device Registry mutation 全部记录 actor / request / outcome。
6. `revoke` fail closed。
7. scheduling pause 不冒充 credential revoke。
8. Web Console 不公开 device inventory。
9. Activity 不记录 tool arguments / result body。
10. enrollment code short-lived + single-use。
11. alias 不能作为安全 identity。
12. mutation 在 device ambiguity 时拒绝执行。
13. 一个设备不能通过声明别人的 `device_id` 获得其身份。
14. credential rotate 不改变 immutable `device_id`。

---

## 24. 兼容与迁移

### Phase 0：冻结术语与协议边界

- 对外统一 `device`；
- 内部允许继续 `workstation`；
- 定义 Worker / device / scheduling / authorization；
- 定义 public Edge contract 与 runtime execution contract 分层方案。

### Phase 1：Worker registry + read-only visibility

- Device Registry；
- stable device id；
- labels / capabilities 基础模型；
- `herdr_devices`；
- Worker `/control/` Overview + Devices；
- 自动 realtime presence；
- Overview 异常优先信息架构；
- version drift / contract compatibility read-only visibility；
- 现有单设备继续工作。

此阶段不开放远程 mutation。

### Phase 2：设备调度管理

- pause / resume；
- pause reason / expiry / auto-resume；
- scheduling router gate；
- dashboard actions；
- CLI actions；
- ChatGPT `herdr_device`；
- admin audit events。

### Phase 3：正式 enrollment

- create new Worker / connect existing Worker onboarding；
- one-time enrollment code；
- optional owner approval mode；
- per-device credential；
- suspend / unsuspend；
- revoke；
- shared secret 进入兼容路径。

### Phase 4：device-aware execution

- request-scoped device routing；
- device-aware refs；
- multi-device public MCP contract；
- project-aware disambiguation；
- labels / capabilities routing filter；
- routing explainability / stable reason codes；
- multi-conversation routing UAT。

### Phase 5：产品化收尾

- Web Console Tasks / Operations；
- queue / dispatch / execution latency；
- busy / capacity visibility；
- Current / Upcoming / History；
- Web Console Activity；
- Executions / Device Events / Audit event streams；
- retention / cleanup；
- Core / Recent Activity / Analytics tier enforcement；
- optional Analytics integration boundary；
- Worker diagnostics；
- migration tooling；
- 文档站正式页面；
- installation agent contract 更新；
- GA UAT。

---

## 25. 第一版明确不做

- 团队 RBAC / 多租户组织管理；
- 自动把 mutation 从一台设备 failover 到另一台设备；
- 跨设备共享本机 filesystem；
- 自动迁移正在运行的 agent session；
- 复杂负载均衡；
- 基于 CPU / GPU 的自动 scheduler；
- 无限期保存调用历史；
- 在 Worker Web Console 展示完整命令 / 文件内容；
- 一次性重命名所有内部 `workstation_*` relay 字段；
- 用全局 `current_device` 保存 ChatGPT 当前设备。

---

## 26. UAT / 验收矩阵

### 26.1 Worker onboarding

- [ ] 新用户可以创建新 Worker；
- [ ] 第二台设备可以连接已有 Worker，不需要 Cloudflare API Token；
- [ ] local-only 可跳过 Worker；
- [ ] Agent 不会在已有 Worker 情况下静默创建第二个 Worker；
- [ ] Worker identity 可验证。
- [ ] owner-approval mode 下 pending device 在批准前不可路由；
- [ ] auto-approve mode 与 owner-approval mode 使用同一 credential issuance contract。

### 26.2 Device lifecycle

- [ ] device enroll 后进入 Registry；
- [ ] device name 可改，id 不变；
- [ ] Link connect 自动 online；
- [ ] heartbeat stale 自动 offline/stale；
- [ ] reconnect 自动恢复 online；
- [ ] pause 后 Link 仍 online；
- [ ] pause 后新普通请求收到 `device_paused`；
- [ ] pause reason / actor / expiry 可查询；
- [ ] 有 expiry 的 pause 到期自动 resume 且产生 audit event；
- [ ] resume 后恢复调度；
- [ ] suspend 后当前 Link 关闭且 reconnect 被拒绝；
- [ ] unsuspend 后允许 reconnect；
- [ ] revoke 后旧 credential 永久失败。
- [ ] rename / labels 修改不改变 immutable device identity。

### 26.3 ChatGPT routing

- [ ] 一台设备时无需显式 device；
- [ ] 多设备时用户显式名称可稳定解析；
- [ ] opaque workspace/pane ref 自动保留 device；
- [ ] 两台设备有同名 workspace 时不串机；
- [ ] device ambiguity 的 mutation fail closed；
- [ ] 两个 ChatGPT conversation 可同时绑定不同设备；
- [ ] 一轮对话可以依次操作两个设备；
- [ ] paused / suspended device 不被自动选为 workload target。
- [ ] routing decision 返回稳定 `routing_reason`；
- [ ] 自动选择可以解释 selected / excluded candidates；
- [ ] label / capability 只能过滤 routable candidates，不能绕过 authorization；
- [ ] capability snapshot 与当前 Link 不匹配时 fail closed，而不是依据过期 Registry 元数据强行调度。

### 26.4 Worker Web Control Console

- [ ] 未授权浏览器看不到 device inventory；
- [ ] Overview 统计与 Registry/WorkstationDO 一致；
- [ ] Devices 自动刷新 connection/health；
- [ ] Overview 优先显示 failures / offline / paused / incompatible / needs-input；
- [ ] Tasks 与 Activity 明确分离；
- [ ] Running / Queued / Needs input / Delivery unknown 可正确分类；
- [ ] queue_wait / dispatch / execution / total latency 能关联到同一 operation；
- [ ] Device card 能显示 busy / available capacity；
- [ ] Device detail 区分 Current / Upcoming / History；
- [ ] runtime / contract version drift 能显示 current / behind / ahead / incompatible；
- [ ] Activity 显示最近调用；
- [ ] Activity 区分 Executions / Device Events / Audit；
- [ ] Activity 不包含 arguments/result body；
- [ ] pause/resume/revoke 后显示权威结果；
- [ ] enrollment code single-use + expiry 生效。

### 26.5 Correctness / safety

- [ ] pause 不重复提交已有 mutation；
- [ ] Worker telemetry failure 不影响 tool result correctness；
- [ ] device revoke 不能通过 reconnect race 绕过；
- [ ] device credential 不能声明其他 device id；
- [ ] shared-secret legacy path 与 per-device credential 迁移可回滚；
- [ ] public contract 升级不要求一次性升级所有 workstation runtime。
- [ ] heartbeat / status poll / Web Console refresh 不因为观察行为增加持续性 DO write；
- [ ] Recent Activity 到达 retention / quota guard 后可以丢弃或降采样而不影响 correctness；
- [ ] Analytics 未启用时 Core / Recent Activity 全部可用；
- [ ] D1 / Analytics Engine / Queues 未配置时多设备主链路仍完整可用。

---

## 27. 需要在实现前冻结的接口

正式开发前建议只冻结以下内容：

1. 用户术语：`Herdr Worker` / `device`；
2. `device_id` 格式和稳定性；
3. connection / scheduling / authorization 三维状态；
4. `pause` 的 drain 语义；
5. enrollment code contract；
6. `herdr_devices` 输出；
7. `herdr_device` action contract；
8. CLI `worker` / `device(s)` 命令树；
9. Web Console authentication；
10. public Edge contract 与 runtime execution contract 的分层；
11. recent activity privacy / retention；
12. Core / Recent Activity / Analytics 三层依赖边界；
13. Tasks / Activity 的 identifier 与状态分类；
14. routing reason code / explainability contract；
15. labels / capabilities 的权威来源与冲突规则；
16. pause reason / expiry / auto-resume contract；
17. enrollment approval mode；
18. version drift / compatibility 派生规则；
19. legacy `DEFAULT_WORKSTATION_ID` 退出策略。

这些边界稳定后再进入实现，可避免在 Device Registry、MCP schema、CLI 和 Web Console 之间反复改名或改变语义。

---

## 28. 推荐的最终用户体验

### 第一台设备

```text
$ herdr-mcp install
$ herdr-mcp doctor
$ herdr-mcp worker setup

How should this device connect to ChatGPT?
> Create a new Herdr Worker

Worker: herdr-main
Device: macbook-main
Control: https://.../control/
MCP:     https://.../mcp
Status:  healthy
```

### 第二台设备

用户在 Web Console / ChatGPT / CLI 生成 enrollment code：

```text
$ herdr-mcp worker connect --code XXXX-XXXX

Worker: herdr-main
Device: build-linux
Status: online / scheduling enabled
```

不需要重新创建 ChatGPT Connector。

### ChatGPT

```text
用户：检查我有哪些 Herdr 设备。
ChatGPT -> herdr_devices

用户：暂停 build-linux 调度，机器继续保持在线。
ChatGPT -> herdr_device(action=pause, device=...)

用户：在 MacBook 改代码，在 build-linux 跑测试。
ChatGPT -> request-scoped device routing
```

### Web Control Console

```text
Herdr Worker

Needs attention  2
  1 scheduling paused
  1 device needs update

Devices
  2 online / 0 offline / 1 paused

Tasks
  2 running / 1 queued / 0 needs input
  queue wait p95  180ms

macbook-main   online   enabled  healthy   2/4 slots busy
build-linux    online   paused   healthy   0/4 slots busy

Recent activity
09:12 execution    herdr_git  macbook-main ok
09:11 execution    herdr_exec build-linux  ok
09:10 audit        pause      build-linux  ok
```

一级导航建议保持：

```text
Overview
Devices
Tasks
Activity
Enrollment
Worker
```

这应成为 Herdr 从“ChatGPT 连接一台工作站”演进到“一个公网控制面管理多台个人计算设备”的正式产品方向。

---

## 29. Prior art / Research traceability

本规划不是按传统 RMM 直接复制，而是吸收多类系统中已经验证过的控制面模式。

| Prior art | 借鉴点 | Herdr 采用方式 |
| --- | --- | --- |
| GitLab Runner Fleet Dashboard | recent failures、online/offline、busy runner、queue wait | Overview 异常优先；busy/capacity；queue/dispatch latency |
| NuNet Device Management Service | fleet discovery、resource/capability description、running allocation | Device capabilities；routable inventory；运行负载摘要 |
| FleetView | queued/running/needs-input/done/failed、agent tree、approval/control | Tasks / Operations；needs input；approval/steer 类状态可见性 |
| FleetDM | labels、targeting、past/upcoming activity | Device labels；Current/Upcoming/History |
| Rudder | pending node enrollment、inventory 后 approve | 可选 owner approval enrollment |
| Salt | Job identity、running job、job cache、event model | operation/task identifiers；live Tasks 与 bounded history 分离 |
| MeshCentral / Tactical RMM | 成熟设备列表、在线状态、操作审计 | 借鉴可见性和设备详情；明确不复制完整 RMM 功能 |
| Cloudflare Durable Objects / WebSocket Hibernation | per-device realtime ownership、hibernation、bounded durable state | WorkstationDO；heartbeat quota protection；DO Budget Guard |

### 29.1 Research finding → design mapping

调研结论进入规划文档必须可追踪，不再把“对话里讨论过”当成“文档已经完成”。

当前映射：

```text
fleet failures / wait time        -> 9.8 / 9.9
live task state                   -> 9.7 / 9.12
device capacity                   -> 9.10
event stream separation           -> 9.11
pending enrollment approval       -> 9.6
labels / capabilities             -> 11.1 / 11.2
routing explainability            -> 15.3
pause reason / expiry             -> 5.2
version drift                     -> 9.13
RMM product boundary              -> 9.14
DO quota / hibernation            -> 12.1 / 12.2 / 12.3
Core / Recent Activity / Analytics -> 12.4
```

后续新增外部调研结论时，必须同时更新这一 mapping 或对应章节，避免再次出现研究结果停留在聊天回复而未进入正式规划的问题。
