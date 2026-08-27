# herdr-mcp Tool Performance Optimization

状态：实施中  
日期：2026-08-27  
主线基线：`5fac467`（PR #43 grep/auto-continue hotfix + PR #44 Rust WebSocket driver 已合入）  
目标 contract：保持 epoch 2 / 18 tools 不变完成 Batch A；只有 Batch B 明确评估 model-visible schema 演进。

## 1. 为什么单独建任务文档

`docs/rust-rearchitecture.md` 继续承担 Rust 原生化架构、生产切流和 readiness 总游标；本文件承担 18 个工具的性能实现细节、基准、ChatGPT 编排策略和 workspace/worktree 生命周期。

原因：

- Rust 总计划已经很长，继续塞入每个工具的微观优化会降低可维护性；
- 性能优化与 link Rust 重构可以并行，但有不同文件边界和验收门；
- Batch A 大部分属于内部实现，不应因为性能工作顺手改变 epoch 2 / 18-tool ABI；
- Batch B 的 batch 参数或 JSON-RPC batch 若改变 model-visible schema，应作为独立 contract 决策，不伪装成普通内部重构。

## 2. 当前问题模型

一次简单 Web → MCP 工具调用包含三层延迟：

```text
model plan/re-entry
  + Connector/MCP round trip
  + local Rust/Herdr/Git/filesystem work
```

当前实测轻量 MCP 调用常有约 0.5–1.0s 外部往返；因此十几个可独立的小调用被模型串行发出时，仅 envelope/connector 往返就可形成数秒级体感延迟。

Rust 内部另有重复工作：多个工具每次都从 `EventCache.snapshot()` 克隆状态，再重复 project topology / managed root / Git status 推导。尤其 `fs_security::managed_roots()` 和 `mutation::working_agents()` 都调用 `projects::derive()`，而 `derive()` 会对 managed roots 执行 Git status。

此外，过量 workspace/worktree 会放大初始化和磁盘成本。当前开发机曾观察到 `~/.herdr/worktrees` 下多个 GB 级 checkout 和重复 `node_modules`；因此 worktree 不是免费临时对象，必须有生命周期。

## 3. 与正在进行的 Rust 重构如何并行

当前 main 已完成：

- Relay/link Batch 1：validation；
- Batch 2：protocol/message model；
- Batch 3：wire adapter；
- Batch 4：reliability kernel；
- Batch 5：lifecycle/heartbeat/policy；
- Batch 6A：transport reactor core；
- Batch 6B：真实 Rust WebSocket driver（PR #44 已合入）。

当前独立 `feat/link-runner-rust-parity-20260827` 工作区只规划/实现下一层 local MCP/runtime dispatcher，不应被性能批次修改。

并行文件边界：

```text
link runner lane
  crates/herdr-mcp/src/link/**
  transport/runtime dispatch parity

performance lane
  projects.rs / fs_security.rs / mutation.rs / fs_patch.rs
  inspect.rs / state_cache.rs / exec_sessions.rs / prompt.rs
  skill.rs / assets/herdr-mcp-SKILL.md
  task docs / benchmarks
```

若文件边界发生重叠，先停 performance lane 对该文件的 mutation，等 link lane 合并后再 rebase，不做双向手工拼接。

## 4. 优化原则

1. **先减少 MCP round trips，再优化纳秒级局部代码。**
2. **先消除重复工作，再增加并发。** 已经重复执行两次的 Git status 不应仅改成并发执行两次。
3. **read-only independent operations 可并行；mutation 默认有序。**
4. **保持 fail-closed safety。** managed-root、secret-path、dirty、busy、idempotency、generation fencing 不为性能让路。
5. **Batch A 不改变 epoch 2 / 18-tool visible contract。**
6. **Batch B 只有基准证明收益后才评估 batch 参数 / JSON-RPC batch。**
7. **模型行为是性能系统的一部分。** `herdr_skill` 必须教 ChatGPT 按 wave 编排工具，并限制 worktree 泄漏。

## 5. 18 个工具的优化清单

| Tool | Batch A：不改 visible contract | Batch B：需 contract 决策时再做 |
|---|---|---|
| `herdr_methods` | schema cache prewarm；stale-while-revalidate；更小内部查找成本 | exact/prefix/compact 参数；validation error 直接附 method schema |
| `herdr_inspect` | fast cached path；避免不必要 full reconcile；Git status 只在需要的 inspect 路径执行 | compact / include_git / include_exec_sessions / workspace filter |
| `herdr_call` | 复用 schema cache；减少 socket/schema重复准备 | `calls:[...]` ordered/parallel native batch |
| `herdr_since` | digest coalescing；workspace filter 同时过滤 workspace payload；限制重复状态事件 | max_events/compact/multi-workspace 参数 |
| `herdr_fs_read` | managed-root 推导不跑 Git status；大文件避免无意义全文加工 | `reads:[...]` multi-read |
| `herdr_fs_list` | managed-root 轻量推导；递归遍历减少不必要 metadata | multi-root；names-only/compact |
| `herdr_fs_grep` | 保留 PR #43 root-relative glob 修复；基准后决定 rg/parallel walker；避免重复 project status | multi-pattern one-scan |
| `herdr_fs_image` | managed-root 轻量推导；避免额外 topology/status | metadata/thumbnail 参数 |
| `herdr_fs_edit` | security + busy routing 不跑 Git status；dirty check 只跑目标文件 | batch edits；expected hash/version |
| `herdr_fs_write` | 同上；保持 atomic write | batch writes；expected hash/version |
| `herdr_fs_patch` | root validation/topology 只做一次；target containment 基于已验证 project root；dirty files 合并 Git query | 暂无新增工具；继续作为首选 multi-file mutation primitive |
| `herdr_git` | managed-root validation 不额外跑全 repo status；status 自身只执行一次 | composite status+diff+log read bundle |
| `herdr_exec_start` | security/busy routing 轻量化；保持 fire-and-forget session | `commands:[...]` independent start batch |
| `herdr_exec_read` | 直接按 offset 读取增量，避免 clone/sort/flatten 全历史 | multi-session read；wait-until-output/exit |
| `herdr_exec_kill` | 维持低优先级；减少无意义 prune | kill-many |
| `herdr_exec` | project selection 与 busy routing 避免重复 status；复用 utility pane readiness | bounded command bundle；明确 local/utility backend policy |
| `herdr_prompt` | 优先复用 `agent.prompt` response 的 agent state，删除固定 250ms sleep + 第三次 probe；legacy response 最多一次 fallback `agent.get` | multi-prompt fanout；后续可进一步用 EventCache 取代 before probe |
| `herdr_skill` | 增加 latency-aware waves、capability-aware policy、worktree/workspace lifecycle；live context 暴露 runtime capability | 当 batch contract 存在时动态教模型 batch 优先级 |

### 用户确认的“最值得先做 6 项”映射

这 6 项全部在本计划内，但按 contract 风险分层推进：

| 优先级 | 项目 | 所属阶段 | 当前状态 |
|---|---|---|---|
| 1 | ProjectTopology / managed roots / busy-agent 派生状态缓存/复用 | Batch A A1/A3 | 第一轮已把最昂贵的 Git status enrichment 从 routing gate 拆掉，并让 inspect 复用 EventCache；更深的 topology object cache 只有基准证明仍是瓶颈时才加，避免先引入复杂 invalidation |
| 2 | `herdr_fs_grep` 换回 ripgrep | Batch A 下一独立切片 | PR #43 已修 root-relative glob；下一轮做 Rust walker vs `rg` 基准并恢复 `rg` fast path，保留安全 fallback |
| 3 | 批量 API | Batch B | 明确属于 model-visible contract 演进；优先扩展现有 18 tools，不增加第 19 个工具 |
| 4 | `herdr_prompt` 去多 probe + 250ms sleep | Batch A A4 | ✅ 第一轮完成 |
| 5 | `herdr_since` 事件合并 + compact | Batch A A3 + Batch B | ✅ A3 仅去除完全相同的相邻 update 并过滤 workspace payload；`compact/max_events` 参数留 Batch B |
| 6 | `herdr_inspect` fast path | Batch A A3 | ✅ 健康 EventCache 走 cache fast path；stale/reconcile 时 fail-safe 回 live fetch |

## 6. Batch A 实施顺序

### A0 — 文档、基准和边界

- [x] 新建本任务文档；
- [x] 在 `rust-rearchitecture.md` 增加本性能 lane 指针与并行边界；
- [ ] 建立微基准/可重复 smoke，至少覆盖 managed-root validation、mutation busy check、multi-file patch；
- [x] 记录基线调用次数，而不是只记录 wall-clock。

### A1 — 消除重复 Git status（第一实现片）

状态：✅ 已实现并通过 targeted tests。

目标：把 project routing/topology 与 Git dirty/status enrichment 分离。

计划：

- 新增 lightweight topology derive：识别 pane→workspace、cwd→Git root、managed root，但不执行 `git status`；
- `fs_security::managed_roots()` 使用 lightweight topology；
- `mutation::working_agents()` 使用 lightweight topology；
- full `projects::derive()` 仍给 `herdr_inspect` 等确实需要 dirty/changed_files 的路径；
- 保持结果语义不变；增加测试证明 lightweight path 不触发 status enrichment，并验证 full derive 仍报告 dirty state。

预期收益：

```text
fs_read/list/grep/image: 1 次全 repo status 扫描 -> 0
fs_edit/write/exec_start: 常见 2 次全 repo status 扫描 -> 0
fs_patch: root/target/busy 路径的多次全 repo status 扫描 -> 0
herdr_git status: validation status + requested status -> 仅 requested status
```

### A2 — `fs_patch` 单次 project validation / dirty batch

状态：✅ 已实现并通过 targeted tests；dirty query 使用 `--no-renames`，包含 rename 回归。

- root managed validation 只做一次；
- 每个 patch target 在已验证 root 下做 lexical/canonical containment + secret path gate，不重新 derive topology；
- 收集所有 dirty sources 后一次 Git status query；
- 保持 atomic transaction 和 cross-project fail-closed。

### A3 — `since` / `inspect` 输出与缓存

状态：✅ 第一轮已实现。

- 只 coalesce 相邻且**语义完全相同**的 workspace/pane/tab update；真实状态变化和 lifecycle event 全部保留；
- inspect 优先读取 EventCache，只有 freshness/reconcile 条件满足时触发 hard refresh；
- 不让 compact 优化改变 epoch2 schema，新增字段/参数留 Batch B。

### A4 — prompt / exec session

状态：✅ 第一轮已实现。

- prompt successful ACK 优先复用 `agent.prompt` response 中的 agent state；legacy response 最多保留一次 fallback `agent.get`，删除固定 250ms sleep + 第二次 probe；
- exec_read 按 byte offset 直接读增量，不重建全部历史输出。

### A5 — ChatGPT operating policy / worktree lifecycle

状态：✅ 已写入 bundled `herdr-mcp-SKILL.md`，live runtime context 同步暴露当前并发/batch capability。

更新 `assets/herdr-mcp-SKILL.md`，至少加入：

```text
Plan tool calls in waves.
Parallelize independent reads.
Reuse known workspace/pane/root identities.
Prefer since(cursor) over repeated inspect.
Create worktrees only for independent mutation lanes.
Reconcile and reclaim completed worktrees before creating more.
```

worktree policy：

1. 创建前：`inspect` + `worktree.list(repo)`，优先复用当前 workspace / sibling pane / existing worktree；
2. read-only 调研、grep、Git facts、review、测试默认不新建 worktree；
3. 只有独立 mutation lane、dirty 隔离或用户明确要求时才创建；
4. 创建 worktree 不等于自动执行 `npm ci` / install；只有任务真正需要依赖时才 bootstrap；
5. 完成后分别核对 Herdr workspace 与 Git worktree，防止 workspace 存活但目录已删除的残留；
6. 只有 no working agent + clean/preserved changes + merged/abandoned state 明确时才 reclaim；
7. `~/.config/herdr-mcp/releases/**` 属于 runtime generation，绝不纳入 dev worktree cleanup；
8. worktree 数量应近似 active independent mutation lanes，而不是历史任务数量。

开发阶段额外加入 `DEVELOPMENT-ONLY herdr-mcp retrospective`：每个 herdr-mcp 开发/调试/发布任务先完成用户原计划和验证，再基于本轮**真实工具调用**做一次 bounded retrospective，最多给 3 条有证据的性能/可靠性建议。该复盘不得自动打断当前计划、另开 worktree 或擅自实现旁支建议；稳定版冻结 Skill 前必须删除或大幅精简这段开发期策略。

### 当前结构性收益

不依赖机器瞬时负载即可确认的调用数量变化：

```text
managed-root / busy gates
  before: project derive 可隐含 full-repo git status
  after:  routing-only derive，不运行 git status

fs_patch (N existing dirty sources)
  before: 1 root validation + per-target validation/derive + N git status
  after:  1 root validation + in-root target validation + 1 batched git status

herdr_exec common success path
  before: eager full derive/git status for candidate metadata
  after:  routing-only；只有真正返回 project-selection error 才 lazy full derive

herdr_inspect with healthy EventCache
  before: ping + session.snapshot + workspace/pane/agent list reconciliation
  after:  ping + live cached snapshot；cache stale/reconciling 时保留原 fallback

herdr_prompt normal native success
  before: agent.get + agent.prompt + agent.get + possible 250ms sleep + agent.get
  after:  agent.get + agent.prompt (response carries agent state)

herdr_exec_read
  before: clone + sort + flatten complete retained output, then slice
  after:  scan chunk metadata and copy only requested offset/limit window

herdr_methods / herdr_call schema
  before: first post-TTL caller can synchronously refresh live schema
  after:  async startup prewarm + stale-while-revalidate after TTL
```

### A6 — benchmark / regression

最低 gate：

- epoch2 contract hash/tool count 不变；
- Rust unit + workspace tests + clippy `-D warnings` + diff-check 全绿；
- Node/Edge/extension smoke 只在触及对应边界时运行；
- 对比 before/after 的 Git subprocess count、project derive count、MCP payload size 和 wall-clock；
- 不使用 production service mutation 验证普通性能代码。

#### A6.1 如何验证效果差异

性能验收分三层，不凭单次主观体感下结论。

**Layer 1 — deterministic work reduction**

先证明工作量确实减少：

```text
fs_read/list/grep/image managed-root gate:
  full-repo git status  1 -> 0

fs_edit/write/exec_start common gate:
  full-repo git status  commonly 2 -> 0

fs_patch N existing dirty sources:
  per-target routing/status + N status -> 1 root routing + 1 batched status

herdr_exec common success path:
  eager full derive/status -> routing-only; detailed status only on project-selection error

herdr_prompt native success:
  get -> prompt -> get -> possible 250ms sleep -> get
  becomes get -> prompt

herdr_inspect healthy event stream:
  ping + snapshot + workspace/pane/agent list reconcile
  becomes ping + EventCache snapshot

herdr_exec_read(offset):
  copy retained history -> copy requested window only
```

**Layer 2 — repeatable local benchmark**

固定同一机器、同一 repo 数据集和 build profile，以 `origin/main` 为 baseline、performance branch 为 candidate；冷/热缓存分别重复 N 次并记录 p50/p95，不用单次结果。至少覆盖：小文件 `fs_read`、大 repo `fs_grep`、1/10/50-file `fs_patch`、inspect cache-live/fallback、since payload bytes、prompt fire-and-forget、1 MiB retained output 的尾部 16 KiB `exec_read`。结果保存为 machine-readable JSON：commit、samples、p50、p95、RPC/subprocess count、bytes。

**Layer 3 — real Web/Connector UAT**

代码合并并 self-upgrade 到本机 candidate/runtime 后，用固定任务脚本验证真实体感，例如：

```text
inspect -> 4 independent reads/grep/git facts -> patch -> validation -> since
```

记录 MCP calls 数、connector external_call_time 总和、用户消息到最终回答 wall-clock、tool response bytes/token、model re-entry 次数，以及是否产生不必要 workspace/worktree。Batch B 是否值得改 visible contract，主要由这一层决定：如果 Batch A 后 Rust 本地执行已经很快，而固定 MCP/model round-trip 仍占主导，才进入 multi-operation batch。

#### A6.1.1 2026-08-27 同机 A/B 实测

已完成第一轮 machine-readable benchmark：

`docs/benchmarks/tool-performance-batch-a-20260827.json`

方法：baseline 与 candidate 同时连接同一个本机 Herdr socket、操作同一个 `/Users/qingxian/Documents/herdr-mcp` 数据集，分别监听 `127.0.0.1:18871/18872`，每个 spec 交替 baseline/candidate 顺序采样，避免把调用顺序和瞬时机器负载固定偏向一侧。baseline 为生产 `main` merge `69fb04e`；candidate 为 `perf/tool-latency-batch-a-20260827` working tree。

| Tool | baseline p50 / p95 | candidate p50 / p95 | p50 变化 | p95 变化 | 结论 |
|---|---:|---:|---:|---:|---|
| `herdr_inspect` | 102.023 / 159.229 ms | 99.984 / 197.961 ms | -2.0% | +24.3% | p50 基本持平，p95 有抖动回归；不能宣称 latency win，保留 fast-path 的结构性收益并在 Layer 3 继续观察 |
| `herdr_since` | 1.436 / 2.537 ms | 1.250 / 1.596 ms | -13.0% | -37.1% | 小幅延迟收益；响应中位 bytes `1412 -> 537`，workspace filter/coalescing 的 payload 收益明确 |
| `herdr_fs_read` | 86.657 / 130.476 ms | 2.894 / 3.932 ms | -96.7% | -97.0% | routing gate 去 full repo Git status 是决定性收益 |
| `herdr_fs_list` | 110.653 / 182.000 ms | 5.855 / 18.699 ms | -94.7% | -89.7% | lightweight managed-root routing 明显生效 |
| `herdr_fs_grep` | 119.622 / 182.588 ms | 25.019 / 34.223 ms | -79.1% | -81.3% | 即使尚未完成下一片 `rg` fast path，仅移除 routing/status 重复工作已经显著加速；继续做 rg 仍有价值 |
| `herdr_git status` | 126.481 / 195.293 ms | 39.953 / 73.312 ms | -68.4% | -62.5% | validation 不再额外做 full status；requested status 成为主要成本 |

冷启动 `herdr_methods` 单样本为 `74.061 ms -> 3.135 ms`，但 harness 固定先打 baseline、后打 candidate，包含明显 first-request/order bias；该数字**不作为性能结论**，后续若要验收 schema prewarm 应改成独立进程多轮、随机启动顺序。

本轮 benchmark 结论：

1. A1/A2 对 filesystem/Git 高频工具已经从“微优化”变成数量级改善，无需再为这些路径引入复杂 topology invalidation cache 才能证明价值；
2. `inspect` 本地 wall-clock 尚未获得稳定改善，下一步不为追逐 p95 盲目堆缓存；优先在真实 Connector Layer 3 观察 model/transport 占比；
3. `fs_grep` 已从 ~120 ms p50 降至 ~25 ms，但仍显著慢于小文件 read/list，下一独立性能切片继续 `rg` fast path；
4. 本地工具已经进入毫秒级后，真实 Web 端固定 MCP/model round-trip 将更突出，是否进入 Batch B 由 Layer 3 再决定。

#### A6.2 提交、发布和 self-upgrade cadence

不采用“每改一个函数就发布”。固定节奏：

1. 每个可独立回滚的实现片形成逻辑 commit；
2. 同一优化轮次在一个 PR 内完成完整 gate + independent review；
3. PR 合入 `main` 后生成/发布一个新的 Rust candidate/alpha generation；
4. 本机 self-upgrade 到该 generation；
5. 重跑 Layer 3 Web/Connector UAT 与关键 smoke；
6. 证据通过后才把该轮标记完成并回收 development worktree。

因此“每轮”都会提交、合并、发布/self-upgrade 和真实运行时验证，但不会让十几个微提交各自触发一次 release 抖动。

#### A6.3 Worktree cleanup gate

每轮结束执行 `worktree.list` + `herdr_inspect` reconciliation：

- merged/abandoned + clean + no working agent 的 development worktree：关闭 workspace 并 remove；
- 当前仍在实现的 sibling lane：保留；
- dirty / outcome unknown / working：保留并报告；
- `.config/herdr-mcp/releases/**` runtime generation：永不按 development cleanup 删除。

截至本轮实时核验，`herdr-mcp` development worktree 只有：

```text
wC9  feat/link-runner-rust-parity-20260827   active sibling lane -> keep
wCA  perf/tool-latency-batch-a-20260827      current lane -> remove after merge/UAT
```

此前扫描到的其他 repo 历史 worktree 另做一次全局 reconciliation；只有满足同一 deterministic evidence gate 的才清理，不因“看起来旧”直接删除。

#### A6.4 本轮执行方法 retrospective

已经从本轮实际调用中得到以下方法改进，并纳入 Skill：

1. 多次独立 `fs_read/grep/git` 串行会放大约 0.5–1s/次的 MCP envelope 成本；后续先组成 read-only wave 再调用。
2. 大而跨多文件的 patch 在上下文变化后容易出现 `PATCH_CONTEXT_AMBIGUOUS/NOT_FOUND`；先读精确小范围，再按逻辑切片 patch，减少失败重试。
3. 独立审计应 fire-and-forget + `since(cursor)` 观察；本轮一次 wait timeout 后已遵守“delivery uncertain 不盲重发”，这是后续固定模式。
4. 新 worktree 不自动 bootstrap Node；本轮 wCA 明确保持 `node_modules=absent`，Rust-only 变化不为跑无关测试复制依赖树。

## 7. Batch B：只有基准证明需要时才进入

Batch B 解决固定 MCP/model round-trip 开销，而不是 Rust 内部微优化：

- existing tools 增加 multi-operation 参数；
- independent read batches 由 Rust 内部 fan-out；
- ordered mutation batch 保持单写者语义；
- 评估 JSON-RPC batch；
- 仍优先保持 18 个工具，不为了 batch 新增第 19 个工具。

因为 tool input schema/hash 是 model-visible contract 的一部分，Batch B 必须先决定：

```text
new contract epoch
or
compatible extension policy explicitly supported by contract governance
```

禁止仅因为当前 Rust HTTP handler写着“batch unsupported”就放弃调研；也禁止绕过 contract 直接静默改变 schema。

## 8. ChatGPT 目标调用形态

错误模式：

```text
read A -> model -> read B -> model -> git status -> model -> grep
```

目标模式：

```text
inspect once
  ↓
read-only wave
  ├─ project instructions
  ├─ grep
  ├─ selected reads
  └─ git facts
  ↓
ordered mutations
  ↓
validation wave
  ├─ tests
  ├─ diff
  └─ status
  ↓
since(cursor) for incremental agent/workspace changes
```

Tool schema description、`herdr_skill` 和 live runtime capability 必须传达同一策略，避免模型只在文档里“知道”，实际调用仍串行。

## 9. Worktree cleanup 不做自动破坏

性能优化不等于后台自动删除 checkout。第一阶段只建立 policy、diagnostics、reconciliation 和安全建议；真正自动 reclaim 必须满足确定性证据，并继续使用 Herdr/Git 原生 lifecycle API。

特别注意两类不同对象：

```text
~/.herdr/worktrees/**             development worktree lifecycle
~/.config/herdr-mcp/releases/**  runtime generation lifecycle
```

两者禁止混用清理逻辑。

## 10. 公网入口 / 内网穿透多 Profile 路线（当前 Edge 止血完成后再实现）

背景：Herdr 的公网入口必须能被 OpenAI/ChatGPT 从境外稳定访问，同时工作站通常位于 NAT/家庭或企业内网后，不能要求用户开放本机入站端口。2026-08-27 的生产优先级先修复当前 Cloudflare Edge Durable Objects `rows_written` 日限额风险：PR #46 / merge `69fb04e` 将已知 read-only MCP 调用从 Durable request ledger 中剥离，read success/error/timeout/link-drop 的 request-ledger/alarm 写入降为 0；mutation 保留 durable fail-closed 语义。多 ingress 不塞入该 hotfix，作为后续独立架构批次。

目标不是维护三套 MCP server，而是提供一个统一 `IngressProfile`：**公网入口可替换，MCP contract / OAuth / Link / runtime generation / mutation safety 保持同一套语义。**

计划至少支持三类用户可选入口：

| Profile | 典型数据路径 | 成本/定位 | 关键约束 |
|---|---|---|---|
| `cloudflare-edge` | OpenAI → Cloudflare Worker/Edge → workstation Link → Rust herdr-mcp | 默认免费/低运维；当前生产路径 | read 不进入 DO request ledger；mutation 才使用 durable delivery；持续监控 DO 日限额与 write amplification |
| `relay-vps` | OpenAI → 境外 HTTPS/WSS Relay VPS → outbound workstation Link → Rust herdr-mcp | 追求稳定/SLA、规避单一 Edge 平台限制 | Relay 必须部署在中国大陆以外且公网可被 OpenAI 访问；优先日本/新加坡/美国等靠近用户/OpenAI 网络的区域；Relay 不应持有工作站业务数据，只承担认证、转发和必要 delivery metadata |
| `local-tunnel` | OpenAI → stable public hostname / Cloudflare Named Tunnel → 本机 ingress → Rust herdr-mcp | 最短数据路径、个人/开发者低成本 | 工作站主动建立出站 tunnel，不开放本机公网端口；必须使用稳定域名/认证，不把 Quick Tunnel 随机 URL 当长期生产配置 |

三种 Profile 必须共享以下 invariants：

```text
same epoch/tool contract
same OAuth / workstation identity
same Relay/Link wire protocol
same generation fencing / runtime A-B semantics
same read-vs-mutation classification
same idempotency / delivery-state taxonomy
```

### 10.1 配置与用户选择

后续 CLI/installer 只暴露“入口选择”，不让用户理解三套内部实现：

```text
herdr-mcp ingress list
herdr-mcp ingress configure cloudflare-edge
herdr-mcp ingress configure relay-vps
herdr-mcp ingress configure local-tunnel
herdr-mcp ingress status
herdr-mcp ingress test
```

配置模型建议：

```text
IngressProfile {
  kind
  public_base_url
  region/provider
  auth_mode
  workstation_link_target
  health
  priority
}
```

默认 profile 应按安装场景选择，而不是写死：普通免费用户优先 `cloudflare-edge`；已有稳定 Cloudflare Named Tunnel 的个人用户可选 `local-tunnel`；要求更高稳定性/可控带宽/跨 Cloudflare 灾备的用户选 `relay-vps`。

### 10.2 自动 failover 的安全边界

多 ingress 不等于任意请求都能自动重发。

- read-only：当入口明确返回 `not_delivered` / transport failure 时，可在不同健康 Profile 间安全 retry/failover；
- mutation/unknown：只有明确 `not_delivered` 才允许换入口重试；一旦为 `sent` / `delivery_uncertain`，禁止跨 ingress 盲重发；
- 自动 failover 若要覆盖 mutation，必须让不同 ingress 共享或能验证同一个 idempotency / delivery ledger；在此之前 mutation 默认 pin 到发起它的 ingress；
- Profile health/circuit-breaker 只能影响**下一次请求的路由**，不能把未知结果的 mutation 当成普通网络重试。

### 10.3 性能与稳定性验收

每个 Profile 使用同一固定 Web/Connector UAT，对比：

```text
OpenAI -> ingress RTT / p50 / p95
MCP request wall-clock
Link reconnect time
read burst throughput
mutation delivery ambiguity rate
public endpoint availability
provider quota / bandwidth / storage cost
```

`relay-vps` 的验收节点必须在中国大陆以外，且从 OpenAI/公共互联网侧验证 HTTPS、OAuth discovery、MCP 和 WSS 可达；不能只在用户本机 `curl` 成功就判定可用。`local-tunnel` 与 `cloudflare-edge` 同样必须从真正的 ChatGPT Connector 路径做 UAT。

### 10.4 实施顺序

1. 当前生产 `cloudflare-edge` 先维持并量化 PR #46 后的实际 `rows_written / MCP call`；
2. 抽象 `IngressProfile` 配置与 health/status，不改 epoch2/18 tools；
3. 将现有 Edge/Link 适配为第一个 profile，证明抽象没有行为漂移；
4. 增加 `local-tunnel`，复用现有 OAuth/contract/runtime，不复制 MCP handler；
5. 增加最小海外 `relay-vps` 参考实现与部署模板；
6. 最后增加 read-only automatic failover；mutation cross-ingress failover 只有 shared idempotency/delivery evidence 完成后才开放。

这条路线与 Batch B 的 model-visible batch contract 独立：IngressProfile 属于 transport/deployment capability，原则上不新增 MCP tool、不改变 epoch2 tool schema。

## 11. 完成定义

Batch A 完成：

- 18 个工具均有明确优化结论；
- 高频工具不再因 managed-root/busy validation 重复执行全 repo Git status；
- `fs_patch` 多文件路径不按文件重复做 topology/status；
- `since`/prompt/exec-read 至少完成一轮可量化内部优化；
- `herdr_skill` 能指导 ChatGPT 使用 tool waves 和 worktree lifecycle；
- Rust link runner 可以继续独立推进，无 cross-lane 文件冲突；
- epoch2/18 tools contract identity 不变；
- 完成分支 merge 后关闭 workspace、删除 dev worktree/branch；保留 runtime release generations。

Batch B 完成条件另立 contract 决策，不和 Batch A 混在一个 PR。
