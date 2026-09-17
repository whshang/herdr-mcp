# Herdr 0.9.1 多机器与双线控制

*让 Herdr saved SSH machine 与 Herdr-MCP Edge device 同时工作，但不混淆两套身份。*

Herdr 0.9.1 与 Herdr-MCP 解决的是多机器控制的不同层面：

| 控制面 | 身份 | 传输 | 主要用途 |
| --- | --- | --- | --- |
| Herdr saved machine | machine profile id + SSH target + Herdr session | SSH / Herdr remote client | 人工多机器 TUI、维护、bootstrap、UAT、恢复 |
| Herdr-MCP Edge device | immutable `dev_*` `device_id` + device credential | outbound Link → Edge → MCP | ChatGPT/Web-AI 路由、设备 affinity、delivery evidence |

同一台物理机可以同时拥有两套身份，但不要把它们合并。显示名称或 hostname 相同并不能证明两条记录就是同一台 workstation。

## 两条路径指向同一个 session 时，哪些状态是共享的

如果 saved SSH machine 与 Edge device 最终都连接到同一个 Herdr server 和同一个 Herdr session，它们操作的是同一份实时资源：

- workspace/tab/pane 拓扑；
- Agent 与 Agent 状态；
- 实际 PTY 与前台进程；
- terminal 输出；
- Git working tree 与文件系统状态。

这不是两份状态之间的复制或最终一致性。两条控制路径实际连接的是同一个 Herdr server/session，因此一条路径完成的变化，另一条路径重新读取时会直接看到。

如果 SSH profile 指向另一个 Herdr session，即使仍是同一台物理机，其 Herdr workspace/pane 状态也属于另一套 session。

Edge 专属状态不会和 saved machine profile 同步：`device_id`、device credential、Link online/offline、runtime generation、reconnect evidence 与 mutation delivery state 都属于 Herdr-MCP。machine profile id、SSH target、session、enabled 状态和 TUI selected 状态属于 Herdr client。

## 不要用裸 workspace/pane id 代表一台机器

Herdr 的 server id 只在对应 server/session 内有意义。两台机器完全可能同时拥有 `w1`、`w1:t1`、`w1:p1`。

走 Edge 时，把 `device_id` 或带 device affinity 的 `herdr_ref_*` 与 workspace/pane 引用一起保存。走 saved-machine 路径时，把 machine profile + Herdr session 与 workspace/pane id 一起解释。不要把裸 `w1:p1` 当作跨机器全局唯一 id。

Herdr 0.9.1 已修复 issue [#3732](https://github.com/herdrdev/herdr/issues/3732) 记录的客户端同 ID 过滤问题。workspace/pane id 仍只在对应 Herdr server/session 内有意义，因此即使 saved machine 与 Edge device 指向同一台 workstation，Herdr-MCP 仍保持显式 device affinity。

## Herdr 0.9.1 原生 machine-scoped CLI

Herdr 0.9.1 已提供 saved SSH machine 的原生 CLI forwarding：

- `herdr --machine <label-or-id> <command>` 直接对该 saved machine 配置的 Herdr session 执行支持的 API 命令，不要求先打开远程 Herdr window；
- workspace、worktree、tab、pane、agent 命令都可以通过这一入口寻址；
- 依赖 CLI forwarding 前，要把本机和远端都升级到 Herdr 0.9.1；
- 远端命令失败时保持失败，禁止回退到 Local；
- `herdr --remote <target>` 继续承担交互式远程 TUI attach。

程序化控制以 saved profile 为路由身份，并在 mutation 前重新发现远端 id：

```bash
# 读取 saved profile；始终把 id、target、session 一起保留。
herdr machine list --json

# 通过 saved machine profile 路由 API 命令。
herdr --machine <label-or-id> workspace list
herdr --machine <label-or-id> pane list --workspace <remote-workspace-id>
herdr --machine <label-or-id> agent list
```

不要假设本机取得的 id 在远端仍代表同一资源。对于仍停留在 0.9.1 之前、需要 bootstrap 或修复的 endpoint，显式 SSH 执行只作为兼容/恢复路径；远端升级完成后，日常程序化控制回到 `--machine`。

upstream 多机器 Ideas 主线程是 [Discussion #515](https://github.com/herdrdev/herdr/discussions/515)。saved-machine profile 仍属于 SSH/Herdr 身份；原生 `--machine` forwarding 不会把它与 Herdr-MCP Edge `device_id` 身份合并。

## ChatGPT 应该走哪条路径

如果 workstation 已经 enroll 为 Edge device，ChatGPT/Web-AI 默认走 Edge。Edge 提供 immutable device identity、generation fence、reconnect state 和 mutation delivery evidence。

saved-machine/SSH 路径用于明确的维护、首次 bootstrap、Debian/Linux UAT，或用户指定的恢复操作。不要把失败的 Edge mutation 静默切换到 SSH 重发：

- `delivery_state=not_delivered`：在重新验证连接和实时状态后，可以通过明确选择的路径安全重发；
- `delivery_unknown`、delivered/uncertain 或缺少 delivery evidence：先检查实时 pane/Git/runtime/resource 状态，禁止盲目重放 mutation。

Herdr TUI 当前选择哪台 machine，不会改变 Edge call 的目标。Edge call 始终绑定显式/默认的 Herdr-MCP device，以及返回的 `herdr_ref_*` affinity。

## 如何验证两条路径确实是同一个底座

在专门用于测试的 workstation 上，优先使用只读证据：

1. 确认 saved-machine profile 指向预期 SSH target 和 Herdr session。
2. 确认 Edge fleet 中存在目标 immutable `device_id`，并且在线。
3. 分别从两条路径读取 workspace/pane。
4. 分别读取 `pane.process_info`。如果 pane id、shell PID / foreground process group、可执行文件与 cwd 全部一致，这是两条路径连接同一 PTY 的强证据，而不仅仅是碰巧存在两个同名 pane。
5. 如仍需验证，可在空闲测试 shell 中从一条路径写入无副作用 marker，再从另一条路径读取，并反向再做一次。

不要在生产/忙碌 Agent pane 中做 marker 测试，也不要为了证明两条现有控制路径共享状态而执行 pairing/revoke。
