# 架构：让网页模型拥有一台可持续工作的开发机

herdr-mcp 的目标很直接：让 ChatGPT 这样的 Web AI 获得接近本地 Coding Agent 的工作能力，同时把代码、终端、凭据和实际执行留在自己的机器上。

理解这套架构，可以先记住三个角色：

- **Web AI 是 planner**：理解目标、拆任务、决定下一步、检查结果。
- **Herdr 是长期存在的本地工作台**：保存 workspace、pane、终端和 Agent 的真实运行现场。
- **herdr-mcp 是远程控制面**：把 Web AI 缺少的本地观察和操作能力安全地送到它手里。

这三层组合后，网页聊天从“给建议的窗口”变成了“可以真正操作开发环境的控制台”。

## 一次请求是怎样穿过系统的

```text
┌──────────────────────────────────────────────┐
│                ChatGPT / Web AI              │
│      理解需求 · 规划 · 调工具 · 检查结果      │
└───────────────────┬──────────────────────────┘
                    │ MCP + OAuth / HTTPS
                    ▼
┌──────────────────────────────────────────────┐
│              Cloudflare Edge                 │
│   OAuth · MCP endpoint · workstation routing │
└───────────────────┬──────────────────────────┘
                    │ authenticated WSS
                    ▼
┌──────────────────────────────────────────────┐
│             workstation / herdr-link         │
│       主动出站连接 · 找到当前 runtime         │
└───────────────────┬──────────────────────────┘
                    ▼
┌──────────────────────────────────────────────┐
│                herdr-mcp                     │
│ inspect · fs · git · exec · prompt · call    │
└──────────────┬─────────────────┬─────────────┘
               │                 │
               ▼                 ▼
      managed Git projects     Herdr Socket API
      files / git / shell      workspace / pane / agent
                                   │
                                   ▼
                            Pi / Grok / other agents
```

例如你在手机上的 ChatGPT 里说：“看看项目为什么 CI 红了，修好并验证。”

ChatGPT 可以先 `herdr_inspect` 确认有哪些工作区；用 `herdr_git` 看仓库状态；读取失败相关文件；直接 patch；跑测试。如果问题适合并行调查，它可以把一个独立任务交给 Herdr 中的 Agent，同时继续检查另一条线。最终结果仍回到同一个网页会话。

## 为什么需要 Edge

ChatGPT 在公网，本机开发环境通常在 NAT、防火墙和公司网络后面。直接把开发机的 MCP HTTP 端口暴露到公网，会把网络可达性、TLS、认证、固定地址和生命周期问题全部推给工作站。

herdr-mcp 采用反方向连接：**工作站主动连出去**。

```text
ChatGPT ──HTTPS──► Cloudflare Edge ◄──WSS── workstation
                                      ↑
                              connection originates here
```

Edge 因而成为稳定的公网身份：

- Connector URL 不随本机 IP 变化；
- ChatGPT 使用 OAuth，不需要知道本机 bearer；
- workstation 不需要公网入站端口；
- 一套 Edge 可以依据 workstation identity 把请求送到正确机器；
- 本地 runtime 更新时，Connector URL 可以保持不变。

Cloudflare Worker / Durable Object 负责公网协议和连接路由，本机 `herdr-link` 维持认证 WSS。它们是传输层，不保存你的 Git 仓库。

## 为什么 Herdr 在最里面

文件和 shell MCP 很容易做成“远程执行器”：给路径，读文件；给命令，跑 shell。软件开发真正麻烦的部分是**时间**。

一个测试可能跑十分钟，一个 Agent 可能工作一小时，一次修复可能跨多个终端、多个仓库和多个网页回合。HTTP 请求结束以后，这些东西仍然需要有稳定身份。

Herdr 提供的正是这层长期状态：

```text
workspace
  └─ tab
      ├─ pane: shell
      ├─ pane: pi agent
      └─ pane: test / server / logs
```

Web AI 每次回来都能重新观察现场。它无需假设“上一次调用结束后世界被冻结了”。机器上真实存在的 pane、cwd、Agent 状态和近期事件才是事实来源。

Herdr 本身拥有大量 Socket API 方法。herdr-mcp 没有把它们逐个包装成 MCP tool，而是保留动态反射入口：`herdr_methods` + `herdr_call`。这样既保留 Herdr 的完整能力，又避免几十上百个工具定义长期占据模型上下文。

## 18 个工具为什么够用

生产公共契约当前固定为 **contract epoch 2 / 18 tools**。设计目标是让模型容易选对工具，而不是让工具列表看起来壮观。

### 1. 观察：我现在在哪

`herdr_inspect` 一次返回连接、workspaces、panes、agents、managed roots 和运行环境。`herdr_since` 只读取某个 cursor 之后的新事件。

这相当于开发者进入办公室先看桌面：哪些项目开着、哪个任务还在跑、谁已经做完。

### 2. 直接操作：能确定的事情直接做

`herdr_fs_read/list/grep/image`、`herdr_fs_edit/write/patch`、`herdr_git`、`herdr_exec` 和长任务 exec session 负责确定性工作。

读一个文件不需要叫 Agent；`git status` 不需要推理；跑一组测试也不需要再消费一次模型调用。Web planner 直接完成这些动作，链路更短，结果也更容易验证。

### 3. 调度 Agent：真正需要另一份推理时再派人

`herdr_prompt` 用于独立调查、并行实现、代码审查等任务。Agent 在 Herdr pane 里工作，所以 planner 可以继续观察它的状态和输出。

这里的核心判断是：**Agent 是计算资源和独立执行者，不是每个 shell 操作的必经中间层。**

### 4. 访问 Herdr 原生能力

`herdr_methods` 动态读取当前 Herdr schema，`herdr_call` 调用原生方法。高级 pane/workspace/session 操作因此无需扩张公共 MCP catalog。

`herdr_skill` 则给 Web planner 提供当前项目策略和与运行版本匹配的 Herdr 使用指导。

## Progressive Skills 与 Capability Truth

epoch 2 的 18-tool catalog 保持不变，但 planner policy 不必永远加载成一份巨大的 Skill 文本。Rust runtime 已内置 compact global `AGENTS.md` 和 7 个按需模块：workstation control、files search、files mutation、Git、execution、agent dispatch、development orchestration。内部 `herdr_mcp.skill.list/describe/load` 仍通过现有 `herdr_call` 进入，不增加第 19 个公共 MCP tool。

模块化 Skill 和“Agent 到底会什么”是两个独立问题。系统不会因为一个进程叫 Pi、Claude、Codex 或 Grok，就直接认定它一定支持 code edit、vision、某个 provider/model 或某档 reasoning。能力事实由 `herdr-mcp scan` 建立：

```text
Herdr Agent manifest
  + executable/version evidence
  + bounded agent-specific probe
  + Herdr live session state
        ↓
Capability Inventory
        ↓
Capability Resolver
        ↓
compact inspect / progressive summary
        ↓
safe dispatch decision
```

静态/半静态 evidence 使用独立 capability SQLite inventory，而不抬高 reliability state DB 的 schema。这样新版本增加 capability 字段后，旧 runtime 回滚时不会因为共享 state schema 过新而无法启动。binary identity、manifest version、probe adapter version 都参与 cache invalidation。status、cwd、project、pane、workspace、session 始终由 Herdr/EventCache 实时负责。

unknown 的语义是**未验证**，不是 false，也不是“按经验应该支持”。probe 必须非交互、有超时、有输出上限、不继承凭据，只允许明确的 Agent 自描述 adapter 把字段升级为 verified。完整 probe evidence 留给诊断，模型可见的 Progressive bootstrap 只得到紧凑计数和已验证 worker trait。

Modular Progressive Skills 已进入 Rust runtime，但通过 `HERDR_MCP_PROGRESSIVE_SKILLS` 保留兼容开关。在 capability-aware 多 Agent UAT 足以支持默认迁移之前，默认行为仍保持 legacy。

## 两条数据路径

系统里有两类通信，理解它们可以解释为什么浏览器扩展和 MCP 都存在。

### 下行：Web AI 操作工作站

```text
Web AI → MCP → Edge → WSS → runtime → files / shell / Herdr
```

这是标准工具调用路径。

### 上行：工作站推动网页继续工作

网页模型有一个天然限制：没有用户消息时，它不会永远主动轮询你的电脑。一个本地 Agent 工作了二十分钟后完成，ChatGPT 本身不会突然醒来问“结果怎么样了”。

浏览器扩展复用同一条受信任本机桥，同时服务两种不同的浏览器路径：

```text
Herdr events → local runtime → Native Messaging → browser extension
                                              │
                                              ├─ Web conversation
                                              │   ├─ progress / settled
                                              │   ├─ recovery
                                              │   └─ conversation rollover
                                              │
                                              └─ Chrome Side Panel
                                                  └─ workspace / pane / Agent 实时观察
```

因此，MCP 负责“网页向机器伸手”；扩展的 Continuity 负责“机器在必要时敲一下正确的网页会话”；Control Center 则把本机真实工作现场送进 Chrome Side Panel。三者职责不同，但共享同一套本机 runtime 与身份边界。

## 浏览器为什么不保存 Herdr bearer

扩展运行在浏览器环境，页面脚本、扩展 storage 和 service worker 都不是保存工作站高权限凭据的理想位置。

当前主链路使用：

```text
content script
   ↓
extension service worker
   ↓ Chrome Native Messaging
native host
   ↓ Unix socket (0600)
herdr-mcp runtime
```

浏览器只和 Native Messaging host 说话；host 再通过仅本机用户可访问的 Unix socket 进入 runtime。静态 `HERDR_MCP_TOKEN` 继续服务本地 curl / Cursor 以及旧 runtime 兼容路径，不应复制到 ChatGPT Connector 或网页脚本。

## managed Git root 是远程文件系统的边界

远程模型不应该默认拥有 `$HOME` 的文件浏览器。`herdr_fs_*` 只接受 Herdr 当前识别的 managed Git project root，并过滤常见 secret-like 路径。

写操作还有几层闸门：

| 闸门 | 作用 |
|---|---|
| managed root | 限制可操作项目范围 |
| `HERDR_MCP_READONLY` | 全局禁止 mutation |
| `HERDR_MCP_WRITE_ROOTS` | 进一步缩小允许写的仓库 |
| dirty confirmation | 防止覆盖未知未提交修改 |
| busy confirmation | 防止和正在工作的 Agent 同时修改同一项目 |

`herdr_exec` 是更强的能力。Shell 可以访问当前用户本来能访问的资源，因此它不具备 `fs_*` 的 secret-path 过滤。这是明确的信任边界：允许远程模型使用 shell，等价于允许它以该工作站用户权限执行命令。

## 为什么 mutation 失败后不能立即再来一次

分布式系统里，“我没收到成功回复”和“操作没有发生”是两件事。

例如 planner 发出 `agent.prompt`，网络恰好在服务器接受任务后断开。客户端看到错误，但 Agent 可能已经开始工作。盲目重试就会生成两个相同任务。

因此 herdr-mcp 把失败阶段尽量结构化：

- `herdr_transport`：连接或 socket 层失败；
- `post_submission_status_wait` / `agent_status_wait_timeout`：任务已经投递，只是等待状态超时；
- `herdr_internal` / `control_plane_taskgroup`：Herdr 控制面瞬时异常；
- 业务错误：根据返回的 `retryable` 和具体语义处理。

mutation 的默认策略始终是：**先重新观察，再决定是否重试。** `herdr_prompt` 支持 `idempotency_key`，同一个客户端意图应复用同一个 key。

## 控制面毛刺为什么不会轻易拖死整个任务

Herdr daemon 的 snapshot / event 聚合偶尔可能出现 TaskGroup / ExceptionGroup 类瞬时错误。herdr-mcp 在几个关键位置采用退化路径：

- `herdr_inspect` 在 snapshot 不可用时可组合 list APIs；
- `herdr_git` 在安全条件满足时可直接调用本机 Git；
- `herdr_exec` 在命令**尚未投递**且 utility pane 控制面失败时，可以切换本机执行；
- 一旦命令已经投递，绝不自动重发，避免双跑；
- SnapshotCache 和 bounded retry 用于只读观察。

这类退化的原则很统一：**读操作尽量恢复可用性，写操作优先保证不会重复执行。**

## Runtime A/B：升级控制面，不惊动 Connector

公网 Edge 地址应该像办公室门牌一样稳定，本机 runtime 可以像机房里的服务器一样升级。

Runtime A/B 把“ChatGPT 连哪里”和“当前跑哪个本地版本”分开。新 runtime 先构建、验证 contract、健康检查，再切换 active generation；旧 generation 可以排空或回滚。详见 [Runtime A/B 自升级](runtime-self-upgrade.md)。

工具契约发生不兼容变化时使用 contract epoch 管理。当前生产为 epoch 2 / 18 tools；旧 epoch 仅用于明确的兼容和回滚场景。

## 进程与职责

| 组件 | 生命周期 | 主要职责 |
|---|---|---|
| ChatGPT / Web AI | 云端会话 | planner、推理、MCP tool use |
| Cloudflare Worker / DO | 公网常驻 | OAuth、MCP endpoint、workstation routing |
| `herdr-link` | 工作站常驻 | WSS、workstation identity、runtime routing |
| `herdr-mcp` runtime | 工作站常驻 | MCP tools、文件/Git/Shell、安全闸门 |
| Herdr daemon | 工作站常驻 | workspace/pane/PTY/Agent/Socket API |
| Native Messaging host | 浏览器按需 | extension ↔ local runtime trusted bridge |
| Browser extension | 浏览器会话 + Chrome Side Panel | HUD 连续工作、Control Center、Queue、JSON→MCP |

## 关键环境变量

| 变量 | 默认 | 作用 |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | 本机 runtime HTTP 端口 |
| `HERDR_MCP_BASE_URL` | 空 | 公网 origin；OAuth `iss` / `aud`，不要带 `/mcp` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr Socket API |
| `HERDR_MCP_READONLY` | 关 | 禁止 mutation |
| `HERDR_MCP_WRITE_ROOTS` | managed roots | 限定可写项目 |
| `HERDR_MCP_ALL_TOOLS` | 关 | 开启高级/兼容工具；正常 ChatGPT 使用保持关闭 |
| `HERDR_MCP_AGENT_ALLOW` | worker + auditor | 控制 inspect/since 展示的 Agent |
| `HERDR_MCP_STATE_DIR` | `~/.config/herdr-mcp` | runtime state / exec journal |
| `HERDR_MCP_OAUTH_DIR` | state dir 下 oauth | OAuth key/client state |
| `HERDR_SKILL_NETWORK` | 开 | `0` 时仅使用 bundled skill |

完整安装变量和 Edge 配置见 [安装](install.md) 与 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## 接着读什么

- 想知道平时怎样用：[最佳实践](best-practices.md)
- 想理解 ChatGPT 公网连接：[ChatGPT Connector](chatgpt-connector.md)
- 想理解网页为何能自己继续：[浏览器扩展](extension.md)
- 想维护本机版本：[Runtime A/B 自升级](runtime-self-upgrade.md)
- 想看工具能力取舍：[能力基准与设计取舍](capability-benchmark.md)
- 遇到异常：[故障排查](troubleshooting.md)
