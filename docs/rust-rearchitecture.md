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
8. `doctor` 当前真实验证 MCP runtime、Herdr RPC、live API schema 和 validated RPC；
9. Rust、root Node、Cloudflare Edge、site build 和 browser extension smoke 已完成整仓回归。

下一批开发按以下顺序推进：

1. 将 `herdr_methods` / `herdr_call` 接入 Rust MCP HTTP transport，并建立 TypeScript/Rust tool-result parity fixture；
2. 迁移 `herdr_inspect` / `herdr_since` 及 snapshot/event state；
3. 迁移只读 fs/git 工具，再迁 mutation/exec/agent 工具；
4. 实现 Rust supervisor、service manager、Native Messaging host；
5. 实现 GitHub Release updater、generation A/B、rollback；
6. 迁移 relay/link；
7. Rust 覆盖 18 tools 与 production transport 后删除本地 Node runtime 和旧 lifecycle scripts。
