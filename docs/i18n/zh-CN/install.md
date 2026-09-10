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

当前 stable runtime 以 <https://github.com/whshang/herdr-mcp/releases> 的 `Latest` stable Release 为准。首台设备 `worker bootstrap` 和已有 fleet 的 `worker connect` 都支持 macOS 与 Linux。macOS 使用稳定的 Full Disk Access/TCC broker 与 Keychain credential helper；Linux 优先使用 `systemd --user`，不可用时使用托管用户进程 backend。Browser Native Messaging 与 macOS 隐私/TCC 仍属于 macOS 专属能力。Windows Release artifact 仍按 release notes 标注的 preview 范围使用。

旧版本安装按[Runtime 自升级](runtime-self-upgrade.md)原地升级。已有 Worker、设备关系和健康的 ChatGPT Connector 不需要为了升级当前 runtime 重新创建。

## 第一步：安装原生 herdr-mcp runtime

从 <https://github.com/whshang/herdr-mcp/releases> 下载当前平台的最新 stable `herdr-mcp` binary，放到 `PATH`，然后执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

`install` 会把不可变 generation 放到 `~/.config/herdr-mcp/runtime/` 并让用户 PATH 入口指向 `runtime/current/herdr-mcp`。普通用户不要用 git clone、`npm` 或 `cargo` 安装本机 runtime。

x86_64 Debian 使用静态 `x86_64-unknown-linux-musl` Release 产物。安装器优先使用 `systemd --user`，没有 user systemd manager 时使用托管用户进程 backend。macOS 使用用户级 LaunchAgent，完全磁盘访问授予稳定 TCC broker；`sudo` 不能替代这项权限。平台服务与常驻细节见 [CLI 参考](cli-reference.md)和[故障排查](troubleshooting.md)。本地 doctor 不健康时先解决 runtime / Herdr 问题，再部署公网 Edge。

## 第二步：部署稳定公网 Edge

如果 ChatGPT 需要从公网访问工作站，使用 Cloudflare Worker 提供稳定 OAuth/MCP 入口。保持 `workers.dev` 作为零域名 bootstrap/诊断 origin；但如果选定的 Cloudflare Account 已有合适的 active zone，应在 Connector/OAuth 授权前优先使用 `herdr-mcp.example.com` 这类专用 Custom Domain 作为长期稳定身份。没有合适 zone 或用户不采用自定义域名时，继续使用 `workers.dev`，不要阻塞安装。

自动化安装时由 Agent 按 [Agent 安装](agent-install.md) / [Agent 安装合同](agent-install.md) 直接执行这段；协议负责 Token 最小权限、Worker 命名、secret 注入、Account 选择和网络 blocker 的处理边界。

手动/operator 安装同样直接运行已安装 runtime：

```bash
herdr-mcp worker bootstrap
```

该命令负责 Worker 命名、Release artifact 校验、Cloudflare API 直接上传、secret、第一台设备 enrollment 与 readiness 验证。普通手动安装不需要源码 checkout、Node.js、npm、Wrangler 或 `wrangler.user.toml`。

同时遵守：

- Cloudflare API Token 只作为临时进程环境变量；
- 不把 Token 写进仓库、日志、截图或 shell history；
- 保持 `workers.dev` 作为零域名 bootstrap origin；已有合适 active zone 时，在 Connector 授权前固化 Worker Custom Domain，不申请通用 DNS Write；
- `LINK_SHARED_SECRET` 作为 Worker secret 保存；
- 工作站只主动建立出站 WSS，不暴露本机公网端口。

普通 bootstrap 合同见 [Agent 协助安装](agent-install.md)。[Cloudflare Edge 部署](cloudflare-edge-deployment.md) 中的源码/Wrangler 流程只保留给维护者与深度运维。

## 第三步：验证 Herdr Link

```bash
herdr-mcp doctor
herdr-mcp link status
```

如果当前工作站无法直连 `workers.dev`，不要重新部署 Worker。Link 自己负责支持的路径选择，可以复用已有本地代理，也可以使用内置签名共享 Relay。先用 `doctor` 与 `link status` 验证结果；只有这些检查确认存在网络故障时，再进入[故障排查](troubleshooting.md)。代理与 PAC 的细节留在排障页，不占用正常安装主流程。

## 第四步：验证公网路径

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

未带 OAuth 的 `/mcp` 返回 `401` 可以是正确结果。真正的成功标准是：runtime 健康、Link 已连接、Edge `/health` 正常、OAuth metadata 可访问。

## 第五步：在 ChatGPT 添加 herdr Connector

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

如果 `herdr_inspect` 能返回真实工作站数据，基础连接已经可用。

详见 [ChatGPT Connector](chatgpt-connector.md)。

## 第六步：需要浏览器连续工作时再装扩展

浏览器扩展用于 Side Panel 控制中心、workspace binding、长对话连续性和“排队”下一轮消息。基础 MCP 连接不依赖它。

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

macOS 上，`reinstall` 会修复 / 替换 managed Rust runtime，同时保留配置与凭据。Linux 的 runtime 修复使用 `herdr-mcp install`，显式服务移除使用 `herdr-mcp service uninstall`；完整 product uninstall 仍属于 macOS lifecycle 集成。macOS 上，generations 按正常 service GC 保留 active / rollback-safe 集合。`uninstall` 会清理经过强 ownership 校验的 herdr-mcp 本机 runtime/config 状态：默认实例覆盖自己的 service、归属明确的每日 auto-update scheduler、Link/watchdog、Native Messaging host、managed CLI link 和 config root；named instance 只删除自己的 service/watchdog/config。产品卸载会在删除 config root 前，把一个极小的 durable update-fence tombstone 写到 config 之外的用户 cache 中，因此即使 config 已完全删除，已经排队的静默 updater 也不能把 service 复活；只有显式且成功的 install/reinstall 才会清除该 tombstone。它明确保留 Herdr 本体（`herdr`、Herdr service/socket/config），以及由浏览器、Cloudflare、Keychain、TCC 分别管理的授权状态。这类 lifecycle mutation 应从独立终端执行，不要在依赖目标 service 的 managed `herdr_exec` 会话内部执行。
