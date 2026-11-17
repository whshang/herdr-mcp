# Agent 安装

*给 Coding Agent 的最小端到端执行合同。人工细节见[安装手册](install.md)，故障诊断见[排障](troubleshooting.md)。*

> **执行角色：Agent。** 读取本页后直接执行，遇到对应 blocker 再打开链接文档。用户只负责登录、系统授权、Cloudflare Token 创建/账户或域名选择、ChatGPT OAuth。

## 1. 执行规则

1. **先规划，再调用。** 第一次 mutation 前先在模型内部确定机器/fleet 状态、人类边界、步骤依赖和最终验收；已知且独立的读取合并成一轮；同项目同安全边界的确定性 shell/Git 步骤放进一个有界执行调用并完成局部校验。只有当结果会改变参数/安全判断、需要用户动作、或 mutation 交付不确定时才重新规划；不为证明“没变化”而轮询。
2. **不破坏已有工作。** 禁止对无关 checkout 做 `reset --hard`、`clean -fd`、覆盖 dirty 文件或重建现有 fleet。
3. **普通安装只用当前 Stable GitHub Release。** 不 clone 本仓库，不用 `npm`/`cargo` 构建本机 runtime；源码开发是另一条流程。
4. **秘密只短暂使用。** Cloudflare Token 不回显、不写 Git、普通日志或 shell history；只通过当前进程环境或 CLI 隐藏输入传递。最终不要把本机 `HERDR_MCP_TOKEN` 或 Cloudflare Token 放进 ChatGPT。
5. **不修改用户网络环境。** 不切代理、不改系统 DNS 或网络节点。唯一例外是 §6 中经过验证的单 Worker hosts 记录。
6. **只在人类边界暂停。** 可自动判断和执行的步骤继续做；需要 Cloudflare 登录/Token、macOS 权限确认、多个 Account/zone 无法安全选择，或 ChatGPT OAuth 时再一次性提示用户。

## 2. 先判断：第一台 Worker 还是加入已有 fleet

按以下顺序判断，不要反复询问：

- 用户已经给出 pairing address：这是**加入已有 Herdr Worker**，跳到 §5。
- 本机已有有效的 Herdr device identity：保留现有 fleet，只做修复/验证，不新建 Worker。
- 否则按**第一台 Worker**准备；拿到 Cloudflare Token 后列出 `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts`，若该 Account 已有 Herdr Worker，则停止新 Worker mutation，要求由任意已登记设备创建 pairing，再走 §5。
- 绝不能在当前正在安装的这台电脑上运行 `herdr-mcp worker pair` 来探测 fleet。
- existing-fleet 修复失败时禁止 fallback 到随机后缀 Worker、第二个 R2 桶或第二个 Connector，除非用户明确改变 fleet 意图。

## 3. 本机安装阶段

不要因为 `herdr` 缺失或 Herdr Server 尚未启动就暂停安装。直接从 <https://github.com/whshang/herdr-mcp/releases> 取得当前 **Latest stable** 的 herdr-mcp 平台二进制，放入用户 `PATH`（推荐 `~/.local/bin/herdr-mcp`），并执行：

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

`herdr-mcp install` 自己负责 Herdr 依赖恢复：按 `HERDR_BIN`、稳定路径、Homebrew/`/usr/local`、PATH 查找；已有 Herdr 验证并尝试更新，缺失时调用 <https://herdr.dev/tui/> 官方安装器，最后确认 Server/API 可达。除非命令明确报告 dependency-recovery 失败，不要再另起手工安装流程。这里的 “Herdr” 指 Herdr TUI（`herdr`）；不要为 herdr-mcp 安装 Herdr app 或 herdr-core（`hc`），v1.1 不支持该独立 runtime。Windows Candidate UAT 必须使用正在验收的精确 candidate artifact。

如果 `~/.local/bin/herdr-mcp` 已存在但交互 shell 找不到它，记为 `installed_but_not_on_shell_path`，修复用户 PATH 后用新 shell 验证。不要重复安装，也不要创建第二个 PATH owner。只有实际需要修 PATH 时再打开[故障排查](troubleshooting.md)。

macOS 在 Cloudflare 工作前先执行 `herdr-mcp permissions status`；仅返回 `needs_setup` 时，集中引导用户为稳定 broker 授予完全磁盘访问权限一次，再执行 `herdr-mcp permissions verify`；不先探测受保护路径，`sudo` 不能替代。broker 只承担 MCP 文件/Git 的 TCC；pane/Agent 的 TCC 取决于执行宿主。Linux 使用 release 自带 user-service / process backend；Windows 用当前用户 Startup 快捷方式 + Credential Manager。普通安装不需要 Node.js、Wrangler、npm 或 Cargo。

### 可选：快速语义判断

核心 runtime 健康后，向用户说明快速 decision 模型属于可选能力。默认推荐参考 [TypeSafe.ai](https://typesafe.ai/)，它不是必选 Provider，也不能阻塞安装。用户可以在那里创建 API Key，然后执行：

```bash
herdr-mcp semantic setup
herdr-mcp semantic status
```

`semantic setup` 通过隐藏终端输入读取 API Key，绝不从 argv 接收。其他兼容 Provider 可通过 `--name`、`--protocol`、`--url`、`--model` 配置。用户跳过这一项时，Herdr 继续保持原有确定性行为，安装流程正常继续。

## 4. 第一台 Worker：Cloudflare + bootstrap

Herdr 用 Cloudflare Workers Free 即可，不需要绑卡；没有账号就先告知可免费注册，推荐 Google 登录。需要 Token 时打开 <https://dash.cloudflare.com/profile/api-tokens>，推荐 **Edit Cloudflare Workers** 模板并限定到本 Account；自定义 Token 需 **Account Settings → Read** 和 **Workers Scripts → Write/Edit**。`workers/subdomain` 返回 403 时指出缺失权限，不要无根据扩大权限。**核心安装不需要 R2**；仅启用 artifact relay 时加 Workers R2 Storage。

Token 仅放入当前进程的 `CLOUDFLARE_API_TOKEN` 或交给 `worker bootstrap` 的隐藏输入。不要把 Token 写进命令行字面量、配置仓库或普通日志。

直接运行：

```bash
herdr-mcp worker bootstrap
```

这个命令负责 Cloudflare API 预检、Account / `workers.dev` subdomain、现有 Herdr Worker 检测、release manifest 与 `herdr-edge-<version>.mjs` artifact attestation、Worker/DO bootstrap、首台 canonical device enrollment、credential 保存和 production Link 对齐。不运行 Wrangler，不需要源码 checkout。

首台 device 凭据与配置已保存后若激活失败，先修复本机错误，在同一台电脑、同一 OS 账号下重跑 `worker bootstrap`。保留 journal 与凭据：重试会直接认证同一设备，不重新配对、不重新下发 fleet 秘密。缺失、吊销或不匹配的凭据 fail closed；已吊销或删除的凭据无法通过此路径恢复。

公网入口只在 Connector 创建前确定一次：

- Account 只有一个明显合适的 active zone，优先使用专用 Custom Domain，例如 `https://herdr-mcp.example.com/mcp`；
- 多个 materially different zones 时只问一次用户选哪个；
- 没有合适 zone、用户不想使用或 hostname 冲突时，直接保留 `workers.dev`，例如 `https://herdr-edge-device.username.workers.dev/mcp`。

正常 Custom Domain 路径不要求通用 DNS Write。后续网络修复不得静默改变已经选定的 OAuth/MCP public origin。

## 5. 加入已有 Worker

已登记设备先生成 pairing；新电脑只消费 pairing：

```bash
herdr-mcp worker connect "<pairing-address>"
```

CLI 要求 6 位验证码时再向用户索取；显示名默认取电脑名，仅用户明确要求时才传 `--name`。此流程不部署新 Worker、不建第二个 Connector，不把 fleet 长期秘密复制到新电脑。

登记后的身份使用不可变的 `device_id`，例如 `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`：`dev_` 加 26 字符 ULID。显示名与身份分开管理。**不要**自造 `WORKSTATION_ID`，也不要从 hostname 推导身份。

详见[加入已有 fleet](existing-worker-connect.md)。

## 6. Link 与网络

`worker bootstrap` / `worker connect` 会建立单设备凭据并对齐 production Link。Agent 只需要验证：

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

没有 Custom Domain 时先直连 `workers.dev`；解析失败时，runtime 依次查询 Cloudflare DNS、Google DNS，候选 IP 通过真实 TLS `/health` Herdr 校验后才写成带标记的单 hostname hosts 记录。Unix 需要时由用户批准交互式 `sudo`，Windows 用管理员终端。随后重试 Link 直连、已有本地代理，内置签名共享 Relay 为最后手段。不得改系统 DNS、网络节点、OAuth issuer 或 public MCP origin。

## 7. 最终验收

把读操作尽量合并成一轮最终验证，不要在每个安装步骤后重复同一组检查。完成必须同时证明：

- `herdr-mcp status` / `doctor` 健康；
- `herdr-mcp link status` 显示已登记 production Link 在线；
- canonical public origin 的 `/health` 与 OAuth discovery 正常；
- 本机存在 canonical `dev_<ULID>` device identity；
- 一条真实认证 MCP 请求能够从公网 origin 往返到当前工作站。

随后打开 ChatGPT **设置 → 应用与连接器（Apps & Connectors）**（旧版“**插件 → 浏览插件**”），启用 **开发者模式（Developer mode）**，点右上角 **+ → Create APP**，填入完整 `https://…workers.dev/mcp`，完成 OAuth 并在本机终端批准。建议名称 `herdr`（支持自定义）。在 ChatGPT Project 中工作；每个新会话的第一条消息用输入框 `+` 加号选择该 App。

Chrome 扩展 / Native Messaging 是可选增强：需要 v1.1 的 Control Center、排队下一轮、无 ID 接力、多平台 WebChat（`ChatGPT`、`Claude`、`Grok`、`Doubao`）、受控 `browser-page` 或项目浏览器技能时，按[扩展文档](extension.md)与[WebChat 指南](local-agent-webchat-control.md)从商店安装或执行 `herdr-mcp extension standalone install`。

## 8. 清理、升级与报告

结束时 unset `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`，删除临时凭据文件。只报告非敏感事实：runtime version、device name/id、Worker 名、最终 public origin、Link 状态、`/health`、OAuth 与 MCP E2E 结果。后续 v1.1 日常升级使用带签名证明校验的 `herdr-mcp update`（提示 Worker 落后时再单独运行 `herdr-mcp worker update`，见[自更新说明](runtime-self-upgrade.md)）。

遇到权限、网络、OAuth、已有 Worker ownership 或交付不确定时，按需打开[安装手册](install.md)或[排障](troubleshooting.md)对应章节，不要提前加载整份维护手册。
