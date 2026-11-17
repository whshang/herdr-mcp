# #332 D-1：重启后可判定的执行幂等设计

状态：设计待批准，未改 schema，未改 Rust，未改变 Runtime Execution Contract。

基线：`origin/main` `495156ecca86b3e092f63fa76f8c5323ea034fd9`。

关联：issue #332，差距分析评论 `6937170052`。

## 结论

D-1 只解决“调用者带着同一个键重试时，重启后的 Runtime 仍能返回第一次结果摘要并阻止第二次副作用”这一件事。推荐复用 schema 16 已有的 `operations` 表，新增一个命名空间明确的 operation kind，不新增 SQLite 表，不把 `SCHEMA_VERSION` 从 16 提升。

当前代码已经有三种 `herdr_exec` 意图：`read_only`、`idempotent_write`、`non_idempotent_write`。它们在 `crates/herdr-mcp/src/utility_exec.rs:112-180` 解析，当前执行契约也在 `crates/herdr-mcp/src/contract.rs:342-359` 固定了这三个枚举值。D-1 不再新增第四种意图，也不重新定义 `idempotent_write`。

当前 `herdr_exec` 的 `idempotency_key` 已经走持久化 `operations` 账本：预留发生在 `utility_exec.rs:262-368`，终态写回发生在 `utility_exec.rs:388-439`，并使用 `operations` 表已有的 schema 16 字段。D-1 解决的是另一条边界：`client_request_id` 目前只包住 `herdr_call`，账本仍是 `StateStore` 中的进程内字段，见 `crates/herdr-mcp/src/state_store.rs:820-823` 和 `crates/herdr-mcp/src/request_ledger.rs:1-13`。因此文档不把两个键混成同一个概念：现有 `idempotency_key` 继续兼容，新的持久化 `client_request_id` 使用独立的 operation kind 和稳定的键摘要。

## 当前实现事实

- `mcp::tool_call` 在 `crates/herdr-mcp/src/mcp.rs:1320-1340` 只对 `herdr_call` 的顶层 `client_request_id` 启用账本。记录在执行前预留，执行后在 `mcp.rs:1420-1465` 结算。
- 账本记录了调用范围、请求摘要、状态、时间和结果摘要，但结果只保留 digest、字节数和错误码投影，避免把正文、输出或文件内容放回 `request.status`，见 `request_ledger.rs:130-239`。
- 账本的 key 是 `HashMap` 内存项，容量为 1024，空闲 30 分钟后淘汰，见 `request_ledger.rs:19-22`、`:271-279`。`StateStore::open` 每次都会构造新的空账本，见 `state_store.rs:839-879`。
- 当前 `request_ledger.rs:184-190` 的 `DefaultHasher` 只适合进程内摘要。D-1 的持久化 key 必须改用跨进程稳定的域隔离 SHA-256，不能把这个摘要直接写成跨重启协议。
- `operations` 表从 schema 1 起就有 `kind`、`idempotency_key`、`request_hash`、`phase`、`state`、`result_json`、`runtime_generation`、`boot_id`、时间和过期字段，见 `state_store/schema.rs:46-93`。`SCHEMA_VERSION` 当前为 16，且代码要求高于已知 migration 的数据库直接拒绝，见 `state_store.rs:64-83`。
- 现有 `herdr_exec` 预留使用 `expires_at = NULL`，所以 pending 记录不会因为时间经过而释放；明确终态才使用 `EXEC_RECORD_TTL_MS = 60` 分钟，见 `utility_exec.rs:31-40`、`:291-294`、`:424-439`。
- 当前 runtime contract 已是 epoch 6，epoch 5 是上一版带 `intent` 与 `idempotency_key` 的 `herdr_exec` 契约，见 `contract.rs:22-42`。D-1 文档不直接修改 frozen 18 工具目录或契约描述。

## 意图和重启后的行为

| 意图 | 是否要求键 | 重启后有终态记录 | 重启后无记录 | pending 或 unknown | `safe_retry_mode` |
| --- | --- | --- | --- | --- | --- |
| `read_only` | 可选 | 返回第一次摘要；同键不必重新读取 | 允许重新执行，因为没有写副作用 | 允许重新读取；不应把读取结果说成写入未知 | `none`；无记录时 `retry_safe_read_only` |
| `idempotent_write` | 必须 | 返回第一次摘要，不重复提交 | 没有记录且使用新键时才执行 | `inspect_before_retry`，不自动重放 | 由持久记录决定，不能由本次调用覆盖 |
| `non_idempotent_write` | 必须 | 返回第一次摘要，不重复提交 | 只有新键才允许新执行；旧键 tombstone 只能拒绝 | `delivery_unknown` 或 `request_in_flight`，必须人工检查 | `inspect_before_retry` |
| 未声明 intent 的旧调用 | 否 | 不进入 D-1 账本 | 保持现有 legacy 行为 | 保持现有 legacy 行为 | 不新增幂等承诺 |

`idempotent_write` 是调用者对命令语义的声明，不是 Runtime 对任意 shell 命令的证明。Runtime 只保证同一个持久化键不会在同一个绑定请求上再次投递；命令本身是否能安全重复，仍由调用者负责。`non_idempotent_write` 更严格：只要第一次结果可能已经送达，重启后就不得因为没有完整结果而再次执行。

同一个键即使第一次结果是 `not_delivered`，重试也返回该第一次摘要。若调用者确实要发起新的尝试，必须生成新键，并重新通过当前的 lease、workspace、project root 和 busy 检查。这样可以避免把“第一次没有送达”误读成“旧键可以无限重用”。

## 键绑定和请求身份

D-1 同时保留两种键：

1. `herdr_exec.idempotency_key` 是现有直接执行接口的 durable key。它继续使用 operation kind `herdr_exec`，旧客户端和 v1.1.x 已经认识这条路径。
2. `client_request_id` 是调用者边界的请求键。D-1 采用新的 operation kind，例如 `herdr_exec.client_request.v1`，避免和现有 `herdr_exec` 记录混用。`herdr_call` 侧可以使用 `herdr_call.client_request.v1`，方法名必须进入 request hash，防止同一键在不同方法之间交叉重放。

同一次调用不得同时提供 `client_request_id` 和 `idempotency_key`。实现获批后应在参数层拒绝这种歧义，避免两个账本各自成功预留。现有 `idempotency_key` 的行为保持不变；新的 `client_request_id` 字段若要直接加入 `herdr_exec` 的公共输入，需要单独的 contract 变更和 N/N-1 评审，D-1 不偷偷扩展 frozen 18 工具契约。

持久化 key 使用以下稳定构造，原始 key 不写入数据库：

```text
key_digest = SHA-256(
  "herdr-mcp/client-request/v1" || 0x00 ||
  caller_scope_digest || 0x00 || client_request_id
)
```

`caller_scope_digest` 至少包含当前调用者的稳定会话或设备范围。租约 token、cookie、bearer 等秘密不得进入明文或摘要之外的结果。请求摘要绑定以下内容：

```text
request_hash = SHA-256(canonical JSON {
  public method,
  workspace_id,
  effective project_root,
  canonical command or structured steps,
  intent,
  timeout_ms,
  confirm_busy,
  caller scope,
  lease scope when applicable
})
```

执行路径已经有一部分相同约束：`utility_exec.rs:224-250` 将 workspace、root、invocation、intent、busy 选项和 timeout 纳入 fingerprint。D-1 要求持久化 client key 的 request hash 至少覆盖这些字段，并将 caller scope 一并纳入。改变 intent、命令、root、workspace 或 scope 都是冲突，不能把同一个键当成新的请求执行。

已结算记录的 replay 必须使用数据库中的原始摘要、`op_id`、`session_id`、状态和 `safe_retry_mode`。不能用重试请求中的 intent 或当前运行态覆盖旧记录。当前 `utility_exec.rs:346-360` 已经遵循“保留原始身份，只增加 replay 标记”的方向，D-1 沿用这个规则。

## 终态如何落表

推荐直接复用 schema 16 的 `operations` 表，不添加列：

| 列 | D-1 用法 |
| --- | --- |
| `op_id` | `op:herdr_exec_client_request:<key_digest 前 32 位>`，稳定且不含原始 key |
| `kind` | `herdr_exec.client_request.v1` 或 `herdr_call.client_request.v1` |
| `idempotency_key` | 上述 `key_digest` |
| `request_hash` | canonical 请求摘要 |
| `phase` | 新预留为 `reserved`，结算为 `settled` |
| `state` | `pending`、`complete` 或 `unknown`；业务终态放在 `result_json.outcome` |
| `result_json` | 有界的首次结果摘要，不能存无限 stdout/stderr |
| `runtime_generation` | 可选写入执行时 generation，便于诊断和回滚审计；不参与 replay 许可判断 |
| `boot_id` | 可选写入执行时 boot，区分重启前后的观察 |
| `created_at`、`updated_at` | 预留、送达、结算和清理时间 |
| `expires_at` | 终态摘要的过期时间；pending 和 unknown 必须为 NULL |

`result_json` 至少包含：

```json
{
  "schema": 1,
  "intent": "non_idempotent_write",
  "outcome": "applied",
  "delivery_state": "delivered",
  "safe_retry_mode": "none",
  "idempotency_persisted": true,
  "result_summary": {
    "ok": true,
    "code": "ok",
    "phase": "completed",
    "execution_started": true,
    "execution_completed": true,
    "exit_code": 0,
    "output_digest": "…",
    "output_bytes": 42
  }
}
```

只保留摘要、digest、字节数和必要的 `op_id`/`session_id`。完整输出继续由现有执行结果或 session 查询提供，不能因为重启幂等而把敏感输出复制进账本。

状态转换如下：

```text
reserved/pending
  ├─ 已证明未送达 → settled/complete + outcome=not_delivered
  ├─ 已有成功终态 → settled/complete + outcome=applied
  ├─ 已有拒绝终态 → settled/complete + outcome=rejected
  └─ 送达或结算不明 → settled/unknown + outcome=unknown
```

`pending` 记录代表第一次调用仍在进行或进程在结算前退出；`unknown` 记录代表副作用可能已经发生。两者都不允许由时间清除，也不允许自动重放。只有人工检查外部状态后，未来单独的 reconciliation 设计才能给出新的结论；reconciliation 不应重新执行原命令。

## `safe_retry_mode` 绑定规则

`safe_retry_mode` 是第一次结算的事实字段，必须跟着 operation 记录持久化。它不是本次重试的建议，也不能由调用者传入覆盖。

| 持久化事实 | `safe_retry_mode` | 语义 |
| --- | --- | --- |
| 终态已应用、已拒绝或已证明未送达 | `none` | 同键只返回第一次摘要；新尝试必须使用新键 |
| pending 或 unknown | `inspect_before_retry` | 先查 session、进程、文件或外部系统；Runtime 不重新投递 |
| `read_only` 无记录 | `retry_safe_read_only` | 可以重新执行只读查询，不把它当成变更重试 |
| 预送达控制面拒绝且没有副作用 | `retry_after_resource_release` | 可在资源恢复后重试；该请求没有取得可执行的持久化结果 |
| durable store 不可用 | `retry_after_durable_store_recovery` | 不执行写命令，待账本恢复后重新提交 |

对 `non_idempotent_write`，永远不产生 `retry_now` 或等价值。对 `idempotent_write`，即使命令理论上可重复，`pending`/`unknown` 也保持 `inspect_before_retry`，除非后续设计增加了可验证的外部 postcondition；D-1 不凭 intent 名称推断外部状态。

## 保留期和清理

建议的 D-1 保留政策：

- `complete` 终态的完整摘要保留 24 小时。这个窗口覆盖常见的重启、网络恢复和人工重试间隔，比当前内存账本 30 分钟和现有 `herdr_exec` 终态 60 分钟更能满足“重启后同键返回第一次摘要”的目标。
- 对 `idempotent_write` 和 `non_idempotent_write`，24 小时后把结果正文投影压缩成只含 key digest、request hash、intent、终态和 `expired=true` 的 hash-only tombstone，不删除键占位。旧键在 tombstone 存在时返回 `idempotency_key_expired` 或等价的不可重试冲突，不得再次执行。这样可以缩短敏感摘要保留期，同时保持写入键不会因为清理而重新获得执行资格。
- `pending` 和 `unknown` 的 `expires_at` 永远为 NULL，不参与自动清理。清除它们会重新打开重复副作用的窗口。
- `read_only` 终态可使用 30 分钟较短保留；过期后可以删除整行并重新读取，因为没有写副作用。
- 清理只能删除 `read_only` 的终态行，或把写入终态压缩为 hash-only tombstone；必须在同一事务中检查 operation kind、state、expires_at 和是否存在待处理的人为 gate。不能通过删除旧行来释放一个尚未结算的键，也不能把写入终态直接删除后当作新请求。
- 每个新 kind 设置有限容量，建议先沿用现有 1024 条的上限作为最小值，评估后再提高。达到上限时只能淘汰只读终态或完整摘要已压缩且允许删除的记录；写入 tombstone、pending 和 unknown 都不可淘汰。如果剩下的全是这些不可淘汰记录，返回 `client_request_ledger_full`，不丢记录、不执行新写命令。
- 清理、容量拒绝和 replay 都必须保留同一个 key digest 的冲突语义，不能因清理路径把相同键当成新请求。

实现时优先复用 `StateStore::reserve_operation` 的事务、唯一索引和现有清理框架。现有 `operations` 的唯一索引按 `(kind, idempotency_key)` 约束，正好可以让 `herdr_exec` 和 `herdr_exec.client_request.v1` 使用不同命名空间而互不碰撞。

## schema 16、v1.1.x 回滚和兼容

### 推荐：不把 schema 从 16 前进

D-1 需要的列已经存在于 schema 16 的 `operations` 表，因此推荐：

- `SCHEMA_VERSION` 继续为 16。
- 不新建 `client_request_ledger` 表。
- 不修改已应用的 migration，只新增新的 operation kind 和运行时逻辑。
- 新 row 的 `phase`、`state`、`expires_at` 使用现有通用值，让旧二进制可以打开数据库并忽略未知 kind。

这样保留了数据库层面的 v1.1.x 回滚兼容：旧二进制打开同一个 state.db 不会因为版本号更高而 fail closed。旧二进制仍然不认识新的 `client_request_id` 语义，所以语义兼容必须单独声明：

- 旧二进制继续支持已有 `herdr_exec.idempotency_key`，这条路径已在 schema 16 上持久化。
- 旧二进制不得把 D-1 的 `client_request_id` 当作没有键的新写请求。部署或回滚门禁应在检测到 D-1 请求字段时返回 `unsupported_client_request_id`，或要求先由新二进制完成 reconciliation；绝不静默执行。
- 回滚到 v1.1.x 前，如果存在 D-1 的 pending/unknown 行，必须停止回滚或要求人工确认。它们代表可能已经发生的副作用，不能因为旧二进制看不懂 kind 就再次投递。
- 终态摘要行可以留在 schema 16 的 `operations` 中；旧二进制忽略未知 kind，不得删除 pending/unknown。恢复到新二进制后，原记录仍可用于 replay/status。

### 如果未来必须升到 schema 17

只有当后续实现证明现有 `operations` 列无法承载稳定 scope、摘要或审计字段时，才另立 schema 17 设计。那时必须：

1. 追加新 migration，绝不修改已应用 migration。
2. 明确 v1.1.x 无法打开更高版本数据库。回滚前需做受验证的备份、导出并降级到 schema 16，不能自动删除新字段。
3. 将“新二进制写入 v17”与“旧二进制可安全回滚”做成同一发布门禁；没有可验证降级路径就禁止回滚。
4. 通过旧二进制实测打开、只读检查、写入拒绝和恢复新二进制后的 replay，不能只看版本号。

D-1 不选择这条路径，因为它会扩大 release/rollback 风险，也没有当前需求证据证明必须新增列。

## API 和 contract 边界

D-1 设计阶段不改 Rust，不改 `contracts/`，不改变当前 epoch 6 和 frozen 18 工具目录。

现有 `herdr_exec` 已经把 `intent` 与 `idempotency_key`作为 epoch 5 的 additive 输入；`idempotent_write` 已在当前实现中存在。后续若决定让**直接** `herdr_exec` 接受名为 `client_request_id` 的新字段，应另开 contract 变更：

- 更新当前 descriptor 和 previous descriptor 关系。
- 维护 v6 到新 epoch 的 N/N-1 兼容矩阵。
- 旧客户端省略新字段时行为完全不变。
- 旧 Runtime 收到新字段时必须拒绝并返回 `unsupported_client_request_id`，不能丢字段后执行。
- 新 Runtime 不能把 `client_request_id` 和 `idempotency_key` 解释成两个独立的执行槽位。

在这个 contract 变更获批前，直接 `herdr_exec` 的稳定键名仍是 `idempotency_key`；`client_request_id` 的重启验收应通过计划中的 caller-boundary 适配层验证，不能把文档目标误写成当前已存在的字段。

## 验收测试清单

### 持久化和重启

- 创建临时 schema 16 state.db，提交一个带 `client_request_id` 的 `non_idempotent_write`，让命令写入唯一外部计数器并返回完整摘要。
- 关闭第一进程，重新打开同一个 state.db，使用相同 caller scope、相同 request fingerprint 和相同 key 重试。
- 断言返回第一次记录的 `op_id`、摘要 digest、状态和 `safe_retry_mode=none`，外部计数器仍为 1，第二次没有启动子进程、没有发送到 utility pane、没有创建新 session。
- 变更命令、root、workspace、intent 或 caller scope 后复用同一 key，断言 `idempotency_conflict`，且外部计数器不变。
- 重启后查询 `request.status`，断言同一 scope 能读到记录，其他 scope 得到 `found:false`，不泄露 key 是否存在。

### 不确定结果和安全重试

- 在命令已经可能送达、结算前强制结束 Runtime，重启后断言同 key 返回 `delivery_unknown` 或 `request_in_flight`，`safe_retry_mode=inspect_before_retry`，不二次执行。
- 在预送达控制面失败时，断言结果为 `not_delivered`、`retry_after_resource_release`，没有持久化可执行的副作用；恢复资源后可以用同一请求重新预留，或按实现约定要求新 key，但不能复制一条 pending 记录。
- 模拟 durable store 不可用，断言写命令不执行，结果使用 `retry_after_durable_store_recovery`。

### 三种 intent

- `read_only` 无 key可以继续执行，带 key 的终态可 replay；过期后重新读取不触发写入。
- `idempotent_write` 使用同一 key 重启 replay，改变 request fingerprint 则冲突；pending/unknown 不自动重放。
- `non_idempotent_write` 终态 replay，pending/unknown 永不自动重放；这是 issue #332 的关键验收。
- 未声明 intent 的 legacy 请求保持既有行为，不能因为 D-1 自动获得持久化幂等承诺。
- 同时提交 `client_request_id` 和 `idempotency_key` 必须在执行前拒绝，不得产生两条记录。

### 账本和清理

- 终态记录在保留期内可重启 replay；写入终态过期后变成 tombstone，旧 key 返回过期冲突而不是重新执行。只有新 key 才能开始新请求。只读终态被清理后，`request.status` 返回未知并允许重新读取。
- pending/unknown 行经过多次清理和重启仍存在，容量达到上限时拒绝新写入而不淘汰它们。
- 结果摘要不包含 stdout、stderr、prompt、文件内容、token、lease token 或其他秘密，只包含状态、digest、字节数和必要的 session/op 标识。
- 两个并发进程使用同一 key 时只有一个获得 `Reserved`，另一个得到 replay 或 in-flight，不能各自执行一次。

### 回滚和兼容

- schema 16 的旧数据库在新实现中可读，新增 operation kind 不影响已有 `herdr_exec` replay。
- v1.1.x 打开同一个 state.db 不报高版本错误；遇到 D-1 字段时拒绝，不静默执行。
- 有 pending/unknown D-1 记录时回滚门禁拒绝，或要求人工 reconciliation；回滚过程中不能删除这些行。
- v1.1.x 已有 `herdr_exec.idempotency_key` 的成功 replay 继续通过；这与新的 caller-level `client_request_id` 语义分开验证。

## 实施顺序和停止条件

1. 先批准本设计和 API 边界，特别是是否在直接 `herdr_exec` 公开 `client_request_id`。
2. 复用 schema 16 `operations` 写一个独立 storage adapter，只加入新的 operation kind、稳定 SHA-256 key digest、bounded summary 和 kind-specific cleanup；不得先改 schema。
3. 在内存账本保留兼容查询路径，完成“双写只读核对”后再切换写入；切换期间同一 key 只能由一个 authority 预留。
4. 完成重启、响应丢失、并发、scope 冲突和 v1.1.x 回滚测试后，才进入 contract/API 变更评审。
5. 任何无法证明“第一次副作用是否发生”的路径都保持 unknown 和 `inspect_before_retry`，停止在这里，不用自动重放冒充恢复。
