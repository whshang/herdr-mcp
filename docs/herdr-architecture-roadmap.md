# Herdr Architecture Roadmap

状态：实施中
原则：用效率最高、可能是最复杂但对用户最友好的方案，不追求短期收益。

**First-GA gate SSOT**：[`docs/ga-release-gate.md`](./ga-release-gate.md)。**当前已发布 Rust runtime stable**：`v0.4.1`；`v0.4.2` source candidate 已合入但尚未 tag/publish。稳定 TCC broker 已完成跨 generation 授权验证；Developer ID 仅为 optional hardening。tag 前仍需 exact-final-source Rust Release qualification、final Artifact Relay/R2 deploy-import-readback UAT、PR #199 / generic relay 收敛、pane-session PR #200 合入；若最终纳入 `continuity.search` 则完成其集成。[`docs/ga-candidate-status.md`](./ga-candidate-status.md) 保留 `v0.4.0` 首次 stable 的 GA closure snapshot。**Release model**：[`docs/release-model.md`](./release-model.md)。是否可正式对普通用户开放只看 GA 门禁（G1–G25 + 八个 veto），不是「Rust 重构做完」。

> 历史迁移/发布 chronology 已拆分到 [`docs/history/architecture/rust-native-rearchitecture.md`](./history/architecture/rust-native-rearchitecture.md)（Rust 原生化重构细节）与 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)（18 个工具性能实现细节与基准）。本文件只保留当前架构与路线。

## 当前产品事实

- **Rust runtime 拥有生产 MCP / service / updater / Link / Native Messaging 路径**；`bin/herdr-extension-host` 只是委托 Rust 的兼容入口，不是第二套 installer/broker。
- **生产 Link 是 Rust**，执行 `~/.config/herdr-mcp/runtime/current/herdr-mcp link run`。
- **公共 MCP contract 保持 epoch 2 / 18 tools**，不增加第 19 个 tool，不因内部优化改变 schema。
- **浏览器控制面是有界的**：不宣称 browser true-steer；高风险的 Terminal Input / 任意 Herdr method 保持 preview-only。
- **`v0.4.2` 未发布特性一律描述为 development/upcoming**，直到 `v0.4.2` 通过 release/UAT gate 并正式发布后才作为当前产品能力呈现。

## 总体目标

Herdr 性能优化不以单点 benchmark 为目标，目标是建立长期可演进的执行架构：

- 用户只看到稳定、高速、低等待的工具体验；
- Rust runtime 负责确定性、安全边界和可靠状态；
- 工具性能优化不破坏 epoch 2 / 18 tools contract；
- 新能力优先进入内部 runtime/state，不增加模型每轮 schema 负担。

核心原则：

1. 先减少 MCP/model round-trip，再优化局部代码。
2. 先消除重复工作，再增加并发。
3. read-only 可以并行，mutation 保持有序和 fail-closed。
4. 性能优化不能降低 managed root、secret path、idempotency、generation fencing 安全等级。
5. 架构一次设计完整，实现按风险和验收顺序推进。

## 当前执行重点

当前主线有两个彼此解耦的执行面：

1. **Browser extension / Store / real-browser UAT** — 浏览器扩展持续独立迭代；Store 发布、实验站点 UAT、Control Center/continuity 修复使用独立扩展版本与分支，不要求每次都发布 Rust runtime。
2. **`v0.4.2` quality/consolidation** — 实施计划与验收证据已归档到 [`history/v0.4.2/v0.4.2-quality-docs-and-operations-plan.md`](./history/v0.4.2/v0.4.2-quality-docs-and-operations-plan.md)。Wave A reliability、Wave B measured efficiency、Wave C docs taxonomy 与 docs-site homepage/navigation redesign 均已完成；稳定版只通过 tag-driven signed Release 与 supported updater dogfood 收口。

当前已发布 stable runtime 仍为 **`v0.4.1`**；`v0.4.2` source candidate 已合入 main，**尚未 tag**。稳定 TCC broker 已完成跨 generation 授权验证；Developer ID 仅为 optional hardening。tag 前仍需 exact-final-source Rust Release qualification、final Artifact Relay/R2 deploy-import-readback UAT、PR #199 / generic relay 收敛、pane-session PR #200 合入；若最终纳入 `continuity.search` 则完成其集成。production Rust ownership / updater / Link / Native Messaging runtime sync 已经是基线，不再重复做 alpha-era cutover。第一版 GA 的 `v0.4.0` G1–G25 与历史 release/UAT 证据继续由 [`ga-release-gate.md`](./ga-release-gate.md) 和 `docs/history/ga/` 保存，不能为了让旧文档看起来“更新”而改写历史版本号。

活跃但不自动进入 `v0.4.2` feature scope 的长期设计包括 [`_wip/multi-device-worker-control-plane.md`](./_wip/multi-device-worker-control-plane.md) 与 Browser Control Plane 的后续扩展；只有测量结果或明确版本计划批准后才转为实现任务。

## 已完成并验收

### Rust Native Runtime

状态：已完成，已验收。

内容：

- Rust runtime 单一产品边界；
- epoch 2 / 18 tools contract 固化；
- transport、state store、runtime generation 基础完成；
- Shared Local State Store 建立 SQLite/WAL/schema migration/transaction 基础。

验收：

- Rust CI、workspace tests、contract tests；
- 18 tools parity；
- runtime state 不替代 live state。

### Modular Progressive Skills / Capability Scan

状态：Progressive implementation 已进入 production binary；根据 `v0.4.2` Wave B 的 contract/consumer 审计，当前继续 opt-in、默认 OFF。Capability Scan / Resolver 已补齐。

冻结边界：

- public MCP 继续 epoch 2 / 18 tools，不增加第 19 个 tool；
- `herdr_mcp.skill.list/describe/load` 只走现有 `herdr_call` local namespace；
- giant policy 拆为 global `AGENTS.md` + 8 个 on-demand Skill；其中 `engineering-robustness` 把 regression-first、silent-wrongness、AI self-verification 与多 state-plane 验收作为按需 reference 内化；
- `HERDR_MCP_PROGRESSIVE_SKILLS` 在真实多 Agent UAT 前保持兼容默认；
- unknown capability 永远不按 Agent 名称猜测。

Capability truth：

```text
Herdr manifest + binary/version + bounded safe probe + live Agent state
    → capability inventory
    → capability resolver
    → inspect/progressive compact projection
    → dispatch decision
```

持久化边界：capability inventory 使用独立 SQLite schema，不提升 shared reliability `state.db` schema，避免新版本写入 capability metadata 后让旧 runtime rollback 因“state schema too new”失效。live status/cwd/project/pane/workspace/session 仍只认 Herdr/EventCache。

生产迁移门禁：scan real smoke、resolver regression、capability-aware dispatch UAT、Progressive candidate ON A/B、CI/Grok audit 全 PASS 后，才评估 default ON；feature flag 是迁移/rollback gate，不应永久替代默认迁移决策。

### Batch A Performance

状态：已完成，已验收。

范围：保持 epoch 2 / 18 tools contract 不变。

已完成：

- A1：消除重复 Git status。
- A2：fs_patch 单次 validation 与 dirty batch。
- A3：inspect EventCache fast path、since 事件压缩。
- A4：prompt/exec_read 高频路径优化。
- A5：herdr_skill tool wave、worktree lifecycle 规则。

验收：

- A/B benchmark：fs_read p50 86.657ms → 2.894ms；fs_list 110.653ms → 5.855ms；fs_grep 119.622ms → 25.019ms；git status 126.481ms → 39.953ms。
- Rust/Node/Edge gate 通过。
- epoch2/18 tools identity 不变。

### Result Optimization first wave

状态：已完成，已验收（#53–#56）。

范围：不改 epoch 2 / 18 tools inputSchema；在结果进入模型前压缩展示。

已合入：

- `herdr_git status` 按目录分组与 counts（#53）；
- exec 成功大输出 head/tail（#54）；
- `herdr_git` diff/log compact（#55）；
- `herdr_fs_grep` group-by-file（#56）。

Evidence Store 未做，等真实恢复需求。

### Project Context Cache first slice

状态：已完成，已验收（#58）。整体 Project Context Cache 仍为 P1。

内容：mutation 工具（`fs_edit` / `fs_write` / `fs_patch` / `exec_start` / `herdr_exec`）在同一请求内复用一次 `projects::derive_routing`，经 `validate_*_with_topology` / `check_with_topology` / `working_agents_from` 传递；不改 epoch/schema，不新增 tool。

后续：更深 cache 需基准证明后再加深；不产生第二事实源。

### Streaming First（#62+#66）

状态：已完成，已验收（#62+#66）。整体 Streaming First 仍为 P1；同步工具仍无 mid-call stream。

内容：

- #62：`herdr_exec_start` / `herdr_exec_read` 结果增加 phase 与 progress；
- #66：同步 `herdr_exec` / `herdr_fs_grep` 完成结果对齐 phase/progress（`phase=completed` 与 timing/counters），不改 epoch/schema，不新增 tool。

未覆盖：同步工具仍无 mid-call stream；MCP sync 调用仍阻塞至完成。

### Skill wave guidance (#61)

状态：已完成，已验收（#61）。skill 层收紧 compact 结果与长 exec 指引；无 runtime Wave Scheduler。

### health_watchdog in service status (#63)

状态：已完成，已验收（#63）。`service status` 单独暴露 `dev.herdr-mcp.health-watchdog`，与 legacy Node watchdog 字段区分。

## 已完成待验收

### Cloudflare Edge read fast path

状态：已完成实现，持续观察；#60 harden 已合入。

原因：DO rows_written 日限额影响整体可用性。

结果：

- read-only 请求脱离 durable request ledger；
- mutation 保留 durable fail-closed；
- #60：link-drop / quota-exhausted 场景下 ephemeral read 先结算再 session 持久化，避免 write 配额耗尽后读路径失效。

后续验收：

- 长期统计 rows_written / MCP call；
- 不同流量模型下额度稳定性；
- 生产流量下确认 #60 harden 后 ephemeral read 仍可用。

### Long Task Progress Observability

状态：已设计，未实现。

原因：解决长任务无反馈问题，不属于单工具 latency。

方案：Task Journal、phase event、checkpoint/evidence、progress rendering。

验收：CI/release/deploy/self-upgrade 全程有阶段状态；新 conversation 可恢复任务阶段；不增加短任务噪声。

### Search Execution first slice

状态：已完成实现，待生产验收（#52）。

内容：`herdr_fs_grep` 优先 rg（含常见 PATH），Rust walker 回退；与 Result Optimization grep compact 共用 finish path；`engine` 为 `rg` 或 `rust`；不新增第 19 个 tool。

后续验收：大仓库延迟与回退行为；IndexBackend 等其余 Search 架构仍属规划中。

### Link candidate daemon staged (#65)

状态：历史实现已完成；后续生产切流也已完成。`#65` 保留为 candidate/staged 演进证据，当前生产 Link 已由 Rust runtime 持有，不存在待完成的 Node → Rust Link cutover。

## 规划中

### Search Execution Architecture

状态：P2，first slice 已合入（见上）；其余（Query Planner、IndexBackend 等）未实现。大仓库 `fs_grep` 目标路径仍为 Security Layer → Query Planner → RgBackend / RustFallback，IndexBackend 更后。不新增第 19 个 tool。

### Batch B Tool Batch Architecture

状态：P2，设计中。只有 Layer 3 Connector UAT 证明 MCP/model round-trip 是主要瓶颈后进入。不新增第 19 个 tool，不绕过 contract epoch。

### IngressProfile

状态：规划中。统一 cloudflare-edge / local-tunnel / relay-vps 入口，同一 MCP contract 与 mutation safety。Edge `rows_written` 观察稳定后再实现。

## AI Tool Runtime Optimization Architecture

状态：Result Optimization first wave、Project Context Cache first slice、Streaming First（#62+#66）已合入。**`v0.4.2` quality/consolidation** 的 implementation waves、documentation taxonomy 与 docs-site redesign 已完成并进入稳定发布/升级验收；更深 PCC 与 Batch B 仍不进入本版本主线。

参考 rtk-ai/rtk：核心不是改工具执行本身，而是在输出进入模型上下文前过滤、分组、截断、去重。Herdr 不复制 CLI proxy，在 Rust MCP runtime 内压缩展示，raw 事实仍可从同一次结果或后续 evidence 恢复。不改变 epoch 2 / 18 tools inputSchema。

优先级：

1. 工具执行速度（Batch A 已验收）
2. 工具结果进入模型的效率（Result Optimization first wave 已合入）
3. 多工具协同调度
4. 长任务持续运行与反馈（Streaming First #62+#66 已合入；同步工具仍无 mid-call stream）
5. 资源生命周期管理

### Result Optimization Layer（P0）

```text
MCP Tool
  ↓
Execution Layer
  ↓
Result Optimization Layer
  ↓
LLM Context
```

First wave（已合入 #53–#56）：

- `herdr_git status`：解析 porcelain `-b`；`counts`；超阈值按目录分组，小仓库保留原始 porcelain；
- exec 成功大输出 head/tail；
- `herdr_git` diff/log compact；
- `herdr_fs_grep` group-by-file（与 Search rg/rust finish path 共用）。

Evidence Store 在有真实恢复需求后再做，不预留抽象接口。

验收：response bytes 下降；失败诊断完整；inputSchema 不变。

### Tool Wave Scheduler（P0 设计 / skill 已有策略）

`herdr_skill` 已要求独立 read 并行、mutation 有序；#61 收紧 compact 结果与长 exec 的 skill 指引。Runtime 级 dependency graph 调度等 Batch B 证明 round-trip 仍是瓶颈后再做；当前仍仅 skill 层，无 runtime scheduler。

### Project Context Cache（P1；first slice 已合入）

First slice（#58）：mutation 路径单次 `derive_routing` 复用。Batch A 已拆掉 routing 上的全仓库 status。更深 cache（跨请求/高频 read 侧）需基准证明后再加深；失效策略正确，不产生第二事实源。

### Streaming First（P1；#62+#66 已合入）

#62：`herdr_exec_start` / `herdr_exec_read` 结果带 phase 与 progress。#66：同步 `herdr_exec` / `herdr_fs_grep` 完成结果对齐 phase/progress。长 grep/exec/test/build 的 mid-call stream 仍未做；MCP sync 工具仍阻塞至完成。下一片是生产装新 generation 验证这些字段，不是再开 Evidence Store 或 Wave runtime。

## 不纳入当前路线

- 仅微优化内存分配；
- 无用户感知收益的小对象优化；
- 复杂索引系统（搜索架构验证前）；
- 过早引入模型专属输出格式；
- 第 19 个 MCP tool、epoch 2 schema 变更。

## 实施顺序

Roadmap 不与当前 Rust parity 并行扩张 public surface。顺序固定为：

```text
18-tool native parity
  → production transport parity
  → Shared Local State Store foundation
  → supervisor / Native Messaging / updater / link
  → 删除 Node runtime
  → Reliability Kernel
  → Continuity 2.0
  → Work Context & Evidence
  → Product Completion hardening
```

截至 `v0.4.1`，`18-tool native parity → production transport parity → Shared Local State Store foundation → supervisor / Native Messaging / updater / link → Node runtime removal` 已成为生产基线。`v0.4.2` 已完成 `Wave A release/test hardening → Wave B measured efficiency → Wave C docs taxonomy → docs-site redesign`，保持 epoch 2 / 18 tools public contract 不变，并加入 crash-safe Continuity Journal foundation：绑定 Web 会话的 finalized turn 可增量进入 Rust `state.db`，新会话可用稳定 `continuity_id` 经现有 `herdr_call` 恢复有界上下文；ID-only 接力前必须实时确认 Rust chain 仍存在，失败时继续使用既有 handoff packet 路径。完整 Continuity 2.0（rolling semantic checkpoint、更多 browser control state Rust 化、长期 retention 策略）与 Work Context/Evidence 保持后续路线。

### 未来版本目标：Continuity 2.0

Continuity 2.0 是 `v0.4.2` 之后的正式未来版本目标之一，但**当前不绑定具体版本号，也不纳入 `v0.4.2` scope**。版本归属应在 `v0.4.2` 发布并完成一段真实 dogfood 后，根据恢复成功率、journal 增长速度、resume token 成本、conversation rollover 频率和浏览器内存/主线程压力等数据进入 release planning。

该阶段的产品目标是把 `v0.4.2` 的“持续保存 raw turn、崩溃后可恢复”升级为“长期任务始终维护一份有界、结构化、可验证的当前工作状态”。核心目标包括：

- rolling semantic checkpoint：把较老 raw turns 增量压成 `objective / completed / decisions / constraints / active / pending / files / branches / commits / anchors / next_actions` 等结构化状态；
- bounded resume：恢复时优先读取“最新 checkpoint + 最近 raw tail”，不重复把完整长会话重新送回模型；
- incremental compaction：Sidecar 只处理 `previous checkpoint + new raw tail`，避免每次 handoff 重新压缩 80k+ conversation；
- verified retention：达到 raw cap 时必须先生成并验证新 checkpoint，再回收最早 raw body；不能直接截断导致信息不可恢复；
- Rust-owned continuity state：逐步把 handoff ticket、ACK、checkpoint、retention 和更多 browser continuity control state 收敛到 Shared Local State Store；
- browser pressure integration：把模型上下文压力、页面内存、长 DOM 与主线程/render 压力共同作为 rollover 信号，并在确认 target 已接管后安全 retire/discard source tab；
- fail-closed recovery：stale generation、重复 wake、正在生成、未确认 mutation、checkpoint/ACK 不确定等状态不得静默推进。

实施顺序继续保持 `Reliability Kernel → Continuity 2.0`。Reliability Kernel 提供 `op_id`、idempotency、delivery phase 与 uncertain reconciliation，使 checkpoint 生成、写入、ACK、raw prune 等有副作用动作在 timeout/runtime restart 后仍能判断真实结果。详细设计与初始阈值见 [`docs/history/architecture/rust-native-rearchitecture.md`](./history/architecture/rust-native-rearchitecture.md#phase-8continuity-20)。

工具性能作为独立 lane 演进，详细历史与基准见 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)。Batch A/B 的普通优化不得改变 epoch 2 / 18-tool visible contract；生产 Rust Link 已是稳定基线，不再作为性能 lane 的并行 cutover 任务。只有测量证明固定 MCP/model round-trip 仍是主要瓶颈后，才在未来明确评估 multi-operation tool schema / JSON-RPC batch 与 contract epoch 演进，禁止把 model-visible schema 变化混入普通 Rust 重构。

长任务可观察性作为性能与可靠性并行 lane 纳入 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)。该 lane 通过 Task Journal、phase event 和 checkpoint 提供长 release/CI/deploy/self-upgrade 过程的阶段反馈，不替代 Git/runtime live state，也不改变 epoch 2 / 18 tools contract。

## 暂不进入主线

以下能力保持观察，不进入当前 Roadmap：

- 自建新的 Agent Team runtime；
- 强制引入 ACP 作为内部主协议；
- 复制 Luvus Task/Lease/merge gate 系统；
- 绑定单一 Coding Agent 或要求本机必须安装某个 Agent；
- 第二套 Web Agent 编排系统；
- 为所有操作强制引入 Task/Project/Workflow 对象；
- 通用浏览器自动化平台。

未来只有在真实使用数据证明现有 Herdr + fs/Git/exec + 可替换 worker 无法表达需求时，再通过 adapter/plugin 或新的 contract epoch 评估。
