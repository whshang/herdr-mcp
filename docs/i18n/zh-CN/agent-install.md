# Agent 安装

*给 Coding Agent 的最小端到端执行合同。人工细节见[安装手册](install.md)，故障诊断见[排障](troubleshooting.md)。*

> **执行角色：Agent。** 读取本页后直接执行。不要为了安装而递归阅读所有链接文档；只有遇到对应 blocker 时再打开详细手册。用户只负责必须由本人完成的登录、系统授权、Cloudflare Token 创建/账户或域名选择、ChatGPT OAuth。

## 1. 执行规则

1. **先规划，再调用。** 在第一次 mutation 前先在模型内部列清：当前机器/fleet 状态、必须由用户完成的边界、各步骤依赖关系和最终验收。把当前已知、互不依赖的读取组成一轮；同一项目、同一安全边界内的确定性 shell/Git 步骤尽量放进一个有界执行调用，并在该调用内部完成必要的局部校验。只有当结果会改变下一步参数/安全判断、需要用户动作，或 mutation 交付结果不确定时才重新规划。不要每执行一条命令就重新 `status`，也不要为了证明“没有变化”而轮询。
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

先检查 `herdr`。缺失时安装官方稳定版：

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows 使用官方 `install.ps1`。安装后验证 `herdr --version` 与 `herdr api schema`。

然后从 <https://github.com/whshang/herdr-mcp/releases> 取得当前 **Latest stable** 的平台二进制，放入用户 `PATH`（推荐 `~/.local/bin/herdr-mcp`），并执行：

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

如果 `~/.local/bin/herdr-mcp` 已存在但交互 shell 找不到它，记为 `installed_but_not_on_shell_path`，修复用户 PATH 后用新 shell 验证。不要重复安装，也不要创建第二个 PATH owner。只有实际需要修 PATH 时再打开[故障排查](troubleshooting.md)。

macOS 在 Cloudflare 工作之前执行 `herdr-mcp permissions status` 和 `herdr-mcp permissions verify`。出现 `needs_setup`，或 `doctor` 明确要求完全磁盘访问（Full Disk Access/TCC）时，由用户本人给稳定 broker 授权后再验证；不要用 `sudo` 替代。Linux 使用 release 自带的受支持 user-service / process backend，不套用 macOS launchd 假设。普通安装不需要 Node.js、Wrangler、npm 或 Cargo。

## 4. 第一台 Worker：Cloudflare + bootstrap

需要 Token 时打开 <https://dash.cloudflare.com/profile/api-tokens>。推荐 Cloudflare 的 **Edit Cloudflare Workers** 模板并限定到本次使用的 Account。自定义 Token 的核心预检需要 **Account Settings → Read** 和 **Workers Scripts → Write/Edit**。Token 验证有效但 `workers/subdomain` 返回 403 时，指出缺少的权限，不要无根据扩大权限。**核心安装不需要 R2**，Workers Free、没绑卡也应可完成；只有用户明确启用 artifact relay 时才增加 Workers R2 Storage。

Token 仅放入当前进程的 `CLOUDFLARE_API_TOKEN` 或交给 `worker bootstrap` 的隐藏输入。不要把 Token 写进命令行字面量、配置仓库或普通日志。

直接运行：

```bash
herdr-mcp worker bootstrap
```

这个命令负责 Cloudflare API 预检、Account / `workers.dev` subdomain、现有 Herdr Worker 检测、release manifest 与 `herdr-edge-<version>.mjs` artifact attestation、Worker/DO bootstrap、首台 canonical device enrollment、credential 保存以及 production Link 对齐。普通用户路径不运行 Wrangler，也不需要源码 checkout。

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

CLI 要求 6 位验证码时再向用户索取。设备显示名默认来自电脑名；只有用户明确要求时才传 `--name`。这一流程不部署新 Worker、不创建第二个 Connector，也不需要把现有 fleet 的长期秘密复制到新电脑。

登记后的身份使用不可变的 `device_id`，例如 `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`：`dev_` 加 26 字符 ULID。显示名与身份分开管理。**不要**自造 `WORKSTATION_ID`，也不要从 hostname 推导身份。

详见[加入已有 fleet](existing-worker-connect.md)。

## 6. Link 与网络

`worker bootstrap` / `worker connect` 会建立单设备凭据并对齐 production Link。Agent 只需要验证：

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

没有 Custom Domain 时先直连 `workers.dev`。解析失败后，含本修复的 runtime 依次查询 Cloudflare DNS、Google DNS；候选 IP 只有通过真实 TLS `/health` Herdr 校验后，才能写成带标记的单 hostname 系统 hosts 记录。`v0.4.8` 由 Agent 先按安装手册执行同等验证与 hosts 恢复，再重跑 bootstrap。Unix 需要时由用户批准交互式 `sudo`，Windows 使用管理员终端。随后重试 Link 直连，再复用已有本地代理；内置签名共享 Relay 保持最后手段。不得改系统 DNS、网络节点、OAuth issuer 或 public MCP origin。

## 7. 最终验收

把读操作尽量合并成一轮最终验证，不要在每个安装步骤后重复同一组检查。完成必须同时证明：

- `herdr-mcp status` / `doctor` 健康；
- `herdr-mcp link status` 显示已登记 production Link 在线；
- canonical public origin 的 `/health` 与 OAuth discovery 正常；
- 本机存在 canonical `dev_<ULID>` device identity；
- 一条真实认证 MCP 请求能够从公网 origin 往返到当前工作站。

随后引导用户在 ChatGPT 中开启需要的 Developer Mode，使用最终 `.../mcp` 地址创建 `herdr` Connector 并完成 OAuth。ChatGPT 授权是最后一个必须由用户本人完成的边界。

Chrome 扩展 / Native Messaging 是可选增强，不是核心 Connector 安装前置。用户需要浏览器连续工作、接力或 Control Center 时再按[扩展文档](extension.md)安装；扩展分发和开发细节留在扩展文档中。

## 8. 清理与报告

结束时 unset `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID`，删除临时凭据文件。只报告非敏感事实：runtime version、device name/id（可缩短展示）、Worker 名、最终 public origin、Link 状态、`/health`、OAuth 与 MCP E2E 结果。

遇到权限、网络、OAuth、已有 Worker ownership 或 mutation 交付不确定时，进入对应[安装手册](install.md)或[排障](troubleshooting.md)章节；不要把整份维护手册提前搬进执行上下文。