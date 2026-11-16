# 1.2 阶段一核实笔记

基线 SHA：73a599d6fef33737301db5a638eef05c2cd3dcfa

## 本地基线测量（只读）

- `~/.local/bin/herdr workspace list`（本机 socket 直连，10 次）：14,11,10,10,9,9,10,10,9,10 ms → 中位数 10ms，P90 11ms。
- relay 路径同机测量：无法在不产生写操作、不影响运行中工作区的前提下测量；Edge relay 经 Cloudflare Worker + DO，本机无可复现只读探针入口。仅给代码层证据。

## P1 路由 —— 成立（同机无旁路）

- Edge 路由三分支全部落 DO：`edge/cloudflare/src/mcp-handler.ts:1499-1552`（fast/resolveDevice/legacy_default_device），无条件 `deps.forward(deps.getStub(route.workstation_id), …)`（`mcp-handler.ts:1758-1762`）。**无"同机→直连"判断**，同机与跨机同路径。
- 本地直连只有本地 MCP 端点 `link/local_mcp.rs:41`（默认 10s 预算），不在 herdr_call 的 Edge 路由分支。
- runtime 侧 dispatch：`mcp.rs::handle` → `tool_call` → `runtime_adapter.rs::public_tool_route`（TUI/HerdrCore backend 选择）→ `dispatch_admitted_tool`；HerdrCore 另有白名单 raw 转发（rpc.discover/server.status/templates.list）。

## P2 超时 —— 成立

- 30s deadline 只在 Edge：`edge/cloudflare/src/limits.ts:40` `DEFAULT_REQUEST_TIMEOUT_MS=30_000`（min 1s/max 65s；grace 5s；link wire max 60s）。
- runtime 默认更长：`link/request_core.rs:16` 60s；`herdr.rs:8-9` RPC 30s/60s；`local_mcp.rs:41` 本地 MCP 10s/120s。
- Edge 下发预算钉死 runtime：`workstation-do.ts:767-782` → `wire.timeout_ms`；`request_core.rs:54-62` 只缩短不延长。
- Edge 到点即结算 + cancel：`workstation-do.ts:1276-1352`；迟到结果被丢（`persistSettlement` 首结算优先 `:1389`）。
- 同步等待必撞：`wait`/`wait_ms` 上限 60s（`prompt.rs:31-32,1841-1847`），Edge 预算只认 `params.timeout_ms`（`mcp-handler.ts:1674-1684`）。文案 `edge/cloudflare/src/errors.ts:163-176`（mutating 分支 `:169`）。

## P3 唤醒 —— 部分成立

- `parent_session_ref` 仅 dispatch 设置：`mcp.rs:748-866,1496-1526`；落库 `state_store.rs:4906-4935`，`COALESCE` 不被 NULL 覆盖（`:4926`）——不会变 null，为 null 是创建时未绑定（非 WebChat 调用者/无 current_user_message 证据 → NotAttempted）。
- `worker_done`：`agent.task.report` → `prompt.rs:696-770` → `notification_state=pending` → `maybe_notify_parent`。
- 主动推送**只有** `parent_target`（pane）：`prompt.rs:1366-1500`。WebChat/浏览器父会话无推送：`parent_session_ref` 只用于 inbox 过滤+ack 鉴权（`prompt.rs:820,875-895`）；扩展轮询 `GET /extension/agent/tasks?parent_session_ref=`（`extension/background.js:5380-5455`），触发源：pane 事件 onPush、progress interval、1min keepalive。

## P4 spawn —— 部分成立

- 构造上不提前投递：`agent.prompt` 在 `prompt.rs:3490`，严格晚于 `spawn_chain`（`:3423`）；链失败 → checkpoint，无 prompt。
- 但"就绪"判定弱：`spawn_wait_ready`（`prompt.rs:4434-4470`）谓词仅 `interactive_ready==true || agent_status=="idle"`（`:4405-4413`），200ms 轮询；无握手、无 composer/prompt 可见性检查、无启动后延迟。CLI 自报 idle 早于可输入时仍会投递 → 10s 后 `delivery_uncertain`。
- 非 spawn 路径（herdr_prompt、herdr_call agent.prompt）**完全没有就绪门槛**。
- `delivery_uncertain` 产生条件：`prompt.rs:1723-1735`：submitted 证据缺失（`:613-619`）、activity_start_timeout 10s（`:1523-1528`）、runtime_boot_changed/unknown（`:1548-1565`）、event_history_gap>2048（`:1595-1600`）、browser lane 传播（`browser_control.rs:284-295`）。
- `repair_budget_exhausted`：inbox 内 advisory escalation 信号（非 dispatch gate）：`prompt.rs:1956-1972`，同 (target,parent_session_ref) lane 6h 内 ≥4 任务 → signal；常量在 `prompt.rs:41-46`。
- 历史清理：记录 TTL 7 天（`prompt.rs:27`），但**无清扫器**——过期行只在 `reserve_operation` 时顺带删（`state_store.rs:1089-1097`）；`put_operation_ledger` 从不 prune；`operation_ledger(op_id)` 读取无过期过滤（`state_store.rs:1677-1705`）。
- inbox 过滤现状（`prompt.rs:1739-1747`）：workspace_id、parent_target、parent_session_ref、include_acknowledged、advisory、limit(≤512)、view、wait_ms。**无 receipt_id/status/since**。long-poll 成立：`wait_ms` 1..=60_000，250ms 轮询（`prompt.rs:984-1022`）。

## P5 inbox —— 与 P4 合并同上

## P6 params —— 成立（仅 schema 过时）

- runtime `mcp.rs:1332-1363`：**已接受 object 或 string**（#908 c2fcca79）。
- Edge `mcp-handler.ts:1391-1432`：也已接受两者；string 仅校验性解析不重写；对象直接过。**Edge 不是拒绝源**——除非经旧 schema 客户端校验。
- `invalid_params_json` 两个发出者：runtime `mcp.rs:1350`、Edge `mcp-handler.ts:1401`。legacy `src/server.ts:954-965` 发 `invalid_params`。
- **唯一不同步是发布 schema**：`contracts/epoch2.json:72-75` type=string；Edge 公共合同 epoch3/epoch8（派生 epoch2.ts:74）同；`src/server.ts:932-937` 注释明说 ChatGPT 拒收 z.record 故 advertise string。skill 文案 `assets/herdr-mcp-SKILL.md:28`、`assets/herdr/skills/workstation-control/SKILL.md:18`、`skill.rs:710` 测试均写死"JSON object string"。
- 接受 object 不破坏现有客户端：三层解析器全部已收 object；放宽 schema 只是放宽。第三方严格校验客户端无法确认（仓内无其代码）。
- `herdr_prompt` 直接收 text 参数——已绕开双层编码（成立）。
- 改 schema ⇒ contract hash 变 ⇒ 需 v6 descriptor + PUBLIC_CONTRACT_EPOCH/HASH + `edge/cloudflare/src/contracts/runtime.ts` COMPATIBLE_RUNTIME_CONTRACTS（**Edge 文件，#775 边界停止条件候选**）+ `src/link/daemon.ts` + `runtime-parity.json`。
- ChatGPT catalog 风险：docs/i18n 警告自由对象/additionalProperties 形态曾致整表拒收；`anyOf:[object,string]` 需 ChatGPT UAT。

## P7 模型别名 —— 成立（无校验）

- `agent.start` 无别名校验：spawn `parse_spawn` 拒绝除 `{kind,name,workspace_id,cwd,tab_label,split_from,direction,startup_timeout_ms}` 外字段（`prompt.rs:3964-3990`），`start_params` **不含 model/args**（`:4574-4579`）；`herdr_call(agent.start)` 仅 live-schema 类型/enum 校验（`native_tools.rs:99-133`→`schema.rs:62-197`），`args:["--model","x"]` 是自由字符串数组。
- 各 agent 可用模型列表启动前**拿不到**（pi 除外）：`capability_probe.rs:187-275` 仅读 `~/.pi/settings.json` 的默认 model；omp/grok/droid/agy `capability_scan.rs:272-275` else 分支为空。SAFE_SELF_DESCRIPTION_AGENTS 无 omp。
- 未知别名静默回退属 CLI 行为，herdr-mcp 无任何 post-start 模型核对（代码层无法确认回退本身，可确认无检测）。

## P8 运行上限 —— 成立（无 max_runtime）

- 现有：`stall_after_ms`（dispatch 参数，1-120min，`agent-dispatch SKILL.md`：20min 无 heartbeat → stalled，仍 non-terminal，**不自动停**）；无 max_runtime_ms、无 on_budget。

## P9 诊断 —— 部分核实

- `status`（`status.rs`）、`doctor`、`scan --probe`、`inspect`、`capability_snapshot` 存在；`herdr api` 子命令有 runtime state。缺：一跳的本机进程+端口+配置快照白名单视图（S7）。

## P2 补充：重连（待 ScoutRouting）
