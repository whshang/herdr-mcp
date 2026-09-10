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

当前 stable runtime 以 <https://github.com/whshang/herdr-mcp/releases> 的 `Latest` stable Release 为准。首台设备 `worker bootstrap` 和已有 fleet 的 `worker connect` 已支持 macOS 与 Linux。1.0 Windows x86_64 candidate 正在加入同一条核心接入路径，使用 Herdr named pipe、Windows Credential Manager、HKCU 登录自启动和独立用户进程；在 Windows 原生 UAT gate 通过前仍按 candidate 支持。Browser Native Messaging、self-update、产品级 reinstall/uninstall 不属于本轮 Windows 核心 UAT。

旧版本安装按[Runtime 自升级](runtime-self-upgrade.md)原地升级。已有 Worker、设备关系和健康的 ChatGPT Connector 不需要为了升级当前 runtime 重新创建。

## 第一步：安装原生 herdr-mcp runtime

从 <https://github.com/whshang/herdr-mcp/releases> 下载当前平台的最新 stable `herdr-mcp` binary，放到 `PATH`，然后执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

`install` 会把不可变 generation 放到 `~/.config/herdr-mcp/runtime/` 并让用户 PATH 入口指向 `runtime/current/herdr-mcp`。普通用户不要用 git clone、`npm` 或 `cargo` 安装本机 runtime。

x86_64 Debian 使用静态 `x86_64-unknown-linux-musl` Release 产物。安装器优先使用 `systemd --user`，没有 user systemd manager 时使用托管用户进程 backend。macOS 使用用户级 LaunchAgent，完全磁盘访问只授予稳定的 macOS 专用 broker：`~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`；Linux 与 Windows 不使用这套 TCC/FDA 路径，`sudo` 也不能替代 macOS 隐私权限。

macOS 先执行 `herdr-mcp permissions status`。只有返回 `needs_setup` 时才执行 `herdr-mcp permissions setup`，在“系统设置 → 隐私与安全性 → 完全磁盘访问权限”中为稳定 broker 授权一次，再执行 `herdr-mcp permissions verify`。setup 前不要为了触发弹窗而主动访问 `~/Documents`。host-capable broker 统一承担 MCP 与 native Herdr pane/worktree 的 TCC 责任，正常首次安装只需要这一处 Full Disk Access 授权，避免每个进程分别弹窗。普通 runtime 更新保留这个 broker；只有明确的 compatibility migration 才执行 `permissions setup --upgrade-broker`。macOS 还可能单独要求稳定的 `herdr-mcp-credential-helper` 访问钥匙串；这属于独立安全边界，首次批准一次，后续更新继续复用。平台细节见 [CLI 参考](cli-reference.md)和[故障排查](troubleshooting.md)。本地 doctor 不健康时先解决 runtime / Herdr 问题，再部署公网 Edge。

## 第二步：部署稳定公网 Edge

Herdr 使用 Cloudflare Workers Free 即可，不需要绑卡。没有 Cloudflare 账号时可在登录页免费注册，推荐直接用 Google 登录，步骤最少。

如果 ChatGPT 需要从公网访问工作站，使用 Cloudflare Worker 提供稳定 OAuth/MCP 入口。保持 `workers.dev` 作为零域名 bootstrap/诊断 origin；但如果选定的 Cloudflare Account 已有合适的 active zone，应在 Connector/OAuth 授权前优先使用 `herdr-mcp.example.com` 这类专用 Custom Domain 作为长期稳定身份。没有合适 zone 或用户不采用自定义域名时，继续使用 `workers.dev`，不要阻塞安装。

自动化安装时由 Agent 按 [Agent 安装](agent-install.md) / [Agent 安装合同](agent-install.md) 直接执行这段；协议负责 Token 最小权限、Worker 命名、secret 注入、Account 选择和网络 blocker 的处理边界。

手动/operator 安装同样直接运行已安装 runtime：

```bash
herdr-mcp worker bootstrap
```

该命令负责 Worker 命名、Release artifact 校验、Cloudflare API 直接上传、secret、第一台设备 enrollment 与 readiness 验证。含可信 DNS 恢复的新 runtime 遇到新建 `workers.dev` hostname 本地解析失败时，bootstrap 先尝试 Cloudflare DNS，再尝试 Google DNS；返回 IP 必须通过真实 TLS `/health` Herdr 合同校验，之后才尝试把该 hostname 写成带 Herdr 标记的系统 hosts 记录。Unix 需要时请求 `sudo`，Windows 在 hosts 不可写时需要管理员终端；hosts 持久化失败不会推翻已经验证成功的当前 bootstrap 连接。普通安装不需要源码 checkout、Node.js、npm、Wrangler 或 `wrangler.user.toml`。

同时遵守：

- Cloudflare API Token 只作为临时进程环境变量；
- 不把 Token 写进仓库、日志、截图或 shell history；
- 保持 `workers.dev` 作为零域名 bootstrap origin；已有合适 active zone 时，在 Connector 授权前固化 Worker Custom Domain，不申请通用 DNS Write；
- `LINK_SHARED_SECRET` 作为 Worker secret 保存；
- 工作站只主动建立出站 WSS，不暴露本机公网端口。

普通 bootstrap 合同见 [Agent 协助安装](agent-install.md)。[Cloudflare Edge 部署](cloudflare-edge-deployment.md) 中的源码/Wrangler 流程只保留给维护者与深度运维。


### v0.4.8 的 `workers.dev` DNS 恢复

v0.4.8 还没有自动持久化可信 DNS 结果。bootstrap 已创建 Worker、但本机无法解析它的 `workers.dev` hostname 时，先完成下面的恢复再重跑可恢复的 bootstrap；首次 enrollment 不切到公共 Relay。

```bash
EDGE_ORIGIN="https://<worker>.<account-subdomain>.workers.dev"
HOST="${EDGE_ORIGIN#https://}"; HOST="${HOST%%/*}"

# 先查 Cloudflare DoH。若 cloudflare-dns.com 自身也无法本地解析，给同一
# 请求依次加 --resolve cloudflare-dns.com:443:1.1.1.1、1.0.0.1 重试。
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://cloudflare-dns.com/dns-query?name=${HOST}&type=A"

# Cloudflare DoH 不通再查 Google DoH；固定入口可依次尝试
# dns.google:443:8.8.8.8、8.8.4.4。
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://dns.google/resolve?name=${HOST}&type=A"
```

从返回值选择一个 IPv4 `A` 地址作为 `IP`，写 hosts 前必须先验证：

```bash
curl --noproxy '*' -fsS --resolve "${HOST}:443:${IP}" "${EDGE_ORIGIN}/health"
```

只有 TLS hostname 校验成功，并且 `/health` 确认是预期 Herdr Worker/contract 后才继续。先检查 `/etc/hosts`；已有不带 Herdr 标记的同 hostname 记录时停止，不覆盖。没有冲突时只新增这一条带标记映射，由用户本人批准 `sudo`，然后重跑 bootstrap：

```bash
grep -n "${HOST}" /etc/hosts || true
printf '%s	%s	# herdr-mcp workers.dev %s
' "$IP" "$HOST" "$HOST" | sudo tee -a /etc/hosts >/dev/null
herdr-mcp worker bootstrap
```

这只修改一个 Worker hostname，不改系统 DNS server、代理、网络节点、OAuth issuer 或 MCP public origin。后续含自动恢复的新 runtime 可以在这条带标记记录失效时自行刷新。

## 第三步：验证 Herdr Link

```bash
herdr-mcp doctor
herdr-mcp link status
```

`workers.dev` 直连失败时，先让 `worker bootstrap` / `worker connect` 用上面的可信 DNS + 单 hostname hosts 记录恢复后重试直连。仍失败再复用已有本地代理；内置签名共享 Relay 只作为最后手段。不要为修复这个 hostname 重建 Worker，也不要改系统 DNS。最后用 `doctor` 与 `link status` 验证。

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

1. 在 ChatGPT 插件设置中开启 **Developer mode**；
2. 进入“**插件 → 浏览插件**”，添加自定义插件，名称建议 `herdr`；
3. 粘贴完整的 `${MCP_URL}`，必须包含最后的 `/mcp`；
4. 完成浏览器 OAuth；首次授权页会按浏览器语言自动使用中文、英文或日文，并明确要求先在终端运行批准命令，再按 CLI 提示输入 6 位验证码；
5. 创建或打开一个 **Project**，后续在项目里工作；
6. 每个新会话的第一条消息都先用输入框的 `+` 加号引用 `herdr`，确保这个会话启用插件。

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
