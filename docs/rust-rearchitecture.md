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
32. 真实 candidate HTTP smoke 已验证 read/list/grep/git 正常工作，并验证 `/etc/hosts`、`.git/config`、`git diff ../...` 分别被 managed-root/secret/escape gate 拒绝；当前 Rust 单测为 56/56。

下一批开发按以下顺序推进：

1. 完成 `herdr_inspect` parity：补 build/runtime metadata、exec-session state，并把已迁工具数量/候选状态纳入 diagnostics；
2. 迁移 `herdr_fs_image`，随后统一实现 mutation safety gate，再迁 `herdr_fs_patch/edit/write`；
3. 迁移 `herdr_exec_start/read/kill` 和短命令 `herdr_exec`，保持 bounded output、busy-agent 和 uncertain-delivery 语义；
4. 迁移 `herdr_prompt` 与 `herdr_skill`，逐个消除剩余 `native_tool_pending`；
5. 补齐 persistent GET/SSE、stateful session/auth compatibility，使 Rust HTTP transport 通过现有 Connector transport parity tests；
6. 实现 Rust supervisor、service manager、Native Messaging host；
7. 实现 GitHub Release updater、generation A/B、rollback；
8. 迁移 relay/link；
9. Rust 覆盖 18 tools 与 production transport 后删除本地 Node runtime 和旧 lifecycle scripts。
