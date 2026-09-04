# Agent 安装

*端到端 Agent 安装协议、Cloudflare 公网入口选择与可靠 Link 配置。*

> **执行角色：Agent。** 本页是 herdr-mcp 普通工作站安装的唯一权威执行合同，包含精简上线路径、安全边界、Cloudflare 部署、Link、浏览器扩展通道选择与验证。[安装参考](install.md) 只用于人工/运维查阅。

Agent 直接读取并执行本文，不需要用户再把本文包装成一段“发给某个 Coding Agent”的提示词。用户只承担 Cloudflare 本人登录/API Token、无法自动判断的 Account 选择、ChatGPT Connector/OAuth 等必须本人完成的授权步骤；Agent 负责环境检查、Release 二进制安装、Cloudflare Worker、出站 WSS Link、可选浏览器扩展通道选择与验证。

> 当前约束：完整的后台服务自动安装路径以 **macOS Apple Silicon** 为第一正式平台。Windows 可有 Release artifact 作为 preview。不要发明未支持的 Linux lifecycle 包装。Edge 部署可临时使用 Node/`wrangler`；本机 MCP runtime **必须**来自 GitHub Releases，而不是 `git clone` + `npm ci`。

## 0. Agent 合同

1. 能自动化的 shell 步骤直接执行；只在 Cloudflare 交互登录 / API Token 创建、确实无法安全推断的 Account/zone 选择，或用户意图尚不明确时一次性确认是否采用推荐 Custom Domain，才暂停。
2. 不破坏已有工作。禁止对无关 checkout 做 `reset --hard`、`clean -fd` 或覆盖用户修改。
3. 首次安装就确定一个 canonical public origin，并让 Worker OAuth、MCP 身份始终使用该入口。`workers.dev` 是零域名的 **bootstrap** 路径；当所选 Cloudflare Account 已经有 active zone 时，它不应默认成为长期 production identity。在创建 ChatGPT Connector/OAuth identity **之前**，Agent 要先发现可用 zone，并优先把 Custom Domain（例如 `herdr-mcp.example.com`）作为稳定 production identity。只有一个明显安全的 zone/hostname 时，把它作为推荐选项；多个 zone 都合理时只问用户选哪个。没有 active zone、用户不想使用、或 hostname 已与现有记录冲突时，直接继续 `workers.dev`，不得阻塞安装。Cloudflare Custom Domain 会自动创建对应 Worker DNS 记录和证书，正常路径不需要通用 DNS Write。Link transport 不得因后续连通性修复而静默改写已经选定的 OAuth issuer。
4. Cloudflare Token 是高敏凭据。禁止回显或写入仓库、`.env`、普通日志、截图、shell history。优先进程环境注入；若必须落临时文件，用 mode `0600` 并在部署后立刻删除。
5. 每个 mutation 后先验证再继续。出错时先判断 mutation 是否已经提交，再决定是否重试。
6. **不要**用 clone 本仓库或 `npm`/`cargo` 安装本机 MCP runtime，除非用户明确要求贡献者/从源码开发会话。
7. 如果网络、登录状态或第三方服务不可用，停止并向用户报告 blocker；不要自行搭代理、切网络节点、修改系统代理或发明绕过路径。

## 0.5 Fleet 所有权闸门——任何 Cloudflare mutation 之前

在请求 Cloudflare Token、命名 Worker、创建 R2、部署 Edge 或新建 ChatGPT Connector 之前，必须先确认 fleet 意图。新电脑本地 `~/.config/herdr-mcp` 为空，**不代表**这是用户的第一台 Herdr 电脑。

- 如果用户已经提供 Herdr pairing address，这台电脑就是加入已有 Worker。安装/验证本机 runtime 与 macOS 权限后，执行 `herdr-mcp worker connect "<pairing-address>"`；CLI 默认使用 macOS Computer Name，只有用户明确要求不同的初始显示名时才传 `--name`。跳过 Worker/R2/Connector 创建。
- 如果用户明确说已有另一台 Herdr 电脑但还没有 pairing address，优先直接在已经授权的 ChatGPT/Herdr 对话中调用 Edge-local `herdr_mcp.device.pair` 创建短期配对；不要求旧电脑在线。把配对地址、一次性 6 位验证码、精确过期时间一起展示。已授权旧电脑上的 `herdr-mcp worker pair` 保留为 CLI fallback。新电脑用 `worker connect` 消费 pairing，并复用已有公网 MCP/OAuth origin。
- **绝不能**在当前正在安装的这台新电脑上执行 `herdr-mcp worker pair` 来“探测是否已有 fleet”。`worker pair` 是 **owner 侧**操作，必须先证明本机已经加入该 fleet。全新机器如果既没有已登记的 device identity，也没有已有 Edge origin，应 fail closed，明确提示“首台设备初始化”或“用 pairing address 执行 `worker connect`”，而不是暴露缺失 LaunchAgent/Keychain 的底层错误。
- 如果用户无法提供 Cloudflare Account/origin 信息，同时也没有任何已经授权的旧设备或已授权 WebChat 可以生成 pairing，就停止并只问下面那一个 fleet ownership 问题。不要猜成“已有 fleet”，也不要执行 `worker pair`。如果用户选择**加入已有 Worker**，必须先从已授权来源拿到 pairing；如果选择**创建第一台 Worker**，才继续 Cloudflare bootstrap，并仅在确实需要 Cloudflare 授权时暂停。
- 如果用户要求永久移除一台旧设备，也优先在同一个已授权 ChatGPT/Herdr 网页对话里完成：先用 `herdr_devices` 列出设备，以不可变 `device_id` 选中目标，再调用 Edge-local `herdr_mcp.device.revoke` 并传 `confirm=true`。绝不按 display name revoke；旧设备无需在线。`herdr-mcp worker revoke <device-id> --confirm` 保留为 CLI fallback。
- 只有用户明确确认这是第一台 Herdr 电脑/第一个 fleet，才继续下面的 first-owner Cloudflare 路径。
- 如果意图不清楚，只问一个所有权问题：**创建第一个 Herdr Worker，还是加入已有 Herdr Worker？** 禁止根据本机缺少凭据自行推断。
- pairing、旧 Worker 升级、hostname 连通性或权限失败都仍属于 existing-fleet 修复路径。除非用户明确改变 fleet 意图，否则禁止 fallback 到随机后缀的新 Worker、R2 桶或 Connector。

## 1. 前置条件

运行 `herdr --version` 与 `herdr api schema >/dev/null`。需要可用的 `herdr` 与 Herdr socket（默认 `~/.config/herdr/herdr.sock`，或显式 `HERDR_SOCKET_PATH`）。若 Herdr 本身未安装/未运行，停下来并引导用户到 <https://herdr.dev>；herdr-mcp 不替代 Herdr。

如果 Herdr 缺失，Agent 直接安装官方稳定版：

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows 使用 `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"`。安装后重新检查健康状态。

### PATH preflight：实际可执行文件与 shell PATH 分开检测

在任何安装步骤之前做一次，安装完成后再做一次：

1. 检查真实二进制：`ls -l ~/.local/bin/herdr-mcp ~/.local/bin/herdr`，存在则先用绝对路径执行。
2. 单独检查用户交互 shell 的 PATH：`zsh -ic 'command -v herdr-mcp'`（或用户的登录 shell）。二进制存在但 `command -v` 为空，属于 **`installed_but_not_on_shell_path`**，不是「未安装」——不要重装，也不要制造第二个 PATH owner 或指向仓库的用户 CLI。
3. 按顺序自修复：先 `export PATH="$HOME/.local/bin:$PATH"` 让当前进程不被阻塞；再持久化修复。zsh 使用 `line='export PATH="$HOME/.local/bin:$PATH"'`，并执行 `grep -Fqx "$line" "$HOME/.zprofile" 2>/dev/null || printf '\n%s\n' "$line" >> "$HOME/.zprofile"`；不能因为启动文件里出现其它 `.local/bin` 文本就误判精确 PATH 条目已经存在。如果不应修改 shell 启动配置，明确说明原因，并继续使用绝对路径。
4. 继续之前分别证明：当前 shell 执行 `herdr-mcp --version`，新的交互 shell 执行 `zsh -ic 'command -v herdr && herdr --version'`，新的登录 shell 执行 `zsh -lc 'command -v herdr-mcp'`。否则 Agent 会话结束后就可能出现 `command not found`。

### macOS 权限 preflight：在后台服务安装之前验证 TCC/FDA

权限检查要放在 onboarding 前段——Cloudflare 工作之前——而不是装完才发现：

```bash
herdr-mcp permissions status
herdr-mcp permissions verify
herdr-mcp doctor
```

`doctor` 权限结果为 `needs_setup`、`denied`、`unknown` 或 `timeout` 时，现在就停下来修复，不得当作健康。提前一次性完成稳定 TCC broker 的完全磁盘访问授权，才能避免之后 runtime、Herdr socket、项目访问反复逐路径弹权限。不要用 `sudo` 代替 broker 授权。

Node.js 只用于临时 Cloudflare Worker 引导（`npx wrangler`）和可选贡献者工具链，**不是**运行本机 MCP runtime 的依赖。

规范公网 MCP URL 示例：`https://herdr-edge-device.username.workers.dev/mcp` 与 `https://herdr-mcp.example.com/mcp`。

## 2. 从 GitHub Releases 安装原生 runtime（主路径）

1. 从 <https://github.com/whshang/herdr-mcp/releases> 下载当前 stable 平台二进制，以 GitHub 标记的 `Latest` stable Release 为准。只有明确测试 preview channel 时才选择 prerelease 标签。
2. 放到 `PATH`（例如 `~/.local/bin/herdr-mcp`）并赋予可执行权限。
3. 执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update          # 下载并应用下一版 stable release
```

`install` 会在 `~/.config/herdr-mcp/runtime/` 写入不可变 generation，并把 `~/.local/bin/herdr-mcp` 指到 `runtime/current/herdr-mcp`。`herdr-mcp update` 是正常的一步升级入口；只有运维明确需要只读检查是否有新版本时才使用 `herdr-mcp update check`。优先使用以上顶层命令。不要把 `herdr-mcp service install` 写成普通安装主路径。

macOS v0.4.3+ 首次安装还会准备固定的 `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`。如果是交互式安装且尚未获得完全磁盘访问，系统设置会打开一次，由用户本人给这个 broker 授权。不要尝试用 `sudo` 代替这一步；`doctor` 若返回 `needs_setup`、`denied`、`unknown` 或 `timeout`，不得当作健康。应提示用户完成完全磁盘访问，然后重新执行 `herdr-mcp permissions verify` 和 `herdr-mcp doctor`。普通 runtime generation 更新会保留同一个 broker，不应再次要求授权。

只有明确测试 prerelease build 时才使用 `update.channel = "preview"`。当前 stable runtime 使用默认 `stable` channel 即可。

## 3. 在内存中生成身份，不要打印秘密

在 Agent 内存中生成：`HERDR_MCP_TOKEN` 与 `LINK_SHARED_SECRET`。**不要**自造 `WORKSTATION_ID`/device id。设备身份契约归 runtime 所有：不可变的 `device_id` 形如 `dev_` + 一个 canonical 26 字符 Crockford ULID（例如 `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`），在 onboarding/pairing 时自动生成。hostname 派生的自由格式 workstation 标识只是遗留部署变量，不是设备身份——pairing 会生成并校验真正的那个。

人类可读的电脑名（例如 macOS Computer Name）是独立的显示名。它默认作为 `worker connect` 的 `--name`，之后可用 `worker rename` 改名，永远不会改变不可变的 `device_id`。

有临时 Edge checkout 时，`WORKER_NAME` 只能通过仓库 helper 生成，Agent 不得自造 hostname slug：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

helper 会把 hostname 小写，把 `[a-z0-9-]` 以外字符安全替换，压缩/修剪 `-`，并保证完整 Worker 名不超过 63 且匹配 `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`。例如 `MacBook.local` → `herdr-edge-macbook-local`。`WORKER_NAME`（Cloudflare Worker 命名）与 `dev_<ULID>` 设备身份故意使用不同语法。秘密用 `openssl rand -hex 32` 一类强随机；最终报告不得包含秘密。

## 4. Cloudflare 授权暂停

有浏览器控制时打开 <https://dash.cloudflare.com/profile/api-tokens>；否则把该 URL 交给用户。

最简单支持路径是 Cloudflare 当前的 **Edit Cloudflare Workers** 模板，限定到本次安装使用的单个 Account。它已经包含推荐 Custom Domain route 所需的 Worker script 与 **Workers Routes Write** 权限，不需要通用 DNS Write。**核心安装不需要 R2**，不能因为未开通 R2 或账户没绑卡而失败。只有用户明确启用可选 artifact relay（§6）时才加 Account → **Workers R2 Storage → Edit**。不要超出所选路径实际需要的权限。

更紧的自定义 token 至少保留 Account → **Workers Scripts → Write/Edit**、Account → **Account Settings → Read**、User → **Memberships → Read**、User → **User Details → Read**。读取账户 `workers.dev` subdomain 需要 `Account Settings → Read`。要发现并绑定 Custom Domain，再增加 Zone → **Zone → Read** 与 Zone → **Workers Routes → Write/Edit**，能限定到目标 zone 时就不要放大范围。正常 Custom Domain 路径**不要**增加 Zone → DNS Write。用户之后启用 artifact relay 时再加 Account → **Workers R2 Storage → Edit**。

告知用户秘密只显示一次，并要求只粘贴到当前本地 Agent 会话；有专用密输通道时优先使用。

## 5. Token 到达后的 Cloudflare 预检

只以临时 `CLOUDFLARE_API_TOKEN` 注入，不要写成命令行字面量。验证 `GET https://api.cloudflare.com/client/v4/user/tokens/verify`，再对临时 Edge 工作目录运行 `npx wrangler whoami`。

- 一个 Account → 自动选择；
- 多个 Account → 只问要用哪个 Account 名；
- Token 无效/权限不足 → 停止 mutation 并指出具体缺哪个权限。

Token 可以验证为**有效**（`/user/tokens/verify` 返回 active）却仍在具体调用上得到 `403`——这说明缺权限，不是 token 坏了。先把失败调用映射到权限，再补授权，不要盲目重建一个更大的 token：

- `GET .../workers/subdomain` 返回 403 → 缺 **Account Settings → Read**；
- `wrangler deploy` / Workers Scripts 调用失败 → 缺 **Workers Scripts → Edit**；
- R2 桶 provisioning 失败 → 未授予可选的 **Workers R2 Storage → Edit**（核心安装本来就不需要；只有用户明确启用 artifact relay 时才算错误）。

按权限诊断，不要无根据扩大权限；补齐权限之前不要重试 mutation。

**部署前的已有 Worker 检测。** 拿到 `ACCOUNT_ID` 后先列 `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts`。如果那里已有 Herdr Worker——或用户已有能够创建 pairing 的已授权 Herdr WebChat/Connector——立即停止部署路径，切换到[多设备控制](existing-worker-connect.md)的既有 fleet 流程（从已授权来源创建 pairing，本机执行 `worker connect`）。`worker pair` 只是在**已经登记的 owner 电脑**上的 fallback，绝不能拿当前这台全新机器执行它来探测 fleet。只有 §0 得到明确「第一台」答复后才允许部署新 Worker。

选定后只把 account ID 放在当前部署进程环境：

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

不要把个人 `account_id` 写进被跟踪的 Wrangler 配置。后续 `wrangler deploy` / `wrangler secret put` 继承该临时环境。

有 `ACCOUNT_ID` 后请求 `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain`。复用已有 account subdomain，**永不改名**。Worker origin 永远是 `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`。

在最终公网 identity 固化前，同时枚举 Token 可见的 active zones。这一步只是发现，不代表可以任意修改 DNS。有 active zone 时，准备一个不冲突的专用 hostname（默认建议 `herdr-mcp.<zone>`），检查它没有被不兼容的 DNS record/Worker 占用，并优先把它作为最终 MCP/OAuth Custom Domain。没有合适 zone 或用户选择不用域名时，把 `workers.dev` 记录为 canonical origin 并直接继续；这不是降级安装错误，工作站侧的网络可达性由 Link transport fallback 负责。

若尚无 subdomain，只在确认不存在时创建；用 `herdr-<short-account-id>`，冲突再加随机后缀。GET 明确无 subdomain 后才：

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

创建后再 GET，要求返回值匹配才继续部署。

## 6. 部署 Edge，不要求永久仓库 checkout

仅为 Edge 部署获取 Worker 源码（临时 shallow clone 或与 Release 相邻的文档包均可）。从已发布的 user example 生成被忽略的 `wrangler.user.toml`，先设置 `name` 与 `DEFAULT_WORKSTATION_ID`。保持 `workers_dev = true`，让 `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev` 始终可作为零域名 bootstrap/诊断 origin。

先部署一次 `workers.dev` 并证明 `/health`，再在创建 ChatGPT Connector 前确定唯一长期 public origin：

- **有 active Cloudflare zone 时推荐：**增加 `[[routes]]`，设置 `pattern = "herdr-mcp.<zone>"`（或用户选定的专用 hostname）与 `custom_domain = true`，把 `OAUTH_ISSUER` 改为 `https://<custom-host>`，重新部署，并要求该 hostname 的 `/health`、未认证 `/mcp`、OAuth discovery 全部通过。Cloudflare 会自动创建 Custom Domain 的 DNS 记录与证书。
- **没有合适 zone / 用户不使用：**保持 `routes = []`，设置 `OAUTH_ISSUER=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`，把这个 `workers.dev` origin 固化为 canonical public identity。

不要先让 Connector 绑定 bootstrap hostname，随后又静默迁域名。OAuth client 一旦接入，public-origin 修改就是显式迁移操作。

**R2 可选，默认关闭。** 核心安装保持 `wrangler.user.toml` 中的 `[[r2_buckets]]` 注释状态，已发布的 user example 就是这样发布的。Edge 代码把 `ARTIFACT_BUCKET` 视为可选，核心部署必须在 Workers Free、无 R2、无绑卡的情况下成功。只有用户明确启用可选 artifact relay 时：取消注释该绑定、确认 token 有 **Workers R2 Storage → Edit**（以及 Cloudflare 要求的 R2 计费步骤），并在部署前执行 provisioning。绑定的桶不存在时 `wrangler deploy` 会 fail-closed——这正是默认路径不发布该绑定的原因。R2 保持关闭时，跳过 provisioning 直接部署：

```bash
npx wrangler deploy --config wrangler.user.toml
```

artifact relay 启用（仅可选路径）：

```bash
node provision-r2.mjs --config wrangler.user.toml
npx wrangler deploy --config wrangler.user.toml
```

除非能证明拥有权，否则不要覆盖已有 Worker。如果目标名称已存在，先停下来判断它是不是用户已有的 Herdr Worker；**禁止**为了绕过所有权确认而直接创建随机后缀 Worker。只有用户明确确认 first-fleet，且能证明已有 Worker 与本次无关时，才选择新的唯一 Worker 名。然后把 WSS 共享秘密存为 Worker secret：

```bash
printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
```

不需要 Zone/DNS mutation。只为 Edge 部署用的临时 checkout **不得**成为 `herdr-mcp` 的生产 PATH。

## 7. macOS 本机 MCP 服务所有权

优先使用已安装的 Release 二进制路径：

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
```

不要重建指向仓库的 `~/.local/bin/herdr-mcp` bridge。不要把 LaunchAgent 指到 git checkout 或 `target/*/herdr-mcp`。

浏览器扩展 / Native Messaging 仍是可选项，不是第一条 ChatGPT 闭环的必需。扩展通道与 Runtime DEV/PROD 分开建模：**STORE / STANDALONE / DEV**。

- STORE：普通用户默认，Chrome Web Store 固定身份与更新。
- STANDALONE：v0.4.3+ 的 GitHub/手动固定身份 package；Store 不可用或用户明确要求独立分发时使用。
- DEV：仅源码开发，Load unpacked repo/worktree `extension/`，ID 路径派生。

Agent 必须先读取当前 runtime 实际支持的 `native-host` 命令；v0.4.2 只有 Store/DEV，不得虚构 standalone。STANDALONE 是独立于源码开发的分发通道，DEV 仍仅用于源码开发。支持该能力的 runtime 使用 `herdr-mcp native-host use standalone` 显式切换。选择并安装通道后执行：

```bash
herdr-mcp native-host status
```

状态应明确显示预期 active channel / extension identity，并确认 Native Host runtime 与当前 runtime generation 一致。详见 [浏览器扩展](extension.md) 与 [浏览器连续性](browser-continuity.md)。

## 8. macOS 持久 Herdr Link

把 `LINK_SHARED_SECRET` 存进 Keychain，服务名 `herdr-edge-link-<WORKSTATION_ID>`。命令文本只能引用环境变量，不能写字面秘密。优先使用已安装 `herdr-mcp` 二进制提供的托管 Link 安装路径（`herdr-mcp link ...` / 当前 stable 产品文档）。不要把生产 Link 所有权留在仓库 Bash 包装上。

Agent 应该自己探测网络可达性，而不是让用户先选择 transport。执行 `herdr-mcp doctor`、`herdr-mcp link status`，并对最终 public origin / backing `workers.dev` 做有界 `/health` 探测。Link 会复用用户环境里**已经存在**的代理配置，识别优先级：`HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`；macOS 也会读取现有 `scutil --proxy` 状态（HTTPS、HTTP、SOCKS）。支持 `socks5://`/`socks5h://`（remote-DNS 语义），不支持代理认证；macOS PAC 只检测不执行。

transport 选择默认无感完成：配置了 Custom Domain 时，Link 使用这条稳定 direct path，不使用共享 Relay Pool；没有 Custom Domain 时，依次尝试 direct `workers.dev` → 已经配置并验证的本地代理 → 内置已验收的 Herdr Relay baseline（Deno 为主、Supabase fallback），若本机存在更新且有效的签名 Relay Pool cache，则完整覆盖 baseline。用户不需要 Deno/Supabase 账号，也不需要配置 Relay URL。不要为了让探测变绿而自行修改系统代理、网络节点、DNS 或已经选定的公网 identity。

## 9. 验证闭环

验证本机 `server/discover`、`herdr-mcp status`、`herdr-mcp doctor`、Link status、Worker `/health`、公网 `/mcp`、OAuth discovery，并同时记录 bootstrap `workers.dev` origin 与最终 canonical public origin。选择了 Custom Domain 就必须在注册 Connector 前证明该 hostname；最终使用 `workers.dev` 时，则证明公网 identity 健康，并确认 `link status` 能看到可用的自动 fallback ladder / Relay candidate pool。doctor 可在不发送 token 的前提下探测 Edge `/health`、OAuth metadata、`/mcp`；永远不要打印 token。

### 区分 Worker 健康与 hostname/网络路径健康

一类探测证明不了两件事，要分开读：

- **Worker 代码健康**：origin 对 `GET /health` 返回 200，未认证的 `GET /mcp` 返回预期的 401。这证明已部署的 Worker、路由与 OAuth metadata——无论哪个 hostname 应答。
- **hostname/DNS/网络路径故障**：超时、DNS 解析失败、TLS/SNI 失败或被过滤，而同一 Worker 的另一个 hostname 可用（例如 `*.workers.dev` 超时而该 Worker 的 Custom Domain 返回 200，或相反）。这是传输路径问题，不是 Worker 缺陷：绝不用重新部署 Worker 或创建第二个 Worker/R2/Connector 来「修」它。

所选 Cloudflare Account 已有 active zone 时，优先建议专用 Custom Domain 作为稳定 production origin，并在客户端接入前完成配置；没有合适 zone 或用户不使用时，继续以 `workers.dev` 为 production origin，网络路径问题由 Link 自动 fallback（direct → 已验证本地代理 → qualified shared Relay）。连通性修复不得顺带改名或迁移 OAuth issuer。

## 10. 清理 bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` 与 `CLOUDFLARE_ACCOUNT_ID`，删除临时凭据文件和不再需要的临时 Edge checkout。不要把 Token 拷进项目配置。若是一次性 Token，建议吊销；否则迁到专用密钥管理/CI secret。

## 11. 最终报告

只回报非敏感事实：已安装 runtime generation/version、本机 MCP 状态、Herdr Link 状态、Cloudflare Account 名 + 缩短 ID、Worker 名、bootstrap `workers.dev` origin、最终 canonical public origin（Custom Domain 或 `workers.dev`）、Link transport/fallback readiness、`/health`、`/mcp`。

最后引导用户开启 ChatGPT Developer mode，用 `/mcp` 创建自定义 MCP Connector 并完成 OAuth。永远不要把本机 `HERDR_MCP_TOKEN` 或 Cloudflare Token 粘贴进 ChatGPT。

## 附录：仅开发者从源码

只有在用户明确要求开发 herdr-mcp 本身时，才允许 clone + `npm`/`cargo`。该路径不得作为普通工作站的 runtime 安装主路径。
