# 平台与兼容性支持矩阵

*生产支持代表经过验证的安全边界，不代表“二进制能够启动”就自动获得支持。*

本页是 Herdr-MCP 1.0 的公开兼容性约定。只有下表明确标为**生产支持**的平台才属于正式 workstation 支持范围。候选或不支持的平台不得被描述为与生产平台等价。

## 协议与运行时兼容性

Herdr-MCP 有多层彼此独立的协议，它们不会共用一个版本号。

| 层 | 当前生产身份 | 有界兼容范围 | 失败关闭规则 |
| --- | --- | --- | --- |
| 公共 MCP 客户端协议 | `initialize` 协商接受 `2025-11-25`、`2025-06-18`、`2025-03-26`、`2024-11-05`、`2024-10-07` | ChatGPT/OpenAI discovery 可公布探测版本 `2026-07-28`；该值不是额外的 `initialize` 协议版本 | 缺失或无法识别的 `initialize` 协议值会明确降到 legacy baseline `2025-11-25`，不会把未列出的协议对外声明为支持 |
| 公共 Edge 工具契约 | epoch **3**，**19** 个 action | 旧对话可能保留历史工具快照，但 Edge 只发布当前公共 catalog | 不在当前公共契约内的工具会被拒绝 |
| Workstation Runtime Execution Contract | epoch **2**，**18** 个工具 | 仅精确冻结的 epoch **1**、**17** 个工具作为紧邻上一代 rollback/旧会话基线 | 其他 epoch/hash 组合在 workstation 执行前直接拒绝 |
| Relay wire protocol | 数字 `protocol_version = 1` | 无 | 缺失、字符串形式或未知版本会在写入 Relay 状态前被拒绝 |
| 持久运行时状态 | 当前二进制 schema；自动激活的 Release 必须声明相同且可 rollback 的 state schema | 旧 store 通过追加式迁移事务性升级 | 旧二进制拒绝比自己更新的 store；自动 updater 对 state schema 或 runtime contract identity 不精确匹配的 Release 拒绝激活 |

当前 ChatGPT 路径按无状态 Streamable HTTP 客户端验证：`openai-mcp` 的 `initialize` 与 `tools/list` 使用所需 SSE framing，`notifications/initialized` 可正常处理，Connector 重连不依赖保留过期的 `Mcp-Session-Id`。Workstation reconnect、Durable Object rehydration、heartbeat hibernation 与 reconnect grace 分别有回归测试，因此浏览器重新连上并不等于 workstation mutation 可以安全重放。

## N、N-1 与 N+1 规则

`N` 表示当前部署的 Edge 契约和当前已验证的 runtime generation。

- **N → N** 是常规生产路径。候选 runtime 只有在健康检查和精确 runtime execution contract 验证通过后才能切换流量；Release 更新还要求 manifest 与本地 durable-state schema 匹配。
- **N-1 runtime execution** 只是有界 rollback，不是任意历史版本兼容承诺。当前仅精确冻结的 epoch-1 runtime contract 与当前 epoch 2 同时接受，并且按 epoch + hash 校验；更老的任意 catalog 不会被接收。
- **N+1 runtime 或 control plane** 不做猜测兼容。未来 epoch、Relay protocol 或 durable-state schema 必须经过显式迁移和验证后才能使用；当前实现会拒绝未知 contract pair 和未来 schema，而不是尝试“尽量降级运行”。
- **durable state 升级是单向迁移。** SQLite migration 采用追加式、事务性执行。只有旧 binary 仍能读取迁移后的状态时 rollback 才安全，因此自动 Release 激活要求声明的 state schema 保持 rollback-compatible。Runtime rollback 只切回执行所有权，不会撤销已经发生的 Git、文件、远程服务或 Agent 副作用。

实现升级与 contract migration 是两种不同操作。普通修复可以在不改变 `tools/list` 的情况下升级 runtime；增加或移除模型可见工具必须显式升级 contract epoch，并重新验证客户端工具快照。

## 平台支持

| 平台 | 状态 | 文件系统边界 | 进程 / 凭据边界 | 网络边界 | 验证说明 |
| --- | --- | --- | --- | --- | --- |
| macOS | **生产支持** | 远程文件/Git mutation 仅允许作用于实时 managed Git root，并叠加 read-only/write-root、dirty/busy、secret-path 等 gate。Shell 仍以登录用户身份执行，**不是 sandbox**。涉及 TCC 的原生访问使用长期稳定的 Herdr-MCP broker responsibility boundary。 | 用户级 launchd 管理 runtime 与 production Link；immutable generation 通过 `runtime/current` 切换。本地浏览器 IPC 使用 Herdr-MCP 所有、mode-0600 的 Unix socket；device/runtime credential 使用 macOS 凭据边界。 | Runtime MCP 仅绑定 loopback 并要求本地 bearer；workstation 主动建立经过认证的 outbound Link，不公开 workstation 入站端口。 | 当前主要生产路径；TCC broker/process、generation rollback、OAuth/Connector 和真实 ChatGPT 路径均有专项回归/UAT。 |
| Linux（原生 Debian 类主机） | **生产支持** | 与 macOS 相同的 managed-Git-root 和 mutation gate，通过 Unix 文件所有权/权限执行。Linux 没有 macOS TCC；shell 同样是 workstation 用户的非 sandbox shell。 | 优先 `systemd --user`；没有 user systemd manager 时使用已验证的 current-user process backend。Device credential 与 Herdr-MCP 所有的 service/control 文件使用私有用户级存储与权限。 | 同样使用 loopback runtime + authenticated outbound Link。 | 原生 Debian 的 existing-fleet 安装、Link/service lifecycle、updater apply 已验证。产品目前不在 Linux 上自带常驻每日更新 scheduler；手动或外部 scheduler 不改变安全边界。 |
| Windows x86_64 | **候选——非生产支持** | 在真实 Windows 机器完成 managed-root/path 语义验证前，不做 1.0 生产承诺，包括 Windows 原生路径边界。 | 候选实现使用 current-user process、Windows Credential Manager、Startup-folder 登录启动项，并做 process ownership fencing。在 [#364](https://github.com/whshang/herdr-mcp/pull/364) 完成实体机 UAT 并合并之前，这些只能算验证中的实现，不是正式支持保证。 | 候选实现沿用 loopback + authenticated outbound Link。 | 升级为生产支持前必须验证：保留身份的重装、Herdr 恢复、模拟登录恢复、Connector/device 保持，以及真实 ChatGPT → Edge → Link → Windows → Herdr 只读链路。若要声明 UNC/网络路径能力，也必须先加入对应 fixture。 |
| WSL | **不支持** | 尚未验证 WSL host/guest 文件系统、Windows drive mount、symlink 与权限模型可等价于原生 Linux。 | 不声明 WSL 专用 service manager、credential、browser IPC 或 host/guest process ownership 边界。 | WSL/NAT/Windows-host 网络关系不属于已验证的 production Link 边界。 | Linux binary 即使能在 WSL 启动，也不能作为支持证据。在单独的 qualification 明确这些边界前，不应把 WSL 用作安全敏感的 Herdr-MCP workstation 目标。 |

## “生产支持”不等于本地 sandbox

生产支持表示 Herdr-MCP 自己负责的边界明确并经过验证，并不把开发电脑变成容器沙箱：

- `herdr_fs_*` 和 managed Git 操作受项目 root gate 约束；
- `herdr_exec*` 有意以 workstation 用户身份执行，可以访问该用户本身可访问的资源；
- Herdr pane/agent 是同一 workstation 安全模型下的持久本地进程；
- Edge OAuth、device authorization、outbound Link authentication 保护远程入口，不会降低本地用户权限。

如果某个平台无法提供等价且经过验证的边界，正确做法是标记为候选或不支持，而不是静默降低要求。

## 候选平台晋级条件

候选平台只有在待合并的精确实现同时满足以下条件后，才能改为生产支持：

1. service/process lifecycle 能通过安装、restart/login recovery、失败激活，并且不会误接管无关进程；
2. credential 在重装/恢复过程中保持私密且继续绑定到同一 device；
3. managed-root 文件操作覆盖该平台原生路径语义，包括所声明的 absolute/relative path，以及任何声称支持的 UNC/网络文件系统行为；
4. runtime/Link 在 workstation 边界保持 loopback/outbound-only；
5. `initialize`、`tools/list`、reconnect，以及一次真实 Web-AI → Edge → Link → workstation 只读请求通过；
6. 该 OS 的 required clean-machine CI 通过，hosted CI 无法证明的边界由实体机 UAT 覆盖；
7. 未验证的子环境继续明确标记为 unsupported，不能自动继承更宽泛的平台支持标签。

相关文档：[架构](architecture.md)、[Runtime 升级](runtime-self-upgrade.md)、[故障排查](troubleshooting.md)和维护者 [Release model](../../release-model.md)。
