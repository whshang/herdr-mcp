# Herdr Architecture Roadmap

状态：实施中
原则：用效率最高、可能是最复杂但对用户最友好的方案，不追求短期收益。

**Release model SSOT**：[`docs/release-model.md`](./release-model.md)。Runtime、浏览器扩展与 Contract 按独立 release plane 演进；发布状态与一次性验收结果不写入本架构路线。


> 历史迁移/发布 chronology 已拆分到 [`docs/history/architecture/rust-native-rearchitecture.md`](./history/architecture/rust-native-rearchitecture.md)（Rust 原生化重构细节）与 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)（18 个工具性能实现细节与基准）。本文件只保留当前架构与路线。

## 当前产品事实

- **Rust runtime 拥有生产 MCP / service / updater / Link / Native Messaging 路径**；`bin/herdr-extension-host` 只是委托 Rust 的兼容入口，不是第二套 installer/broker。
- **生产 Link 是 Rust**，执行 `~/.config/herdr-mcp/runtime/current/herdr-mcp link run`。
- **first-party DEV/PROD 公共 Edge contract 为 epoch 8 / 19 actions，workstation Runtime Execution Contract 为 epoch 5 / 18 tools**；第 19 个 `herdr_devices` 由 Edge 本地执行，不转发到 workstation；紧邻上一代冻结的 epoch 4 与冻结的 epoch 2 catalog 仅作为有界的 rollback/compatibility 身份保留，epoch 3 仅属历史；公共 Edge epoch 3 身份仍作为非 DEV/PROD 的有界基线保留。
- **浏览器控制面是有界的**：不宣称 browser true-steer；普通终端只开放有 target fencing 的窄化 `Run command -> pane.send_input + Enter`，任意 Herdr method 仍保持 preview-only。
- **Semantic/Jev 是 Runtime 统一的 advisory decision plane，不是第二执行权威**：浏览器 Auto 的普通 post-turn 顺序保持 deterministic safety/scope gates → Runtime typed-evaluation route（Jev）→ Runtime chat route（LLM）→ bounded script fallback；Goal Supervisor 可消费 Jev 五信号 prior，但 Work Memory/TODO 与 deterministic guards 仍独占完成、等待、接力、人工边界和 uncertain-delivery 的执行许可。Parent orchestration 复用同一个 `SemanticService`：非平凡 dispatch 可用 planning advice；durable inbox 只在出现未 acknowledge terminal 时，对最多 16 个 child summary 做一次 1.5 s bounded attention batch；`verify_completion` 进入确定性 validation，`continue_unobserved` 让独立 child 继续；validation 只能重排冻结检查，closeout 只做 post-validation 分类，cleanup advice 不能改变 `safe_to_delete`。缺少/超时/失败的 semantic route 必须保持原确定性流程，Browser 不再维护第二套 semantic wait/阈值。扩展只消费 Runtime 语义能力，不保存 Provider endpoint/model/key；Provider 配置只有 mode-`0600` 的单机 `config.json` 与 Cloudflare Worker 全局 route pool 两层，并统一使用 `name / protocol / url / model / api_key` JSON route；typed evaluation/chat 模式由 `protocol` 推导。不得重新引入浏览器侧 Provider 配置、环境变量 Provider 配置、用户可选语义策略、可调概率边界、可编辑 judge prompt/completion token 或第二套 orchestration/Goal authority。
- **1.0 已于 2026-09-23 正式发布**；`v0.4.8` 保留为历史 0.4.x 升级基线。1.1 尚未发布的 Browser Adapter Platform 能力必须继续描述为 development/upcoming。

## 总体目标

Herdr 性能优化不以单点 benchmark 为目标，目标是建立长期可演进的执行架构：

- 用户只看到稳定、高速、低等待的工具体验；
- Rust runtime 负责确定性、安全边界和可靠状态；
- 工具性能优化不得原地修改已冻结的 Runtime Execution contract；当前为 epoch 5 / 18 tools，model-visible 变化必须显式演进新 epoch；
- 新能力优先进入内部 runtime/state，不增加模型每轮 schema 负担。

核心原则：

1. 先减少 MCP/model round-trip，再优化局部代码。
2. 先消除重复工作，再增加并发。
3. read-only 可以并行，mutation 保持有序和 fail-closed。
4. 性能优化不能降低 managed root、secret path、idempotency、generation fencing 安全等级。
5. 架构一次设计完整，实现按风险和验收顺序推进。

## 当前执行重点

当前执行面彼此解耦：Runtime、Browser Extension、Edge/Link Contract 可以独立演进，但必须遵守 [`release-model.md`](./release-model.md) 的兼容边界。浏览器扩展持续独立迭代，不要求仅为扩展变化发布 Rust runtime。

`v0.4.8` 已提供 Connector/OAuth exact-instance lifecycle、Automation Client、first-Worker onboarding、multi-device enrollment/admin、Linux service/credential/updater、Relay/reconnect/backpressure 与 Native Messaging/Unix-socket 等 pre-1.0 stabilization 能力。1.0 直接继承这些边界，不再另写同类机制。

Herdr-MCP 1.0 已于 2026-09-23 正式发布。稳定版结论见 [`releases/v1.0.0.md`](./releases/v1.0.0.md)，发布前阶段台账已归档到 [`history/architecture/v1.0-status-closeout-20260923.md`](./history/architecture/v1.0-status-closeout-20260923.md)。1.0 的正式里程碑序列如下：

```text
alpha.1  Fleet Control Kernel
alpha.2  Project Work Memory
alpha.3  Browser Endpoint / Resource Registry
alpha.4  ChatGPT Web adapter vertical
alpha.5  Gemini second-provider vertical
alpha.6  Claude provider vertical
alpha.7  Grok provider vertical
alpha.8  bounded generic Page Assist
alpha.9  Toolchain Efficiency（output compaction / 18-tool efficiency gate）
beta.1   reliability / postcondition（reload/stale-view 恢复、uncertain delivery 结算）
beta.2   multi-device / multi-endpoint / multi-account reservation + failover
rc.1     security / migration / N-1·N+1 兼容 / rollback / 双设备验收
```

编译加速与内存/资源治理不是额外里程碑，也不引入第二调度器或状态权威。它们作为 1.0 横向工程约束随上述阶段验证：保持 Runtime / production Link / Supervisor 的可靠性边界；以 Work Memory 的 checkpoint + bounded raw tail、轻量 browser/provider resource state、aggregate byte admission、beta.2 Fleet Control resource evidence 为核心；crate/linker/allocator/Cargo profile 等优化只按 Herdr 自身 touched-file、CI、RSS/PSS 与 burst-to-idle 基准决定。1.0 阶段采用的方案已归档到 [`history/architecture/v1.0-performance-resource-plan.md`](./history/architecture/v1.0-performance-resource-plan.md)；后续性能工作按新的实际问题单独建立当前计划。

**序列权威说明**：冻结规划基线（[`docs/history/architecture/v1.0-architecture-plan.md`](./history/architecture/v1.0-architecture-plan.md)，自 `47a6f80` 逐字恢复）只记录早期计划与 provenance；后续真实实现补入第二 Provider、Claude/Grok、Page Assist 与 Alpha 9，因此上表是当前唯一正式里程碑序列。alpha.5 用于满足冻结 plan §17 的 “at least two providers prove the semantic adapter seam” gate，证明 adapter seam 是 provider-neutral 的。Alpha 9 不增加工具数量；native-default `herdr_exec` 因会改变 epoch-2 明确的 VISIBLE utility-pane 语义而未在旧 hash 下发布，若未来继续该方向必须显式演进 Runtime Execution contract。

1.0 以正式 v0.4.8 的 pre-1.0 stabilization 合同为基础，不复制 Connector/OAuth authority、Automation Client、onboarding/readiness、runtime/EventCache liveness、multi-device/Relay、Linux platform support、Native Messaging local-auth 与 browser continuity/target-fencing 等实现。稳定版最终边界见 [`releases/v1.0.0.md`](./releases/v1.0.0.md)。

阶段规格：alpha.1–alpha.9、beta.1、beta.2 与最终 1.0 closeout 均已归档到 `docs/history/architecture/`。beta.2 设计见 [`history/architecture/v1.0-beta2-webchat-orchestration.md`](./history/architecture/v1.0-beta2-webchat-orchestration.md)，最终发布状态见 [`releases/v1.0.0.md`](./releases/v1.0.0.md)。

1.0 的两个正式跨 Provider 验收场景固定为：

1. **Planner Handoff**：ChatGPT 转移 Planner Lease，Gemini 使用同一 `work_chain_id` 从 Herdr verified checkpoint、recent raw tail 与 project evidence 恢复同一项目并继续；
2. **Cross-WebChat Delegation**：ChatGPT 保持 Planner Lease，把有界任务委派给 Gemini Web Chat execution lane，Gemini 结果回写同一 Work Chain，Gemini 不成为第二 Planner。

Alpha 2 只实现支撑这两个场景的 Work Memory / compact Fleet checkpoint 状态合同；Gemini DOM adapter、Browser Endpoint Registry 与 External Dispatch 均留到后续阶段。

## 1.1 核心：Browser Adapter Platform

1.1 的主轴是把 1.0 已验证的 WebChat Browser Registry、generation fencing、idempotency、delivery evidence、Page Assist 与 Extension/Native Messaging 链路推广成一个**有界、可扩展、可自开发 Adapter 的浏览器执行平台**。OpenCLI / opencli-mcp 作为场景和能力模型参考，目标覆盖 Generic Browser Use、登录态内容提取、社交搜索/读取、AI 网页生成、下载/artifact、站点 Adapter 与 Adapter author/repair 这类能力范式，不要求首版复制其全部站点命令。详细设计见 [`docs/_wip/v1.1-browser-adapter-platform.md`](./_wip/v1.1-browser-adapter-platform.md)。

产品目标：

- **Browser Extension 是 1.1 唯一 Web/browser 执行面**；2026-09-26 起停止 ChatGPT Desktop tweak 路线，不把 Electron preload/renderer 注入、桌面客户端 patch/install/update/restore 纳入 1.1；桌面应用仍可作为 MCP/CLI client 使用 Herdr，但网页控制统一经过 Herdr Extension；
- ChatGPT / Grok / Claude 继续保留冻结的 provider/session/result/Continuity 语义，只下沉共享 tab/page lifecycle、observe/act/verify/finalize 基础，不改走 generic Adapter catalog；
- Alpha 8 Page Assist 吸收到同一 Generic Web kernel；未知用户授权网站先拥有 open/claim/observe/click/fill/expect/screenshot/finalize 最小纵向，额外 typed primitive 由真实 reference Adapter 证明后再加；
- `BrowserPage` 只是短生命周期、generation-fenced view handle；Browser Registry 继续独占 WebChat durable identity，不新增第二套持久 page catalog；
- 提供 reviewed builtin + user-local Browser Adapter；Bilibili/X/Doubao 先以 builtin Adapter 验证签名、鉴权和长任务边界，再冻结 local `adapter.json`；
- 本地 Adapter 使用 version/digest、显式 origin/access、声明式 typed Browser operations，不要求 Rust Runtime 内嵌第二套 JavaScript 执行引擎；
- Adapter authoring 采用 draft -> real try -> atomic activate，失败 trial 不覆盖当前 active Adapter；
- Adapter 命令通过现有 Progressive SkillService 按需引导 search/describe/run，不把所有站点命令永久塞进 MCP schema，也不建立第二套 Skill registry；
- Browser mutation 继续复用一个 Runtime reservation、`not_applied / applied / uncertain` 与 verify-before-retry；Adapter 不得建立第二套 retry/idempotency/wait 权威；
- **Jev 作为可选本地快速决策层**：Runtime 只把有界 BrowserPage observation 和确定性候选动作交给现有 `SemanticService`，Jev 只能选择下一步 typed action / done / blocked / escalate；权限、generation/ref、幂等、delivery、敏感字段、人工边界与资源清理仍由 Runtime 决定。Jev 未配置、超时、异常或 uncertain 时直接回到普通 planner 路径，Generic Web 仍完整可用；
- Chrome 用户 tab 与 Herdr-created tab 明确区分 ownership；Service Worker restart 后通过 Extension session ownership + Runtime operation evidence 恢复/清理 Herdr-owned resources，finalize 不得误关用户 tab；
- 正常 Store 扩展在安装/加载时一次申请 required `<all_urls>` host access，Generic Web 后续不再逐站弹出 Herdr 权限请求；每次 BrowserPage 访问前仍检查 Chrome 对目标 origin 的实时权限，用户在 Chrome 里限制站点后继续 fail closed。1.1 核心执行不增加 mandatory `debugger` 权限。

首批真实参考场景：

1. **Bilibili transcript**：reviewed builtin Adapter 处理已验证的视频/字幕 identity 与站点签名边界，返回结构化字幕/分段与来源 URL，再交给模型总结；
2. **X search/read**：reviewed builtin Adapter 使用当前浏览器登录态与已验证的 page-origin request/UI strategy，返回结构化帖子与 canonical URLs，再由模型总结；
3. **Doubao image generation**：reviewed builtin UI Adapter 只提交一次图片生成 mutation，立即保留稳定 operation/dispatch identity，后续通过 status/reconciliation 等待完成并返回 artifact evidence / deliverable tab；uncertain 时禁止重复提交。

1.1 的核心阶段顺序：

```text
alpha.1  Provider plugin boundary + Browser ownership + Alpha 8 Page Assist absorption
alpha.2  Generic Web typed BrowserPage + screenshot + optional Jev fast path + real-browser UAT
alpha.3  Bilibili / X / Doubao reviewed builtin Adapter real-browser UAT
alpha.4  Project Skill Adapter model (normal project SKILL.md + existing Progressive SkillService)
beta.1   Adapter draft / try / activate / repair / rollback + authoring Skill
beta.2   ChatGPT / Grok / Claude convergence on shared Browser Kernel
rc.1     packaging / permissions / compatibility / rollback / multi-device acceptance
```

2026-10-03 当前 1.1 DEV 线已完成 alpha.1、alpha.2 和 alpha.3 的 C1/C2/C3 真实浏览器 UAT，Phase C PASS。C1 Bilibili 与 C2 X 结论保持不变。C3 Doubao image generation 的 `doubao.image.generate` + `doubao.image.status` 复用 exact Page Assist grant、`page_ref`、现有 generic `operations` ledger 和 Web Artifact cache；没有新增表、migration、依赖、CLI 或公共 MCP tool。真实 signed-in UAT 依次修正 composer `More` reveal、exact page focus、same-origin route drift、generated-image detection 和 extension-context artifact capture；这些修复都保持 submit-once/status-only/idempotency 语义不变。最终 durable operation 为 `complete / applied / retry_safe=false`，生成结果被捕获为 `image/png` artifact（`1,300,345` bytes，SHA-256 `375b8e01f045d4b61c3930285c7b5a78d62798977aa522be30b4b39edbbd5c08`），prompt 明文不进入 durable result。此前失败均为 proven `not_applied`，因此同一 idempotency identity 的安全重试没有产生重复生成。最终 owned BrowserPage `finalize` 返回 `tab_cleanup_verified=true`，诊断/reload tabs 和临时文件均清零。`3c6148d7` 上 targeted Doubao、Browser Actuation、extension smoke、全量 `npm test`、fmt 和 1299-test Rust suite 全部通过；air DEV 已同步到该 commit，MacBookPro prod 保持未变。下一步进入 alpha.4 / Phase D 的 Project Skill Adapter model，不再扩 C3 站点能力。

2026-10-04 alpha.4 / Phase D 的 Project Skill Adapter model 在托管 DEV runtime 上完成 live saved-Adapter UAT。可复用的站点工作流就是普通 project Skill：`<project-root>/.agents/skills/herdr-browser-<slug>/SKILL.md`，由现有 Progressive SkillService 发现，没有新增 registry、`adapter.json`、database 或第二 executor，永久 MCP schema 仍是 18 个 tool。UAT 在无害只读页面 `https://example.com` 上走完完整生命周期：builtin `browser-adapter-author` 可 load；写入临时 project Skill 后不重建/重启即被 `skill.list/describe/load` 发现；fresh BrowserPage replay、失败修复不改 saved Skill、最小成功修复更新 digest、Git rollback 恢复原 digest 都通过，owned BrowserPage 与临时资源均已回收。资格过程中修复了两个信任/运行边界：`.agents/skills` symlink-root 解析出 scope 时 fail closed，同时保留合法 in-scope symlink；macOS 受 TCC 保护项目根不再由旋转 runtime 直接 `read_dir`/`open`，改走既有 stable TCC broker 的 `fs_list`/`fs_read`，broker 失败不回退直接读。focused tests、完整 Rust workspace 1301 tests、clippy `-D warnings`、fmt、diff check 全绿。最终 post-fix live 复测在 DEV generation `rust-259a3256039f405e`、source commit `cad63a68` 上直接使用真实 `/Users/qingxian/Documents/herdr-mcp`：`skill.list` 正常返回 59 个 Skills；project-local `typesafe-ai` 的 `source_identity=project:/Users/qingxian/Documents/herdr-mcp`、digest `sha256:577c72513…`；`skill.describe` 与 digest-pinned `skill.load` 返回相同 identity/digest，load 大小 `10,039` bytes。此前 protected-root hang 因而在包含修复的实际 runtime 上关闭。

1.1 不把“任意网页”解释为无限制 RPA。普通生产路径继续禁止 local Adapter 任意 JS/eval、cookie/storage secret 导出、任意 shell/filesystem 访问与隐式接管用户 tab；高影响发布、支付、删除、授权等动作继续走确定性 human-boundary。

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

- workstation Runtime Execution Contract 当前为 epoch 5 / 18 tools；first-party DEV/PROD 公共 Edge contract 当前为 epoch 8 / 19 actions，其第 19 个 action `herdr_devices` 独立于这套 runtime tool catalog；Progressive Skills 本身不增加 runtime tool；
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
- 未经独立 contract-epoch 演进就增加第 19 个 workstation MCP tool，或原地改变当前 Runtime Execution schema。

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

截至 `v0.4.1`，`18-tool native parity → production transport parity → Shared Local State Store foundation → supervisor / Native Messaging / updater / link → Node runtime removal` 已成为生产基线。`v0.4.2` 已完成 `Wave A release/test hardening → Wave B measured efficiency → Wave C docs taxonomy → docs-site redesign`，保持 epoch 2 / 18 tools public contract 不变，并加入 crash-safe Continuity Journal foundation：绑定 Web 会话的 finalized turn 可增量进入 Rust `state.db`；显式 `continuity_id` 可经现有 `herdr_call` 恢复有界上下文，而同一已绑定 ChatGPT Project 里手动新开会话后只说“继续 / 接着上次”时，planner 会先通过 `continuity.resolve` / `continuity.search` 找回 durable chain。只有稳定 conversation/Project/workspace identity 本身唯一时才允许自动 resume，纯文本或多候选必须确认，且禁止按最近/最相似直接猜；ID-only 接力前仍必须实时确认 Rust chain 存在，失败时继续使用既有 handoff packet 路径。完整 Continuity 2.0（rolling semantic checkpoint、更多 browser control state Rust 化、长期 retention 策略）与 Work Context/Evidence 保持后续路线。

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

工具性能作为独立 lane 演进，详细历史与基准见 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)。Batch A/B 的普通优化不得原地改变当前 Runtime Execution visible contract；历史 Batch A/B 在 epoch 2 / 18-tool 下完成，后续 model-visible 变化必须走独立 contract epoch；生产 Rust Link 已是稳定基线，不再作为性能 lane 的并行 cutover 任务。只有测量证明固定 MCP/model round-trip 仍是主要瓶颈后，才在未来明确评估 multi-operation tool schema / JSON-RPC batch 与 contract epoch 演进，禁止把 model-visible schema 变化混入普通 Rust 重构。

长任务可观察性作为性能与可靠性并行 lane 纳入 [`docs/history/architecture/tool-performance-optimization.md`](./history/architecture/tool-performance-optimization.md)。该 lane 通过 Task Journal、phase event 和 checkpoint 提供长 release/CI/deploy/self-upgrade 过程的阶段反馈，不替代 Git/runtime live state，也不原地改变当前 epoch 5 / 18 tools Runtime Execution contract。

## 暂不进入主线

以下能力保持观察，不进入当前 Roadmap：

- 自建新的 Agent Team runtime；
- 强制引入 ACP 作为内部主协议；
- 复制 Luvus Task/Lease/merge gate 系统；
- 绑定单一 Coding Agent 或要求本机必须安装某个 Agent；
- 第二套 Web Agent 编排系统；
- 为所有操作强制引入 Task/Project/Workflow 对象；
- 浏览器之外的通用桌面 GUI/RPA 平台；
- ChatGPT Desktop preload/renderer tweak、Electron 客户端 patch/install/update/restore 路线；该 2026-09-21 spike 已于 2026-09-26 决定退役，1.1 网页执行统一走 Herdr Browser Extension；
- 把任意 JavaScript/CDP/cookie/storage 权限作为 1.1 普通 Store 用户的默认浏览器能力。

浏览器 Adapter Platform 已进入 1.1 主线；其它跨桌面/跨应用自动化只有在真实使用数据证明现有 Herdr + fs/Git/exec + Browser Adapter + 可替换 worker 无法表达需求时，再通过 adapter/plugin 或新的 contract epoch 评估。
