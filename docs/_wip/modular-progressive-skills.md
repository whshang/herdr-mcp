# herdr-mcp Modular Progressive Skills

状态：WIP / 架构讨论稿
日期：2026-08-27
目标：把当前单体 `herdr_skill` 演进为小型全局 `AGENTS.md` + 可发现、按需加载的模块化 Skill 系统，同时不阻塞当前 GA，不修改 epoch 2 / 18-tool 公共 MCP contract。

---

## 1. 背景与问题

当前 `herdr_skill` 承担了过多职责：

- workstation/live-state 操作原则；
- filesystem / Git / native search；
- long exec / streaming；
- coding agent delegation；
- worktree lifecycle；
- mutation retry / idempotency / evidence；
- browser / public Edge 边界；
- release / update / rollback；
- 性能和 tool-wave 编排。

这种设计在早期非常有效：Web ChatGPT 只需要调用一次 `herdr_skill` 就能获得完整操作策略。但随着 herdr-mcp 变成真正的 workstation control plane，继续扩充单个 Markdown 会产生三个问题：

1. **Context inflation**：与当前任务无关的规则也进入模型上下文；
2. **Instruction dilution**：Skill 越长，真正关键的局部规则越容易被淹没；
3. **Capability drift**：Agent/model/search/browser/runtime 能力是动态的，静态巨型 Skill 很快失真。

目标不是把一个大文件机械拆成多个小文件，而是建立一个 **Progressive Skill Runtime**：先暴露最小全局操作宪法与技能目录，只有任务真的触发某个能力域时才加载对应 Skill，并把动态设备事实与静态策略分离。

---

## 2. 核心原则

### 2.1 默认全局只加载 `AGENTS.md`

第一版冻结：默认全局策略直接采用 **`AGENTS.md` 语义**，不再另造一个 `HERDR.md` 或专有“超级 Skill”格式。

这里的含义是：

```text
Global Herdr AGENTS.md
= Web planner / coding agent 使用 herdr-mcp 时的最小、稳定、跨项目基础规则
```

它只回答：

- live workstation state 是事实源；
- 优先使用最低成本且正确的 deterministic tool；
- read 可以组成小型 dependency-aware wave；
- mutation 默认有序，除非已经做了明确隔离；
- mutation timeout 不代表没有执行，禁止盲重试；
- target 使用 explicit workspace/pane identity；
- 长任务启动一次，以 session identity 继续读取；
- 任务命中能力域时按需加载对应 Skill；
- 能用 deterministic native tool 完成时，不派 coding agent；
- 完成以真实 evidence 为准。

它**不应该**包含：

- 每个 Herdr method 的详细说明；
- 每种 coding agent/model 的静态排名；
- Search 参数细节；
- Browser Control Plane 全部规则；
- release/updater 内部事务；
- 每一种 worktree edge case；
- 当前设备实时 Agent/model 清单。

### `AGENTS.md` 的层级

项目自己的 `AGENTS.md` 继续遵循通用约定：从项目根目录向目标目录逐层应用，越接近目标路径的 `AGENTS.md` 对其子树越具体、优先级越高。

herdr-mcp 的全局 `AGENTS.md` 作为 **host-injected global base**，项目 `AGENTS.md` 在项目约定层可以细化/覆盖它；但下面这些安全边界不依赖 Markdown precedence，而由 runtime 强制执行，因此任何项目文件都不能绕过：

- managed-root / path confinement；
- dirty/busy gates；
- mutation fencing / idempotency；
- stale target fail-closed；
- browser/public credential boundary；
- service/update guardian；
- provider/OS 权限边界。

这避免把“文本指令 precedence”误当成“安全授权 precedence”。

源码中不要复用仓库根已有的开发者 `AGENTS.md` 作为产品资产。建议把产品级全局文件作为独立 bundled resource，例如：

```text
assets/herdr/AGENTS.md
```

runtime 对 Web planner 暴露的逻辑资源名仍为 `AGENTS.md`。

目标大小建议：约 1–3 KB；只保留长期稳定的基础规则。

### 2.2 Skill 是策略，Capability Snapshot 是事实

必须区分：

```text
Skill
= 如何使用一种能力、什么时候用、风险和完成标准

Capability Snapshot
= 当前这台设备此刻具体有哪些能力
```

例如 `agent-dispatch` Skill 可以描述如何选择 worker，但不能静态写死“Pi 一定存在”“某模型一定最快”。

真正的设备事实来自 runtime：

- 当前安装的 coding agents；
- Agent kind/provider/version；
- 可用 model/profile；
- reasoning / vision / tool / context 能力；
- 当前 idle / working / blocked；
- cwd / project affinity；
- 并发限制；
- 当前运行成本/延迟级别（如果 runtime 有可靠数据）；
- 哪些 worker 允许自动 dispatch；
- 哪些 worker 只适合 audit / human-interactive。

因此模型的决策应该是：

```text
agent-dispatch Skill
        +
live Capability Snapshot
        ↓
选择当前最合适的 Agent / Model / Pane
```

而不是把当前机器的设备配置硬编码进 Skill。

### 2.3 Discovery != Load != Authorization

严格拆开三个动作：

```text
Discover
“有哪些 Skill？”

Load
“当前任务需要读取这个 Skill 的策略。”

Authorize / Mutate
“允许执行某个动作。”
```

加载 `terminal-control` 或 `release-operations` Skill 不意味着自动获得高风险 mutation 权限。

Skill 永远不能成为绕过：

- managed-root；
- dirty/busy；
- target fencing；
- confirmation；
- idempotency；
- browser/public boundary；
- service mutation guardian；
- provider-specific authorization

的隐式授权机制。

### 2.4 Skill Identity 不能只用名字

未来统一使用类似：

```text
SkillIdentity {
  source_identity,
  uri,
  digest,
  version?
}
```

其中：

- URI 是地址，不是完整身份；
- source/server identity 区分同名 Skill；
- digest 用于内容完整性与 cache key；
- dynamic capability data 不进入静态 Skill digest。

这与 herdr-mcp release 的“URL != identity”原则保持一致。

### 2.5 Progressive Disclosure 是性能系统的一部分

模块 Skill 的目标之一就是减少 MCP/model round-trip 与上下文开销。

因此：

- 只列 metadata，不自动读取全部 Skill；
- `AGENTS.md` 不复制子 Skill 正文；
- 已加载且 digest 未变化的 Skill 不重复发送；
- supporting references 在真正需要时才加载；
- 不因为“可能会用到”就加载 Browser/Release/Agent 等所有规则。

---

## 3. 推荐架构

```text
Web ChatGPT / Other MCP Host
            │
            ▼
      herdr-mcp MCP surface
            │
            ├── existing 18 tools (GA frozen)
            │       └── herdr_skill
            │
            └── herdr_call (native long tail)
                    │
                    ▼
              SkillService
              ├── `AGENTS.md`
              ├── Skill Catalog
              ├── Skill Resolver
              ├── Skill Loader
              ├── Integrity / Cache
              ├── Capability Snapshot Adapter
              └── Load Evidence
                    │
       ┌────────────┼─────────────┐
       ▼            ▼             ▼
 bundled skills  project skills  future remote/MCP skills

Future adapter:
SkillService
    └── MCP Skills Extension adapter
```

关键点：**内部先建立 SkillService，外部协议 adapter 后置。**

Web ChatGPT 即使暂时不支持 Skills over MCP，也可以先通过现有 `herdr_skill + herdr_call` 使用同一个内部模型。

---

## 4. GA 兼容策略

当前 GA 期间不做以下变化：

- 不增加第 19 个 public MCP tool；
- 不改变 epoch 2 / 18 tools 集合；
- 不把 SEP-2640 draft 直接硬编码成 production ABI；
- 不要求 ChatGPT Host 支持 MCP Skills Extension；
- 不把模块化 Skill 变成 GA blocker；
- 不影响 G5 Rust production Link、G1 stable version、G18 clean-install UAT 等 P0。

### 4.1 `herdr_skill` 兼容层

短期继续保留 `herdr_skill`。

其长期职责从：

```text
返回完整巨型策略
```

逐步收敛为：

```text
返回 `AGENTS.md`
+ Skill Catalog metadata
+ live capability summary
+ 精确的按需加载方法提示
```

### 4.2 模块 Skill 如何在不改 18-tool contract 的情况下加载

首选使用已经存在的动态长尾：

```text
herdr_call(method="herdr_mcp.skill.list", ...)
herdr_call(method="herdr_mcp.skill.describe", ...)
herdr_call(method="herdr_mcp.skill.load", ...)
```

这些是 **herdr-mcp 本地保留 namespace**，不是假设 Herdr core 已经实现了 Skill API。`herdr_call` 的内部 dispatch 顺序改为：

```text
method starts with herdr_mcp.*
  → validate against herdr-mcp local method registry
  → execute locally

otherwise
  → validate against live Herdr schema
  → passthrough to Herdr socket exactly as today
```

因此不修改 Herdr core，也不新增 public MCP tool。保留 `herdr_mcp.*` 前缀是为了避免未来与真正的 Herdr method 或 MCP Skills 标准冲突。

如果 Web planner 已经从 `herdr_skill` 得到精确 local method schema/usage，可以直接调用 `herdr_call`；只有未知的真正 Herdr native method 才需要 `herdr_methods`。

这使第一阶段可以实现真正的模块化/按需加载，同时保持：

```text
epoch 2 public tools = 18
```

未来 Host 支持 Skills Extension 时：

```text
MCP Skills Extension
        ↓
SkillService
```

只是增加一个标准 adapter，而不是重写 Skill 核心。

---

## 5. 第一版 Skill Taxonomy：严格从当前 17 个非-Skill tools 出发

当前 epoch 2 有 18 个 public tools，其中 `herdr_skill` 是现有兼容入口。模块化设计的第一版以其余 **17 个执行 tools** 为边界，不为了“看起来完整”额外制造过多 Skill。

当前 17 tools 分组：

```text
State / native control (4)
- herdr_methods
- herdr_inspect
- herdr_call
- herdr_since

Filesystem read/search (4)
- herdr_fs_read
- herdr_fs_list
- herdr_fs_grep
- herdr_fs_image

Filesystem mutation (3)
- herdr_fs_edit
- herdr_fs_write
- herdr_fs_patch

Git (1)
- herdr_git

Execution (4)
- herdr_exec
- herdr_exec_start
- herdr_exec_read
- herdr_exec_kill

Agent delegation (1)
- herdr_prompt
```

因此第一版冻结为 **6 个 tool-domain Skills + 1 个 compositional Skill**。

### 5.1 `workstation-control`

对应：

```text
herdr_methods
herdr_inspect
herdr_call
herdr_since
```

负责：

- workspace / tab / pane / agent 概念；
- snapshot vs incremental event；
- `inspect` 一次、后续优先 `since(cursor)`；
- explicit workspace/pane identity；
- focus != mutation target；
- native long-tail method discovery/use；
- `herdr_mcp.*` local virtual methods 与真实 Herdr passthrough 的 namespace/validation 边界；
- reconnect、stale cursor、live state 优先于聊天摘要。

它就是“Herdr 窗口 / 本地工作现场 Skill”的主体。

### 5.2 `files-search`

对应：

```text
herdr_fs_read
herdr_fs_list
herdr_fs_grep
herdr_fs_image
```

负责：

- 文件浏览；
- bounded reads；
- native search / ripgrep fast path + fallback；
- 图片读取；
- compact search result；
- 独立 read-only wave；
- 什么时候该缩小 scope 而不是重复全量读取。

实现细节由 runtime capability 决定，Skill 不写死“永远使用某个 backend”。

### 5.3 `files-mutation`

对应：

```text
herdr_fs_edit
herdr_fs_write
herdr_fs_patch
```

负责：

- edit / write / patch 的选择；
- patch preflight；
- atomic/transactional expectation；
- dirty/busy/managed-root；
- mutation outcome evidence；
- uncertain outcome verify-before-retry。

通用 mutation 不盲重试等原则继续保留在全局 `AGENTS.md`，这里仅放 filesystem mutation 的具体策略，因此不再单独加载一个 `mutation-reliability` Skill 才能做普通编辑。

### 5.4 `git-repository`

对应：

```text
herdr_git
```

负责：

- status/diff/log/branch/worktree 的确定性事实读取；
- compact Git result；
- branch/worktree ownership；
- 不把 Git 查询委派给 coding agent；
- merge/rebase 前后需要哪些 evidence。

虽然只有一个 public tool，但 Git 是高频且规则独立的能力域，单独成为 Skill 比塞入 files-search 更清晰。

### 5.5 `execution`

对应：

```text
herdr_exec
herdr_exec_start
herdr_exec_read
herdr_exec_kill
```

负责：

- short deterministic shell vs long-running session；
- long task start once；
- `phase` / `progress`；
- `next_offset` delta read；
- kill/cancel；
- bounded output；
- Web turn 结束后继续同一个 session；
- process exit 与最终任务 evidence 的区别。

### 5.6 `agent-dispatch`

主要对应：

```text
herdr_prompt
```

同时读取 `workstation-control` 提供的 live Agent/model capability。

负责：

- 是否需要 Agent；
- 自动选择哪个本地 coding agent / model / pane；
- worker busy/idle/blocked 判断；
- capability / reasoning / vision / context / latency fit；
- explicit user target 优先；
- delivery evidence；
- worker 只拥有被分配的任务，不把整体 orchestration 再转交 worker。

第一版目标是**尽量自动 dispatch**，不是 recommendation-only。安全自动化条件见第 8 节。

### 5.7 `development-orchestration`（组合 Skill）

它没有独占 public tool，而是组合以上 Skills，负责“串行 / 多线开发”的拓扑决策：

```text
dependent mutation
→ serial

independent read-only investigations
→ parallel panes / tool waves

independent non-overlapping mutations
→ separate branch/worktree lanes when isolation is actually needed

shared files / shared runtime / production mutation
→ serialize or establish explicit ownership
```

负责：

- dependency-aware waves；
- lane ownership；
- worktree 是否真的必要；
- coding agent 自动派工；
- cross-lane reconcile；
- validation wave；
- 已完成 workspace/worktree 的安全回收。

### 5.8 第一版不作为独立 tool-domain Skill 的内容

以下能力以后仍可通过同一个 SkillService 增加 domain pack，但**不属于“当前 17 tools 的首批拆分”**：

- Browser Control Plane；
- release/install/update/rollback；
- future MCP Skills Extension；
- Work Context；
- remote registry。

Browser / Release 等规则在对应产品线成熟后再模块化，不要求第一批 Skill 重构同时完成。

## 6. 全局 `AGENTS.md` 的建议内容

默认全局加载的 canonical policy 就叫 `AGENTS.md`。

内容建议保持在以下骨架附近：

```text
1. Live workstation state is authoritative.
2. Prefer the cheapest correct deterministic tool.
3. Use small dependency-aware read waves.
4. Keep mutations ordered unless explicitly isolated.
5. Never blind-retry uncertain mutations.
6. Long work starts once and resumes by session identity.
7. Use explicit workspace/pane targets.
8. Load domain Skills only when the task requires them.
9. Delegate automatically when independent reasoning is useful and a safe compatible worker exists.
10. Completion requires evidence.
```

同时携带一个紧凑的 Skill Catalog：

```text
Available policy modules:
- workstation-control
- files-search
- files-mutation
- git-repository
- execution
- agent-dispatch
- development-orchestration
```

不复制模块正文。

### Project `AGENTS.md` layering

当任务进入具体项目后：

```text
Herdr global AGENTS.md
        ↓
project-root AGENTS.md
        ↓
nested AGENTS.md along target path
```

项目层按 AGENTS.md 通用“更具体路径优先”的规则解析。全局 Herdr policy 提供默认行为；项目 policy 提供 build/test/style/repo-specific workflow。

若文本指令和 runtime safety boundary 冲突，runtime fail-closed，不允许通过项目 `AGENTS.md` 获取额外系统授权。

## 7. Skill Catalog 模型

建议 metadata：

```text
SkillDescriptor {
  id
  name
  description
  source_identity
  uri
  digest
  size
  version?
  triggers[]
  requires_capabilities[]
  related_skills[]
  risk_domains[]
}
```

示例：

```json
{
  "id": "builtin:agent-dispatch",
  "name": "agent-dispatch",
  "description": "Choose an available local coding agent/model from live workstation capabilities.",
  "source_identity": "herdr-mcp:builtin",
  "uri": "skill://herdr-mcp/agent-dispatch",
  "digest": "sha256:...",
  "triggers": ["delegate", "agent", "parallel implementation", "audit"],
  "requires_capabilities": ["agent.list"],
  "risk_domains": ["agent-mutation"]
}
```

Triggers 只是 selection hint，不能成为模型不可见的强制隐式执行器。

---

## 8. Dynamic Agent + Model Selection

### 8.1 不硬编码 Agent 排名

错误做法：

```text
Pi always first
Grok always auditor
Codex always implementation
```

这些可以是某一版本/设备的经验，但不能成为长期协议事实。

正确做法是建立 runtime profile：

```text
WorkerCapability {
  agent_id
  kind
  provider
  model
  profile
  supports_code_edit
  supports_shell
  supports_vision
  reasoning_tier
  latency_tier
  context_tier
  interactive_only
  can_run_headless
  allowed_for_auto_dispatch
  current_status
  current_project
}
```

### 8.2 第一版就尽量自动 dispatch

第一版不止输出 recommendation；在满足明确安全条件时，Web planner 可以直接使用现有 `herdr_prompt` / `herdr_call(agent.start...)` 完成自动派工。

自动派工顺序：

```text
1. deterministic native tool 能完成？
   → 直接执行，不派 Agent

2. 任务是否适合独立 delegation？
   → 否：Web planner 自己执行

3. 读取 live worker/model capability + current status

4. 过滤：
   - allowed_for_auto_dispatch
   - capability match
   - project/cwd compatible
   - not blocked
   - no conflicting ownership

5. 选择最低成本且满足质量要求的 worker/model

6. 自动提交任务

7. 用 delivery evidence + since/inspect 验证提交状态
```

允许自动的典型情况：

- 独立代码审阅；
- 独立小型实现；
- 可清晰划分文件所有权的并行子任务；
- 与主 mutation lane 不冲突的测试/验证；
- 需要特定 capability（vision/high reasoning 等）且存在明确匹配 worker。

禁止或必须收敛为显式决策的情况：

- 用户明确指定了 agent/model/pane，不能静默换人；
- production/runtime destructive mutation；
- shared dirty checkout 且存在写冲突；
- 目标 worker blocked 或 capability 不满足；
- 需要把整体 planner/orchestrator 角色转交给 worker；
- dispatch outcome uncertain 时重复 prompt；
- 仅因为“有空闲 Agent”就制造没有收益的并行任务。

如果首选 worker busy，可自动选择**能力等价且 policy 允许**的 worker；如果只能明显降级质量/能力，则不静默降级，应由 Web planner自己完成或明确说明没有安全等价 worker。

每次自动派工保留结构化 `DispatchDecision` evidence：

```text
DispatchDecision {
  task_profile
  selected_agent
  selected_model/profile?
  selected_pane
  matched_capabilities[]
  rejected_candidates[]?   # bounded
  reason
  ownership_scope
  validation_boundary
}
```

普通用户不需要看到完整候选评分表，但 debugging / handoff 时应能解释为什么选了这个 worker。

### 8.3 未来可以加入反馈闭环，但不能自强化失控

后续可以记录：

- observed task latency；
- success/failure；
- retries；
- review acceptance；
- tool/output efficiency。

用于调整 dispatch recommendation。

但第一版不要建立自动学习排名系统；先保持规则透明、结果可解释。

---

## 9. 串行与多线开发 Skill

`development-orchestration` 的目标不是“尽可能多开 Agent”，而是选择正确拓扑。

### 9.1 默认拓扑

```text
Web planner
   │
   ├── deterministic read/tool wave
   │
   ├── mutation lane A (ordered)
   │
   └── independent review/investigation lane B
```

### 9.2 何时开新 worktree

只有：

- independent mutation；
- 文件所有权可以清晰分开；
- 需要隔离 unrelated dirty work；
- 用户明确要求。

不因为：

- 读代码；
- grep；
- review；
- 跑测试；
- “有另一个 Agent 可用”

就新建 worktree。

### 9.3 Lane Descriptor

未来可让 Skill 推荐但不自动持久化：

```text
Lane {
  objective
  project_root
  branch?
  worktree?
  owner_agent?
  file_scope[]
  dependencies[]
  validation[]
}
```

这与未来 Work Context 可以兼容，但不要求现在实现 Task Center。

---

## 10. Skill 加载流程与回合成本

第一版明确：**Skill 不是每条命令都重新加载。**

### 10.1 加载粒度：conversation/task-context sticky

建议规则：

```text
新 Web conversation / 新上下文
    ↓
加载全局 AGENTS.md + compact catalog       # 一次
    ↓
根据当前任务分类能力域
    ↓
herdr_mcp.skill.load(ids=[...])                       # 通常 0–1 次额外调用
    ↓
模块在当前 conversation/context 内 sticky
    ↓
后续同域命令直接执行，不重复 load
```

只在以下情况重新加载模块内容：

- 新 conversation / handoff 后需要重新进入模型上下文；
- Skill digest/version 改变；
- source identity 改变；
- 用户/开发者显式 refresh；
- 当前任务首次进入一个此前未加载的新能力域。

**新的用户 turn 本身不触发 reload。** 同一个对话里连续几十次文件读取/exec/agent follow-up 都不应该每次多一个 Skill round trip。

### 10.2 一次可批量加载多个模块

为了避免：

```text
load workstation
load agent
load orchestration
load git
```

产生 4 个 round trip，第一版 `herdr_mcp.skill.load` 应支持：

```text
herdr_mcp.skill.load(ids=[
  "workstation-control",
  "agent-dispatch",
  "development-orchestration",
  "git-repository"
])
```

一次返回多个模块，顺序稳定、每个模块带 identity/digest/evidence。

如果任务分类一开始已经很明确，就一次把这一批加载出来。

### 10.3 Catalog 与 load schema 在 bootstrap 时给够

`herdr_skill` compatibility bootstrap 应直接返回：

- 全局 `AGENTS.md`；
- compact catalog metadata；
- `herdr_mcp.skill.load` 的精确调用形态/参数约定；
- live capability summary/revision。

这样 Web planner 已知 `herdr_mcp.skill.load` 时不需要为了“怎么调用”再跑一次 `herdr_methods`。

### 10.4 Live capability 不等于 Skill 内容

Agent busy/idle、pane、model/profile、runtime capability 会变化，但这不要求重新加载 `agent-dispatch` Skill。

```text
Skill policy
    → sticky by digest

Live capability/state
    → refresh via inspect/since/runtime snapshot
```

因此几十次状态变化不会导致几十次 Skill 重新注入上下文。

### 10.5 典型回合成本

普通 grep：

```text
bootstrap: AGENTS.md + catalog
首次命中 files-search: load once
后续 grep/read/list: 0 additional Skill calls
```

多 Agent 开发：

```text
bootstrap
→ one batched herdr_mcp.skill.load([
    workstation-control,
    agent-dispatch,
    development-orchestration,
    git-repository,
    files-mutation
  ])
→ 后续派工/查看/修改不重复加载这些模块
```

任务中途第一次进入 release/browser 等未来域时，再额外 load 一次对应 domain pack 即可。

### 10.6 Runtime cache 与模型 context cache 是两层

- runtime 可以按 `(source_identity, uri, digest)` 缓存 bytes；
- Web planner 仍需在新的模型上下文中收到实际策略文本；
- handoff 可以携带 `loaded_skill_ids + digests`，帮助新会话只 rehydrate 真正仍相关的模块，而不是重新 discover 全世界。

目标不是绝对零 round trip，而是把额外成本压成：

> **每个新能力域通常只多一次加载，而不是每条命令或每个用户 turn 多一次。**

## 11. Cache 与内容完整性

Skill 内容建议内容寻址：

```text
(source_identity, uri, digest)
```

加载时：

1. resolve source；
2. 检查 manifest metadata；
3. 读取 required resource；
4. SHA-256 verify；
5. immutable cache；
6. 返回 load evidence。

对于 builtin Skill，digest 可在 build/release 时生成。

对于 project-local Skill：

- local path + Git/worktree identity；
- 文件发生变化时 digest 改变；
- dirty project Skill 必须明确标记 `working-tree` 来源。

### 11.1 v0.4.2 已落地：Local Skill Registry（本地 skill 注册表）

v0.4.2 为现有 `ProgressiveSkillService` 增加了本地 skill 注册，用于确定性、有界、
canonical-path 收敛地发现与读取本地 `SKILL.md`/`skill.md`，不新增 public MCP tool。

冻结的优先级（从高到低）：

```text
herdr-mcp builtin / 上游 Herdr usage（最高）
        ↓
project  <project_root>/.agents/skills/*/SKILL.md
        ↓
user     ~/.agents/skills/*/SKILL.md
```

- **优先级（从高到低）：**

  ```text
  herdr-mcp builtin / 上游 Herdr usage（最高）
          ↓
  project  <project_root>/.agents/skills
          ↓  <project_root>/.claude/skills
          ↓
  user     ~/.agents/skills
          ↓  ~/.claude/skills
  ```

- 同名更低优先级 skill 不允许覆盖更高优先级；builtin 名称（如 `files-search`）被
  project/user 同名 shadow 时直接丢弃。
- 每个 scope 下 `.agents/skills` 先扫描，再扫描 `.claude/skills`；同一 id 首次出现的
  位置胜出，因此 project `.agents` > project `.claude`、user `.agents` > user `.claude`。
- `SKILL.md` 与 `skill.md` 均接受。
- discovery 只返回 metadata（`id`/`name`/`description`/`source_identity`/`uri`/`digest`/
  `version`/`size`）；`load` 按需返回正文，并以 `(source_identity, uri, digest)` 为 cache key。
- `list`/`describe`/`load` 接受可选 `project_root`，使项目 skill 确定性解析；缺省则跳过项目 scope。
- 仅做确定性元数据读取，绝不执行任意脚本，不接入 SkillHub/网络分发。

收敛与安全：候选目录 canonicalize；子目录或 `SKILL.md` 的 symlink 解析到 `.agents/skills`
base 之外即拒绝；per-scope 数量上限与单文件 512 KiB 大小上限限制发现；`load` 校验所服务
字节的 digest 并在相同大小上限内读取，避免后续膨胀文件被服务。

外部本地 skill 使用 `project:<root>` / `user:<home>` source identity 与
`skill://local/<id>` URI，digest 为 trim 后正文的 SHA-256。

常见 frontmatter（`name`/`description`/`summary`/`version`，含 `metadata.version` 与折叠
`>` 标量）已足够解析真实 `~/.agents/skills` skill，例如 `ego-browser` 与 `opencli-usage`。

> 后续若需要新的 scope/目录，仍应作为显式的独立决策，而不是默认漫游扫描。


对于未来 remote/MCP Skill：

- server/source identity + URI；
- 不仅依赖 URL；
- cache key 必须包含 source identity。

---

## 12. Load Evidence

建议每次模块加载返回：

```text
SkillLoadEvidence {
  skill_id
  source_identity
  uri
  digest
  loaded_at
  bytes
  cache_hit
  capabilities_snapshot_revision?
}
```

这不是为了给用户制造日志噪音，而是让：

- reconnect；
- handoff；
- debugging；
- stale Skill detection；
- future Work Evidence

有可靠依据。

普通对话不需要把所有 evidence 展示给用户。

---

## 13. 与 MCP Skills over MCP 的关系

当前不把该标准作为实现 blocker。

内部模型应先做到协议无关：

```text
SkillService
├── list metadata
├── resolve identity
├── load core/resource
├── verify digest
├── cache
├── capability snapshot
└── evidence
```

然后：

```text
Adapter A: current herdr_skill + herdr_call
Adapter B: future MCP Skills Extension
Adapter C: optional local CLI/Herdr native interface
```

如果未来 ChatGPT 支持 Skills over MCP，只需要替换/增加 adapter。

不要在现在冻结 SEP draft 的具体 method/URI/schema。

---

## 14. 与 GA 的隔离

这个特性明确为独立产品线，类似 Browser Control Plane：

```text
GA P0 mainline
- Rust production Link
- stable version
- CLI seal
- clean install UAT
- extension GA decision

             ║ independent
             ║
Modular Progressive Skills
- WIP design
- internal SkillService
- builtin Skill modules
- live capability adapter
- legacy herdr_skill compatibility
```

第一阶段实现不得修改：

- epoch 2 tool count；
- public MCP tool names；
- production Link cutover；
- GA install/update/rollback semantics；
- Browser Control Plane mutation line。

若出现冲突，GA 主线优先。

---

## 15. 推荐独立开发阶段

### Phase A — Internal SkillService

独立 branch/worktree。

实现：

- SkillDescriptor / SkillIdentity；
- builtin catalog；
- digest；
- loader/cache；
- pure tests；
- 不改变 public tool contract。

### Phase B — Split Current Giant Skill

把现有 `herdr-mcp-SKILL.md` 拆成：

- `AGENTS.md`；
- 初始模块 Skill。

要求 old `herdr_skill` compatibility output 在语义上仍能工作。

可选择过渡模式：

```text
legacy_full=true (internal compatibility/testing only)
```

但默认 Web planner 应开始走 progressive path。

### Phase C — Native Skill Methods

通过 long-tail schema 增加：

```text
herdr_mcp.skill.list
herdr_mcp.skill.describe
herdr_mcp.skill.load
skill.read_resource (if needed)
```

通过 `herdr_call` 使用，不增加 public MCP tool。

### Phase D — Live Capability-driven Agent Dispatch

实现 capability snapshot adapter：

- coding agent inventory；
- model/profile metadata；
- status/busy；
- policy allowlist；
- capability traits。

然后让 `agent-dispatch` Skill 使用动态事实做 recommendation。

注意第一版只输出 recommendation/decision evidence，不做不可解释的自动模型 ranking。

### Phase E — Development Orchestration Skill

把现有 tool-wave + worktree lifecycle + delegation 规则收敛为独立 Skill。

验证：

- serial dependency；
- parallel read；
- isolated mutation lanes；
- worktree reuse；
- safe cleanup；
- no middle-manager delegation。

### Phase F — Future MCP Skills Adapter

只有标准与 Host 支持稳定后再做。

不作为当前 GA / Phase A–E blocker。

---

## 16. 测试要求

至少覆盖：

1. `AGENTS.md` 不包含全部模块正文；
2. catalog metadata 稳定；
3. Skill identity 包含 source identity；
4. digest mismatch fail closed；
5. cache hit 不重复加载 bytes；
6. Skill 修改后 digest/revision 改变；
7. project-local same-name Skill 不与 builtin/remote 冲突；
   （v0.4.2 已由 `local-skill-*` 测试覆盖：precedence、same-name shadowing、
   symlink escape 拒绝、缺失目录、size/path 边界、metadata-only + load、外部
   身份/digest、frontmatter 解析。）
8. discovery 不自动 load；
9. load 不授予 mutation 权限；
10. agent-dispatch 使用 live capability，而非硬编码存在性；
11. unavailable/busy agent 不被静默选中；
12. deterministic task 推荐 direct tool；
13. independent read lane 可并行；
14. dependent mutation lane 保持串行；
15. worktree 只在必要时推荐；
16. old 18-tool contract hash 不变化；
17. `herdr_skill` 旧客户端仍可工作；
18. Web ChatGPT 不支持 Skills Extension 时功能完整可用。

---

## 17. 性能验收

模块化不是为了“架构更漂亮”，必须证明 context/latency 有收益。

建议基准：

- current giant `herdr_skill` bytes/tokens；
- `AGENTS.md` bytes/tokens；
- typical fs-only task loaded bytes；
- agent-delegation task loaded bytes；
- release task loaded bytes；
- Skill cache hit ratio；
- extra MCP round trips；
- model task success / retry count。

目标：

> 大多数普通任务的 Skill context 显著小于当前巨型 `herdr_skill`，且按需加载造成的额外 round-trip 不抵消收益。

如果 loader 导致每个动作前都多 2–3 个 MCP round trip，则设计失败，需要通过 catalog/schema/cache/prefetch hint 收敛。

---

## 18. 第一版架构决定（已冻结）

2026-08-27 本轮讨论后，以下问题不再作为实现前置疑问：

1. **全局默认策略采用 `AGENTS.md`。** 产品 bundled source 与仓库开发者 `AGENTS.md` 分离，逻辑语义遵循 AGENTS.md；项目内 nested `AGENTS.md` 按“更具体路径优先”的通用规则解析。
2. **第一版 taxonomy 按当前 17 个非-`herdr_skill` tools 划分。** 固定为 6 个 tool-domain Skills + `development-orchestration` compositional Skill。
3. **Skill 正文以 Markdown 为 canonical content。** descriptor / identity / digest / load evidence / capability snapshot 使用结构化数据。
4. **Project instruction precedence 按通用 AGENTS.md 模型。** 项目/嵌套文件可以细化工作流，但不能覆盖 runtime 强制安全边界。
5. **Agent dispatch 第一版尽量自动。** 满足 allowlist、capability、busy/dirty/ownership、安全边界时直接派工；用户明确 target 优先；不允许静默质量降级或递归中间管理。
6. **Skill 按 conversation/task-context sticky。** 不是每条命令 reload；首次命中新能力域时额外加载一次，一次可以 batch 多个 Skill；live state 单独通过 inspect/since 更新。
7. **当前 Web ChatGPT 不要求支持 Skills over MCP。** 使用 `herdr_skill` bootstrap + `herdr_call(method="herdr_mcp.skill.load", ...)` 完成 progressive loading。
8. **不新增第 19 个 public MCP tool，不改变 epoch 2 / 18-tool contract。**
9. **MCP Skills Extension 仅作为未来 adapter。** 不冻结当前 draft 的具体协议细节。
10. **模块化必须用 token/bytes/round-trip/task-success benchmark 证明真实收益。**

## 19. 实现边界与验收口径

### 19.1 第一条独立开发线应完成

```text
Global AGENTS.md
+ SkillService
+ SkillIdentity / Descriptor
+ digest + immutable cache
+ compact catalog
+ batched herdr_mcp.skill.load(ids[])
+ 6 tool-domain Skills
+ development-orchestration Skill
+ live Agent/model capability projection
+ safe automatic agent dispatch
+ legacy herdr_skill compatibility adapter
+ unchanged 18-tool contract tests
+ token/round-trip benchmark
```

### 19.2 第一条开发线明确不做

- MCP Skills Extension production adapter；
- remote Skill registry；
- automatic self-learning/ranking；
- Browser Control Plane mutation；
- Work Context Task Center；
- public MCP tool schema/count change；
- GA Link/install/update/rollback 改造。

### 19.3 成功标准

至少证明：

1. 普通 fs/search 任务不再注入 giant Skill 全文；
2. 一个 conversation 内同一个 Skill 只加载一次（digest 不变前提）；
3. 多 Skill 可以一次 batched load；
4. 17 tools 的使用策略全部有明确 module owner；
5. deterministic tool 优先于 Agent；
6. 合适的独立开发任务可以依据 live capability 自动选择并派给安全 worker；
7. busy/blocked/capability-mismatch worker 不被错误选择；
8. 多线 mutation 的 worktree/lane ownership 不冲突；
9. 旧 `herdr_skill` 客户端仍然工作；
10. epoch 2 public tool set/hash 不改变；
11. 非 `herdr_mcp.*` 的 `herdr_call` 仍保持 live Herdr schema validation + socket passthrough，行为无回归；
12. `herdr_mcp.*` unknown local method fail closed，绝不误透传 Herdr；
13. 相比当前 giant `herdr_skill`，典型 fs-only / exec-only / delegation task 的注入 bytes/tokens 明显下降；
14. progressive loading 增加的 MCP round trip 通常为每个新能力域 0–1 次，而不是每条命令一次。

## 20. 推荐后续开发线

讨论已经足够冻结第一版，可以直接创建：

```text
branch: feat/modular-progressive-skills
worktree: herdr-mcp/modular-progressive-skills-20260827
```

按下面阶段连续推进，不需要等待 MCP Skills 标准：

```text
Phase A  SkillService + identity/catalog/digest/cache
Phase B  AGENTS.md + 6 tool-domain Skill 拆分
Phase C  local `herdr_mcp.skill.list/describe/load` + batched loading
Phase D  live Agent/model capability + safe automatic dispatch
Phase E  development-orchestration + lane/worktree policy
Phase F  benchmark + Web ChatGPT smoke + compatibility seal
```

每一阶段都必须保持：

```text
public tools = 18
herdr_skill remains compatible
GA mainline unaffected
production Link unaffected
```

未来等 MCP Skills Extension 与 Web Host 支持稳定后，再单独增加标准 adapter。

先证明 progressive loading 在当前 Web ChatGPT 上真实可用且确实减少上下文，再扩展其余模块。

---

## 21. 参考标准

- AGENTS.md open convention: <https://agents.md/>
  使用其 repository / nested instruction 模型：更接近目标目录的 `AGENTS.md` 对对应子树更具体、优先级更高。
- MCP Skills over MCP Working Group: <https://modelcontextprotocol.io/community/working-groups/skills-over-mcp>
  仅作为未来标准 adapter 的方向参考；当前实现不冻结仍在演进中的 SEP 细节。

这里采用 AGENTS.md 的**指令组织/层级约定**，但 herdr-mcp runtime 的安全授权仍由可执行代码强制，不交给 Markdown 文件决定。
