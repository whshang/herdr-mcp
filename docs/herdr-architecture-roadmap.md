# Herdr Architecture Roadmap

状态：实施中
原则：用效率最高、可能是最复杂但对用户最友好的方案，不追求短期收益。
来源：合并 `rust-rearchitecture.md` 与 `tool-performance-optimization.md`；本文件是唯一规划基线。RTK 的结果压缩思路进入 AI Tool Runtime Optimization，不复制 CLI proxy。

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

## 当前路线状态

更新时间：2026-08-27

```text
Herdr Architecture Roadmap
├── 当前路线状态
├── 已完成并验收
│   ├── Rust Native Runtime
│   ├── Batch A Performance
│   ├── Result Optimization first wave
│   ├── Project Context Cache first slice
│   ├── Streaming First (#62 start/read + #66 sync completion parity)
│   ├── Skill wave guidance tighten (#61)
│   └── health_watchdog in service status (#63)
├── 已完成待验收
│   ├── Cloudflare Edge read fast path (#60 harden)
│   ├── Long Task Progress Observability
│   ├── Search Execution first slice
│   └── Link candidate daemon staged (#65; production still Node)
├── 规划中
│   ├── Search Execution Architecture (remaining)
│   ├── Batch B Tool Batch Architecture
│   └── IngressProfile
├── AI Tool Runtime Optimization Architecture
│   ├── Result Optimization Layer (first wave landed)
│   ├── Tool Wave Scheduler (skill-level only; #61 guidance)
│   ├── Project Context Cache (first slice landed; deeper waits on benchmarks)
│   └── Streaming First (#62+#66 landed; no mid-call stream for sync tools)
├── Appendix A
│   └── Rust architecture history
└── Appendix B
    └── Performance implementation history
```

状态定义：

- 已完成并验收：代码、测试、真实 smoke 或 benchmark 已通过；
- 已完成待验收：实现完成，需要长期运行数据或生产验证；已设计未落地的项在本节标明；
- 进行中：已有明确实现路径，当前正在推进；
- 规划中：架构方向确定，等待前置条件。

## 路线状态总览

| 状态 | 含义 | 当前重点 |
|---|---|---|
| 已完成并验收 | 代码、测试、smoke/benchmark 已通过 | Rust runtime、Batch A、Result Optimization first wave、Project Context Cache first slice、Streaming First（#62+#66）、skill waves（#61）、health_watchdog status（#63） |
| 已完成待验收 | 实现完成或设计完成，需要生产数据或后续落地 | Edge read path（含 #60 harden）、observability、Search first slice、Link candidate daemon（#65 staged） |
| 进行中 | 已进入实现阶段 | （无独立实现片；下一片见下） |
| 规划中 | 架构确定，等待前置条件 | Search remaining、Batch B（仍等 Layer 3）、Ingress、PCC deeper cache |

## 当前执行重点

**Result Optimization first wave 已合入 main**（epoch 2 / 18 tools 不变）：`herdr_git status` compact（#53）、exec 成功输出 head/tail（#54）、`herdr_git` diff/log compact（#55）、`herdr_fs_grep` group-by-file（#56）。Evidence Store 仍等真实恢复需求，不为本波预留抽象。

**Search Execution first slice 已合入，待生产验收**：`herdr_fs_grep` 优先 rg、Rust walker 回退（#52），与 grep compact 共用 finish path。IndexBackend 等其余 Search 架构仍属规划中。

**Project Context Cache first slice 已合入**（#58）：mutation 路径单次 `derive_routing` 复用；整体仍为 P1，更深 cache 等待基准再加深。

**Streaming First 已合入**（#62+#66）：`herdr_exec_start` / `herdr_exec_read` 带 phase 与 progress；同步 `herdr_exec` / `herdr_fs_grep` 完成结果已有 phase/progress 字段对齐，仍无 mid-call stream（MCP sync 工具仍阻塞至完成）。

**Edge read path harden 已合入**（#60）：ephemeral read 在 DO write quota 压力下继续观察；见下「已完成待验收」。

**Skill wave guidance**（#61）：skill 层收紧 compact 结果与长 exec 的 wave 指引；仍无 runtime Wave Scheduler。**health_watchdog**（#63）：`service status` 已单独暴露 `dev.herdr-mcp.health-watchdog`。**Link candidate daemon**（#65）：Rust daemon 组装仅 staged；生产 Link 仍走 Node，切流不是当前片。

**Tool Wave Scheduler** 仍仅 skill 层策略。**Batch B** 仍等 Layer 3 Connector UAT，不是当前片。

下一唯一实现片：**生产装新 generation 验证 Streaming 字段**。

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

状态：已完成实现，未生产切流（#65）。Rust `link::daemon` 组装仅 candidate/staged；生产 Link 继续走 Node。
## 规划中

### Search Execution Architecture

状态：P2，first slice 已合入（见上）；其余（Query Planner、IndexBackend 等）未实现。大仓库 `fs_grep` 目标路径仍为 Security Layer → Query Planner → RgBackend / RustFallback，IndexBackend 更后。不新增第 19 个 tool。

### Batch B Tool Batch Architecture

状态：P2，设计中。只有 Layer 3 Connector UAT 证明 MCP/model round-trip 是主要瓶颈后进入。不新增第 19 个 tool，不绕过 contract epoch。

### IngressProfile

状态：规划中。统一 cloudflare-edge / local-tunnel / relay-vps 入口，同一 MCP contract 与 mutation safety。Edge `rows_written` 观察稳定后再实现。

## AI Tool Runtime Optimization Architecture

状态：Result Optimization first wave、Project Context Cache first slice、Streaming First（#62+#66）已合入；下一片为生产装新 generation 验证 Streaming 字段。更深 PCC 等待基准。Batch B 仍等 Layer 3。

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
- 生产 Link 切流、第 19 个 MCP tool、epoch 2 schema 变更。

---

# Appendix A: Rust Native Architecture Details (merged from rust-rearchitecture.md)

# Herdr MCP Rust 原生化重构计划

状态：Accepted / In progress
目标分支：`refactor/rust-supervisor-20260825`

## 核心决策

Herdr MCP 本地产品重构为 Rust 原生单一运行时。Rust 不只是 supervisor，而是最终本机产品边界。

正式安装只提供 `herdr-mcp` 二进制，负责：

- CLI
- MCP runtime
- Herdr transport
- relay/link
- supervisor
- watchdog
- update
- generation
- Native Messaging host
- diagnostics
- macOS/Linux/Windows service integration

Cloudflare Worker 和浏览器扩展继续使用 TypeScript/JavaScript，因为它们属于不同运行环境。

### Lifecycle one-shot 安全约束

service / update / native-host 的破坏性 lifecycle mutation **禁止使用 `launchctl submit`**。inferred launchd job 可能在命令退出后继续 replay，从而重复消费 rollback 或重复执行其他非幂等 mutation。独立执行时使用受管 lifecycle 路径；确需 launchd one-shot job 时，必须使用显式 plist，并设置 `RunAtLoad=true`、`KeepAlive=false`。

## 不采用长期双 runtime

不保留长期架构：

```text
Rust supervisor -> Node MCP runtime
```

迁移期间 TypeScript runtime 作为行为参考实现。Rust 完成 parity test 后，删除对应旧实现。

不维护：

```text
Rust MCP server
TypeScript MCP server
```

两套生产实现。

## 为什么现在重构

当前项目已经进入产品化阶段，需要解决：

- 用户无需安装 Node/npm/Python；
- 单二进制分发；
- 跨平台服务管理；
- 自升级和回滚；
- 本机权限和 IPC 管理；
- Native Messaging 集成；
- 明确的 runtime 状态机。

Rust 的主要收益来自系统工程能力。MCP HTTP 解析性能不是主要目标。

## 开发模式

开发和生产使用同一 Rust 产品。

开发模式：

```bash
cargo run -p herdr-mcp -- dev
```

允许迁移早期启动 TypeScript reference runtime，但最终 dev 和 production 都运行 Rust runtime。

隔离：

```text
production ~/.config/herdr-mcp

development ~/.config/herdr-mcp-dev
```

## 迁移阶段

### Phase 1

- Cargo workspace
- CLI
- status
- doctor
- dev bootstrap
- Rust CI

### Phase 2

- supervisor
- watchdog
- service manager
- structured state

### Phase 3

- Native Messaging
- local IPC

### Phase 4

- GitHub Release manifest
- update
- checksum/signature
- generation activation
- rollback

### Phase 5

迁移 Herdr transport 和 MCP tools。

迁移锚点保持：

```text
contract epoch 2
18 tools
```

### Phase 6

Rust runtime 完成后删除本地 Node runtime。

## 分发目标

支持：

- macOS arm64
- macOS x86_64
- Linux x86_64
- Linux aarch64
- Windows x86_64

安装方式参考 Herdr：

- 官方 installer
- GitHub Release
- Homebrew
- mise
- Nix
- Docker

包管理器安装由包管理器负责升级，direct install 使用 `herdr-mcp update`。

## 当前实施

截至 2026-08-25，本分支已经完成第一批原生化基础：

1. 创建 Cargo workspace，固定 Rust 1.97.1，并把 `fmt`、`clippy -D warnings`、workspace tests 接入 CI；
2. 实现 `version`、`status`、`doctor`、`config` 和隔离的 `dev` bootstrap；
3. production/dev 默认分别使用 `:8772` 与 `:8872`，状态目录分别为 `~/.config/herdr-mcp` 与 `~/.config/herdr-mcp-dev`；
4. 将 epoch 2 / 18 tools 固化为语言无关的 `contracts/epoch2.json`，Rust 与现有 Edge contract 共同受测试约束；
5. 实现 Rust Herdr Unix-socket newline-JSON RPC client，包含超时、1 MiB 响应上限和 daemon error 映射；
6. 实现 `herdr api schema --json` 原生反射、60 秒缓存、8 秒加载上限，以及 required/type/enum/unknown-param 校验；
7. 建立 Rust `herdr_methods` / `herdr_call` 核心 service，validated call 已通过真实 Herdr daemon smoke；
8. `doctor` 当前真实验证 MCP runtime、Herdr RPC、live API schema、validated RPC、snapshot state 和 Rust inspect projection；
9. 原生 snapshot 层使用 `session.snapshot`，并发以 `workspace.list` / `pane.list` / `agent.list` 覆盖 live collection；aggregate 失败时回退 list assembly；
10. Rust `herdr_inspect` 核心投影已覆盖 workspace/tab/pane/agent、Git project、dirty/changed-files、shared project 和 heterogeneous workspace；
11. Git project discovery 优先父目录 `.git` 确定性扫描，异常布局才回退有超时的 `git rev-parse`；managed project 的 dirty status 并发、有界执行；
12. Agent soft visibility 已迁移，默认 allowlist 与当前 production 一致，并支持 `HERDR_MCP_AGENT_ALLOW=*`；
13. `workstation_info` 已由 Rust 提供 default cwd、managed Git roots、read-only/write-root 状态及原生 executable discovery；未来正式产品不把 Node/npm/Python 作为运行依赖；
14. 第一检查点 `3e93917 feat: bootstrap native Rust runtime` 已提交并推送到 `origin/refactor/rust-supervisor-20260825`；Rust、root Node、Cloudflare Edge、site build 和 browser extension smoke 已完成整仓回归。
15. Rust 已实现 `events.subscribe` 长连接 wire protocol，支持字符串/对象两类 event envelope、1 MiB frame 上限、有界 read tick、订阅 deadline 和 daemon error 映射；
16. `EventCache` 已成为原生常驻状态层：snapshot bootstrap、25 秒重订阅、30 秒 full-snapshot TTL、250ms 可中断 poll、断线重连、unknown-workspace admission gate、workspace/pane/tab/agent 增量归并；
17. Event cache 保存最多 2048 条 cursor history；`cursor=0` 返回最近 64 条，并维护 Agent `last_activity_at` 与从 session filename 推导的 `started_at`；
18. Rust `herdr_since` service 已实现 boot id、cursor reset、workspace id/label filter 和 Agent visibility；它直接读取 EventCache，不在 MCP 调用时轮询 daemon；
19. `doctor` 已真实启动/停止 EventCache，并验证 background `events.subscribe` 已进入 live 状态；当前 Rust 测试为 39/39；
20. 第二检查点 `e39ecf7 feat: migrate native inspect state` 已提交并推送。
21. 第三检查点 `367fa84 feat: add native event state cache` 已提交并推送；
22. Rust candidate MCP HTTP transport 已建立，使用 Axum/Tokio，仅绑定 `127.0.0.1`，启动时强制要求 `HERDR_MCP_TOKEN`，不会启动匿名 MCP endpoint；
23. candidate 已支持 `initialize`、`server/discover`、`tools/list`、`tools/call`、`ping` 和 initialized notification；initialize/tools-list 在客户端声明 `text/event-stream` 时保持 SSE handshake framing；
24. `tools/list` 直接读取 `contracts/epoch2.json`，真实 HTTP smoke 确认精确暴露 epoch 2 的 18 tools；
25. 已迁的 `herdr_methods`、`herdr_inspect`、`herdr_since`、`herdr_call` 已通过 candidate HTTP 调用真实 Herdr daemon；未迁工具统一返回 `native_tool_pending` + `isError=true`，因此 candidate 不会伪装成完成态；
26. candidate 使用迁移期命令 `herdr-mcp candidate --port 8873`。它不是最终 CLI contract；生产切换前还需完成 persistent GET/SSE、完整 18-tool implementation、auth/session compatibility 和 Edge parity；
27. 第四检查点 `0beef01 feat: add Rust MCP candidate transport` 已提交并推送；当时 Rust 单测为 46/46，真实 candidate smoke 验证了 unauthorized=401、health、SSE initialize、18-tool catalog、4 个 native tool call 和 pending-tool rejection；
28. 新增 `contracts/runtime-parity.json`，固定 Node reference 与 Rust candidate 共享的 server name、SDK wire protocol、supported versions、epoch/hash/tool count 和 stateless SSE/JSON framing 分类；Rust 与 Node fixture tests 同时消费该文件；
29. managed-root 安全层已经迁入 Rust：managed roots 只来自实时 snapshot 的 Git project，existing path 必须 canonicalize 后仍位于同一 root；secret-ish path 和 `.git/config` 直接拒绝，symlink escape fail-closed；
30. Rust 已原生实现 `herdr_fs_read`、`herdr_fs_list`、`herdr_fs_grep`：read 保持完整行 byte budget，list/grep 不跟随目录 symlink、跳过 `.git` 和 secret path，grep 使用 Rust regex/目录遍历，不把 `rg` 作为正式运行依赖；
31. Rust 已原生实现只读 `herdr_git` 的 `status/diff/log`，参数不经 shell；diff path 不能逃逸 managed root，Git stdout/stderr 边 drain 边限额，15 秒超时后强制终止；
32. 真实 candidate HTTP smoke 已验证 read/list/grep/git 正常工作，并验证 `/etc/hosts`、`.git/config`、`git diff ../...` 分别被 managed-root/secret/escape gate 拒绝；当时 Rust 单测为 56/56。
33. 第五检查点 `975273f feat: expose native runtime diagnostics` 已提交并推送；Rust candidate 的 health/inspect 已统一暴露 build metadata、EventCache boot id、native migration 进度与 exec-session readiness，迁移状态不再依赖人工对照源码。
34. 第六检查点 `b13b5e9 feat: migrate native image tool` 已提交并推送；`herdr_fs_image` 复用 managed-root/secret/symlink 安全边界，支持 PNG/JPEG/GIF/WebP、2 MiB 默认/8 MB 上限和 MCP `text + image` content；真实 candidate HTTP smoke 已验证图片成功返回、`image_too_large` 与 `unsupported_image`，当时 Rust 单测为 60/60。
35. `10beb44 test: make Rust fs fixtures collision-safe` 已修复 Rust 并行测试临时 Git repo 的低概率命名碰撞；连续 5 轮 workspace tests 均通过，避免把时间粒度碰撞误判成 fs/security 回归。
36. 第七检查点 `18f4a0a feat: add native mutation safety and fs writes` 已提交并推送；统一 mutation policy 覆盖 readonly、write-root、working-Agent、dirty confirmation、managed target、secret/symlink 与 atomic single-file write；`herdr_fs_edit` / `herdr_fs_write` 已通过真实 candidate smoke，验证新建、overwrite 拒绝、dirty 拒绝、`confirm_dirty` 放行和 secret-path fail-closed；当时 Rust 单测为 65/65，整仓 Rust/Node/Edge/site/extension 回归通过。
37. 第八检查点 `958711f feat: migrate native patch transactions` 已提交并推送；Rust 已原生实现 Codex/coding-tools 风格 patch parser、unique-context hunk、CRLF/BOM 保持、multi-file preflight、dirty/busy gate 与同目录 temp/backup 原子事务回滚；`herdr_fs_patch` 真实 candidate smoke 已验证 dry-run 不写、dirty fail-closed、`confirm_dirty` 下 add/update/delete 同事务成功、`../` path escape 拒绝。当前 native migration 为 12/18，Rust 单测 72/72；完整 gate 为 Node 314/314、Edge 212/212、双语站点 21 篇/语言并全部通过。
38. `17e50ad feat: migrate native exec sessions` 已提交并推送；Rust 原生 `herdr_exec_start/read/kill` 使用单一 `ExecRegistry`，每流 512 KiB bounded output、跨 stdout/stderr sequence、独立 Unix process group、SIGTERM → grace → SIGKILL、兼容 shell/PATH，并把 restart journal 限定为 `session_id + pid` fencing identity，不长期保存 command/cwd。
39. `d32495d feat: migrate visible utility exec` 已提交并推送；短命令 `herdr_exec` 保持 workspace/project-root 选择、可见 `herdr-mcp:utility` pane、busy-agent gate、pager 抑制、超时后 observation 提示，以及“发送前可 fallback、发送后 outcome unknown 时禁止自动重发”的安全边界。
40. `88d7b7a feat: migrate native agent prompt` 已提交并推送；`herdr_prompt` 已迁入 Rust，保留 idempotency key、parallel/conflicting reuse gate、socket `agent.prompt`、wait 状态与 post-submit observation，native migration 达到 17/18。
41. `af7ea63 test: harden Rust temp fixtures` 修复 mutation policy 与 Unix-socket 测试的并发临时资源命名碰撞；`34cd115 fix: fence recovered exec process groups` 进一步把 restart fencing 从仅检查环境 marker 升级为受控 shell `argv[0]` session marker + `PGID == journal PID` 双重验证，并保留环境 marker 兼容路径。
42. live-orphan restart smoke 已在 macOS 真进程上闭环：candidate 启动 `sleep 300` 后终止 runtime，确认 session shell 以 PPID 1、独立 PGID 继续存活；使用同一 state dir 重启后旧 session 返回 `recovery_state=reaped_on_restart`，`herdr_inspect.workstation_info.exec_sessions_diagnostics.reaped_on_boot=1`，原 PID 随后消失。PID reuse/marker 无法验证时继续返回 `detached_unverified`，不会盲杀不明进程。
43. `45ea1b8 feat: complete native tool parity` 完成最后一个 `herdr_skill`：project policy 支持 upstream fetch + TTL cache + stale fallback + bundled policy，native Herdr reference 使用有界 `herdr --skill` + bundled fallback，runtime/self-update/worker fallback context 只投影白名单字段；`workstation_info.agent_skill` 与 production pointer 对齐。
44. 当前 Rust candidate 已达到 epoch 2 **18/18 native tool parity**，`native_parity_ready=true`、`pending_tool_count=0`；此时仍保持 `production_ready=false`。该里程碑整仓 gate：Rust 94/94、Node 314/314、Edge 212/212、双语站点 21 篇/语言，全部通过。
45. `68b0e8d feat: add Rust transport sessions and SSE` 已完成本机 MCP transport 的核心 production parity：bounded in-memory stateful session registry、`Mcp-Session-Id`、`/mcp/` alias、session DELETE、valid-session persistent GET SSE，以及 ChatGPT/OpenAI stateless 分类；persistent SSE 首帧为 `: connected`，15 秒 heartbeat 为 `: keepalive`，并保持 `no-cache, no-transform` / keep-alive / no-buffering 响应头。
46. OpenAI/ChatGPT transport 明确采用 stateless 语义：`openai-mcp` UA 或 initialize `clientInfo` 为 OpenAI/ChatGPT 时忽略 stale/poisoned `Mcp-Session-Id`，initialize/tools-list 继续按 Accept 使用 SSE，tools/call 与 server/discover 使用 JSON，不返回新的 session id；`server/discover` 保持 SDK wire `2025-11-25` 排第一，同时兼容探测版本 `2026-07-28`。普通 stateful 客户端的 stale session 继续 fail-closed 为 HTTP 404 / JSON-RPC `-32001`。
47. `756def9 test: lock Rust transport session parity` 已把上述行为固化为真实 Axum router regression；`21e2ba8 test: lock Rust transport restart semantics` 进一步验证 runtime restart 后旧 stateful SID 被 fencing 为 404，而相同 stale SID 在 `openai-mcp` 客户端仍按 stateless 语义成功，不产生“Session terminated”式污染。
48. `53c2030 fix: harden Rust transport session state` 把 session registry 锁异常改为 HTTP 500 / JSON-RPC `-32603` fail-closed，不再返回无法验证的 SID；新增 100 次 OpenAI poisoned-session stress，确认全部请求成功且 registry 始终为 0，不把 stateless 客户端偷偷转成 stateful。
49. production transport 真实 smoke 已覆盖：同一 TCP 连接连续 `initialize → tools/list → tools/call → notifications/initialized`，18-tool catalog 与 202 notification framing 均正常；OpenAI poisoned-session 在同一 TCP 上连续 100 次 tools/list 全部 200、无 SID；同端口 runtime restart 后旧 stateful SID 为 404，而 OpenAI stale SID 为 200/18 tools；persistent GET 实测连接保持开放并收到 15 秒 heartbeat。Rust transport regression 当前为 100/100 单测通过。
50. 本阶段的 auth 边界按目标架构收紧：Rust runtime 继续只绑定 loopback，并强制要求本机 bearer；Web Connector 的 OAuth 仍由 Edge 负责（`Web AI → MCP/OAuth → Edge → link → Rust runtime`），extension/Native Messaging 的本机信任通道留到后续 supervisor/Native Messaging 阶段，不复制旧 Node runtime 的 OAuth/extension-session 逻辑到 Rust HTTP transport。
51. 至此 **local production transport parity 核心闭环完成**：persistent SSE、stateful/stateless session、OpenAI UA、restart/reconnect framing、raw keep-alive、stale-session reverse guard 与 loopback bearer 均已有自动测试和真实 smoke。
52. `1b59f8e feat: add secure Rust local state store foundation` 已建立 Shared Local State Store：嵌入式 SQLite、WAL、foreign keys、busy timeout、显式 schema migration、事务、0600 database / 0700 parent directory、安全路径与 future-schema fail-closed；它属于内部 runtime state，不改变 epoch 2 / 18-tool contract。
53. `03c1148 feat: persist Rust exec session fences in SQLite`、`592f0c7 feat: persist prompt idempotency across restart`、`751331e fix: align exec restart recovery with SQLite` 已把首批可靠性状态迁入统一数据库：exec 只持久化 process fencing identity，command/cwd 继续不进入 durable state；prompt idempotency 可跨 runtime restart replay/conflict-check；旧 exec restart recovery 继续保留 `reaped_on_restart` / `closed_before_restart` / `detached_unverified` 的真实语义。
54. 浏览器侧 `cd567cd` / `3314c63` 已把 ChatGPT binding 收敛到稳定 Project scope；`475d537` / `c98211c` 已为长对话增加 bounded scheduler、latest-turn cache 与 UI pressure meter，减少 MutationObserver/DOM 全量扫描和重复 `/push/state` 压力。
55. `ea3a064 feat: add opt-in Rust extension IPC socket` 已加入迁移期 Rust extension IPC：仅在显式配置 `HERDR_EXTENSION_IPC_SOCKET` 时启用 Unix socket，socket mode 为 0600；live socket 二次 bind fail-closed，stale socket 可安全替换，guard 只删除自己持有的 inode。TCP MCP endpoint 继续要求 bearer，trusted extension IPC 才使用本机 socket 信任边界。
56. `7f9ee15 feat: add Rust extension push channel` 已把 `/push/state` 与共享 `/push/events` SSE 接入同一个 Rust `EventCache`，支持 `hello`、`agent_working`、`agent_settled`、15 秒 keepalive 和 agent/pane/workspace filter；不会为每个浏览器绑定再开 Herdr daemon subscription。workspace catalog 已对齐 Node 的 Project root fallback（projects → workspace cwd → agent cwd → pane cwd），避免 Rust 切换后 HUD/Project binding 丢失 roots。
57. 该上行通道真实 smoke 已验证：trusted Unix socket 无 bearer 可读取 `/push/state`/`/push/events`，同一路由从 TCP 无 bearer 返回 401；SSE 首帧包含 `retry: 2000`、`event: hello`、`herdr-mcp-push/v1`，w77 的真实 worktree root 可在 state 与 hello 中一致恢复。该 checkpoint gate 为 Rust **125/125**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。
58. `1d79963 test: stabilize extension IPC stale-socket fixture` 修复 macOS Unix listener 刚关闭后短时间仍可能成功 `connect()` 的测试竞态：production `prepare_socket_path` 继续把可连接 socket 视为 live 并 fail-closed，测试等待内核确认 stale 后才验证替换；同时临时 socket 名加入高熵 nonce。该针对性测试已连续运行 20 次通过。
59. `edd8c89 feat: add Rust Native Messaging host data plane` 新增 `herdr-mcp extension-host <chrome-extension://.../>`：实现 Chromium 4-byte little-endian Native Messaging framing，严格校验 caller origin、loopback HTTP base URL、既有 proxy path/method/header allowlist，并把 `request` / persistent `stream` 直接转发到 mode-0600 Rust Unix IPC。browser bearer 不进入 host；`Authorization` 不在 forwarded-header allowlist；request/native frame/response 分别有 1 MiB / 1 MiB / 8 MiB 边界，普通 request 的连接与 response body 都受 timeout 约束。`stream` 使用 64 KiB base64 frame 持续转发 SSE，不缓存完整长连接。
60. Rust Native Messaging host 当前只实现现代 extension 的 `request` / `stream` 数据面；旧 `session` 消息明确返回 `legacy_session_requires_compat_host`。现有 `bin/herdr-extension-host install/status` 与浏览器 manifest 仍保持 Node compatibility host，尚未切换 production host path，因此不会让旧 extension build 或仍运行 Node runtime 的安装失效。
61. 真实 native-framing smoke 已闭环：Rust candidate 在 `8892` 暴露 `/tmp/herdr-mcp-native-host-smoke/extension.sock` 后，Rust `extension-host` 以当前 unpacked extension 的路径派生 origin `chrome-extension://ciggfiookaelnpaaocdapmohgmaghgge/`，`request /push/state` 返回 `transport=ipc/status=200` 并恢复 w77 worktree root；`stream /push/events` 返回 `stream_open → stream_chunk`，chunk 内含 `event: hello` 与 `herdr-mcp-push/v1`。candidate 停止后 8892 与 socket 均清理。最新整仓 gate 为 Rust **130/130**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。
62. `a2438ec feat: add Rust Native Messaging host lifecycle` 新增 `herdr-mcp native-host install|status|uninstall` candidate 管理面。`install` 把当前 Rust 单二进制原子复制到稳定 `~/.config/herdr-mcp/native/herdr-mcp`，Chrome manifest 永远指向同目录稳定 wrapper `herdr-extension-host`；wrapper 只注入 exact extension origin 并执行 colocated Rust binary，不携带 bearer。native dir / binary / wrapper / manifest 分别收紧为 0700 / 0700 / 0700 / 0600，并拒绝 symlink target。
63. Native Messaging 安装身份不进入 `state.db`：`status/uninstall` 优先从已注册、指向稳定 wrapper 的 Chromium manifest 恢复 exact origin，因此源码 worktree 不存在、`state.db` 损坏或 runtime generation 切换时仍可识别安装。多个已注册 manifest 若 origin 冲突则 fail-closed。`uninstall` 只有在 manifest 精确匹配 host/type/path/origin 且 wrapper 含 Rust ownership marker 时才删除；现有 Node compatibility wrapper、被篡改 manifest 与 symlink 均保留并报告非 owned，避免迁移工具误删当前生产桥。
64. lifecycle 真实 smoke 全部使用临时 HOME，未修改真实浏览器安装：`install → status → uninstall` 验证 manifest 0600、wrapper/binary 0700；故意设置 `HERDR_MCP_ROOT=/definitely/missing` 时 `status` 仍从 manifest 恢复 `ciggfiookaelnpaaocdapmohgmaghgge`，`extension_path=null` 但 `ok=true`。更完整链路还验证了临时 Chrome manifest → stable wrapper → copied Rust binary → mode-0600 Unix IPC → copied Rust candidate（8893），`request /push/state` 与 persistent `/push/events` SSE 均成功，随后 candidate/socket/manifest/wrapper/binary 全部清理。
65. 最新整仓 gate 为 Rust **134/134**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。`production_ready` 继续保持 `false`：Native Messaging data plane 与 candidate install/status/uninstall 已完成，但真实用户 HOME 的 manifest 仍由 Node compatibility installer 管理；在 Rust supervisor/service manager 能持续拥有 production `extension.sock` 之前不做 live cutover。尚未完成 supervisor/service manager、actual manifest cutover/UAT、updater/generation production cutover、relay/link Rust production path 和 Node runtime removal。
66. Rust service manager 第一阶段已经完成代码闭环：新增 `herdr-mcp service install [--adopt-node]|status|start|stop|restart|uninstall`，生产 label 继续使用 `dev.herdr-mcp.server`；runtime 复制到内容寻址的 `~/.config/herdr-mcp/runtime/generations/rust-<sha16>/herdr-mcp`，launchd 只指向稳定的 `runtime/current/herdr-mcp`，`current` 使用受管 symlink 原子切换。默认 `install` 遇到现有 Node service fail-closed；只有显式 `--adopt-node` 才允许接管已识别的 herdr-mcp Node plist，并保留现有 token、Edge base URL、contract profile、Herdr socket、skill-network 与 PATH 配置，不把 bearer 写入 state DB 或 CLI 输出。
67. service manager 同时收口旧 watchdog 的双 supervisor 风险：`dev.herdr-mcp.watchdog` 只有在精确识别为旧 `watchdog.sh once` 后才允许随 Node adoption 退休；接管前备份原 server/watchdog plist，记录其 loaded 状态。Rust service 启动后必须在 10 秒内通过 `127.0.0.1:8772/health`；任一步失败会 bootout 新 service、恢复旧 plist、恢复上一 generation pointer，并按原 loaded 状态重新 bootstrap Node server/watchdog。`start/stop/restart/uninstall` 若发现 legacy watchdog 仍存在则拒绝，避免两个 supervisor 同时拥有 production runtime。
68. Shared Local State Store schema 升到 v4：v3 新增 `runtime_generations` 与 bounded `service_events`，v4 再新增 `service_rollbacks`。generation staging/activation 使用事务保证同一时刻最多一个 `active`，上一 generation 进入 `previous`；service event detail 最多 512 字符。已有 v2 `operations` / `exec_sessions` 数据升级到最新 schema 的回归已验证不丢行。generation activation 是 cutover 强一致门；纯 evidence 记录是 best-effort，并通过 `evidence_recorded` 显式暴露，避免 service mutation 已成功却因日志写入失败而向调用方制造 outcome-unknown。
69. 新 Rust `service status` 已在真实用户 HOME 做**只读**生产分类 smoke：正确识别当前仍为 `implementation=node`、`dev.herdr-mcp.server` loaded、legacy watchdog present+loaded、`runtime/current` 尚不存在；该检查没有修改真实 launchd/plist。最新整仓 gate 为 Rust **139/139**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。`production_ready` 继续保持 `false`，本轮没有执行 `service install --adopt-node`，真实生产仍运行 Node runtime + legacy watchdog。
70. `herdr-mcp service rollback` 已补齐 post-commit rollback：`install` 对旧 Node 或上一 Rust service 创建 mode-0600 plist backup，并把 backup path、source kind、原 loaded 状态、previous generation target 与本次 activated generation 写入 `service_rollbacks`，不把 plist/token 正文写入 SQLite。generation activation 与 rollback `prepared → ready` 在同一个 SQLite transaction 内提交；rollback 先把唯一 `ready` 记录原子 claim 为 `consuming`，严格校验当前 generation、backup confinement 与 symlink 边界，恢复旧 server/watchdog/current pointer 后再把 rolled-back Rust generation 与 rollback consumption 同事务结算。rollback 本身失败时会尝试恢复当前 Rust plist/current/loaded 状态，并把 claim 释放回 `ready`；若二级恢复也失败则标记 `rollback_failed`，不允许盲目重复 mutation。
71. rollback health verification 同时覆盖两类 source：上一 Rust generation 使用 `/health`，旧 Node 使用带原 bearer 的 sessionless `server/discover`，token 只存在于进程内请求头。真实生产只读 smoke 已确认当前 Node `server/discover` 返回成功；在仍运行 Node+watchdog 的现网上调用新 `service rollback` 会在任何 state/launchd mutation 前拒绝为 `service is not installed as an owned Rust service`，且 server/watchdog plist 的 mtime/size/inode 前后完全一致。Rust gate 当前为 **143/143**；真实 production 仍未执行 `--adopt-node`。
72. Rust 已补齐 Node Native Messaging compatibility host allowlist 中最后一条本机路由 `/push/mcp-activity`。实现保持 Node 契约：仅在内存保存最多 2000 条 finished `tools/call` 元数据，字段限于 timestamp、tool、`herdr_call.method`、User-Agent 与 HTTP status，不保存 prompt/arguments/result；查询默认 `ua=openai-mcp`、最大回看 30 分钟、`count` 统计全部命中而 `tools` 最多返回最后 50 条，trusted extension IPC 无 bearer 可读、TCP 无 bearer 仍为 401。真实 smoke 已验证 `openai-mcp` 的 `herdr_methods` 经 Rust TCP 入 ring 后，现有 Node Native Messaging host 通过 Rust `extension.sock` 查询得到 `transport=ipc/status=200/count=1/tool=herdr_methods`；candidate 停止后 socket 正常清理。Rust gate 当前为 **146/146**。
73. post-commit rollback + MCP activity 完成后的整仓 gate 已再次全绿：Rust **146/146**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。至此第一轮 production service-manager UAT 的四条 compatibility-host proxy path（`/mcp`、`/push/state`、`/push/events`、`/push/mcp-activity`）均有 Rust 实现与真实 Unix-IPC smoke，且 runtime health 失败有 install-time auto rollback、browser/UAT 后置失败有显式 `service rollback`；真实 production 仍未发生 cutover。
74. 第一轮真实 `service install --adopt-node` UAT 已执行并安全回滚：preflight 确认 installed Node compatibility host 的 main-checkout extension id 为 `dklcamincneeijhcelpkdbcekfemldii`、5/5 Native Messaging manifest allowed，且 `/push/state` 基线经现有 host 走 `transport=ipc`。cutover mutation 在旧 Node `bootout` 后首次 Rust `launchctl bootstrap` 返回 macOS `Bootstrap failed: 5: Input/output error`；install transaction 随即恢复原 Node server plist、loaded 状态和 legacy watchdog。真实 state v4 证据为 Rust generation `staged`、rollback `auto_rolled_back`、service event `install/rolled_back`，production 8772 随后仍由原 Node runtime 正常提供服务。
75. 上述 failure 与仓库既有 `herdr-self-update` launchd 经验一致：macOS 在 `bootout` 后 domain 尚未 settle 时会短暂返回 error 5。Rust service manager 现已统一采用 `wait-launchd-absent + 250/500/1000/2000ms bounded bootstrap retry`，总计最多 5 次；`bootout` 也等待 label 真正消失。install、start/restart、显式 rollback、install-time auto rollback、rollback 失败后的 Rust 恢复全部复用同一 helper，不再各自承担 race。新增单测验证两次 I/O error 后第三次成功，以及耗尽 bounded attempts 必须 fail-closed。修复后的完整 gate：Rust **148/148**、Node **321/321**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过；第二轮 production cutover 仍需重新构建 release 后再执行。
76. 第二轮 production service-manager cutover 已成功提交。由 `4081d6a` 重新构建 release，SHA-256 为 `8b80ab11e3cd9e5e619d708a311804350418c5862b8b9f661c37628ca8ca4c01`；执行一次 `service install --adopt-node` 后返回 `rc=0`，active generation 为 `rust-8b80ab11e3cd9e5e`，launchd `dev.herdr-mcp.server` 已指向 `~/.config/herdr-mcp/runtime/current/herdr-mcp candidate --port 8772`，`HERDR_MCP_SERVICE_IMPL=rust-v1`，legacy watchdog plist 已删除且 label unloaded。state v4 保留唯一 `ready` Node rollback `rb-1787662450010-node-8b80ab11`，原 Node server/watchdog plist 已备份；因此当前 production runtime 是 Rust，但仍保留确定性的 Node 回退能力。
77. cutover 后健康验证区分了“同一 MCP handler 内同步 self-probe”和真实外部客户端。若在正在处理 `herdr_exec` 的 Rust runtime 内同步 `curl 127.0.0.1:8772` 或运行 `service status`，会因为 handler 嵌套等待自身而制造 5 秒 timeout/`healthy=false` 假象；改用独立 `herdr_exec_start` 让 MCP 请求先返回，再延迟 500ms 外部探测后，`/health` 约 2.3ms 返回 200、`runtime=rust-candidate`、`native_parity_ready=true`，`tools/list=18`，外部 `service status` 也稳定为 `implementation=rust/healthy=true`。后续 doctor/UAT 不应使用同步 nested self-probe 判断同进程 runtime 健康。
78. production Native Messaging compatibility UAT 已覆盖 allowlist 四条路径。真实 installed origin 仍为 main-checkout extension id `dklcamincneeijhcelpkdbcekfemldii`，5/5 browser manifest allowed，wrapper 仍为 `~/.config/herdr-mcp/native/herdr-extension-host`。经该现有 Node compatibility host：`/mcp` 命中 Rust Unix IPC、HTTP 200、18 tools；`/push/state` 为 `ipc/200` 并返回当前 workspace/agent catalog；`/push/events` 为 `ipc/200` 且收到 `event: hello`；`/push/mcp-activity` 为 `ipc/200`。为避免 runtime restart 后空 ring 造成假失败，另由外部 `User-Agent: openai-mcp/1.0.0` 发起只读 `herdr_methods`，随后通过 installed host 查询 activity 得到 `count=1/tool=herdr_methods/ua=openai-mcp/1.0.0`。
79. 真实 ChatGPT Project 页面 `https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/project` 已完成只读浏览器 HUD UAT：页面登录态与 title 正常，DOM 中存在 `#h2w-page-hud`，shadow DOM 已渲染 `Herdr ● 已完成` 并读到 `w68/w74/w77/w7G`；`自动 开` 正常，Project 首页按设计锁定“手动继续 / herdr监控 / LLM 分析”，同时“手动接力”保持可用。两次 cutover 后 Rust PID 变化进一步通过 `state.db service_events + w77:p2 utility pane history` 归因到另一网页编排会话显式执行的两次 `service restart`，不是 Rust crash、link daemon 或 launchd 自发重启。该 UAT 发生在 Node compatibility Native Messaging host 仍作为浏览器入口的阶段。
80. production Native Messaging manifest/wrapper 随后已切到 Rust host。切换后发现 Chromium 正常启动 Native Messaging host 时不会追加 caller-origin positional argv，而稳定 wrapper 只通过 `HERDR_EXTENSION_ORIGIN` 注入已注册身份；旧 CLI 因要求 positional origin 会在 Chrome 正常调用前提前退出。`818a7d1 fix: make Rust native host launchable by Chromium` 修复为“无 positional arg 时使用受管环境中的 exact origin，显式 arg 仅保留测试/兼容调用”，并让 `native-host install` 优先继承已有 registered origin，避免从当前 build/worktree 的 extension path 重新派生 ID 导致身份漂移。热修前已把 binary/wrapper/5 个 manifest 保存到 `~/.config/herdr-mcp/backups/native-host-pre-cli-fix-20260825-214417`；热修后 stable production wrapper 无参数启动真实 framing smoke 已验证 `/mcp=ipc/200/18 tools`、`/push/state=ipc/200`、`/push/events` 收到 `event: hello` + `herdr-mcp-push/v1`。第一类 native-host binary/manifest 自动 rollback 仍需单独产品化，不能把这份人工 backup 当长期 rollback 机制。
81. `dcb35d4 feat: add Rust release artifact manifest` 建立 Rust release 供应契约：GitHub Actions 先执行 Rust/root/Edge/site/extension release gate，再分别在 hosted runner 原生编译 `aarch64-apple-darwin`、`x86_64-apple-darwin`、`aarch64-unknown-linux-gnu`、`x86_64-unknown-linux-gnu`、`x86_64-pc-windows-msvc` 五个 raw binary；`release-manifest.json` 固定 product/version/tag、epoch2 contract identity、target/name/size/SHA-256/asset URL。workflow 默认 `contents: read`，只有 tag publish job 提升到 `contents: write`。
82. `6b279bf feat: add rollback-safe Rust updater` 新增 `herdr-mcp update check|apply|status` 与内部 detached `update worker`。manifest 最大 1 MiB、binary 最大 64 MiB；默认只从 GitHub latest manifest 检查，显式 override 也只允许 HTTPS 或 loopback HTTP，URL credentials 拒绝；下载必须同时满足 exact target、semver、epoch2 hash/tool-count、declared size、lowercase SHA-256，并在 staging 后再次 hash + 执行 `version` 验证 `herdr-mcp <version>`、`contract epoch 2 / 18 tools` 与 `state schema`。同版本/降级在 staging 前拒绝。`apply` 不直接拥有 launchd，而是启动下载到的候选 binary 作为独立 process group worker，由该 worker复用现有 `service install` 的 content-addressed generation、health gate、auto rollback 和 evidence；worker spawn 时显式移除 `HERDR_MCP_EXEC_ID`，不放宽 service manager 的 managed-exec 自重启防护。
83. updater 的事务状态故意**不进入 runtime `state.db`**。普通 A/B 更新必须能够回到上一 runtime generation；如果候选先把 runtime DB 从 schema 4 升到 5，旧 generation 会按 future-schema fail-closed 而无法回滚。因此 main `SCHEMA_VERSION` 保持显式 4，release manifest 携带 `state_schema=4`，普通 update 只接受与本机完全相同的 runtime state schema；将来 schema 升级必须走专门的 expand/contract migration handoff。Updater 自己使用独立 rollback control domain `~/.config/herdr-mcp/update/state.db`（schema 1、WAL、0700 dir/0600 DB、future-schema fail-closed），只保存 job/version/target/asset/hash/confined staging path/state/PID/512-byte detail，不保存 release URL/token；partial unique index 保证最多一个 `queued|installing` job。真实无副作用 fake-candidate smoke 已验证：父调用显式设置 `HERDR_MCP_EXEC_ID` 时 detached worker 仍看到 `unset`；只创建 `update/state.db` 而不创建/升级 runtime `state.db`；live worker 阻止第二次 apply；dead worker 可被标记 failed、清理 confined staging 后恢复。由于当前 production binary 已使用 `0.4.0-alpha.1`，rollback-safe updater 的初始候选先提升为 `0.4.0-alpha.2`，避免同版本对应不同 binary，也确保 semver updater 能把 production alpha.1 识别为可升级来源。最新整仓 gate 为 Rust **157/157**、Node **323/323**、Edge **212/212**、双语站点 **21 篇/语言**，extension smoke 全通过。
84. release portability 与真实五平台 supply smoke 已完成。Ubuntu PR CI 曾暴露 macOS-only updater/native-host helper 在 Linux 上因 `cfg` 边界过宽触发 `clippy -D warnings`；`970149e fix: gate macOS lifecycle code in Rust CI` 将 lifecycle imports/helpers 收到 `target_os = "macos"`，随后 CI `32861936286` 的 Rust/test 双 job 全绿。`rust-release.yml` 首次 `workflow_dispatch` 又暴露 release verify 未启动 pinned headless Herdr，导致 root integration tests 找不到 `herdr api schema`；`179b1cd fix: align Rust release verify with CI` 复用普通 CI 的 runtime lifecycle。修复后非发布 run `32862731754` 完整通过 verify、macOS arm64/x64、Linux arm64/x64、Windows x64、manifest，publish 按设计 skipped；代理仅用于开发机额外下载验证，产出的 `aarch64-apple-darwin` binary 在本机实际执行为 `0.4.0-alpha.2 / epoch 2 / 18 tools / state schema 4`。生产下载源仍只有 GitHub Release。
85. `59dbdad ci: attest Rust GitHub release assets` 将 tag 发布链改为 `manifest → attest → publish`：tag-only `attest` 使用 `actions/attest` v4.2.2 的 immutable commit `1e69f48acb82d1966a394da916b4c1698aa569d6`，权限最小化为 `contents: read + id-token/attestations/artifact-metadata: write`，一次覆盖五个 release binary 与 `release-manifest.json`；普通 `workflow_dispatch` 不生成 provenance，也不 publish。首次 tag `v0.4.0-alpha.2` 在 run `32865241048` 的最前置 Rust verify 中因 `exec_sessions::session_captures_output_and_exit` 偶发 1 秒未观察到后台 monitor thread 而 fail-closed，build/manifest/attest/publish 全部 skipped，**没有创建 GitHub Release 或 attestation**。该 tag 已公开后不再移动。第一层修复让 `herdr_exec_read` 在读前 opportunistic `try_wait()`，使 session completion 不再完全依赖 25ms monitor thread 调度，并把 close/persist transition 约束为只发生一次；新增“无 monitor thread 仍可由 read reap”的确定性回归。PR #5 的第一轮 Ubuntu CI 随后证明两个短 session 测试仍能同时超过 1 秒，说明剩余根因是 child 本身尚未退出：Unix `shell_command` 当时先启动 marker shell，再由它启动第二层 `-lc` login shell。启动路径因此收敛为单层 `arg0(process_marker) + shell -lc command`；fencing 仍由 argv marker 与 `HERDR_MCP_EXEC_ID` 双重保证，PID/process-group/exit-code 语义保持不变，同时测试的失败上限放宽到 5 秒以容纳 hosted runner 调度，而不是把 1 秒启动延迟误判为执行错误。单层 shell 后原短 session case再次连跑 **100/100**，完整 Rust **158/158**。下一发布版本因此提升为 `0.4.0-alpha.3`。

86. `v0.4.0-alpha.3` 已完成第一轮真实 GitHub-only provenance 发布闭环。tag-triggered Rust Release run `32866767219` 生成五个平台 binary、`release-manifest.json` 与 GitHub/Sigstore attestation；原 publish 阶段失败时 immutable tag 未移动。`a094f7b fix: name GitHub repo in release publish` 补齐显式 repository identity，随后 `c48f590 ci: recover attested GitHub releases` 增加受约束 recovery workflow：只接受既有 immutable tag 与对应 tag-push source run，要求 source SHA/tag/workflow/verify job 精确匹配，重新校验 manifest contract、size/SHA-256 和完整 bundle，再对每个 asset 执行 `gh attestation verify` 并固定 signer workflow/source ref，最后才允许发布。恢复后 GitHub Release `v0.4.0-alpha.3` 含五个 binary + manifest；独立本机 verifier 也已验证 `release-manifest.json` 的 Sigstore certificate、SLSA provenance、workflow/ref/source commit 与 Rekor transparency-log evidence。
87. `87c75de feat(updater): verify GitHub release provenance`（PR #8，merge `44db040`）已把 release trust chain 接入 Rust updater，而不再只依赖 manifest 自声明或 GitHub 下载地址。新增 `release_trust.rs` 固定 `whshang/herdr-mcp`、`rust-release.yml`、GitHub OIDC issuer、SLSA provenance type 与 Sigstore bundle media type；updater 通过 GitHub attestation API 获取 bundle，并使用 production trust root 验证 signature/certificate/SCT/transparency log、DSSE subject digest、workflow/repository/ref/source revision/builder identity。缺失 attestation、checksum/subject mismatch、错误 repo/workflow/ref/revision、篡改 payload 或无 transparency-log evidence 均 fail closed。合并后的 `workflow_dispatch` run `32873516681` 再次通过 verify、五平台 build 与 manifest；P0 release-trust 的 Grok 独立复验结果为 PASS。
88. release trust 的**代码与供应链证据**已经完成，但 production updater bootstrap 尚未完成：当前 active production binary 仍是 `0.4.0-alpha.1`；`v0.4.0-alpha.3` immutable tag 指向 `ab9eebb`，早于 PR #8，因此不能移动/复用该 tag 来伪装成含 provenance verifier 的 binary。源码下一可信发布版本统一提升为 `0.4.0-alpha.4`，它应成为首个内置 PR #8 verifier 的 bootstrap release。由于 production alpha.1 本身没有这套 verifier，首次切到 alpha.4 必须作为一次受控 bootstrap，由独立 terminal/process 使用既有 service-manager generation/health/rollback 边界完成；待 alpha.4 成为 active runtime 后，真正的 updater production UAT 必须从 alpha.4 升级到**严格更高版本**的候选 artifact。`production_ready` 在 updater UAT、Native Host rollback、Rust relay/link 和 Node cleanup 全部通过前继续保持 `false`。
89. `494f253 ci: narrow default Rust release targets`（PR #9，merge `06270be`）在 alpha.4 bootstrap 前独立收敛了默认发布面：正式 GitHub Release 只要求 `aarch64-apple-darwin` 与 `x86_64-pc-windows-msvc`，macOS Intel 与 Linux arm64/x64 暂缓到出现明确分发需求时再恢复。该变更不是 portability workaround：PR #8 合并后的五平台 rehearsal run `32873516681` 已全绿；PR #9 自身 targeted release tests **11/11**、Node **328/328**、Edge **212/212**、双语站点 **21 篇/语言**、extension smoke 与独立 Grok audit 均通过。当前仍有一个 alpha.4 发布前必须先修正的契约缺口：`rust-release-recover.yml` 的 recovery verification 仍硬编码“恰好五个平台 asset / Release 共六个 assets”，尚未与 PR #9 的两平台 `RUST_RELEASE_TARGETS` 对齐；正常 tag path 与 manifest 已是两平台，但 recovery path 在新的 bundle 上会 fail closed。应先让 normal release 与 recovery 共用同一 authoritative target set/asset-count contract，再创建 alpha.4 immutable tag。
90. alpha.4 发布前的 recovery 契约已经收口：PR #11（`b54b2ea fix(release): share Rust target contract with recovery`，merge `cbf5339`）把正常 release matrix、manifest builder 与 recovery 都收敛到 `.github/rust-release-targets.json`；后续 PR #13（main `07bc355`）补齐旧 provenance bundle/tag 的受约束 recovery 兼容，但不放宽新 schema-v2 release 的 identity / attestation gate。默认 supply 继续只发布 macOS ARM64 + Windows x64，Linux 与 macOS Intel 不再阻塞当前 alpha 发布。
91. `v0.4.0-alpha.4` 已成为首个真实内置 PR #8 provenance verifier 的可信 release。tag release run `32881579462` 完整通过 `targets → verify → macOS ARM64 → Windows x64 → manifest → attest → publish`；Release 为 non-draft prerelease，恰好包含两个 binary 与 `release-manifest.json`。独立本机校验确认 manifest schema v2、source commit/repository/workflow/tag/target identity 与 asset SHA 一致，并对 manifest、macOS ARM64 binary、Windows x64 binary 分别执行 `gh attestation verify`，固定 GitHub OIDC issuer、repository、signer workflow、tag ref 与 source commit，三者全部通过。
92. 第一轮受控 production bootstrap `alpha.1 → alpha.4` **没有激活 alpha.4，并暴露了 launchd lifecycle 的真实恢复缺口**。candidate generation `rust-0cd5af7340ca0a64` 只停在 `staged`；`runtime/current` 与 runtime ledger 仍指向 active `rust-40909ae4ef9004c6 / 0.4.0-alpha.1`。失败发生在 `launchctl bootout` 已被 launchd 接受、但旧 label 在 2 秒预算内尚未消失时；activation 因 `remained loaded for 2000ms after bootout` 返回错误，install-time rollback 又对同一仍在异步退出的 label 重复 bootout 并再次超时。数秒后旧 service 最终被卸载，却没有重新 bootstrap，因此 8772 一度不可达。随后只执行 `service start` 恢复既有 alpha.1 plist/current；production alpha.1 重新健康，generation 数据没有出现假激活。
93. `d19eb9f fix(service): recover from asynchronous launchd bootout`（PR #12，merge `e4a0497`）修复上述 race：service manager 显式跟踪已经提交但尚未 settle 的 bootout，正常 quiesce 与 recovery 使用独立 bounded settle，install rollback 与显式 `service rollback` 的失败恢复都禁止对同一 in-flight transition 再发第二次 destructive bootout；candidate 已启动后失败仍可先 quiesce candidate 再恢复 current。独立 Grok 最终验收 PASS，PR CI 与 main push CI `32914242702` 全绿。
94. `347e068 feat(service): guard interrupted runtime mutations`（PR #14，merge `ae98598`）新增开发/生产共用的 **one-shot service mutation guardian**，但没有重新引入常驻 watchdog 或第二 supervisor。`service install/rollback` 在第一条 destructive lifecycle mutation 前必须完成 guardian `armed → watching` handshake；parent/guardian 通过 pipe EOF/POLLHUP 做 death-fence，guardian 继承 `flock` service-mutation lock 保持 single-writer，并把 pre-mutation known-good plist/current/watchdog/rollback identity 持久化到严格 0700/0600 transaction domain。parent 正常 commit 或同步恢复时 guardian 自动退出；parent 在 transaction 中途消失时 guardian 最多恢复一次 known-good，绝不自动重试/激活 candidate。第一轮 Grok 审计发现并修复了 rollback `ready→consuming` 早于 guardian arm、handshake timeout lost-update、缺少真实 exec/FD fault test 三个 blocker；最终 closure audit 与增量 hardening audit 均为 **PASS / MUST-FIX none**。最终 gate 为 Rust unit **176/176** + actual built-binary guardian integration **1/1**、Node **332/332**、Edge **212/212**、双语站点 **21 篇/语言**、extension/package/diff 全通过；PR #14 与 main push CI `32918461327` 全绿。production 仍保持健康 `0.4.0-alpha.1 / epoch2 / 18 tools`，`production_ready=false`。
95. 下一可信 bootstrap release 因此提升为 `0.4.0-alpha.5`。alpha.5 必须同时包含 P0 release trust verifier、PR #12 launchd async lifecycle 修复与 PR #14 one-shot guardian；**不得再重试 alpha.4 production install**。alpha.1 本身没有 provenance verifier/guardian，所以 `alpha.1 → alpha.5` 仍是一次受控、已独立验签 artifact 的 bootstrap；真正的 updater provenance production UAT 要在 alpha.5 成为 active 后，使用严格更高版本（预计 alpha.6）执行。
96. `v0.4.0-alpha.5` 已完成真实 GitHub release trust 闭环。tag 固定在 `a32353d0546ee571d20caaf97ae799dd76ffc86a`；tag run `32920127361` 的 `targets → verify → macOS ARM64 → Windows x64 → manifest → attest → publish` 全部成功。Release 恰好包含两个 binary 与 `release-manifest.json`；本机独立校验确认三项 SHA 与 GitHub digest/manifest 一致，并分别用 `gh attestation verify` 固定 repository、signer workflow、`refs/tags/v0.4.0-alpha.5`、source commit、GitHub OIDC issuer 与 hosted-runner policy，三个 subject 均通过 SLSA/Rekor 验证。
97. 受控 `alpha.1 → alpha.5` production bootstrap 已成功。执行前确认 alpha.1 active/healthy、无 updater/service mutation worker、mutation lock/guardian 均空闲；独立 detached worker 在调用 `service install` 前再次校验 current generation、alpha.5 binary SHA/version/epoch2/18 tools/state schema。cutover 期间远端 8772 短暂不可达但 install 只提交一次，没有重复 mutation。最终 `runtime/current=generations/rust-210293d4dc72fa54`，alpha.5 active、alpha.1 previous，新 rollback `rb-1787709578526-rust-210293d4` 为唯一 ready；guardian `gtx-1787709578539-21583-c3f9d3ce355c` 以 `observed_committed` 结束，mutation lock 无 holder。独立外部验收确认 exact launchd label loaded/running、8772 health 为 alpha.5、真实 `/mcp tools/list` 为 18/18。Grok production bootstrap audit 为 **PASS / MUST-FIX none**；`production_ready` 仍为 `false`。
98. alpha.5 成为 production active 后，第一条真实 `herdr-mcp update check` 暴露了 P1 discovery blocker：旧默认 URL 使用 GitHub `/releases/latest/download/release-manifest.json`，而 GitHub `latest` 不选择 prerelease，因此当前全-alpha 发布线返回 HTTP 404。`aa3dd6f fix(updater): discover prerelease Rust releases`（PR #16，merge `8b69aa2`）改为从固定仓库的 bounded GitHub Releases API 只发现候选 canonical semver tag（包含 prerelease、跳过 draft/无 manifest/非法 tag），随后自行构造 tag-specific manifest URL；API 不成为信任根，manifest/binary 仍必须通过 schema-v2 identity、SHA-256、Sigstore/SLSA provenance。真实无 override `update check` 已返回 alpha.5 `available=false / provenance_verified=true`；PR CI `32922600405` 与 main push CI `32922901416` 全绿，Grok 结论 **PASS / MUST-FIX none**。固定读取前 20 个 releases 目前只构成可用性上限，不削弱 trust boundary。
99. 下一候选版本提升为 `0.4.0-alpha.6`，用于完成第一轮真正的 **P1 updater production UAT**。由于已安装的 immutable alpha.5 binary 本身仍包含 `/releases/latest` discovery bug，`alpha.5 → alpha.6` 的一次性 bootstrap 必须显式传入 alpha.6 的 **tag-specific GitHub manifest URL**；这只绕过 discovery，不绕过 alpha.5 已内置的 manifest schema/repository/workflow/source/tag/Sigstore/SHA/binary verification。alpha.6 激活后，第一条无 override `update check` 必须能够自动发现当前 prerelease 并返回 `available=false / provenance_verified=true`，随后才视为默认 discovery 正式闭环。
100. alpha.6 release-prep 合入后，macOS 本地 release gate 复现了 `exec_sessions::session_captures_output_and_exit` 的剩余竞态：child 已退出并被标记 `running=false` 时，stdout/stderr reader thread 仍可能尚未 drain，导致调用方看到完整 exit status 却暂时缺失尾部输出。`7584a62 fix(exec): drain output before reporting completion`（PR #18，merge `43facf2`）为每个 session 显式跟踪 output reader，并在报告 closed 前执行最多 500ms 的有界 drain；该等待不持有 child lock，不改变 exec fencing / kill / persistence 语义。独立 Grok release-blocker audit 为 **PASS / MUST-FIX none**，PR #18 合并后的 main CI `32925262423` 全绿。
101. alpha.6 tag 前的 macOS 高负载 gate 还暴露了两个 guardian 测试稳定性边界。其一，`flock` 会随 open file description 短暂跨 `fork → exec` 继承，因此 single-writer lock 单测在并行子进程的 CLOEXEC 窗口可能瞬时仍被占用；PR #19 仅在该精确错误上增加最多 1s 的测试侧重试，持续 lock leak 仍 fail。其二，真实 `service __guardian` integration 使用 debug test binary，高负载下 exec-to-main 调度曾超过 6s；production `GUARDIAN_HANDSHAKE_BUDGET` 从 2s 调整为 5s，但仍严格位于任何 destructive service mutation 之前，超时继续先 durable abort，再 kill/reap guardian，绝不放宽 fail-closed 顺序；integration 自身使用 15s 观察窗口，把 debug scheduler latency 与 production 5s safety fence 分离。最终 commit 为 `19aeff5 fix(service): harden guardian startup timing` + `a1d4530 test(service): decouple guardian debug startup budget`（PR #19，merge `f7543b0`）；更新后 GitHub CI run `32926497548` 的 Rust / Node jobs 与 CodeRabbit 均通过，15s integration 在高负载本机另做 **40/40** 连续验证，Grok 最终为 **PASS / MUST-FIX none**。截至这些修复合入，`v0.4.0-alpha.6` 尚未创建；必须以包含 PR #18/#19 的最新 main 重新完成 release rehearsal 后再建立 immutable tag。

下一批开发按以下顺序推进：

1. 从最新 `main` 发布 immutable `v0.4.0-alpha.6`：先完成 version/docs sync、完整 release gate 与 Grok 审计，再做 non-tag `Rust Release` rehearsal，确认 authoritative target contract 仍只生成 macOS ARM64 + Windows x64；tag path 必须完整通过 `verify + targets（并行） → build → manifest → attest → publish`。发布后继续对 manifest 与两个 binary 独立验证 SHA-256、schema-v2 identity 与 GitHub/Sigstore attestation；
2. 用 production alpha.5 执行一次**显式 tag-specific manifest** 的 `alpha.5 → alpha.6` updater success-path UAT：`check → apply queued → detached worker → provenance/SHA verify → stage → service install/one-shot guardian → generation activation → launchd restart/health → updater status/commit`。这次 explicit manifest 只补偿 alpha.5 的 discovery bug；任何 provenance/identity/SHA mismatch 继续 fail closed，8772 cutover 期间不得因控制连接中断重发 apply/install；
3. alpha.6 成为 active 后立即执行无 override `update check`，证明 PR #16 的 prerelease discovery 已进入真实 production binary；随后覆盖坏 checksum、坏 provenance/repo/workflow/ref/source revision、active-job 冲突、interrupted/dead worker recovery、重复 apply/idempotency、service health failure rollback、current/previous generation 正确性。普通 updater 继续强制 same `state_schema=4`；schema migration 另走 expand/contract handoff；
4. 给 production Rust Native Messaging host 增加 first-class A/B 或 `native-host rollback`，并在真实浏览器页面重做 Rust-host-only 的 service-worker restart/reconnect、Project binding/Auto 恢复 UAT；runtime `service rollback` 与 native-host rollback 保持两个独立故障域；

> 已完成 first-class `native-host rollback`（独立故障域，不改共享 SQLite schema=4）。`install` 在覆盖前把当前受管 runtime binary/wrapper/受影响 manifest（或缺失）快照成 immutable evidence（`native/backups/<rollback-id>/`），以 `rollback.json` 记录 READY 回滚（含 activated 指纹：runtime+wrapper+每个 manifest 的 presence/sha256），`install-pending.json` 记录 in-flight 事务。安装前先恢复 pending、校验 managed-or-absent 所有权、拒绝 symlink/foreign/unowned 与 tampered activated 指纹；提交后才原子替换 READY 记录并清除 pending，绝不用旧 READY 覆盖。`native-host rollback` 恢复先前 owned regular-file/absence 且绝不重建 binary，成功后 consume READY（顺序保证：先 unlink READY，再删 backup，避免 READY 指向已删除 evidence）；`status` 只读报告 `rollback_available`/`recovery_required`（含 rollback-pending）。`uninstall` 仅在完全 owned 移除后消费证据。不允许凭据进 evidence/日志。
>
> 崩溃一致性收口：READY/pending 同 id 判定为已提交（只清残留 marker，不误恢复）；`rollback-pending.json` 仅在其 rollback_id 与当前 READY 匹配时才视为 in-flight 拒绝 install/uninstall，READY 缺失或 id 不匹配（consume 后 crash、快照被取代）的 stale marker 会被清理，绝不让后续操作被死状态卡死；restore/pending recovery 在任何写/删之前预校验完整 confined evidence 集（runtime+wrapper+每个受管 manifest），截断/篡改 evidence 直接 fail closed 且保留 READY/backup；`activated_manifests` 传播 hash 错误而非提交 `sha256:null`；新增确定性 failpoint（install mutation 中途失败→事务 abort 恢复原快照；restore 中途失败→同快照 resumable 重试），均带 production no-op 与非同义断言测试。本机 Rust 单测 **222/222** 全绿（含 22 个 native_host_install 用例）；fmt 与 clippy `-D warnings` 干净。
5. 迁移 relay/link，使 Edge → 本机 transport、reconnect/backoff、heartbeat/auth refresh 与 runtime generation fencing 进入 Rust production path；stale generation 必须 fail closed，完成前继续保留现有 Node link 作为生产路径；

> Relay/link Batch 1 已完成纯验证层移植：`src/relay/validation.ts` 的 Relay Protocol v1 raw UTF-8 frame gate、identifier/UTF-16 length 语义、tree/payload budgets、strict unknown-field、correlated `request_id`、runtime/capabilities/resume 与 9 种 message kind 校验已移植到 staged Rust `crates/herdr-mcp/src/relay/validation.rs`。Node 与 Rust 共用 `tests/fixtures/relay-validation-parity.json` 作为 parity oracle；Rust validator **14/14**，完整 Rust **236/236 + guardian 1/1**，Node relay oracle/shared parity **24/24**，Edge/relay contract suite **212/212**，fmt、clippy `-D warnings` 与 diff-check 全绿。本批没有把 Rust validator 接入 live link，没有修改 Edge transport/runtime 行为，也没有删除 Node `src/relay/validation.ts`/`src/link/**`；生产 link 仍保持 Node 路径，后续再分批迁移 protocol/message model、transport/reconnect/heartbeat/auth refresh/generation fencing。
>
> Relay/link Batch 2 已完成 canonical protocol/message model 移植：`src/relay/protocol.ts` 的 protocol version、9 种 message kind、5 种 correlated kind、30 个 validation code、3 种 delivery state、7 个 `hello_ack` failure code、3 个 resume state，以及 9 类 typed wire message 已收敛到 staged Rust `crates/herdr-mcp/src/relay/protocol.rs`。Rust 使用手动 `serde_json::Value` wire 编码而不是直接反序列化未受信 JSON；入站仍先走 Batch 1 validator。`RuntimeContractInfo` 的 required-nullable 字段保持显式 `null`，`OptionalNullable<T>` 区分 absent/null/value，`ToolResult.result` / `ToolError.details` 区分缺失与显式 JSON null；`DeliveryState` 和 validator 的 protocol/message/correlation 常量改由 protocol module 单一来源提供。PI 独立产出的 `tests/fixtures/relay-protocol-shared.json` 经主线修正后作为 Node/Rust 共享 oracle，覆盖 15 个代表性 valid wire shape；Rust protocol **8/8**、validation **14/14**、errors **11/11**，完整 Rust **244/244 + guardian 1/1**，Node relay + 两份 shared fixture **26/26**，Edge/relay contract suite **212/212**，fmt、clippy `-D warnings` 与 diff-check 全绿。本批仍未接入 live Rust link、未修改 `src/link/**` 或 Edge transport/runtime，也未删除 Node `src/relay/protocol.ts`；生产 link 继续保持 Node 路径，下一批再迁移 wire encode/decode adapter 与 transport/link state machine。
> Relay/link Batch 3 已完成 staged wire adapter：新增 `crates/herdr-mcp/src/relay/wire.rs`，把 canonical frame decode → Batch 1 strict validation → typed model、typed model revalidation → compact JSON encode → final UTF-8 frame byte gate，以及 hello/heartbeat/status/tool_result/tool_error/cancel_ack/compact-oversized pure builders 收敛到 Rust。builder 不读取隐藏时钟；需要时间戳的路径由调用者显式传入。PI 独立产出的 `tests/fixtures/relay-adapter-builders.json` 作为 Node/Rust 共享 builder oracle，覆盖 7 个 wire builder case、2 个 inbound translation case 与 compact oversized 稳定字段；特别锁住 Node `result: undefined` 经 JSON wire 投影后省略 `result`，Rust 用 `Option<Value>` 保持 absent/null/value 语义。最终 Rust wire **10/10**、完整 Rust **254/254 + guardian 1/1**、Node relay/adapter **30/30**、Edge/relay contract **212/212**，fmt、clippy `-D warnings` 与 diff-check 全绿。本批仍没有把 Rust wire adapter 接入 socket/reconnect/auth/heartbeat timer/runtime dispatch，没有修改 `src/link/**` 或 Edge production transport；生产 link 继续走 Node。下一批进入 transport/link state machine、reconnect/backoff、heartbeat/auth refresh 与 runtime-generation fencing，stale generation 必须 fail closed 后才允许切流。
> Relay/link Batch 4 已完成 staged reliability kernel：新增 Rust `crates/herdr-mcp/src/link/backoff.rs` 与 `generation_fence.rs`。backoff 精确保持 Node `ExponentialBackoff` 的 defaults、floor/sanitize、full-jitter/cap、next/reset 语义；generation fence 只保存 link-local request→serving-generation ownership，不复制或取代 `state_store/runtime_generations` 的 durable service ledger。activation 后旧 generation 的 in-flight request 继续由旧 owner drain，新请求绑定新 active；cancel 按 owner 路由；duplicate request-id 在 manager 边界 fail closed；completion 必须声明与 ownership lease 完全相同的 serving generation，mismatch 不消费 lease、不丢恢复证据，而正确的旧 owner completion 即使 generation 已 draining/standby 仍允许完成。PI 独立产出的 `tests/fixtures/link-reliability-batch4.json` 经主线纠正 stale 语义后成为 Node/Rust shared oracle：3 个 sanitization、3 个 peek、1 个 sequence、4 个 Node generation parity 场景和 2 个 Rust safety-strengthening 场景。最终 Rust backoff **6/6**、generation fence **8/8**、完整 Rust **268/268 + guardian 1/1**、Node backoff/runtime-generation/shared **18/18**、Edge/relay contract **212/212**，fmt、clippy `-D warnings` 与 diff-check 全绿；Grok 最终审计 `PASS`，无 MUST-FIX/非阻塞项。本批无 socket/HTTP/token/timer/launchd/runtime install I/O，也未修改 `src/link/**` 或 Edge production transport；生产 link 继续走 Node。下一批迁移 reconnect/link phase/heartbeat scheduling/auth refusal policy，再把 validated wire + reliability kernel 组合进 staged Rust transport；未完成真实 transport 与 runtime-generation integration 前不得切流。
>
> Relay/link Batch 5 与 6A-6D 已在 main 落地：lifecycle/policy/heartbeat、transport reactor、真实 WebSocket driver、runtime runner core，以及非阻塞 fairness 的 outer I/O loop。这些层仍是 staged library，CLI/daemon/launchd/`runtime/current` 均未接入；生产 Link 继续走 Node。
>
> Relay/link Batch 6E 已完成 staged `RuntimeGenerationManager`：新增 `crates/herdr-mcp/src/link/runtime_generation.rs`，把 Batch 4 的 `GenerationFence` 与 Batch 6C 的 `LocalMcpTransport` 组合成 Node `src/link/runtime-generation.ts` 对等的多 generation 本机 MCP 路由。注册路径用真实 loopback `tools/list` + `compute_contract_hash` 校验 contract；activation 后旧 generation 的 in-flight 请求继续在原 endpoint drain，新请求走新 active；observation health 失败会把 active pointer 滚回。Rust 加强一项 Node 检查：`expected_runtime_version` 对照 health probe 发现的真实版本，而不是 transport 配置回声。本批仍不包含 `runtime-control.json` 轮询、CLI `link run`、daemon、launchd 或生产切流。完整 Rust **351/351 + guardian 1/1**，fmt、clippy `-D warnings` 与 diff-check 全绿。
>
> Relay/link Batch 6F 已完成 staged `RuntimeControlLoop`：新增 `crates/herdr-mcp/src/link/runtime_control.rs`。完整 Rust **354/354 + guardian 1/1**。candidate-only Link 组装仍 staged；完成前生产 Link 继续走 Node。当前产品性能主线是 Result Optimization，不是 Link 切流。
6. 只有在 18-tool parity、transport、state store、supervisor/Native Messaging、trusted updater、Native Host rollback、Rust link 均通过生产回归后，删除本地 Node MCP runtime/旧 lifecycle scripts，并把 `~/.local/bin/herdr-mcp` 从 repository-linked compatibility wrapper 迁到 installed Rust runtime；
7. 完成 production readiness gate 并更新 `production_ready` 后，再进入 Reliability Kernel → Continuity 2.0 → Work Context & Evidence → Product Completion，保持 Web Chat 是 planner，herdr-mcp 是控制面。

## Rust 完成后的产品演进 Roadmap

Rust 原生化完成后，重点继续打磨 Web AI 到本机开发现场的完整闭环。保持 herdr-mcp 是控制面，不扩展成新的 Agent runtime，也不强制绑定 Claude、Codex、Pi、OpenCode、Grok、Droid 等具体执行器。

最终闭环保持两条方向同时成立：

```text
下行：Web AI → MCP/OAuth → Edge → link → Rust runtime → files/Git/exec/Herdr
上行：Herdr/runtime event → Native Messaging → browser → 当前 Web conversation
```

核心原则：

- Web Chat 继续负责理解目标、讨论方案、任务取舍和验收；
- herdr-mcp 负责提供真实本机状态、确定性工具、可靠传输和恢复证据；
- Herdr 负责持久工作现场，workspace/pane/Agent/runtime state 以 live state 为准；
- 本地 Agent 是可替换 worker，用于并行调查、边界明确的实现和第二视角审查；
- 小任务、调研任务、讨论任务保持轻量路径，不要求先创建 task/workflow；
- 复杂开发可以携带 work context，但它只是恢复与观察元数据，不接管 Web Chat 的任务管理；
- public MCP contract 继续保持小而稳定，新增能力优先进入内部 runtime/state，而不是增加模型每轮 schema 负担。

### Shared Local State Store：统一 Rust 本机持久状态层

目标：在继续迁移 supervisor、Native Messaging、updater、link 与浏览器连续工作之前，建立统一的本机 durable state 层，避免每个模块各自维护 JSON journal、lock/status 文件和恢复逻辑。

默认实现采用嵌入式 SQLite：

```text
production ~/.config/herdr-mcp/state.db
development ~/.config/herdr-mcp-dev/state.db
```

基础要求：

- SQLite 由 Rust runtime 独占管理，不引入外部数据库服务；
- 启用 WAL、foreign keys、busy timeout 与显式 schema migration；
- 所有 mutation/recovery 状态更新优先使用事务，跨对象 cutover 不依赖多个 JSON 文件的先后写入；
- public MCP epoch 2 / 18 tools 不因数据库引入而扩张；数据库属于内部 runtime/state implementation；
- runtime live state、Herdr workspace/pane/agent state 和 Git working tree 仍从真实来源读取，数据库不成为第二套事实源；
- API key、OAuth secret 等敏感凭据进入 OS Keychain/平台安全存储，不写入通用 state.db；
- 大段 stdout/stderr、附件、图片、release binary 和完整 MCP payload 不写入数据库；需要保留的 command output 使用有界 spool file，数据库只保存 identity、offset、hash、状态和 TTL metadata。

首批统一对象：

```text
meta
operations
exec_sessions
runtime_generations
browser_bindings
continuity_transfers
work_contexts
evidence
```

后续 Continuity 2.0 再增加：

```text
conversation_checkpoints
conversation_turns
```

主要消费者：

1. Reliability Kernel：`op_id`、idempotency key、request hash、delivery phase、uncertain/reconcile result；
2. `herdr_exec*`：session identity、PID/process group、start/end、exit/signal、restart 后 detached/reaped/closed 状态；完整 command/cwd 默认只存在于当前 runtime 内存与有界即时返回中，不进入 durable state；
3. Browser Continuity：Project/conversation binding、Auto 状态、continuity chain、handoff ticket、ACK、checkpoint 与有界 raw tail；
4. runtime generation/update：installed/candidate/active/previous generation、activation transaction、checksum/signature、rollback evidence；
5. Work Context 与 Evidence：`work_id`、scope、acceptance、operation relation、Git/test/artifact evidence；
6. diagnostics/link：最近失败阶段、restart/disconnect reason、generation/boot identity、有限历史观测和 TTL 数据。

保留边界：

- `EventCache` 继续保存实时 Herdr snapshot 和短期 cursor history，不把 workspace/pane/agent live state 长期写入数据库；
- Git HEAD、dirty、diff 等实时状态由 Git 提供，数据库只保存特定 operation 的 before/after evidence；
- exec stdout/stderr 默认使用有界文件 spool，session 完成并超过 TTL 后清理；
- diagnostics、operation evidence、failed transfer 和 conversation raw text 都必须有数量/时间/体积上限；
- runtime generation 仍保留极小、可独立恢复的启动指针或 atomic symlink，使 state.db 损坏时当前 generation 仍可启动并进入 doctor/recovery。

Shared Local State Store foundation 已由 `1b59f8e` 建立；`03c1148` 已把 `ExecRegistry` process fencing identity 从临时 JSON journal 迁入 SQLite，`592f0c7` 又把 prompt idempotency 迁入同一 store。后续 supervisor/updater/Native Messaging/continuity 继续复用这套 schema migration、transaction 和权限边界，避免再新增各自独立的 durable JSON 状态格式。

### Phase 7：Reliability Kernel

目标：让有副作用的远程操作具有可追踪生命周期，并能在 transport timeout、runtime restart、Edge reconnect 后回答“动作到底发生了没有”。

建设：

- 稳定 `work_id`：可选；只有复杂/长任务需要，临时操作可以没有；
- 每次 mutation 都有 `op_id`，贯穿 request、delivery、执行和 evidence；
- mutation delivery phase 至少区分 `not_submitted`、`submitted`、`observed`、`settled`、`uncertain`；
- idempotency key 与 `op_id` 关联，重复请求返回已有事实，不能默默再次执行；
- uncertain result recovery 统一走“重新观察事实 → 判断 → 再决定是否执行”；
- operation evidence 记录足够的 hash/id/state，不把大段命令输出或代码复制成第二份日志系统；
- `runtime_generation + boot_id + event_cursor` 共同形成恢复边界。

首批覆盖：

1. `herdr_prompt`：提交证据、Agent state observation、idempotency；
2. `herdr_exec*`：process/session identity、退出状态、cancel evidence；
3. runtime activation：candidate、generation switch、rollback；
4. browser handoff：source/target conversation、binding 与 ACK。

验收门：

- 注入 timeout / disconnect 后 mutation 不出现重复执行；
- runtime 重启后能区分旧 operation 与新 generation；
- 所有 `uncertain` 都给 planner 一个确定的下一步观察动作；
- 不增加新的 task/workflow public tool。

### Phase 8：Continuity 2.0

目标：让长任务跨网页回合、conversation rollover、扩展重启和本机 runtime 重启保持连续。

建设：

- handoff ticket state machine：`prepared → target_created → summary_delivered → target_ack → source_retired`；
- Project/workspace binding 与 Auto 开关由 durable local state 持有，浏览器刷新后可以恢复；
- source conversation 只有在 target ACK 后才完成 retirement，防止“新会话开了但摘要/绑定丢了”；
- browser extension service worker 重启后从 Rust runtime 重新读取 authoritative binding/handoff state；
- progress / settled 基于 event cursor 和 live evidence，不基于页面里最后一句文字；
- stale generation、重复 wake、正在生成、未确认 mutation 等场景 fail closed；
- ChatGPT、z.ai、DeepSeek 等网页只实现薄 site adapter，不扩张成通用网页自动化系统。

#### Sidecar Compactor：接力压缩不再依赖故障中的 Web 模型

当前兼容实现由 source conversation 自己生成 `HERDR_HANDOFF_V1`。Continuity 2.0 将此路径降为 legacy fallback；正常接力由用户已配置的 OpenAI-compatible 小模型作为 Sidecar Compactor 完成，使 source ChatGPT 已达到上下文上限、持续无回复或恢复耗尽时仍可创建新 conversation。

Sidecar LLM 配置复用现有浏览器“小模型判定”连接信息：

```text
base_url
api_key
model
```

逻辑任务拆分为独立 prompt/profile：

```text
judge            # 回合结束后判断是否继续
handoff_compact  # Continuity checkpoint/raw tail -> handoff packet
```

连接配置共用，prompt、输出 schema、timeout/retry 和失败策略独立。后续 UI 可以把“小模型判定”提升为 Sidecar LLM/辅助模型，但不要求新增第二套 provider 凭据。目标架构下 API key 通过 Native Messaging 交给 OS Keychain/平台安全存储，state.db 只保存 secret reference；迁移期间继续兼容当前 extension 本机配置。

核心原则：任何用于恢复 Web conversation 的机制，都不能要求故障中的同一个 Web conversation 再成功完成一次恢复操作。

#### Continuity Journal 与 rolling checkpoint

浏览器在每个已经完成的 user/assistant turn 结束后，把可见正文增量提交给 Rust Local State Store。Rust 保存当前 continuity chain 的有界 journal，不依赖 ChatGPT 长页面始终保留完整 DOM。

逻辑数据：

```text
conversation_turns
  continuity_id
  conversation_id
  turn_id
  role
  text
  token_estimate
  fingerprint
  observed_at

conversation_checkpoints
  continuity_id
  checkpoint_id
  through_turn
  summary
  anchors
  created_at
```

checkpoint 至少覆盖：

```text
objective
completed[]
decisions[]
active[]
pending[]
constraints[]
files[]
branches[]
commits[]
urls[]
task_ids[]
next_actions[]
```

其中 `continuity_id`、workspace/project/conversation identity、Auto 状态、binding、transfer id 等控制信息由 Rust/extension 确定性生成，不交给模型自由改写。路径、commit hash、URL、task id 等 literal anchor 单独提取/保留，减少压缩遗漏。

默认增量策略：

- 正常工作只追加新完成的 raw turn；
- Auto 开时每约 8 个 user turn 或新增约 12k estimated tokens 生成一次 rolling checkpoint；
- 进入 `HANDOFF_PREPARE` 时强制刷新 checkpoint；
- Auto 关时仍记录有界 journal，但不主动调用外部 Sidecar LLM；手动 handoff 时再按 checkpoint + raw tail 压缩；
- Sidecar 调用只处理 `previous checkpoint + new raw tail`，不在每次 handoff 时重新发送完整 80k+ conversation；
- target ACK 前保留 source 所需 raw journal 作为恢复依据；ACK 后保留 compact checkpoint/handoff/anchors，回收旧 source raw body。

建议的有界 retention 初始值：

```text
per-conversation raw cap: 16 MiB
global continuity raw/checkpoint budget: 128 MiB
checkpoint history: latest 3
failed/uncertain transfer TTL: 7 days
```

达到单 conversation raw cap 时必须先生成并验证新 checkpoint，再 prune 最早 raw turns；禁止直接无摘要截断。具体阈值允许后续依据 dogfood metrics 调整。

#### Browser Performance Budget：内存与主线程压力也是 rollover 信号

Continuity 2.0 不只解决模型上下文长度，也要避免长 conversation 把浏览器页面拖入高内存、滚动卡顿和输入延迟。这里区分两类问题：

- **memory pressure**：Web 应用自身 React/DOM/history、旧 conversation tab、扩展缓存的正文；
- **main-thread/render pressure**：MutationObserver、全 DOM 查询、`innerText` 强制布局、长页面 style/layout/paint 与流式更新。

扩展不能安全删除 ChatGPT/z.ai/DeepSeek 自己管理的历史 DOM，也不能依赖主动 GC；因此降低实际页面内存的主要手段是“更早 rollover + retire 后 discard source tab”，降低卡顿的主要手段是“事件增量化 + 限频 + 不扫描整页”。

当前实现迁移时优先消除以下热点：

1. ChatGPT turn watcher 不再在 `document.documentElement` 上以 `childList + subtree + characterData` 监听所有流式字符并立即执行完整 `onTick()`；
2. persistent permission watcher 不再对整个 `document.body` 的广泛 attributes/style 变化触发全页面 permission button 扫描；
3. `lastMessageByRole`、assistant streaming/tool checks、turn count 不在每个 800ms tick/每个 mutation 上重复 `querySelectorAll` 全部历史节点；
4. conversation pressure 不在每回合结束时重新扫描全部 mounted user/assistant 正文，而是复用 Continuity Journal 的当前完成 turn 做增量统计；
5. monitoring/fingerprint 优先读 `textContent` 与稳定 identity，只有确实需要视觉文本语义时才使用可能触发布局的 `innerText`。

目标 watcher 结构：

```text
conversation structural observer
  childList only
  narrow conversation root
  inspect added/removed nodes only
        ↓
cache latest user/assistant element + stable turn identity
        ↓
active generation sampler (bounded, e.g. 500-1000ms)
  read latest cached assistant only
        ↓
turn settled
  append one completed turn to Rust journal
  update context/performance pressure once
```

permission watcher 同样采用候选驱动：全局只观察新增节点；发现可能的 permission card/button 后，只对该候选的 enabled/hidden 属性做短期局部观察。禁止因为任意页面 `style` 变化而重新扫描所有按钮和祖先文本。

后台/隐藏标签页策略：

- `document.hidden` 时暂停高频 DOM sampling、HUD reconciliation 和非关键页面 health polling；
- Herdr live event、binding、operation state 继续由 service worker/Rust runtime 保持；
- wake/handoff 到达隐藏页时只临时恢复完成该动作所需的观察，页面重新 visible 时立即 rehydrate latest-turn cache；
- service worker 与 content script 都只保留有界 metadata/待发送 tail，不在 JS heap 长期复制完整 conversation journal。

增加独立的 `ui_pressure`，不与模型 token pressure 混为一个数。优先使用低成本指标：

```text
visible/mounted conversation turn count
recent long-task count / duration when Long Tasks API is supported
bounded event-loop timer drift fallback
turn-settle latency
DOM-monitor callback rate
optional Chromium memory signal only as advisory, never as correctness input
```

Long Tasks API 仅作为 Chromium 优化信号，不作为跨浏览器硬依赖。指标只保存 rolling window/聚合值，不持久化完整 performance timeline。

rollover 触发改为双通道：

```text
semantic/context pressure ─┐
                           ├─> HANDOFF_PREPARE -> safe-boundary handoff
browser ui pressure ───────┘
```

也就是说，即使模型上下文尚未达到固定 token/message 阈值，只要页面已经持续出现明显主线程压力，也可以提前 prepare；真正自动 cutover 仍必须满足当前 fail-closed 的 workspace quiescent、无 streaming/tool/permission、无人工草稿、无 uncertain mutation 等安全条件。

handoff target ACK 后：

- target conversation 成为 continuity/binding authoritative page；
- source tab 不自动关闭，保留用户可回看能力；
- 当 source tab 已非 active 时，extension service worker 主动请求浏览器 discard 退休 source tab，使其 Web 页面从内存卸载；用户之后点击旧 tab 时由浏览器正常 reload；
- 同一 continuity chain 更早的 retired tabs 也可按 bounded policy discard，不长期保留多份完整 Web runtime；
- discard 失败只记录 diagnostics，不影响已经完成的 binding cutover。

可选的第二阶段渲染优化是对确认稳定的旧 turn container 试验 `content-visibility: auto` / containment，减少屏幕外 subtree 的 layout/paint；它只能降低渲染成本，不能替代 rollover/discard 来释放页面 heap。由于第三方 Web App 可能已有虚拟列表、滚动锚点和自身测量逻辑，此能力必须 site-specific、可关闭、经过真实滚动/搜索/复制/回到旧消息 UAT 后才能默认启用。禁止直接 `remove()`、替换或重建页面拥有的历史消息 DOM。

初始性能验收门：

- assistant 流式输出期间，扩展 callback 数量不随 token/character mutation 线性增长；
- 长 conversation 下 watcher 不在固定短周期扫描全部历史 user/assistant/buttons；
- hidden tab 的扩展 DOM 工作接近静止，但 Herdr binding/continuity state 不丢失；
- rollover ACK 后退休 source tab 可被 discard，重新激活时能够 reload 且不会恢复旧 authoritative binding；
- 连续 rollover 多次后，只保留当前 target Web runtime 常驻，retired tabs 不造成线性浏览器内存增长；
- performance-pressure rollover 与 context-pressure rollover 共用同一安全 cutover，不产生双重 handoff 或重复 mutation。

handoff 生成顺序：

```text
latest valid checkpoint
  + raw tail
  + deterministic binding/work/evidence metadata
        ↓
Sidecar handoff_compact
        ↓
validated HERDR_HANDOFF_V1
        ↓
fresh target conversation
        ↓
seed confirmed / target ACK
        ↓
atomic binding + Auto cutover
        ↓
source retirement + raw prune
```

降级顺序：

1. Sidecar Compactor 成功：使用最新结构化 handoff；
2. Sidecar 超时/HTTP/网络失败：使用最后一个有效 checkpoint + raw tail + deterministic metadata 生成 degraded handoff，仍允许新 conversation 恢复；
3. 旧 conversation 没有 Continuity Journal 且 source Web model 仍能回复：兼容现有 source-model summary；
4. source Web model 已无回复时，不因无法生成新摘要而阻塞已有 checkpoint 的接力。

Sidecar Compactor 只负责历史语义压缩。Git/runtime/Herdr 当前事实仍由 target conversation 在任何 mutation 前重新读取 live state，handoff packet 不作为实时状态证明。

验收门：

- conversation rollover 过程中任一点刷新浏览器都能恢复；
- Auto 开/关、Project binding、workspace binding 在手动/自动接力后按规则继承；
- 同一个 settled event 不会导致重复继续；
- handoff 失败时旧 conversation 仍可继续，不出现双活控制。
- source Web model 已持续无回复时，存在有效 checkpoint 的 conversation 仍能由 Sidecar Compactor 或 degraded checkpoint 完成 handoff；
- browser extension service worker 和 Rust runtime 分别重启后，journal/checkpoint/handoff ticket 可以恢复且不重复迁移 binding；
- checkpoint/prune 后数据库体积受 retention policy 约束，不永久保存所有历史 conversation raw text；
- Sidecar 失败不会丢失最后一个已验证 checkpoint，也不会触发重复 mutation。

### Phase 9：Work Context 与 Evidence

目标：让复杂任务更容易观察、恢复和验收，同时保持“直接工具优先、Agent 按需使用”的灵活工作方式。

轻量 work context：

```text
work_id?              # optional
objective?            # brief human-readable goal
scope?                # repo/path/worktree hints, not a lock
acceptance?           # concise exit criteria
parent_work_id?       # optional delegation relation
```

建设：

- work context 可以附着到 exec session、Agent prompt、browser binding，不要求独立 Task Center；
- Git evidence：before/after head、dirty state、diff summary；
- test/build evidence：command identity、exit code、bounded summary、artifact/hash when useful；
- Agent progress projection：只投影 Herdr 已有 live status/metadata，不建立第二套 Agent registry；
- handoff summary 引用 work/evidence identity，下一 conversation 仍必须重新读取 live state；
- 为 end-to-end dogfood 建立指标：tool calls、result bytes、p50/p95、重复 mutation、handoff 成功率、人工介入次数。

验收门：

- 没有 `work_id` 时临时 `read/grep/git/exec` 仍保持当前轻量体验；
- 有 `work_id` 时可以从 inspect/diagnostics 找到相关 operation 与证据；
- work context 不自动拆任务、不决定 Agent、不替 Web planner 做调度。

### Phase 10：Product Completion

目标：形成用户无需理解 Node/npm/Python、Cloudflare 内部结构或 launchd/systemd 细节的稳定产品。

建设：

- `herdr-mcp install/status/doctor/update/rollback` 形成完整 CLI 生命周期；
- 单二进制正式覆盖 macOS/Linux/Windows，Native Messaging host 与 service manager 同源；
- signed release manifest、checksum/signature verification；
- stable/beta/dev release channel；
- generation A/B：新版本先 candidate health + contract check，再 activate，旧 generation 可快速 rollback；
- relay/link 断线自动恢复，公网 Connector URL 与本机 generation 解耦；
- doctor 输出 Edge/link/runtime/Herdr/Native Messaging/browser binding 的分层诊断；
- 安装流程尽量自动完成本机凭据与浏览器 Native Messaging 配置，不要求用户复制长期 bearer token；
- 发布 gate 包含 Rust、Edge、extension、site、真实 candidate smoke 和 rollback smoke。

验收门：

- 新机器安装后不需要 Node/npm/Python 即可完成本地 runtime + browser continuity；
- 升级失败可以在不修改公网 Connector URL 的情况下恢复上一 generation；
- `doctor` 能区分公网、link、本机 runtime、Herdr daemon、browser bridge 五类故障；
- production 不再包含本地 Node runtime 和旧 lifecycle scripts。

### 实施顺序

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

当前已完成 `18-tool native parity → production transport parity → Shared Local State Store foundation`，实施游标进入 `supervisor / Native Messaging / updater / link`。Shared Local State Store foundation 保持 SQLite、schema migration、transaction/retention 与首批 exec/prompt durable identity 的范围，不提前扩张 Reliability/Continuity 的产品行为。Reliability/Continuity 所需的基础 identity、diagnostics、event cache 可以在 Rust 迁移阶段提前建设，但不能因此修改 epoch 2 / 18 tools 的行为契约。

工具性能作为独立并行 lane 推进，详细计划见本文件 Appendix B。Batch A 只做不改变 epoch 2 / 18-tool visible contract 的内部优化、输出压缩与 `herdr_skill` 编排/Worktree 生命周期规则；当前 Rust link runner 继续独立推进 `crates/herdr-mcp/src/link/**`。只有性能基准证明固定 MCP/model round-trip 仍是主要瓶颈后，才在 Batch B 明确评估 multi-operation tool schema / JSON-RPC batch 与 contract epoch 演进，禁止把 model-visible schema 变化混入普通 Rust 重构。

长任务可观察性作为性能与可靠性并行 lane 纳入本文件 Appendix B。该 lane 通过 Task Journal、phase event 和 checkpoint 提供长 release/CI/deploy/self-upgrade 过程的阶段反馈，不替代 Git/runtime live state，也不改变 epoch 2 / 18 tools contract。

### 暂不进入主线

以下能力保持观察，不进入当前 Roadmap：

- 自建新的 Agent Team runtime；
- 强制引入 ACP 作为内部主协议；
- 复制 Luvus Task/Lease/merge gate 系统；
- 绑定单一 Coding Agent 或要求本机必须安装某个 Agent；
- 第二套 Web Agent 编排系统；
- 为所有操作强制引入 Task/Project/Workflow 对象；
- 通用浏览器自动化平台。

未来只有在真实使用数据证明现有 Herdr + fs/Git/exec + 可替换 worker 无法表达需求时，再通过 adapter/plugin 或新的 contract epoch 评估。


---

---

# Appendix B: Tool Performance Implementation Details (merged from tool-performance-optimization.md)

# herdr-mcp Tool Performance Optimization

状态：实施中
日期：2026-08-27
主线基线：`5fac467`（PR #43 grep/auto-continue hotfix + PR #44 Rust WebSocket driver 已合入）
目标 contract：保持 epoch 2 / 18 tools 不变完成 Batch A；只有 Batch B 明确评估 model-visible schema 演进。

## 1. 为什么单独建任务文档

`docs/herdr-architecture-roadmap.md` 是唯一规划基线；Appendix A 承担 Rust 原生化历史，Appendix B 承担 18 个工具的性能实现细节、基准、ChatGPT 编排策略和 workspace/worktree 生命周期。

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
- [x] 在规划基线中增加本性能 lane 指针与并行边界；
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

方法：baseline 与 candidate 同时连接同一个本机 Herdr socket、操作同一个仓库数据集，分别监听 `127.0.0.1:18871/18872`，每个 spec 交替 baseline/candidate 顺序采样，避免把调用顺序和瞬时机器负载固定偏向一侧。baseline 为生产 `main` merge `69fb04e`；candidate 为 `perf/tool-latency-batch-a-20260827` working tree。

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

## 11. Long Task Progress Observability（Batch C 规划）

目标：让长任务在执行过程中自动产生可恢复的阶段状态，避免依赖模型临时记忆或用户主动询问。

适用场景：

- CI、release、deploy、self-upgrade；
- 长测试和 benchmark；
- 多 worktree / 多 Agent 协作任务。

设计：

```text
task run
  ↓
phase state
  ↓
task events
  ↓
checkpoint/evidence
  ↓
progress report
```

状态阶段：

```text
TASK_CREATED
STATE_VERIFIED
PLAN_LOCKED
IMPLEMENTING
VALIDATING
WAITING_EXTERNAL
RELEASE_READY
DEPLOYING
VERIFYING
COMPLETED
```

利用 Rust Shared Local State Store 扩展 Task Journal，不新增第二套任务系统：

```text
task_runs
  task_id
  objective
  workspace_id
  phase
  status

task_events
  task_id
  event_type
  evidence
  created_at

checkpoints
  task_id
  phase
  commit
  tests
  next_action
```

自动产生 progress event：

- 任务预计超过 3 分钟；
- 工具调用超过 5 次；
- 创建/关闭/reclaim worktree；
- CI 开始或完成；
- 等待外部系统；
- release/deploy/self-upgrade；
- blocker 或 failure。

保持低噪声：单次 read、grep、小 patch、普通 git status 不产生用户级汇报。

`herdr_skill` 在开发阶段增加长任务汇报策略：

```text
announce phase start
report major milestone
report external wait
report blocker immediately
complete with evidence
```

稳定版冻结前精简该规则，只保留运行原则。

实施顺序：

1. checkpoint/event schema；
2. tool lifecycle event emission；
3. `herdr_since` task cursor；
4. progress rendering；
5. handoff 恢复读取 checkpoint。

验收：

- 长任务不会出现无解释静默等待；
- 新 conversation 可从 checkpoint 恢复阶段；
- progress event 不增加短任务噪声；
- checkpoint 不替代 Git/runtime live state。

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

---
