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

下一批开发按以下顺序推进：

1. 迁移 `herdr_exec_start/read/kill`：先建立 Rust 原生 exec-session registry、稳定 session identity、bounded ordered output、process-tree cancel 和 restart 后的 truthful closed/detached 状态；
2. 在同一 registry 上迁移短命令 `herdr_exec`，保持 workspace/project-root 选择、visible utility-pane 语义、busy-agent gate 与 uncertain-delivery 分类；
3. 用真实 exec-session registry 补齐 `herdr_inspect`，替换当前显式 `exec_sessions_ready=false` 占位；
4. 迁移 `herdr_prompt` 与 `herdr_skill`，逐个消除剩余 `native_tool_pending`，其中 prompt 必须保留 idempotency、delivery evidence 和 post-submit observation；
5. 补齐 persistent GET/SSE、stateful session/auth compatibility，使 Rust HTTP transport 通过现有 Connector transport parity tests；
6. 实现 Rust supervisor、service manager、Native Messaging host；
7. 实现 GitHub Release updater、generation A/B、rollback；
8. 迁移 relay/link；
9. Rust 覆盖 18 tools 与 production transport 后删除本地 Node runtime 和旧 lifecycle scripts。

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

验收门：

- conversation rollover 过程中任一点刷新浏览器都能恢复；
- Auto 开/关、Project binding、workspace binding 在手动/自动接力后按规则继承；
- 同一个 settled event 不会导致重复继续；
- handoff 失败时旧 conversation 仍可继续，不出现双活控制。

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
  → supervisor / Native Messaging / updater / link
  → 删除 Node runtime
  → Reliability Kernel
  → Continuity 2.0
  → Work Context & Evidence
  → Product Completion hardening
```

其中 Reliability/Continuity 所需的基础 identity、diagnostics、event cache 可以在 Rust 迁移阶段提前建设，但不能因此修改 epoch 2 / 18 tools 的行为契约。

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
