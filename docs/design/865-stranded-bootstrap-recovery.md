# #865：首台设备半成品 bootstrap 的诊断与恢复边界

状态：设计待评审。本文只定义本地诊断、证据门槛和恢复出口，不改 Rust，不改 Edge，不改变当前拒绝语义。

## 问题范围

当前 bootstrap journal 记录 `phase`、发布 commit、runtime version、账户、Worker、workers.dev origin 和可选的 `canonical_device_id`，文件格式为独立的 `JOURNAL_SCHEMA = 1`，见 `crates/herdr-mcp/src/worker_bootstrap.rs:30-80`。

本设计只处理三类首台设备可能遇到的半成品状态：

1. journal 为 `SecretsProvisioned`，但 `canonical_device_id` 为空；
2. journal 和本地 enrollment 丢失，但目标 Worker 健康且已经存在 `LINK_SHARED_SECRET`；
3. 本地 runtime 升级后，旧 journal 因发布 commit 或版本变化被 `validate_journal` 拒绝。

这些状态的共同约束是：本地文件、Worker 名称或 secret 名称都不能单独证明设备登记表为空，也不能证明某个设备就是本次中断留下的设备。诊断可以解释证据，恢复必须由可验证的 Cloudflare 和 Edge 事实授权。

## 不变的安全边界

- 继续使用 `prove_unenrolled_registry_empty` 作为首台登记的唯一空 fleet 证明。它先写入短时临时 operator secret，再通过目标 Worker 读取设备登记表；登记表读取失败或非空都必须撤销临时凭据并停止，见 `worker_bootstrap.rs:1921-1991`。
- 不因 secret 名称、`workers.dev` 健康响应、旧 journal 或本地 `edge_device_id` 缺失而推断空登记表。
- `LINK_SHARED_SECRET` 只作为 fleet material 的观察结果，不能作为设备身份、设备所有权或首台资格证明。secret 值永不进入诊断输出。
- 诊断阶段只读取本地 journal、runtime/config 绑定和经 Cloudflare 认证读取的账户、脚本、settings、secret 名称与 health 证据；不写 journal、config、secret、Worker script 或设备登记表。
- 任一证据读取失败、目标身份不精确、secret inventory 不完整、登记表无法证明为空时，状态必须是 `unproven`，出口只能是人工检查或已有 enrolled computer 的 pairing。
- 该设计不修改 `state.db`，不修改 `SCHEMA_VERSION = 16`，不新增 migration，不改变旧 runtime 打开数据库的能力。
- 不修改 `edge/cloudflare/`、Edge API、设备登记协议或 #775 Edge 路径。

## 诊断投影

新增的实现切片应提供只读 `bootstrap journal` 诊断投影。它只暴露以下非秘密字段：

```text
state
journal_present
journal_schema
journal_phase
journal_release_match
account_match
worker_match
origin_match
canonical_device_present
local_device_binding
worker_health
worker_secret_inventory
registry_proof
recovery_exit
```

`worker_secret_inventory` 只使用分类值：

- `absent`
- `probe_only`
- `fleet_material_present`
- `unreadable`
- `unproven`

不得输出 secret 值、operator credential、pairing token、设备 credential、完整 request body 或 Cloudflare token。现有 secret 分类规则见 `worker_bootstrap.rs:1520-1533` 和 `:1656-1674`；目标 Worker 的健康、settings 和 secret 名称观察见 `:1768-1850`。

`registry_proof` 只允许使用三值：

- `empty`
- `not_empty`
- `unproven`

`empty` 只能来自 `prove_unenrolled_registry_empty` 的成功结果，不能由“没有本地 enrollment”“没有 journal”或“只看到一个 Worker”替代。

## 状态一：SecretsProvisioned 但没有 canonical device

当前代码在写回 journal 后发现该状态即停止，见 `worker_bootstrap.rs:1226-1255`。这是必要的拒绝：接下来可能已经创建过设备，但本地还没有写回设备 ID；再次生成 secret 或创建设备可能造成第二个首台设备。

诊断可以展开：

- journal 的 phase、账户、Worker、origin 和 release match；
- `canonical_device_id` 缺失；
- Worker health 是否能按当前允许的 contract 识别；
- secret inventory 是否可读，以及是否含 fleet material；
- 本地是否存在可验证的 enrolled credential；
- `registry_proof = unproven`，除非已经执行并成功完成正式空登记表证明。

诊断不得把“secret 已存在”或“Worker 健康”显示为可恢复。恢复出口分为两种：

1. 有另一台已 enrolled computer：转入既有 fleet 的 `worker pair` / `worker connect` 流程，由已 enrolled 身份发起 pairing。
2. 没有已 enrolled computer：保持阻断，要求人工完成一次带 Cloudflare 账户授权的状态核对和明确的孤儿设备处置。该处置必须先取得目标 Worker 身份和设备登记状态的只读证据；不能通过删除 journal、删除 `LINK_SHARED_SECRET`、重写 secret 或重新跑首台 enrollment 绕过证明。

如果未来要支持无 enrolled computer 的恢复，必须新增显式的恢复命令和审计输出，并把“登记表确实为空”的检查绑定到同一目标 Worker、同一账户和同一 origin。命令仍必须调用现有 `prove_unenrolled_registry_empty`，不能把本设计变成自动修复。

## 状态二：Worker 健康、存在 LINK_SHARED_SECRET，但本地 journal/enrollment 丢失

当前 `secret_disposition` 将 `LINK_SHARED_SECRET` 归为 `FleetMaterial`，`classify_fleet` 因而返回 `ExistingFleet`，随后提示用户从已 enrolled computer 执行 pairing，见 `worker_bootstrap.rs:1557-1653` 和 `:1656-1669`。这能防止首台路径覆盖已有 fleet，但没有解释“secret 已写、登记可能仍为空”的半成品。

诊断应把该状态单独显示为 `stranded_fleet_material`，并列出：

- Cloudflare 认证账户与目标 Worker 是否精确匹配；
- workers.dev subdomain、Worker 名称和 origin 是否精确匹配；
- health 是否是当前允许的 Link contract；
- settings 是否满足 `proven_unenrolled_bootstrap_identity` 的完整条件，见 `worker_bootstrap.rs:1717-1766`；
- secret inventory 是否完整可读；
- `registry_proof` 是否为 `empty`、`not_empty` 或 `unproven`。

只读证据不足时，出口仍是 `worker pair` 或人工检查。若目标 Worker 的身份、settings、secret inventory 和账户均已精确匹配，且显式恢复流程随后通过 `prove_unenrolled_registry_empty` 得到 `empty`，未来实现才可以进入“孤儿 fleet material 的首台恢复”分支。该分支必须：

- 在空登记表证明前不上传 script、不覆盖 fleet secret、不创建设备；
- 复用已有 secret 时使用严格 inherit，不能因 secret 值不可读而静默替换；
- 对额外未知 secret、不可读 secret inventory 或任一身份不一致保持拒绝；
- 在设备登记和本地 canonical binding 成功后才写入新的 journal；
- 失败时保留可诊断状态并清理临时 operator，沿用现有 TTL 和 rollback 语义。

因此，`LINK_SHARED_SECRET` 本身永远不能打开首台恢复出口；它只能让诊断知道需要走“证明登记表为空后再决定”的分支。

## 状态三：旧 journal 被新 runtime 拒绝

当前 `read_journal` 先要求 journal schema 可识别，`validate_journal` 再要求 `source_commit`、`runtime_version`、`account_id` 和 `worker_name` 全部与当前运行时相同，见 `worker_bootstrap.rs:4181-4196` 和 `:4237-4251`。这能阻止新 runtime 把另一个发布、账户或 Worker 的 journal 当作本次恢复依据，但会把合法升级后的半成品也归入同一错误出口。

设计一个显式的只读 `journal_stale` 分类，不直接放宽现有 `validate_journal`。分类器可以读取旧 journal 的非秘密字段并报告差异：

- `schema_unsupported`：格式未知，不能迁移；
- `account_or_worker_mismatch`：目标身份不一致，不能迁移；
- `release_mismatch_with_canonical_device`：已有 canonical device，可走强证据迁移；
- `release_mismatch_without_canonical_device`：必须走首台空 fleet 证明，不能直接续跑；
- `current`：继续使用现有校验路径。

### 有 canonical device 的旧 journal

只有以下只读证据全部成立时，才允许未来实现把旧 journal 元数据迁移到当前 release：

1. 用户重新完成 Cloudflare 认证，目标账户和 Worker 由 API 返回值确认；
2. journal 的账户、Worker 和 origin 与当前目标精确匹配；
3. Worker health 与当前允许的 contract 匹配；
4. journal 的 `canonical_device_id` 是合法 `dev_*` 值；
5. 本地 config 的 `edge_device_id` 与 journal 中的设备 ID 相同；
6. 本地 stored bootstrap credential 能通过目标 Worker 的 fleet-admin 只读验证；
7. 不需要重新创建 secret、设备或 pairing。

迁移只更新 journal 的 release metadata 和更新时间，保留 phase、origin 和 canonical device；迁移前先写备份，迁移后仍要重新运行现有的本地绑定与 fleet-admin 验证。任一证据缺失就停在 `journal_stale`，不得尝试激活。

### 没有 canonical device 的旧 journal

旧 journal 只能作为诊断线索，不能授权继续执行。若 phase 早于 `SecretsProvisioned`，目标 Worker 身份、settings、secret inventory 和 workers.dev origin 全部匹配后，仍必须由 `prove_unenrolled_registry_empty` 证明当前登记表为空，才能按当前 release 重新建立首台流程。

若 phase 为 `SecretsProvisioned` 或更晚但缺少 canonical device，按状态一处理。即使 journal 看起来来自当前设备，也不能由 journal 自己证明设备登记没有发生。

### 未知 journal schema

未知 schema 不做就地升级，不删除原文件，也不把未知字段丢弃后覆盖。诊断只显示 `schema_unsupported` 和安全的文件元信息；恢复使用人工检查或新建、重新证明的流程。

## 与回滚和发布的关系

Journal 迁移是配置目录中的文件级兼容逻辑，与 SQLite schema 独立。实现不得通过提高 `SCHEMA_VERSION` 解决 journal 兼容，也不得编辑已应用 migration。

每个恢复出口都必须说明：

- 使用哪一条 Cloudflare 只读身份和 health 证据；
- 是否需要 `prove_unenrolled_registry_empty`；
- 是否允许写 journal、secret、Worker 或设备登记表；
- 失败后是否清理临时 operator；
- 旧 runtime 是否能继续打开同一 schema 16 `state.db`。

在这些证据和回滚演练完成前，发布说明只能把 S4 标为诊断设计，不能声称首台无 enrolled computer 恢复已经可用。

## 验收清单

- `SecretsProvisioned` 且无 canonical device：诊断显示歧义、没有自动修复按钮，不会覆盖 secret 或创建第二个设备。
- journal/enrollment 丢失但存在 `LINK_SHARED_SECRET`：诊断显示 `stranded_fleet_material`；registry 未证明为空时只提示 pairing 或人工检查。
- 目标 Worker identity 精确匹配且 `prove_unenrolled_registry_empty` 返回 `not_empty` 或 `unproven`：没有 script、secret、设备或 journal mutation。
- 目标 Worker identity 精确匹配且 registry 返回 `empty`：只有显式恢复流程才能继续，且复用严格 secret inherit 和现有 rollback 清理。
- 有 canonical device 的旧 journal：只有本地 config、stored credential、Worker health 和账户/Worker/origin 全部匹配才允许迁移 metadata。
- 无 canonical device 的旧 journal：不能直接续跑，必须回到首台空 fleet 证明。
- 未知 journal schema：原文件保留，诊断拒绝迁移。
- 所有诊断输出不含 token、secret value、pairing credential、设备 credential 或完整 secret inventory 值。
- 设计实现不改 `edge/cloudflare/`、#775 Edge 代码、SQLite schema 或 `SCHEMA_VERSION = 16`。
