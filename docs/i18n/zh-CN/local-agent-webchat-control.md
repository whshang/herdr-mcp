# 本地 Agent 控制 WebChat

*本地 coding Agent 如何通过 herdr-mcp 创建、继续、接力并观察受支持的 WebChat 会话。*

本文面向**本地 Agent**：Pi、Codex、Claude，或任何运行在这台开发机上、需要让 ChatGPT/WebChat 参与工作的 coding Agent。它只回答一个问题：

> 我是本地 Agent。什么时候应该通过 herdr-mcp 去驱动一个网页会话？当前到底能做什么？怎么做才不会破坏身份、幂等和交付证据？

Web AI 并不是 Herdr 浏览器能力的唯一调用方。本文的目的就是让本地 Agent 不必等用户先告诉它具体 API 名称。

## 1. 这是什么

Herdr-MCP 有两个方向，它们是不同的契约：

```text
Web AI（planner）                      本地 coding Agent（planner/executor）
      │                                          │
      │ MCP + OAuth Connector                    │ herdr-mcp CLI
      ▼                                          ▼
                    herdr-mcp（控制与状态边界）
                        ├─ Continuity / Work Memory
                        └─ Browser / WebChat control
                                    │ 本地可信路径
                                    │（Native Messaging → 0600 socket）
                                    ▼
                        Chrome 扩展 / 已注册的 browser endpoint
                                    │
                                    ▼
                            ChatGPT / 受支持的 WebChat
```

- **Web AI → herdr-mcp → 开发机**：Web AI 负责规划，herdr-mcp 给它本机工具。
- **本地 Agent → herdr-mcp → WebChat/browser**：本地 Agent 负责规划与执行，herdr-mcp 给它一条受控的、作用于网页会话的通道。

两个方向落在同一个 runtime、同一份 Continuity journal、同一份 browser registry 上。这里没有引入任何“消息总线”，也没有第二套任务状态权威：本地 Agent 复用的就是现有的 Continuity journal、Work Memory 分区、browser 资源和 browser control plane。

| 组件 | 负责什么 |
| --- | --- |
| 本地 coding Agent | 判断*为什么*、*何时*需要网页协作；组织任务；验证结果 |
| herdr-mcp runtime | 控制与状态边界：身份、consent/能力闸门、幂等、交付证据、Continuity、Work Memory |
| Chrome 扩展 | 浏览器侧执行、绑定、唤醒与观测边界；真正执行页面操作并回报观测结果 |
| Browser endpoint | 一个已注册、已授权的可控浏览器 |
| ChatGPT / 受支持 WebChat | 远端会话本身。它**不是**本地 shell，也不是终端 |

网页会话是*按轮次*协作的远端伙伴。Herdr-MCP 把一条有界消息送进去、记录它是否真的送达，并把持久任务状态留在 Continuity 里——而不是留在页面上。

## 2. 什么时候使用

本地 Agent 的典型场景：

- 在同一个 ChatGPT Project 里新建一个会话，让工作留在同一个 Project；
- 继续一个已经绑定到本机的 WebChat 会话；
- 向已有绑定 session 再 dispatch 一条消息；
- 在依赖某个 browser/WebChat session 之前，先确认它是否还活着；
- 本地上下文快满、或工作应转到 Web AI 侧时，做一次 canonical handoff；
- 通过 Continuity/Work Memory 有界恢复历史，再在会话里继续；
- 本地 Agent 完成代码后，让 Web AI planner 接着处理。

多 WebChat / 多账号的可用范围，就等于返回的 browser registry：只能操作 `herdr-mcp webchat resources` 对已检查 endpoint 返回的那些 `session_ref`。没返回的对象不可寻址；除非本文明确说明，任何能力都不是“实验性”的（见“当前边界”一节）。

## 3. 什么时候不要使用

- 普通网站爬取，或任何不是受支持 WebChat 表面的页面。
- 下载文件、在机器之间搬运数据（扩展不是文件传输通道）。
- 把 Herdr 当成 Playwright/Selenium/AppleScript 的通用替代品。
- 绕过账号授权、consent 提示或扩展的控制开关。
- 猜测 Project、账号、会话或 session 标识。
- 直接读取用户 ChatGPT 私有历史正文。从未进入本地 journal 的会话，无法仅凭 MCP 取回。
- 用用户日常浏览会话去做无关的事情。

## 4. 前置条件

以下都是真实的本地前置条件，不是形式要求：

1. **本机 herdr-mcp runtime 健康。**
   ```bash
   herdr-mcp status
   herdr-mcp doctor
   ```
   如果这是源码开发 runtime，先用 `herdr-mcp dev status` 确认通道，确保你知道自己到底在和哪个 runtime 二进制对话。CLI 永远只与*当前生效*的 runtime 通信，不会使用仓库构建产物。
2. **浏览器扩展已安装并连接到该 runtime。**
   ```bash
   herdr-mcp native-host status
   herdr-mcp extension standalone status
   ```
   浏览器控制属于扩展**不能**省略的场景之一：它就是浏览器侧的执行边界。（核心的 Web AI → 开发机链路可以不带扩展运行；这一条不行。）
3. **已注册且授予 WebChat 控制权的 endpoint。**
   ```bash
   herdr-mcp webchat endpoints
   ```
   endpoint 必须报告 `consent.webchat_control: true`。`consent.tool_bridge` 与 `consent.tool_bridge_workstation_mutation` 是另一个能力（网页内 Web AI 工具调用）的独立开关，这里不需要。
4. **扩展已观测到已登录的 provider 账号。** 账号、ChatGPT Project（`space` 资源）和会话（`session` 资源）都以该 endpoint 下的资源形式出现。
5. **不要手工处理凭据。** `herdr-mcp` CLI 是第一方本地客户端，会自己附带受信任的本地调用方 grant。永远不要打印、复制或向用户索要 runtime bearer、浏览器 cookie 或 Connector 凭据。

ChatGPT Connector（OAuth MCP）是**另一个方向**。本地 Agent 控制已绑定 WebChat 会话并不需要 Connector；装了 Connector 也不会自动获得浏览器控制权。

`HERDR_ENV` 与这条链路无关：它管的是原生 Herdr pane/tab/workspace 操作，而不是 `webchat`、`continuity`、`memory`。

## 5. 能力发现

不要从硬编码的 API 名称或记忆里的 `*_ref` 开始。先发现当前真实的能力与身份集合：

```bash
# 1. 哪些浏览器已注册并授权？
herdr-mcp webchat endpoints --limit 5

# 2. 该 endpoint 下有什么？
herdr-mcp webchat resources --kind account
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF

# 3. 这个具体对象是什么？当前可用吗？
herdr-mcp webchat inspect SESSION_REF
```

结果阅读方式：

| 字段 | 含义 |
| --- | --- |
| `endpoint_ref` | 已授权的 browser endpoint。ref 是不透明哈希，永远不要自己合成或改写 |
| `resource_ref` | 精确的账号 / `space`（Project）/ `session`（会话）身份 |
| `parent_ref` | 层级关系：account → space → session |
| `observation_generation` | 扩展针对该资源报告的 generation，作为 `--expected-generation` 传入 |
| `consent.webchat_control` | 该 endpoint 是否允许被驱动 |
| `actuation_available` / `actuation_reason`（来自 `inspect`） | 表示*这次 inspect 调用*的能力，而不是通用写操作预检。写操作的真相以操作返回的 delivery state 为准 |

`herdr-mcp webchat resources` 会返回 `actuation_evaluated: false` 与 `actuation_reason: "not_evaluated_by_list"`；列表结果从不代表健康状态或 consent 状态。

私有方法 `herdr_mcp.browser_endpoint.inspect` 还会暴露扩展声明的能力操作（`capabilities.operations`）和输入契约（仅文本消息、大小上限、不支持附件）。需要机器可读的能力细节时用它；需要资源级结论时用 `herdr-mcp webchat inspect`。

**当前支持的浏览器写操作**（其余一律 fail closed，返回 `code: "unsupported"`）：

| 操作 | CLI | 状态 |
| --- | --- | --- |
| `browser_session.create`（ChatGPT，不带 reasoning effort、不带 required apps） | `herdr-mcp webchat create` | 支持 |
| `browser_dispatch.submit`（普通消息） | `herdr-mcp webchat send` | 支持 |
| `browser_dispatch.status` | `herdr-mcp webchat dispatch-status` | 支持（只读） |
| `browser_session.archive` | `herdr-mcp webchat archive` | 支持 |
| `browser_session.open` | — | 支持的私有方法，无 CLI 包装 |
| `browser_dispatch.stop` | — | 支持的私有方法，无 CLI 包装 |
| `browser_message.append` | — | 不支持 |
| `browser_composer.set_reasoning` / `set_apps` | — | 不支持 |
| `browser_space.create` / `browser_space.open` | — | 不支持 |
| 带 `reasoning_effort` 或 `required_apps` 的 `dispatch.submit` | — | 不支持（普通调用支持） |
| `herdr_mcp.browser_handoff.prepare` | `herdr-mcp webchat handoff` | 支持（canonical packet，可选自动投递） |

能力缺失时如实报告，不要悄悄改用另一套自动化栈。

## 6. 基本工作流

### 工作流 A —— 查看已有 WebChat/browser 会话

```bash
herdr-mcp webchat endpoints --limit 5
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF --limit 10
herdr-mcp webchat inspect SESSION_REF
```

报告你实际使用的身份链：endpoint → account → Project（`space`）→ session，并带上 `observation_generation`。session ref / Project ref 是绑定到本机的（`br_*`/`bep_*`），不是可移植名称，也绝不能靠标签猜测。

如果存在相关 Work Memory 分区，它的 locator（`project_ref`、`repo_id`、`work_chain_id`）来自 `herdr-mcp memory` / Continuity 结果，永远不来自 browser ref，也不能由仓库路径合成。

### 工作流 B —— 在已有 Project 里创建新会话

```bash
herdr-mcp webchat create \
  --endpoint-ref ENDPOINT_REF \
  --provider chatgpt \
  --account-ref ACCOUNT_REF \
  --space-ref SPACE_REF \
  --display-label "简短任务标签" \
  --message "新会话的第一条消息" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]
```

说明：

- `--space-ref` 就是 ChatGPT Project。只有确实要创建账号级默认会话时才省略。
- `--display-label` 只是标签，不是身份；身份以返回的 session ref 为准。
- `--expected-generation` 必须是针对目标范围观测到的 generation。过期 generation 会被拒绝，而不会被施加到错误目标上。
- `--work-chain-id` 用于把新会话关联到已有 Work Memory 链，不要自己编造。
- 一次明确意图的修改 = 一个 idempotency key。发送前就记录下来，重试判断时复用它。
- 当前不支持的能力（例如经 `browser_space.create` 的建 Project 流程、composer reasoning/apps 选择）不得用别的工具去模拟。

成功时返回 `ok: true`，以及创建出的资源和交付状态。请报告返回的 `session_ref`，而不是你自己拼出来的 URL。

### 工作流 C —— dispatch 消息并观察结果

```bash
herdr-mcp webchat send \
  --session-ref SESSION_REF \
  --message "下一条指令" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]

herdr-mcp webchat dispatch-status DISPATCH_ID
```

`send` 返回 dispatch 对象：

| 字段 | 含义 |
| --- | --- |
| `dispatch.dispatch_id` | 持久操作身份，后续所有读取都用它 |
| `dispatch.delivery_state` | `applied`、`not_applied`、`uncertain`、`rejected`、`browser_offline`、`resource_unavailable`、`stopped` |
| `dispatch.execution_state` | 目标轮次是否仍在运行 |
| `dispatch.result.settled` / `assistant_message_ref` / `evidence_id` | 该轮次的持久 settlement 证据 |
| `replayed` | 同一 idempotency key 命中已记录 dispatch 时为 `true` |

超时后的“继续”**不是**重发。用同一个 `dispatch_id` 读 `dispatch-status`；若状态为 `uncertain`，先重新观测会话（`herdr-mcp webchat inspect SESSION_REF`）与真实页面，再决定任何事。`browser_offline` / `resource_unavailable` 表示目标当前不可达——等待并重新观测，绝不要为了探测存活而更换 idempotency key。

停止正在运行的轮次是另一个私有操作（`herdr_mcp.browser_dispatch.stop`），当前没有 CLI 包装。需要它而手上只有 CLI 时，请如实说明这个边界，不要伪造一个“已停止”。

### 工作流 D —— Canonical handoff

canonical handoff 的准备步骤是只读私有方法 `herdr_mcp.browser_handoff.prepare`：

```text
continuity_id（持久任务状态）
   └─ herdr_mcp.browser_handoff.prepare { continuity_id, source_url, [objective], [work_chain_id], [handoff_id] }
        ├─ handoff.message                                   （唯一 canonical 消息）
        ├─ automatic_delivery.params = { source_url, message, work_chain_id }
        ├─ manual_delivery.copy_prompt  == handoff.message    （逐字节一致）
        └─ safety { pre_delivery_retry_limit: 1, retry_requires_no_execution_evidence: true,
                    preserve_mutation_idempotency_key: true, rewrite_rejected_payload: false }
              └─ herdr_mcp.browser_session.create（原样传入 automatic_delivery.params）
                   └─ 目标会话第一步是 continuity.resume <continuity_id>
```

保证安全的关键规则：

- `prepare` 是**只读**的，且从已注册的源会话推导目标路由（account/Project）；它不会先去枚举 endpoint、账号、Project、generation 或 device id。
- 自动投递与手动 **复制提示词** 使用*同一份* canonical 消息。不要另写第二份 handoff 消息，不要编码/混淆它，不要更换 transport，也不要递归包裹被拒 payload。
- 目标会话必须先用**已有的** `continuity_id` 调 `continuity.resume`。handoff 绝不创建第二条 Continuity 链，页面也不会成为任务状态权威。
- 目标恢复后要重新检查实时 workspace / Git / runtime 状态；journal 是历史，不是实时真相。
- 若 host 在 Herdr 尚无执行证据时拒绝投递，最多用**相同**参数与相同 idempotency key 重试一次，随后直接展示已准备好的 Copy Prompt。
- delivery 不确定时，在 reconciliation 证明首次投递未生效之前不会开放 Copy Prompt 路径，因此不会凭猜测创建出第二个会话。

**正式入口**：

```bash
herdr-mcp webchat handoff \
  --continuity-id hc:... \
  --source-url 'https://chatgpt.com/g/g-p-.../c/...' \
  [--objective TEXT] [--work-chain-id ID] [--handoff-id ID] \
  [--idempotency-key KEY] [--prepare-only]
```

`webchat handoff` 是上面那套 canonical 实现的本地薄包装，不是第二套 handoff 实现：

- 它向 runtime 索取 canonical packet（`herdr_mcp.browser_handoff.prepare`），与 Web planner、扩展 HUD 用的是同一条路径；
- 然后把该 packet 自己的 `automatic_delivery.params` **原样**交给基于 source 的 `browser_session.create`：CLI 绝不拼接、改写或重新编码消息；
- `--source-url` 既是审计锚点也是唯一的路由输入：runtime 从已注册会话解析出现有 WebChat 路由（endpoint / account / Project），命令行上不需要传任何 routing id；
- `--objective` / `--work-chain-id` / `--handoff-id` 映射到同一组 `prepare` 入参；`--handoff-id` 还让 packet 拥有稳定身份。

输出是 canonical packet 加投递证据：

| 字段 | 含义 |
| --- | --- |
| `handoff` | canonical packet（continuity_id、source_url、message、work_chain_id、target_context） |
| `automatic_delivery.params` | canonical 创建参数，与 packet 逐字节一致 |
| `automatic_delivery.attempted` / `completed` | 是否发起投递，以及是否到达 `applied` |
| `automatic_delivery.delivery_state` / `reason` / `replayed` | runtime 自己的 delivery 词表与 replay 标记 |
| `automatic_delivery.session_ref` / `dispatch_id` / `result` | 创建出的会话与其 dispatch 证据 |
| `manual_delivery.copy_prompt` | 同一条 canonical 消息，用于手动继续 |
| `instruction` | 用自然语言说明实际发生了什么 |

Idempotency：一次 logical handoff 只用一个 key。不传 `--idempotency-key` 时，CLI 复用 canonical `handoff_id`，因此“原样重跑同一条命令”就是同一次 logical handoff——这正是 canonical 的“用同一 key 最多重试一次”规则。重试绝不要换新 key，也不要期待 CLI 替你重试 uncertain 投递。

`--prepare-only` 跳过投递，只返回 packet（`automatic_delivery.attempted=false`、`reason="prepare_only"`）。

**准备完成不等于已投递。** 当源会话当前未注册、或 browser control 不可用时，packet 与 `manual_delivery.copy_prompt` 仍会返回，同时 `automatic_delivery.attempted=false` 并带上 runtime 的原因。这是可用于手工接力的结果，**不是**已完成的 handoff：只有 `automatic_delivery.completed=true`（即 `delivery_state=applied`）才代表真的新建了会话。`uncertain` 状态会如实返回，绝不会自动重试。

**仍然成立**：`herdr-mcp webchat create` 不接受 `source_url`。基于 source 的投递只留在这条 handoff 路径里，普通 create 接口依旧要求显式 routing id。

## 7. 本地 Agent 示例

一个本地 Agent 任务可能是：

> “在当前已绑定的 ChatGPT Project 里创建一个新会话，把持久 continuity `hc:...` 接过去，让目标会话先 resume，然后在那边继续当前任务。”

Agent 应按这个顺序做：

1. **能力发现** —— `herdr-mcp webchat endpoints`、`herdr-mcp webchat resources --kind space`、`herdr-mcp webchat inspect SPACE_REF`。确认 `consent.webchat_control` 为真并记录 `observation_generation`。
2. **解析身份** —— 从返回资源里挑出确切的 `endpoint_ref` / `account_ref` / `space_ref`。不要猜，也不要复用来自其它机器或其它 Project 的 ref。
3. **解析持久状态** —— `herdr-mcp continuity resume hc:...`（先做有界的 `herdr-mcp continuity search ... --project-path <checkout>` 也可以，但必须遵守 `confirmation_required`）。这是任务持久状态的唯一来源。
4. **执行 canonical handoff** —— `herdr-mcp webchat handoff --continuity-id hc:... --source-url '<确切会话 URL>'`（该链已有 work chain 时加 `--work-chain-id`）。它一步完成 canonical 准备与自动投递；`--prepare-only` 只返回 packet。
5. **验证交付** —— 读 `automatic_delivery.completed` / `delivery_state`。若 `completed=false`，说明什么都没创建：改用 `manual_delivery.copy_prompt`，已有 dispatch 时可用 `herdr-mcp webchat dispatch-status <dispatch_id>`。绝不要换新 `--idempotency-key` 重试。
6. **汇报** —— 返回精确的 `session_ref`、交付状态，以及要求目标做什么。明确说明 `continuity.resume` 在目标侧执行，且“只准备好”不等于已完成 handoff。

永远不要把真实账号 id、token 或生产密钥写进计划、消息或汇报。CLI 返回的 ref 是不透明标识，可以传回 CLI 并向用户报告，但它们不是凭据。

## 8. 投递与重试语义

| 情况 | 正确处理 |
| --- | --- |
| 没有 Herdr 执行/结果证据 | 不推断本地已经执行；使用 canonical 手动路径，或在已有 dispatch 时重新观测 exact dispatch |
| `applied` | 修改已经持久生效，可以继续，并用 dispatch/evidence 身份做后续操作 |
| `not_applied` | 什么都没送达；再次尝试需要明确决策，不能自动循环 |
| `uncertain` | 先重新观测（`webchat inspect`、`dispatch-status`）；绝不盲目重试写操作 |
| `browser_offline` / `resource_unavailable` | 目标当前不可达；等待、重新观测，再按原意图重试——不要用换 idempotency key 的方式探测 |
| `rejected` | 把本次尝试视为已被拒绝；如实报告并停止自动重试 |
| `stopped` | 轮次是被有意停止的；把它当作结果，而不是需要重试的失败 |

补充规则：

- **一次明确意图 = 一个 idempotency key。** 复用同一 key 会返回已记录的 dispatch（`replayed: true`），而不会重复执行。
- **绝不合成身份。** account、Project、session、generation 以及 Continuity/work-chain 标识都必须来自返回结果。
- **交付证据优先于乐观判断。** 超时不是交付；没有报错不是交付；终端或对话滚屏不是 settlement 证据。
- **写操作按账号范围串行化**，不要试图并行执行互相冲突的修改。
- **不要创建第二套状态权威。** 页面、Project、会话都不能替代 Continuity/Work Memory。

## 9. Continuity / Work Memory / Browser session 的区别

| 概念 | 负责什么 | 不要混淆成 |
| --- | --- | --- |
| **Continuity** | 跨会话的持久任务状态；单个 `continuity_id` 的权威 journal | 实时浏览器标签页；Work Memory 分区 |
| **Work Memory** | 更精确的历史分区：`project_ref` + `repo_id` + `work_chain_id` | browser session；单独的仓库路径 |
| **Browser / WebChat session** | 当前网页会话的执行载体（`session_ref`） | 持久任务状态；repo/work chain |
| **Browser endpoint** | 已注册、已授权、可作为控制目标的浏览器 | 用户账号或 Project |
| **Browser 扩展** | 浏览器侧执行、绑定、唤醒与观测 | 文件传输、agent runtime、通用 RPA 层 |
| **Herdr workspace** | 本地开发环境（pane、agent、终端、worktree） | 网页会话 |

最常见的失败模式是把它们压成一个“session id”。`continuity_id`、`work_chain_id`、`session_ref`、`space_ref`、`endpoint_ref` 以及 Herdr pane/workspace id 属于不同命名空间，生命周期也不同。

## 10. 故障排查

| 现象 | 检查什么 |
| --- | --- |
| 完全没有 endpoint | `herdr-mcp native-host status`、`herdr-mcp extension standalone status`、`herdr-mcp doctor`（STANDALONE 通道关注 `standalone-extension-load` 提示） |
| 有 endpoint，但 `consent.webchat_control: false` | 该 endpoint 尚未授予扩展控制开关/consent；这是浏览器侧动作，不是 CLI 参数 |
| account / Project 有歧义 | 重新列举并按返回 ref 选择；不要猜，也不要按“最近一次”选 |
| `browser_resource_not_found` / session 失效 | 重新执行 `herdr-mcp webchat resources`；会话可能已关闭、归档，或被更新的观测取代 |
| 同一个会话被两个 browser endpoint 观测过 | canonical URL 会解析到**最新**的那次观测，因此切换浏览器 profile / 扩展 identity 不会让本来新鲜的会话变成不可用；只有当两个不同 session 的最新观测时间完全相同时才 fail closed（`browser_canonical_url_ambiguous`） |
| dispatch 超时 | 先读 `herdr-mcp webchat dispatch-status DISPATCH_ID`，再观测会话，然后才决策 |
| 写操作 delivery 不确定 | 先重新观测。不要换新 idempotency key 重发 |
| `code: "caller_grant_missing"` | 你不在受信任本地路径上（例如裸 TCP MCP 客户端）。请使用 `herdr-mcp` CLI，它自己附带本地 grant；grant 无法通过 TCP 自证 |
| `code: "unsupported"` | 该操作不在当前支持矩阵内（见“能力发现”），请如实报告而不是模拟实现 |
| 能找到 continuity，但 browser session 已不在 | 先解析持久状态（`continuity resume`），再创建/打开目标会话；不要把页面消失当成历史丢失 |
| session 存在但 `work_memory` 为 `null` | 停在 Continuity 层。没有可用的 `project_ref`/`repo_id`/`work_chain_id`，也不允许合成 |
| 与 Connector 混淆 | Connector 是 Web AI → 开发机方向。本地 WebChat 控制走扩展 → 本地 IPC → runtime，不需要 Connector |

## 11. 当前边界

这一节刻意写清楚，避免有人针对尚不存在的能力写文档或做开发：

- **不支持的浏览器操作**（runtime 返回 `code: "unsupported"`）：`browser_space.create`、`browser_space.open`、`browser_message.append`、`browser_composer.set_reasoning`、`browser_composer.set_apps`，以及带 `reasoning_effort` 或 `required_apps` 的 `browser_dispatch.submit`。
- **支持但当前没有 CLI 包装**：`browser_session.open`、`browser_dispatch.stop`、`browser_endpoint.inspect`、`browser_space.inspect`。它们可通过 runtime MCP 私有方法边界调用，本地 CLI 没有对应子命令。
- **Handoff**：canonical 准备路径是 `herdr_mcp.browser_handoff.prepare`，本地 Agent 通过 `herdr-mcp webchat handoff` 使用它（复用它并接着做基于 source 的投递）。Web planner 与扩展 HUD 仍直接调用该私有方法。`webchat create` 仍不接受 `source_url`，也没有对应的 handoff 参数。
- **`ego-browser`** 是开发/UAT 基础设施，既不是用户依赖，也不是这条 control plane 的替代品。
- **完全没有暴露**：读取用户 ChatGPT 私有历史正文、经 dispatch 契约发送附件、任意 DOM 访问，以及 registry 未报告的任何 provider。

请把这一节当作实现事实而不是路线图承诺：依赖之前先用已安装的 runtime 实测。

## 相关文档

- [浏览器连续工作](browser-continuity.md) —— 页面侧的 continuity、Auto/Queue、手动 handoff 与 Copy Prompt。
- [Browser Control Center](browser-control-center.md) —— Chrome Side Panel 的 workspace/pane/绑定状态。
- [浏览器扩展](extension.md) —— 扩展身份、本地安全边界与 JSON → MCP bridge。
- [CLI reference](cli-reference.md) —— 完整的 `herdr-mcp` 命令面。
- [故障排查](troubleshooting.md) —— runtime、link 与浏览器诊断。
