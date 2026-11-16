AI 生成的设计草案，基于 b8d660a02f5d501f625b89ec5a91aa40414c3f8c（origin/main，含已合入的 #946）。初稿基于 73a599d6，本文所有 文件:行 证据已重新核对到 b8d660a0。

# #768 Phase 2：Core 公共调用路由分阶段设计

## 决策与边界

冻结 Runtime Execution Contract Epoch 5 的 18 工具目录及既有参数、结果语义；显式选择 `herdr-core` 时，不使用 TUI snapshot、socket、agent 或 terminal 作为替代。`hello.methods` 只证明对端宣称有方法，不是调用授权、目标认证或策略等价证据。以每个可独立验收的窄路由逐一解封，缺少安全前提时继续拒绝。写操作切片（4–6）是**有外部前提的设计与验收门槛**，不是宣称当前 Core 已能安全实现。

范围外（不设计、不实现）：写操作目标绑定实现、同 UID 防伪实现、同进程原路径重绑、Windows IPC 实现、终端会话生命周期、#775 Edge。同机直连的 transport 实现属 PR #947 的 S6（见对照节），本文只定义其与 Phase 2 的边界与共用逻辑。

本草案 6 个切片：1–3 为只读路由与进程隔离；4–6 依赖独立安全机制，未满足时保持现有拒绝，不得通过删 fence 冒充完成。每片单独 PR、单独 gate、可回滚。本设计未运行测试或实机验证。

## 基线：代码事实（核对到 b8d660a0）

- 入口链：`crates/herdr-mcp/src/mcp_http.rs:2939 post_mcp` → `mcp.rs handle` → `mcp.rs:1245 tool_call`（`mcp.rs:1172 normalize_tool_call_arguments` 归一化名称/参数）→ `runtime_adapter.rs:1910 public_tool_route`（`runtime_adapter.rs:1932 verify_admitted_backend` 校验启动 pin）→ `runtime_adapter.rs:2037 public_tool_route_for_selection` → `mcp.rs:1301 dispatch_admitted_tool`。`PublicToolRoute::HerdrCore` 携带本请求 admitted client；TUI 分支直接用 `HerdrClient`/本机 registry/snapshot，**不经 `TuiRuntimeAdapter` 分派**（该 trait 目前只抽象能力/状态，`runtime_adapter.rs:466-530` 区段）。
- **实际进程阻断**：`main.rs:443 candidate_startup` 在孤儿清理、TUI client 构建、worker 启动前拒绝非 TUI backend（`main.rs:450`）。`mcp_http.rs` 的 Core 路由测试用测试 state 直连 router（`mcp_http.rs:4063 spawn_core_execution_fixture` 等），不代表生产可启动 Core candidate。
- 18 工具冻结名单：`runtime_meta.rs:10 MIGRATED_TOOLS`、`contract.rs::tool_names`（解析 `contracts/epoch2.json`）：`herdr_methods, herdr_inspect, herdr_skill, herdr_call, herdr_since, herdr_fs_read, herdr_fs_list, herdr_fs_grep, herdr_fs_patch, herdr_fs_image, herdr_git, herdr_exec_start, herdr_exec_read, herdr_exec_kill, herdr_exec, herdr_fs_edit, herdr_fs_write, herdr_prompt`。
- Core 下当前仅 `herdr_methods`（`runtime_adapter.rs:2294 herdr_core_methods`）、`herdr_inspect`（`native_tools.rs:24 inspect_with_selection` → `runtime_adapter.rs:2646 inspect_herdr_core_with_client`）、受限 `herdr_call` 可服务。连接前拒绝表：`runtime_adapter.rs:2162 CORE_POLICY_RESTRICTED_PUBLIC_TOOLS`（`herdr_fs_read/write/edit`、`herdr_exec`）、`herdr_prompt`（`runtime_adapter.rs:2238 herdr_core_prompt_refusal`）；能力检查后拒绝 `runtime_adapter.rs:2168 EXEC_SESSION_UNSUPPORTED_PUBLIC_TOOLS`（三会话工具，`runtime_adapter.rs:2185 herdr_core_exec_session_unsupported`）与 `runtime_adapter.rs:2138 TUI_ROUTED_PUBLIC_TOOLS`（list/grep/image/patch/git/since）。`herdr_skill` 及 TUI 依赖本地方法在 `mcp.rs:1301 dispatch_admitted_tool` 前置拒绝 + `runtime_adapter.rs:2251 herdr_core_local_method_refusal`。
- raw 转发白名单：`runtime_adapter.rs:61 HERDR_CALL_FORWARDABLE_HC_METHODS = [rpc.discover, server.status, templates.list]`；`runtime_adapter.rs:2421 herdr_core_call` 先白名单拒绝，`runtime_adapter.rs:1338 call_allowed_method` 再要求 hello 宣告。客户端存在 `machine_exec`（:795）、`file_read/write/edit`（:972/:1019/:1061）、`terminal_*` 原语，但公共路径不可达——原语存在≠公共授权。
- 能力推导：`runtime_adapter.rs:352 from_herdr_core_methods` 实际只用 `methods`；`features` 被解析/展示但不参与判定（模块头注释比实现宽）。per-tool 闸门 `runtime_adapter.rs:399 check_public_tool`。Core inspect 仅 `server.status` 全局计数（`runtime_adapter.rs HerdrCoreClient::inspect_terminals`），`terminals:[]`、`terminal_details:"unavailable_without_ownership"`，不代表已证明无 terminal。
- #941：`runtime_adapter.rs:1932 verify_admitted_backend` / `:1986 verify_admitted_backend_config` + `mcp_http.rs:439 admitted_backend_gate`（`/health` 503、其余 409，`mcp_http.rs:453 admitted_backend_refusal`）检测 backend/socket 漂移；出站 `runtime_adapter.rs:2732 connect_hc_socket` 在 hello 前 `:2764 verify_hc_peer_uid` 同 UID 校验。这是**出站 Core socket** 检查，不是入站 IPC 身份认证；入站 `extension_ipc.rs` listener 无 peer 凭证检查，`mcp_http.rs:3685 request_authorized` 在 trusted IPC 上免 bearer（仅 0600 socket 屏障）。`mcp_http.rs:2211 push_events_response` 每次 wake 复查、首帧靠入口 gate；`mcp_http.rs:3763 persistent_sse_response` 的已开 GET 流无流内复查——均仍开放。
- #943：`runtime_adapter.rs:690 admit_public_tool` 捕获 hello + peer PID；`:1388 with_negotiated_session_budget` 在每次连接上：admitted PID 比对（:1421-1431，hello 前）→ hello → generation/version/core_id 比对（:1447-1458），漂移分别 `herdr_core_target_changed` / `herdr_core_identity_changed`。`core_name/methods/features` 变化不触发 identity 拒绝。请求内一致性，不是同 UID 认证。
- **#946（已合入 b8d660a0）**：`mcp.rs:1211 normalize_herdr_call_params` 在**任何 backend 准入/connect 之前**把 `herdr_call.params` 的对象与 JSON 字符串两种形式按同一函数归一化一次；`mcp.rs:1238 herdr_call_is_server_status` + `tool_call` 内提取 `expected_core_target`，非 `server.status` 方法直接在准入前拒绝（`runtime_adapter.rs:2398 herdr_core_expected_target_unsupported`，与准入后同一信封）。合法目标经 `runtime_adapter.rs:2016 hc_expected_unix_target` 严格解析为 `(pid, dev, ino)` 传入 `public_tool_route(..., expected_target)`（`mcp.rs:1245 tool_call` → `runtime_adapter.rs:1910`），格式非法 → `invalid_params`（`runtime_adapter.rs:2070-2081` 区段）；client 记入 `pre_hello_expected_target`（`runtime_adapter.rs:610`），`with_negotiated_session_budget` 在 **connect 前**用 `:671 listener_node_fingerprint`（symlink_metadata 的 dev/ino）比对（:1396-1409），**connect 后、admission hello 前**用 OS peer PID 比对（:1411-1420），不符均 `herdr_core_target_changed`。hello 后仍比对 core_id（#943 路径）。测试：`mcp_http.rs:5094 public_core_restart_rejects_stale_target`（#945，真实两个 PID 子进程复用同 socket 路径）、`mcp_http.rs:5323 public_core_conditional_target_shape_parity`（#946，对象/字符串形式同判定）、`mcp_http.rs:4811 herdr_core_public_routes_fail_closed_without_delivery`（#944）。
- 目标参考仍是 advisory：`runtime_adapter.rs:641 advisory_target_ref` = `unix:{peer_pid}:{dev}:{ino}:{core_id}`，`listener_node_fingerprint` 注释明确 pathname TOCTOU 与 inode 复用仍存在（:663-670 区段）；`herdr_core_call` 在 hello 后仍做 `admitted_target_ref` 等值比较（`runtime_adapter.rs:2436-2470` 区段）并把 `expected_core_target` 从 params 移除后才转发。
- #938：`text_transfer.rs:1008 safe_home_path`、`:216 write_fenced`、`:167 CommitFence::check_target`（Unix pinned-parent publish 内 rename 前复查）；Core 下 `herdr_mcp.text.write` 被 `herdr_core_local_method_refusal` 拒绝。TUI 普通文件写只有准入层 `mcp.rs:3020 check_mutating_tool_root_lease`（提交时复查仅 text.write 有，`mcp.rs:3151 text_write_commit_fence`）；lease 判定在 `state_store/mutation_leases.rs:849 check_root_mutation_lease_for_path`。
- delivery 语义：`runtime_adapter.rs:2579 HC_PREFLIGHT_ERROR_CODES` + `:2594 hc_delivery_state`（非 mutating 或 preflight 码 → `not_delivered`，否则 `unknown`）；`:2604 herdr_core_error_json` 统一 `fallback_allowed:false, retryable:false`。注意：只读传输失败当前一律 `not_delivered`，不能解释为“RPC 从未发出”（见开放问题 5）。

## 路由与失败约定

顺序：冻结目录/参数验证 → admitted backend/transport fence → 工具/方法安全策略 → 同一 admitted client 的协商与能力检查 → 目标与操作特定授权 → 原语 → 保持公共结果语义。workstation-local `herdr_call` 方法按现有权限运行，TUI 依赖者保持拒绝；raw 白名单不随 `hello.methods` 扩大。

| 情况 | 语义 |
|---|---|
| backend/socket 漂移 | `invalid_runtime_backend`，`not_delivered`，`fallback_allowed:false`；transport gate `/health` 503、其余 409 |
| 工具/策略缺失 | `runtime_capability_unsupported` + 具体 `required_capability`，`not_delivered`，不回退；缺 socket 为 `herdr_core_socket_unconfigured` |
| raw 方法不允许 | `herdr_core_method_prohibited`；允许但未宣告 → `runtime_capability_unsupported`；不交给 Core |
| 目标/hello 改变 | `herdr_core_target_changed`（#946 起可发生在 connect 前、connect 后 hello 前、或 hello 后）/ `herdr_core_identity_changed`；业务操作未发，`not_delivered`，不回退 |
| peer UID/PID 未验证或不匹配 | `herdr_core_peer_uid_unverified/mismatch`、`herdr_core_peer_pid_unverified`；操作前拒绝，不回退 |
| 参数不合法 | `invalid_params` 等现有码；#946 起 `expected_core_target` 相关拒绝在准入前即返回且对象/字符串形式一致 |
| 变更 RPC 已发出后超时/断连/结果不明 | `delivery_state:"unknown"`，`retryable:false`，`fallback_allowed:false`；不自动重试，不降级为 `not_delivered`；仅有发送前证据才可报 `not_delivered` |

## 切片 1：冻结契约与窄路由基线（含 #946 回归）

**范围**：`mcp.rs:1245 tool_call`、`:1301 dispatch_admitted_tool`，`runtime_adapter.rs:2037 public_tool_route_for_selection`、`:2294 herdr_core_methods`、`:2421 herdr_core_call`，`native_tools.rs:24 inspect_with_selection`。保持三条只读 Core 路径与本地方法分类；投影 declared capability / forwardable / policy-blocked 的区别；`mcp.rs:2239 workstation_preflight_call` 透传同一 admitted client，identity 错误不得包装成成功。不新增工具或 raw 方法。

**安全前提**：#936 双层拒绝、#941 pin/出站 UID、#938 边界、#943 hello/PID、#944/#945/#946 目标规则全部保留。target ref 仅 advisory。

**失败语义**：上表现有码；预检拒绝 `not_delivered`、`fallback_allowed:false`；TUI 结果不变。

**测试**：单元（白名单/宣告正交、方法名 trim、local 拒绝、失败投影）；公共 `tools/call` 经真实 router + Core fixture（方法/inspect/三允许方法成功，`file.read`/`machine.exec` 拒绝且 Core 日志无操作），TUI 对照；复用 #943–#946 既有测试（含 `mcp_http.rs:5323` 形状 parity）。Linux Rust CI + 契约测试；Windows 现有 build lane 上补平台无关分类单测与 Core IPC 明确不可用断言。router 测试不声称生产 Core candidate 可启动。

**不做**：文件/exec/prompt/session 解封；扩 raw 白名单；宣称 PID/UID 是认证。

## 切片 2：Core candidate 最小进程隔离与传输边界

**范围**：`main.rs:443 candidate_startup`、`mcp_http.rs:253 serve_candidate`、`:372 candidate_router`、`:439 admitted_backend_gate` 及 TUI client/cache/worker 构造点。设计独立 Core 启动分支：仅在证明不构建/不启动/不暴露 TUI 依赖状态与 worker 后移除启动拒绝；只暴露切片 1 允许的行为，不用空 snapshot 冒充 TUI 状态。逐入口审查 HTTP/IPC 路由，TUI 依赖入口拒绝而非只查 `tools/call`。`GET /mcp` 持久流与 `/push/events` 首帧建立可验证 admission 边界，证明不了就在 Core 模式拒绝对应入口。

**安全前提**：切片 1；#941 gate 不降级；#936 全操作拒绝保持；入站 0600 socket ≠ 出站 peer UID ≠ 同 UID 防伪，安全声明限定威胁模型。

**失败语义**：隔离/admission 未满足 → 启动失败或明确拒绝，不启动 TUI 替代；漂移 `invalid_runtime_backend`、`not_delivered`、不回退；已开流漂移后终止不得表示成功。

**测试**：进程级真实 candidate + 隔离 Core fixture，实际 HTTP `tools/list`/`tools/call` 验证只读链路；TUI socket/worker 哨兵断言零连接零启动；改 backend/socket 后 POST、health、已开 GET、push 首帧/后续帧全部 fail closed。**同机直连三入口一致性**（见同机直连节）并入本片。Linux CI 进程测试；macOS Unix peer/进程 smoke；Windows CI 编译 + 不支持 Core IPC 的拒绝测试（编译≠支持）。

**不做**：Windows IPC、Edge、同 UID 防伪、终端生命周期；隔离未证明前不提前去掉 `candidate_startup` fence。

## 切片 3：只读 Core 方法与目标条件收口（#946 之后）

**范围**：`runtime_adapter.rs:690 admit_public_tool`、`:1388 with_negotiated_session_budget`、`:641 advisory_target_ref`、`:671 listener_node_fingerprint`、`:2421 herdr_core_call`、`:2294 herdr_core_methods`。#946 已把条件目标的格式校验和 PID/dev/ino 比对提前到 admission hello 前——本片**不重复实现**，只收口剩余歧义：(a) 文档/结果中 advisory target、capability、全局计数不得表述为授权/认证/terminal 所有权；(b) 明确 #946 未覆盖面：无条件请求（不带 `expected_core_target`）的首次 admission 仍是先 hello 后取 PID，`listener_node_fingerprint` 仍是 pathname stat（TOCTOU/inode 复用注释在案）；(c) 新的跨请求只读条件若扩展，逐方法定义目标、数据暴露与合同兼容。不扩白名单。

**安全前提**：#941、#943–#946 已合入行为保持；不声称 #946 解决同 UID 冒充、PID 复用或同进程原路径重绑。

**失败语义**：无效条件 `invalid_params`（#946 起准入前）、旧目标 `herdr_core_target_changed`、hello 漂移 `herdr_core_identity_changed`；业务操作前拒绝 `not_delivered`、不回退。“未送达”指业务操作，握手可能已发送。

**测试**：单元（target 缺失/格式/匹配/移除不转发、对象与字符串 parity 回归）；进程级重启同路径旧 ref 拒绝且无业务 RPC、新 ref 成功；connect 前指纹漂移与 connect 后 PID 漂移分别在 hello 前拒绝（复用/扩展 `mcp_http.rs:5094`）；`herdr_methods`/`herdr_inspect`/`workstation.preflight` 共用同一请求 client。Linux CI Unix fixture；macOS peer PID；Windows `herdr_core_windows_ipc_unverified` 拒绝，不伪造支持。

**不做**：写操作、同 UID 防伪实现、Core events 订阅、同进程重绑。

## 切片 4：只读文件路由（条件解封）

**范围**：仅 `herdr_fs_read`：`mcp.rs:1301 dispatch_admitted_tool` 对应 arm、`runtime_adapter.rs:2037` 拒绝表、`:972 HerdrCoreClient::file_read`；对照 `fs_tools.rs read`、`fs_security.rs validate_existing_validated`。前提兑现后提供等价读取与公共结果/大小/编码/错误行为；只移此工具的 policy fence，`herdr_call` raw `file.read` 仍禁止。

**安全前提**：切片 1–3；可信 managed-root 来源、访问/秘密路径保护、**授权检查与 Core 实际打开对象不可分离或等价安全的机制**（覆盖 symlink、祖先替换、Core cwd 差异）。现有 `hc_secret_like_path_guard` 路径检查与 test-only `herdr_core_absolute_file_target` 明确关不掉检查-打开竞态；`expected_core_target`/#946 目标校验也不授权文件。机制不存在则保持 `public_tool_policy_parity` 拒绝，本片不解封。

**失败语义**：策略/capability 缺失 → `runtime_capability_unsupported`（`public_tool_policy_parity` 或对应能力），`not_delivered`、不回退；路径拒绝沿用公共错误；读取响应丢失不得伪称 RPC 未发；不得退回本地/TUI 读取。

**测试**：单元对照 TUI 合同与路径/secret 拒绝；进程级授权路径成功、根外/secret/symlink 交换拒绝且未授权对象未被读取；同步屏障测竞态（不用 sleep）。Linux CI；macOS 同语义；Windows 保持拒绝 + 编译。

**不做**：写目标绑定实现、list/grep/image/patch、以 HOME text transfer 冒充本路由安全模型。

## 切片 5：文件写与编辑（各自独立解封）

**范围**：先 `herdr_fs_write` 再 `herdr_fs_edit`，各自独立验收；`mcp.rs:1301` 对应 arm、`:3020 check_mutating_tool_root_lease`、`runtime_adapter.rs:1019 file_write` / `:1061 file_edit`；对照 `fs_mutation.rs write/edit`、`mutation.rs check_with_topology`、`state_store/mutation_leases.rs:849`。保留只读模式、managed root、secret、busy-agent、覆盖/dirty 确认、编辑唯一匹配与结果合同；raw `file.write/edit` 与 `herdr_mcp.text.write` 仍禁止绕过。

**安全前提**：切片 4 保障 + 独立完成并验证的写目标绑定、授权与实际提交耦合、**提交时** root lease 复查，且 Core 端能阻止检查后路径替换与过期租约写入。普通文件路由现有准入 lease 检查不是提交时保障；#938 pinned dirfd/commit fence 不约束另一个 Core 进程。协议给不了保证就继续拒绝。

**失败语义**：前置拒绝用现有 `readonly_mode`/路径/租约错误或 `runtime_capability_unsupported`，`not_delivered`、不回退；发出后断线/超时/提交不明 → `unknown`，不自动重试不回退；成功须有符合公共合同的提交结果。

**测试**：分别单元合同 + 进程级公共入口：成功、只读、他人 holder/epoch、租约提交前失效、路径/symlink 替换、overwrite/dirty/唯一匹配拒绝（同步屏障控制竞争，拒绝不改目标）；模拟写已提交响应丢失 → unknown 且无重放。Linux CI 两工具分别跑；macOS 真实文件系统验证；Windows 仅拒绝 + 编译，等价机制另立实现与原生测试。

**不做**：外部写目标绑定机制实现、patch/git/text.write 解封、凭 capabilities 解封。

## 切片 6：一次性执行（条件解封）

**范围**：仅 `herdr_exec`：`mcp.rs:1301` 对应 arm、`runtime_adapter.rs:795 machine_exec`、对照 `utility_exec.rs:468 run_durable` 的公共语义。逐项确认 command/argv、cwd、超时、输出/截断、退出状态、持久结果与未知结果语义；不能等价表达的参数明确拒绝，不静默忽略。raw `machine.*` 仍禁止。

**安全前提**：切片 2–3 + 独立完成的执行目标绑定、cwd/managed-root 策略、只读模式、busy-agent 与 root lease 的操作时约束；证明 Core 执行进程与授权目标一致。`hc_exec_invocation` 目前 `#[cfg(test)]`，`machine_exec` 原语存在不证明 `run_durable` 合同或授权等价。证明不了保持拒绝。

**失败语义**：策略未满足 → `runtime_capability_unsupported`/`public_tool_policy_parity`；参数不支持 → `invalid_params` 或既有能力错误，业务未发 `not_delivered`；发出后超时/断连 → `unknown`，禁止自动重试与 TUI fallback；退出状态未知不得报成功。

**测试**：单元参数/结果合同对照；进程级允许命令/cwd/结果、越界/只读/租约冲突拒绝；持久外部计数器验证响应丢失时命令不重放。Linux CI + macOS smoke；Windows 编译 + 明确拒绝。

**不做**：`herdr_exec_start/read/kill`、`terminal.run/read/kill/wait`、`herdr_prompt`、`terminal.send`；会话生命周期、foreground/attach、prompt 提交确认。

## 保持拒绝的其余工具

`herdr_since`（缺 Core 重启/事件 cursor/丢事件模型）、`herdr_fs_list/grep/image/patch`、`herdr_git`（缺等价路由与策略）、`herdr_skill`（TUI 状态）、`herdr_exec_start/read/kill`（缺 runtime-owned session/所有权/游标/清理）、`herdr_prompt`（缺目标绑定/foreground/attach/提交确认/task 幂等合同）。均 `runtime_capability_unsupported`、`fallback_allowed:false`；具体 capability 以 `public_tool_route_for_selection` 现有检查顺序为准。

## 同机直连：本机目标绕开 Edge relay 与 Phase 2 的关系

**现状（代码确认，b8d660a0）**：Rust candidate 内没有按 `device` 参数选本机/远端的执行分支——`mcp.rs:1172 normalize_tool_call_arguments` 仅移除 null `device`，`dispatch_admitted_tool` 对 `herdr_call` 允许 `device` 顶层参数但不据此改路由；所有工具一律走本进程 admitted backend。Rust relay 帧（`relay/wire.rs`、`protocol.rs`）无 device 字段；按设备寻址与本机旁路判定在 Edge/Link TS 侧（本 worktree 无该源码，PR #947 P1 已核实 Edge 恒转发设备 DO、`device` 在本地被忽略）。本仓库可见的承接端：`link/local_mcp.rs:955 dispatch_routed` 按可信上下文选 `:711 dispatch_http`（loopback `POST /mcp`）或 `:733 dispatch_trusted_ipc`（`extension.sock`）；`link/runner.rs LinkRunnerCore` 把 Edge 帧驱动到该本地传输。`link/ladder.rs:363 build_ladder_routes` 的“直连优先、relay 兜底”是本机→Edge 的上行 WebSocket 阶梯，不是按目标 device 的本机旁路，勿混淆。

**结论：共用同一后端选择点，不新增第二套逻辑**。同机直连只改变请求如何到达本机 runtime；进入 `post_mcp → tool_call` 后与任何本地调用同路：`public_tool_route` 用进程启动 pin 的 `runtime.backend` 决定 TUI/Core，`device` 不参与。Phase 2 的 18-tool 冻结、#936 fence、#941 gate、#943–#946 目标检查对同机直连请求自动生效，不为“本机目标”开授权捷径：

- 本机 + `tui`（默认）：行为不变，只省 Edge 往返。
- 本机 + `herdr-core`：受本草案全部切片约束；当前生产 candidate 被 `main.rs:443 candidate_startup` 挡在启动，同机直连绕不过（fence 在进程启动不在请求路径）。
- 远端 device：仍由 Edge/Link 路由到目标设备 runtime，按那台设备自己的 per-device 选择执行；Phase 2 不改跨设备语义（#775 范围外）。

**边界与失败语义**：同机直连保留与 relay 等价准入——loopback 走 bearer + `admitted_backend_gate`；trusted IPC 走 `request_authorized` 的 `trusted_extension_ipc`（免 bearer、0600 屏障、入站无 peer UID 检查，开放问题在案）。`local_mcp_*` 错误码属传输层，不改变工具层 `not_delivered/unknown` 与 `fallback_allowed:false`；本机 Core 不可达时不得静默改 TUI，也不得转发 Edge 让另一台设备代执行。未来 TS 侧“device=本机则直连”判定必须以设备注册身份（如 `mcp_http.rs:239 AppState.local_device_id`）为准、两侧一致，不能信任调用方自报 device 字符串（设计约束，非现状）。

**测试增补（并入切片 2）**：同一 `tools/call` 分别经 loopback HTTP、trusted IPC、模拟 Link runner 注入，断言 admitted backend、拒绝码、结果一致；Core 选择下三入口同样 fail closed，无入口绕开 `public_tool_route`。

## 与 #946 及 PR #947（1.2 设计，S6）的对照与不重叠

**#946（已合入 b8d660a0，"在 Core admission hello 前拒绝错误目标"）**：即原 #935 讨论的 pre-hello 目标比对切片的落地。它做了三件事：params 双形式在准入前归一化一次（`mcp.rs:1211`）、`expected_core_target` 提前到准入前校验并拒绝非 `server.status` 携带（`mcp.rs:1245 tool_call` + `runtime_adapter.rs:2398`）、条件目标在 connect 前比 dev/ino、connect 后 hello 前比 OS peer PID（`runtime_adapter.rs:1396-1420`）。**本文与其不重叠**：切片 3 不再把 pre-hello 比对列为待做依赖（初稿如此，已按 b8d660a0 修正），改为回归保护 + 收口其未覆盖面（无条件请求首次 admission 仍 hello 后取 PID；pathname 指纹 TOCTOU/inode 复用；advisory 措辞）。切片 4–6 的外部前提（文件打开耦合、写提交耦合、执行绑定）与 #946 正交：#946 是必要非充分条件，不授权任何文件/exec 操作。

**PR #947 `docs/design/1.2-local-transport-and-delivery.md`（已合入 main：mergeCommit `856543cea8ac3710f0fe59a4a599c72d4da6e37a`，mergedAt 2026-11-16T15:13:57Z；审阅时 head 为 c0a2de27，本文引用行号以该 head 为准）**：其 S6"Same-machine direct transport"明确 folds into #768 Phase 2，并给出同机谓词（device_id 相等 + #941 peer UID + #943/#944/#945 一致性）。分工与不重叠：

- **S6 拥有**：同机判定与 transport 切换（何时不走 Edge）、`transport/timing_ms` 信封字段（S0）、重连窗口写请求 `not_delivered` 化、300s heartbeat vs 90s silence 修复、跨传输治理记录可比性。本文不设计这些。
- **本文（Phase 2）拥有**：到达本机后的 backend 选择与逐工具 Core 授权/解封门槛、进程隔离、目标校验收口。S6 不新增任何 Core 授权：其谓词 2/3 引用的 `verify_hc_peer_uid`/`admit_public_tool`/`expected_core_target` 全部是本文保护的既有机制；S6 落地时这些检查原样复用，切片 4–6 的 refusal-first 不受 S6 影响（S6 自己也如此声明）。
- **一处需修正的 S6 表述**（供 #947 作者参考）：S6 谓词 2 写“local socket passes the #941 peer UID check (`verify_hc_peer_uid`)”——该检查是**出站 Core socket** 的（`runtime_adapter.rs:2764`），TUI 本机 socket 路径并不执行它；入站 IPC listener 也无 peer UID 检查。同机谓词若要求入站方向的对端身份，需要新机制（范围外），不能引用现有函数充当已具备。
- **顺序**：按 #947 §5，S6 在 #768 Phase 2 之后、协调者确认后启动；本文切片 1–3 不依赖 S6，切片 2 的三入口一致性测试为 S6 提供基线。S1（v6 contract epoch）改变 `herdr_call.params` 的 advertised shape，与本文冻结的是 18-tool 目录不冲突，但切片 1 的契约测试需在 S1 合入后按其新 epoch pin 重跑（串行约束见 #947 §5：触碰 `mcp.rs`/`runtime_adapter.rs` 的切片与 credentials deny-list 切片串行）。credentials deny-list 切片已以 #949 合入 main（squash `7a1d6983a9a7b8c566baf73645683ccb9e124464`，"拒绝凭据目录的整棵子树（#938 残留）"，触及 `fs_security.rs`/`fs_tools.rs`/`text_transfer.rs`）：该串行约束就此解除，且切片 4–5 的路径策略对照须以 #949 之后的凭据目录整树拒绝为准。

## 切片顺序与停止条件

**顺序**：1 → 2 → 3 串行（2 依赖 1 的基线测试；3 依赖 2 的进程可启动才能做进程级收口验证，单元部分可并行）；4 → 5（write → edit）→ 6 严格条件串行，各自等待外部安全机制证据。与 #947 的全局排序（#946 → S1 → deny-list → S0 → S2…）协调：deny-list 已以 #949 合入（`7a1d6983`），本文触碰 `mcp.rs`/`runtime_adapter.rs` 的切片只需与 S1 串行，一片一 PR 一 gate。

**每片通用停止条件**（命中即停，写文档上报协调者，不得变通实现）：
1. 需要修改冻结 18-tool 目录、公共参数或结果语义才能推进 → 停（合同变更需独立决策）。
2. 需要实现范围外机制（写目标绑定、同 UID 防伪、Windows IPC、会话生命周期、同进程重绑、#775 Edge 文件）→ 停，只记录依赖。
3. 需要削弱/删除 #936/#941/#938/#943–#946 任一 fence 才能通过测试 → 停。
4. 需要 `state.db` schema 变更 → 停并记录（参照 #947 同款约束）。
5. 解封前提（切片 4–6 各自的授权/绑定/提交耦合机制）拿不到可验证证据 → 该片保持拒绝并关闭为"条件未满足"，不做部分解封。
6. 进程级测试无法在 CI 复现（仅本机能过）→ 停，先修测试基建。
7. 发现与 #947 S6/S1 或 credentials deny-list 切片的文件冲突 → 停，交协调者排序。

## N/N-1 升降级与回滚

`config.rs JsonRuntimeOutput` 对默认 `tui` 与空 socket 省略字段，保 N-1 `deny_unknown_fields` 可读默认配置；显式 `runtime.backend:"herdr-core"`/`herdr_core_socket` 不保证 N-1 识别。回滚前备份配置、移除新字段与 Core env 覆盖、显式选 TUI、独立启动旧代验证——这是切换到不同 agent/terminal 命名空间，绝非自动 fallback；要保留 Core 目标则 N-1 无该能力，停止并报不兼容。冻结目录≠新增结果字段对旧客户端全兼容：新字段仅作可忽略附加信息，用 N/N-1 交叉与显式 Core 配置回滚测试验证。切片回滚后未提交操作拒绝；已发出的写/exec 若 unknown 不因回滚重放。跨代 target ref（PID/dev/ino/core_id）advisory，不保证稳定，旧 ref 拒绝后重新发现，不能用于写授权。不得改生产活动代/安装代验证本设计。

## 开放问题与验收阻塞

1. Core 能否提供可审计的 root/文件对象授权与实际 open、commit 耦合原语？没有则切片 4–5 永久保持拒绝，不能靠重复 canonicalize 补洞。
2. Core `machine.exec` 能否满足 `utility_exec.rs:468 run_durable` 全部公共结果/持久性/未知交付合同并在执行时绑定授权 cwd 与租约？未证实切片 6 阻塞。
3. candidate 除 MCP handler 外哪些 worker/HTTP/IPC 路由依赖 TUI？隔离清单 + 逐入口进程级反证是切片 2 退出条件。
4. #946 之后剩余目标缺口（无条件首次 admission 的 hello-先于-PID、pathname TOCTOU、PID/inode 复用、同 UID、同进程重绑）各自需要什么机制、哪些明确不修？需协调者定界。
5. 只读传输失败一律 `not_delivered`（`runtime_adapter.rs:2594 hc_delivery_state`）与实际发送阶段不符的风险如何在冻结合同下修订？需消费者兼容性决定。
6. Windows Core IPC 身份/ACL/路径与原生 CI 无实现；`.github/workflows/windows-candidate.yml` 仅 build/UAT，Rust tests 在 Linux lane；拒绝路径需实际 Windows 测试。
7. `GET /mcp` 长流与 `/push/events` 首帧的 backend 漂移竞态如何封闭？机制完成前 Core 模式禁用对应入口或维持启动 fence。
8. OSC 7501 上游多字段 grammar 与 reported 语义需协议证据；不从屏幕内容或缺失字段推断状态。
9. S6 同机谓词的入站方向身份检查（见对照节修正）由谁、以什么机制提供？在答案出现前 S6 不得声称入站对端已验证。
