# 能力对标

读者：做架构取舍和 ADR 决策的 Maintainer / Contributor。本页不是安装教程，也不是普通用户日常使用指南。

> 目的：记录 herdr-mcp 对官方 Herdr、其他 Herdr MCP 实现与 coding-tools-mcp 的能力取舍，避免重复调研、盲目抄功能或扩大工具面。

## 设计原则

herdr-mcp 的目标不是重新包装一遍 Herdr，也不是成为通用 coding sandbox。它服务“网页模型作为 planner，远程驱动本机 Herdr + 工作站”的场景，因此优先：

1. 固定、紧凑、可缓存的 MCP 工具面；
2. 原生 Herdr socket 能力通过 `herdr_methods` + `herdr_call` 保持可达；
3. 文件、Git、shell、图片等“远程网页本身拿不到的工作站能力”才做一等 MCP 工具；
4. mutation 有明确的 delivery / idempotency / rollback 语义；
5. ChatGPT 公网连接与本地 runtime 生命周期解耦；
6. 不为了对标而增加重复 agent orchestration 层。

## 当前吸收矩阵

| 来源/思路 | 能力 | 当前状态 | herdr-mcp 的实现/决策 |
|---|---|---|---|
| coding-tools-mcp | 固定工具目录，不随 runtime/权限模式动态改 tools/list（当前上游固定 20 tools） | **已吸收原则，不照抄目录** | herdr-mcp production contract epoch 2 固定 18 tools，包含 `herdr_skill`。Herdr 约 90 个原生方法不逐一注册。 |
| coding-tools-mcp | workspace 原语：read/list/search/patch | **已吸收** | `herdr_fs_read/list/grep/patch`，另保留 `write/edit` 作为 Herdr 场景的精确写入工具。 |
| coding-tools-mcp | 多文件 patch 的原子性/失败清理 | **已吸收等价保证** | `herdr_fs_patch` + `commitAtomic`；新增文件提交失败会清理，写入受 managed-root/dirty/busy/readonly gates 约束。 |
| coding-tools-mcp | 长命令使用独立 handle，可 read/kill | **已吸收** | `herdr_exec_start/read/kill`；与短命令 `herdr_exec` 分开。 |
| coding-tools-mcp | Git 事实工具 | **部分吸收** | `herdr_git status/diff/log`；暂不单独增加 `show/blame`，需要时可走 `herdr_exec git ...`，避免扩大工具面。 |
| coding-tools-mcp | 图片读取 | **已吸收** | `herdr_fs_image` 直接返回 MCP image。 |
| coding-tools-mcp | HTTP session 与长命令 session 分层；command handle 可跨多次 tool call 继续交互 | **吸收核心思想，ChatGPT 适配不同** | 当前 coding-tools-mcp 的每个 `Mcp-Session-Id` 拥有独立 Runtime；herdr-mcp 为解决 ChatGPT 重启后复用 stale sid 的实际兼容问题，OpenAI/ChatGPT 路径反而保持无状态，而长命令状态独立归 `herdr_exec_start/read/kill` 管理。 |
| coding-tools-mcp | structuredContent 作为稳定机器结果 | **已吸收** | 工具结果保留结构化结果；Relay 对完整 `CallToolResult` 透传，图片等非文本内容不丢失。 |
| coding-tools-mcp | OAuth / PKCE / DCR / protected resource metadata | **已吸收并扩展** | Cloudflare Edge 终止 OAuth；DCR/CIMD、PKCE S256、refresh rotation、private_key_jwt、issuer continuity。 |
| coding-tools-mcp | safe/trusted/dangerous command permission policy | **未照搬** | 当前采用 `READONLY` / `WRITE_ROOTS` / busy/dirty confirmation；shell 是明确的高能力边界，不伪装成完整 sandbox。 |
| coding-tools-mcp | root project instructions 自动注入 initialize | **未吸收** | Herdr/Agent 指令归官方 skill 与具体 agent，自行扫描项目指令容易与 AGENTS/agent runtime 重复。 |
| 官方 Herdr | live socket API 是事实源 | **已吸收** | `herdr_methods` 反射 live schema，`herdr_call` 做 schema-validated passthrough；不复制 90+ 方法。 |
| 官方 Herdr | Agent Skill | **已吸收并完成 remote-planner 适配** | `herdr_skill` 是 epoch 2 的只读工具，返回项目策略 + 与 release 对齐的上游 Herdr guidance，并明确区分站外 Web planner 与 Herdr-managed pane 内 agent；网络失败时回退内置 skill。 |
| 官方 Herdr | Plugin v1 / plugin registry / event hooks / link handlers | **原生可达，不新增专用 MCP tools** | 官方 plugin 能力继续由安装中的 Herdr 提供；`herdr_methods` + `herdr_call(plugin.*)` 可发现/调用 live socket surface。herdr-mcp 不复制一套 plugin 管理 API。 |
| 官方 Herdr | agent prompt 生命周期/blocked/idle/done | **已吸收** | `herdr_prompt` 调原生 `agent.prompt`，默认 fire-and-forget；返回 delivery evidence，wait timeout 与 transport error 分离。 |
| 官方 Herdr | events / session state / persistent background server | **已吸收适合网页的部分** | `herdr_since` cursor 增量恢复、SnapshotCache、`boot_id`；网页 planner 不需要暴露所有 session/lifecycle 方法。 |
| herdr-mesh | agent-to-agent relay / handoff / wait/read | **部分吸收，刻意不做一工具一动作** | `herdr_prompt` + `herdr_since` + `herdr_call(agent.*)` 可完成同类流程；web planner 自己编排，不再增加 `handoff` 中间编排器。 |
| herdr-mesh | 为每种 pane/workspace/session 操作建立独立 MCP tool | **不吸收** | 会造成工具目录膨胀；通过 `herdr_methods` + `herdr_call` 保留完整可达性。 |
| DeepSeek Harness | `dsh --profile headless "job"` 非交互 coding agent | **已验证为备用 worker** | 本机 0.1.1-rc.2 实测可纯回答，也能在临时 Git repo 完成真实代码修改；最终回复可能晚于 60s，因此必须通过 `herdr_exec_start/read` 当长任务运行，timeout 后先查 Git/test 再决定是否重试。Pi/Herdr-native worker 仍优先。 |
| dsh-tui | Harness 全屏交互 UI / session 接管 | **人工 fallback** | 本机 `@deepseek-harness-tui/dsh-tui@0.9.0` profile 可成功 compose；适合人类接管、恢复 session、审批和调试，不作为 Web planner 的默认机器调用接口。 |
| 其他 herdr-mcp | recipe engine / React playground | **不吸收** | recipe 容易形成第二套 workflow DSL；本项目 planner 是网页模型。调试以 tests、CLI、真实 ChatGPT UAT 为准。 |
| 其他 herdr-mcp | HTTP bridge | **已由产品架构覆盖** | `/mcp` + Cloudflare Worker/DO + WSS Link，且公网端与本机 runtime 生命周期解耦。 |

## 本项目额外形成、不是简单对标复制的能力

- `herdr_inspect`：把 workspace/tab/pane/agent、build、exec environment、managed roots 聚成一次廉价 snapshot。
- `herdr_since`：面向“用户发消息才运行”的网页模型，用 cursor 只取新变化。
- `herdr_prompt`：idempotency key、delivery evidence、TaskGroup 与 post-submit wait 的错误分层，禁止 mutation 盲重试。
- `herdr_exec`：可见 utility pane；只有在投递前控制面失败时才 local fallback，投递后绝不双跑。
- SnapshotCache + list-API fallback：Herdr `session.snapshot` 毛刺不再把远程文件/Git工作误判为业务阻塞。
- Cloudflare stable Edge：OAuth、MCP transport、Durable Object、单 active link fencing、runtime offline 的结构化错误。
- Runtime generation A/B：在**同一 contract epoch** 内原子切代、drain、rollback，不重启 Link；Edge heartbeat 同步当前 generation/version。跨 epoch 迁移单独受控执行。
- 浏览器扩展反向通道：Herdr → `/push/events` → 浏览器 → 当前网页对话，补足 MCP 只有请求方向、长任务后网页不会自动继续的问题。

## 暂不加入

1. **后续 contract epoch 不允许隐式变化。** epoch 2 / `herdr_skill` 已成为 production 目标；以后任何 tool catalog 变化都必须冻结成新 epoch、显式迁移，并在新的 ChatGPT 对话中验证 tool snapshot。
2. **不复制几十个 pane/agent/workspace MCP 工具。** Live Herdr API 通过两个通用工具可达。
3. **不建立 recipe DSL / 第二 planner。** Web ChatGPT 是唯一高层 planner。
4. **不宣称 shell sandbox。** 权限边界保持显式，后续若需要更强隔离单独设计。
5. **不把浏览器扩展改成走公网 Worker。** 扩展是同机反向通道；当前版本通过 Native Messaging + runtime 权限为 `0600` 的 Unix Socket 通信，公网 OAuth 与本机静态 runtime 凭据继续分离，浏览器侧不接收 Herdr bearer。

## 维护方式

- 每次准备新增 MCP 工具，先判断是否能由 `herdr_call` 或现有工作站原语表达。
- 每次对标上游项目，更新本表的“吸收 / 部分吸收 / 不吸收”结论，而不是直接复制 API。
- production ChatGPT tool catalog 变更必须走新的 contract epoch；runtime 实现升级不等于 ABI 升级。
