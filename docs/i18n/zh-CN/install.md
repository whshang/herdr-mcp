# 手动安装

*从一台 Herdr 工作站到可用的 Web AI 开发环境。*

> **定位：人工/运维参考。** herdr-mcp 的主安装协议直接写给执行 Agent，见 [Agent 安装](agent-install.md) 和 [Agent 安装合同](agent-install.md)。本页用于人工检查、排障或需要理解每个阶段时查阅，不再提供“复制一段提示词给某个 Coding Agent”的入口。

目标是把一台本地工作站接到 ChatGPT / Web AI，同时保持代码和真实执行环境留在自己的机器上。

## 安装前确认

### 1. Herdr 已经可用

```bash
herdr --version
herdr api schema >/dev/null
```

如果 Herdr 未安装，推荐直接使用 Herdr 官方 stable 安装器：

```bash
# macOS / Linux
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

安装后重新执行 `herdr --version`。Herdr 本体的详细安装行为以 <https://herdr.dev/docs/install/> 为准。

### 2. 明确要连接的客户端

- ChatGPT / 其它公网 Web AI → 需要 Cloudflare Edge + 出站 Herdr Link；
- 只在本机用 MCP 客户端 → 可以直接连接 loopback runtime，不需要 Cloudflare；
- 浏览器扩展 → 是基础 Connector 连通后的可选能力，不是第一次安装前置条件。

## 支持平台

当前已发布的 stable runtime 是 **`v0.4.3`**。稳定 TCC broker 已完成跨 generation 授权验证；Apple Developer ID 仅为可选加固。v0.4.3 的安装流程继续保持同一个 broker compatibility revision，并在会轮换的 runtime service 启动前先确保固定 broker 已存在。macOS 交互式首次安装会直接打开 **完全磁盘访问（Full Disk Access）**，但授权动作仍必须由用户本人在系统设置中确认；`herdr-mcp permissions setup` 可再次打开同一设置页，`herdr-mcp permissions verify` 用于验证。普通 runtime generation 更新不会重写同 revision 的 broker。最充分的 clean-machine qualification 证据仍来自 `v0.4.0` 的 **macOS Apple Silicon** 验收。Windows x64 Release binary 已提供，但 Windows 端到端 UAT 仍在继续；Linux runtime 暂不作为当前 stable 的正式支持面承诺。

macOS 的 v0.4.3 还把生产设备凭据从会轮换的 runtime 代码中分离出来：Keychain 读写统一经过固定的 `~/.config/herdr-mcp/herdr-mcp-credential-helper`。已有安装第一次迁移到这个 helper 时，macOS 可能只需要一次明确的钥匙串授权；该预检发生在 service / Link mutation 之前，弹窗被忽略或拒绝时会直接中止，不会进入反复重启 Link、反复弹窗的状态。普通 runtime 升级会保留同 compatibility revision 的 helper，因此每个新的 `runtime/generations/rust-*` 不再分别成为新的 Keychain client。这个 credential helper 与上面的完全磁盘访问 / TCC broker 是两个独立的稳定身份。

## 第一步：安装原生 herdr-mcp runtime

从 <https://github.com/whshang/herdr-mcp/releases> 下载当前平台的最新 stable `herdr-mcp` binary，放到 `PATH`，然后执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

`install` 会把不可变 generation 放到 `~/.config/herdr-mcp/runtime/` 并让用户 PATH 入口指向 `runtime/current/herdr-mcp`。普通用户不要用 git clone、`npm` 或 `cargo` 安装本机 runtime。

macOS 上服务仍然是普通用户级 LaunchAgent；不需要 `sudo`，而且管理员权限本身也不能替代 TCC 授权。需要授予完全磁盘访问的是固定的 `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`，不是持续变化的 `runtime/generations/rust-*`。非交互式安装会先准备好 broker，但不会强行打开系统设置；如果尚未授权，在用户终端执行一次 `herdr-mcp permissions setup` 即可。

## 第二步：先把本地 runtime 跑通

至少确认：

```bash
herdr-mcp doctor
herdr-mcp status
```

如果本地 doctor 不健康，不要继续部署公网 Edge。先解决本机 runtime / Herdr 问题。

## 第三步：部署稳定公网 Edge

如果 ChatGPT 需要从公网访问工作站，使用 Cloudflare Worker 提供稳定 OAuth/MCP 入口。首次安装优先 `workers.dev`，除非你明确需要自有域名。

自动化安装时由 Agent 按 [Agent 安装](agent-install.md) / [Agent 安装合同](agent-install.md) 直接执行这段；协议负责 Token 最小权限、Worker 命名、secret 注入、Account 选择和网络 blocker 的处理边界。

手动执行时，至少遵守：

- Cloudflare API Token 只作为临时进程环境变量；
- 不把 Token 写进仓库、日志、截图或 shell history；
- 默认 `workers_dev = true`、`routes = []`；
- Worker 名使用仓库 helper：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

- `LINK_SHARED_SECRET` 作为 Worker secret 保存；
- 工作站只主动建立出站 WSS，不暴露本机公网端口。

详细手动协议见 [Agent 协助安装](agent-install.md) 与 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## 第四步：安装并验证 Herdr Link

```bash
herdr-mcp link install
herdr-mcp link status
```

如果 `workers.dev` 在当前网络不可达，优先复用已有代理：

```text
HERDR_LINK_PROXY
HTTPS_PROXY / https_proxy
HTTP_PROXY / http_proxy
ALL_PROXY / all_proxy
```

macOS 也会读取 `scutil --proxy`。不要还没验证代理就把网络问题扩大成 DNS / Custom Domain 变更。

## 第五步：验证公网路径

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

未带 OAuth 的 `/mcp` 返回 `401` 可以是正确结果。真正的成功标准是：runtime 健康、Link 已连接、Edge `/health` 正常、OAuth metadata 可访问。

## 第六步：在 ChatGPT 添加 herdr Connector

这一步需要用户本人操作。让 Coding Agent 暂停并指导：

1. 打开 ChatGPT 设置中的 Apps / Connectors；
2. 当前 UI 需要时开启 Developer mode；
3. 添加自定义 MCP Connector，名称建议 `herdr`；
4. URL 填部署后的 `${MCP_URL}`，必须以 `/mcp` 结尾；
5. 完成浏览器 OAuth；
6. 在新会话或 Project 中启用该 Connector。

然后先做只读验证：

```text
分析我的 Herdr 里有哪些项目。只读，不要修改。
```

如果 `herdr_inspect` 能返回真实工作站数据，基础闭环就已经可用。

详见 [ChatGPT Connector](chatgpt-connector.md)。

## 第七步：需要浏览器连续工作时再装扩展

浏览器扩展用于 Side Panel 控制中心、workspace binding、长对话连续性和“排队”下一轮消息。它不是基础 MCP 闭环的必需项。

扩展分为三种身份：**STORE / STANDALONE / DEV**。v0.4.2 的 Native Host 只支持 Store/DEV ownership；v0.4.3+ 增加固定身份的 STANDALONE，作为正式手动/GitHub 分发路径。

- STORE：普通用户默认，Chrome Web Store 固定身份与更新；
- STANDALONE：v0.4.3+，固定非 Store 身份；Store 不可用或用户明确选择独立分发时使用；
- DEV：仅源码开发，Load unpacked repo/worktree `extension/`，身份随路径派生。

安装/切换后运行：

```bash
herdr-mcp native-host status
```

要求 active channel、extension identity、Native Host runtime generation 与预期一致。不要把 DEV 当普通用户 fallback，也不要把 GitHub/manual standalone package 称为 dev。

详见 [浏览器扩展](extension.md) 与 [浏览器控制中心](browser-control-center.md)。

## 什么叫“装好了”

至少满足：

- `herdr --version` 正常；
- `herdr-mcp doctor` 健康；
- Herdr Link 已连接；
- Edge `/health` 正常；
- ChatGPT OAuth 完成；
- 新会话能调用 `herdr_inspect` 读取真实工作站；
- 可选扩展如果已安装，`herdr-mcp native-host status` 正常且 Side Panel 能看到 workspace。

## 自动化执行入口

需要自动完成安装时，执行 Agent 直接读取 [Agent 安装](agent-install.md)；需要完整权限、安全和异常边界时读取 [Agent 安装合同](agent-install.md)。

更深入的内容按需查看：

- [故障排查](troubleshooting.md)
- [架构](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge 部署](cloudflare-edge-deployment.md)

维护者 UAT、GA gate 和发布证据不属于普通用户安装流程。

## 修复、重装与卸载

v0.4.3+ 应使用产品级 lifecycle 命令，不要手工删除 launchd plist 或 runtime 目录：

```bash
herdr-mcp reinstall
herdr-mcp uninstall
```

`reinstall` 会修复 / 替换 managed Rust runtime，同时保留配置与凭据；generations 按正常 service GC 保留 active / rollback-safe 集合。`uninstall` 会清理经过强 ownership 校验的 herdr-mcp 本机 runtime/config 状态：默认实例覆盖自己的 service、归属明确的每日 auto-update scheduler、Link/watchdog、Native Messaging host、managed CLI link 和 config root；named instance 只删除自己的 service/watchdog/config。产品卸载会在删除 config root 前，把一个极小的 durable update-fence tombstone 写到 config 之外的用户 cache 中，因此即使 config 已完全删除，已经排队的静默 updater 也不能把 service 复活；只有显式且成功的 install/reinstall 才会清除该 tombstone。它明确保留 Herdr 本体（`herdr`、Herdr service/socket/config），以及由浏览器、Cloudflare、Keychain、TCC 分别管理的授权状态。这类 lifecycle mutation 应从独立终端执行，不要在依赖目标 service 的 managed `herdr_exec` 会话内部执行。
