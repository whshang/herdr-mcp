# Herdr 0.9 多机器与双线控制

*让 Herdr saved SSH machine 与 Herdr-MCP Edge device 同时工作，但不混淆两套身份。*

Herdr 0.9 与 Herdr-MCP 解决的是多机器控制的不同层面：

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

Herdr issue [#3732](https://github.com/herdrdev/herdr/issues/3732) 已经记录了 0.9.0 中真实存在的跨机器 workspace-id 歧义，因此即使 saved machine 与 Edge device 指向同一台 workstation，Herdr-MCP 仍保持显式 device affinity。

## Herdr 0.9 当前的程序化接口限制

Connecting Machines TUI 已经可以展示和切换 saved machines，但 Herdr 0.9 暂时还没有在普通 CLI/socket 接口中提供 machine-scoped pane/workspace addressing：

- TUI 中选择远端 machine，不会让另一个本地 `herdr pane ...` / `herdr workspace ...` 命令自动切换到该 machine；
- `herdr --remote <target>` 当前用于附着远程 TUI，不能再与 pane/workspace 子命令组合；
- 本地和远端 server 可以同时存在相同的 pane id。

在 upstream 提供原生 machine-scoped addressing 之前，程序化控制使用显式桥接：

```bash
# 读取 saved profile；始终把 id、target、session 一起保留。
herdr machine list --json

# 显式进入目标 server/session 后再执行 Herdr CLI。
# <target> 通常是 SSH config alias，认证仍由 SSH 管理。
ssh <target> '~/.local/bin/herdr --session <session> pane list'
```

进入远端 server 后，先重新读取该 server 的实时 workspace/pane id，再执行 mutation。不要假设本机取得的 id 在远端仍代表同一资源。

upstream 多机器 Ideas 主线程是 [Discussion #515](https://github.com/herdrdev/herdr/discussions/515)。未来如果 Herdr 提供原生 machine-scoped API，并且 live schema/capabilities 能证明接口存在，Herdr-MCP 应优先使用原生路径；SSH bridge 只作为显式兼容路径，不成为第二套身份系统。

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
