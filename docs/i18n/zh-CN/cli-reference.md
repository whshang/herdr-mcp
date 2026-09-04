# CLI 参考

*herdr-mcp 本机管理与运维命令。*

本页只记录 **herdr-mcp** 自己的命令面。Herdr 本体的 workspace、pane、agent、session CLI 请看官方 [Herdr CLI Reference](https://herdr.dev/docs/cli-reference/)。

herdr-mcp 的命令可以按用途理解，而不是按 `bin/` 文件名死记。

## 日常管理：`herdr-mcp`

普通用户应使用 GitHub Release 安装的原生 Rust CLI：

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update auto
herdr-mcp update status
herdr-mcp rollback
herdr-mcp reinstall
herdr-mcp uninstall
```

从 v0.4.3 起，`update` 是普通用户的一步升级入口，等价于 `update apply`。已安装的 v0.4.2 binary 仍会把 bare `herdr-mcp update` 当作只读检查，并把 `herdr-mcp update apply` 返回为 `next_action`，因此从 v0.4.2 跨版本升级时应执行一次 `herdr-mcp update apply`。只有明确需要只读检查版本与 provenance 时才使用 `update check`。

交互式 `update` / `update apply` 会继续把最终机器可读结果写到 stdout，同时把人类可读进度写到 stderr：检查 Release/provenance、验证 artifact attestation、按百分比显示下载进度、校验 candidate、显示 installer 状态，并最终给出经过 health gate 的成功/失败结果。真正的 activation 仍由 detached update worker 执行；前台 CLI 只在有界时间内观察 durable update job，不自行执行 service lifecycle mutation。如果安装确实超过观察窗口，命令会返回 `update_queued`，并带 `next_action: herdr-mcp update status`。`update auto` 继续保持非交互、静默后台模式。已经发布且不可变的 v0.4.2 binary 早于这套进度 UI，因此从 v0.4.2 执行一次 `update apply` 时，网络阶段仍可能暂时没有输出。

`update auto` 是后台调度入口。默认 macOS PROD 实例执行 `service install` 时会 reconcile 归属明确的 `dev.herdr-mcp.auto-update` LaunchAgent；任务在加载时先执行一次，随后每天触发。自动安装严格限制为 **PROD runtime + Stable Release**：编译为 DEV 的 runtime、`[update] check = false`、named instance、`preview` 都会在访问网络前直接跳过。发现严格更高的 Stable Release 后，继续复用正常的 provenance 验签、detached worker 和 rollback-safe 更新事务，不新增第二套下载器，也不绕过回滚门槛。`service uninstall` 会先写入归属明确的持久 update fence 并移除 scheduler；detached worker 在真正 activation 前会再次检查该 fence，因此已经排队的静默更新不能在卸载后复活服务。显式成功的 install 才会解除 fence。

`reinstall` 是产品级修复 / 重装入口：重新执行 managed Rust service lifecycle，并保留配置与凭据；runtime generations 继续遵循正常 service GC，仅承诺 active / rollback-safe 保留集合。`uninstall` 会完整清理**经过强 ownership 校验的 herdr-mcp runtime/config 状态**。默认实例负责自己的 service、归属明确的 auto-update scheduler、Link/watchdog、Native Messaging host、managed user CLI 和 config root；named instance 则严格限定为自己的 service/watchdog/config，不取得默认 scheduler、Link、Native Host 或 user CLI 的 ownership。默认 product uninstall 会在 config 删除后保留一个很小的用户 cache update-fence tombstone，防止此前 detached 的 updater 复活 service；只有显式且成功的 install/reinstall 才清除它。两者都明确**不会**卸载或修改独立的 `herdr` executable、Herdr service/socket/config，也不会删除浏览器扩展账号状态、Cloudflare 资源、macOS Keychain 项或 TCC 授权。`service uninstall` 仍然只是更窄的高级 service primitive。

`service ...`、`link ...`、`native-host ...`、`candidate` 属于高级/内部命令；`dev` 是下面单独说明的**源码开发**入口。正常安装本机 runtime 不需要仓库 checkout、Node.js、npm，也不应把 `service install` 当作普通用户入口。

## 源码开发 Runtime：DEV / PROD

v0.4.3+ 只有这一条正式路径可以让开发机 dogfood herdr-mcp 源码，同时始终保留稳定 PROD 恢复源：

```bash
herdr-mcp dev status
herdr-mcp dev sync --dry-run
herdr-mcp dev sync
herdr-mcp dev rollback
```

- `dev status` 只读，显示 runtime channel、active/dev/prod generation、source repo/branch/commit/dirty provenance、`runtime/current` 是否与记录一致，以及固定 PROD 快照是否通过校验。
- `dev sync --dry-run` 只展示计划，不构建、不切换 runtime。
- `dev sync` 默认要求 clean source checkout，把源码构建成例如 `<version>-dev` 的 DEV identity；进入 DEV 前先把现有 PROD binary 与 SHA-256 evidence 固定保存在 `~/.config/herdr-mcp/runtime/channels/prod/`，再复用正式 transactional service install。只有 server、Native Host、`dev.herdr-mcp.link-prod` 都 reconcile 到同一个 managed generation，激活才算成功。
- `dev sync --allow-dirty` 是给明确的本地实验使用的 provenance override，不应作为日常默认值。
- `dev rollback` 校验并重新安装固定 PROD binary。连续多次 DEV sync 保留最初固定的 PROD 恢复源，不会把“上一个 DEV generation”当作新的 PROD。

DEV/PROD 切换只影响本机 runtime lifecycle，不部署 Cloudflare Edge，不修改 DNS/OAuth，也不创建第三套长期 test 环境。Runtime DEV/PROD 与扩展 DEV/STANDALONE/STORE 身份模型相互独立。

## macOS 权限

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

`status` 会返回 `granted`、`denied`、`needs_setup`、`unknown` 或 `timeout`。`setup` 会尽可能直接打开**完全磁盘访问（Full Disk Access）**，但不会声称已经替用户授权；macOS 仍要求用户本人确认。`verify` 通过固定 TCC broker 验证受保护目录。v0.4.3 起，交互式 `herdr-mcp install` 会在 service 启动前准备 broker，并在仍需授权时打开同一设置页。若 file/git 工具返回 `macos_tcc_access_blocked`，给 `herdr-mcp-broker` 开启完全磁盘访问后再执行 `verify`。

## Agent 能力发现：`scan`

`doctor` 回答的是**“这套安装现在健康吗？”**；`scan` 回答的是**“这台机器上的 Agent 到底有哪些已经被证据确认的能力？”**。

```bash
herdr-mcp scan
herdr-mcp scan --json
herdr-mcp scan --probe
herdr-mcp scan --refresh --probe
```

`scan` **不会自己重做 Herdr 的 live-agent detection**，而是组合当前安装的 Herdr/runtime 已经拥有的三类证据：

- `agent.list` 是 live Agent 实例、status、pane/workspace、cwd 的权威来源；
- `server.agent_manifests` 是 Herdr 当前加载 detection manifest 的权威来源；
- 当前安装的 `herdr agent start --help` 声明用于发现“这个 Herdr 版本明确支持启动哪些 Agent kind”。

herdr-mcp 对这三类 kind 做有界并集，再检查对应 executable 是否真实存在于 `PATH`，分别记录 `herdr_startable`、`executable_available`，并派生 `available_for_start`。只有 **Herdr 自己声明可启动 + 本机 executable 存在** 时，才认为这个 kind 可用于新任务分派。这样 herdr-mcp 不需要复制一份很快过期的 Agent kind 硬编码清单，同时仍能记录当前客户端的真实可用能力。

默认 scan 只有对通过副作用 smoke、进入显式 self-description adapter allowlist 的 Agent 才会执行有界 `--version`。`--probe` 额外运行有界、非交互的 `--help` adapter，只接受 Agent 自己 CLI 能明确证明的能力。发现 binary 但没有可信 adapter 时，只记录“已安装、未 probe”；`--refresh` 会显式重载 Herdr Agent manifest 并绕过已有 probe cache。

probe 子进程没有 stdin，超时上限为三秒，输出有大小上限，并且先清空继承环境，只恢复非敏感运行变量。API key、bearer token、provider credential 不会被继承。拿不到或存在歧义的字段继续保持 unknown；herdr-mcp 不会仅凭 Agent 名称猜 provider、model、vision、reasoning quality 或 code-edit 能力。

静态 evidence 放在 herdr-mcp config 目录下有界的 capability inventory 中。status、cwd、project、pane、workspace、session 等实时事实始终来自 Herdr/EventCache，inventory 不会成为第二套 live truth。`herdr_inspect.capability_inventory.available_agents` 默认暴露本机真实发现且可用的全部 kind；只有显式配置 `HERDR_MCP_AGENT_ALLOW` 时才缩小可见范围。发现某个 Agent 只代表它进入候选事实集，不代表固定角色，也不要求派工；Web planner 根据任务结构、实时负载、已验证能力和资源状态自行决策，质量、成本、延迟等拿不到证据的字段继续保持 unknown。

### Web planner 的动态规划建议

v0.4.3+ 继续保持 18 个 public MCP tools，不新增专用 planning tool。`herdr_skill` 的 progressive bootstrap 会声明一个现有 `herdr_call` 可调用的本地只读方法：

```text
herdr_call(
  method="herdr_mcp.planning.advise",
  params={
    "project_root":"/path/to/project",
    "requires_code_edit":true,
    "requires_shell":true,
    "independent_units":2,
    "ownership_isolated":true
  }
)
```

返回值把事实与决策分开：live compatible/rejected workers、scan 证明可启动但当前未运行的 Agent kind、direct deterministic option、parallelism opportunity，以及 workspace/pane/worktree/utility-pane 资源信息。它不会启动 Agent、不会创建 worktree、不会自动选择 worker。用户明确指定 target 时保留 target；任务要求的能力没有 evidence 时 fail closed；可选质量/成本/延迟证据缺失时继续保持 unknown。

Web planner 可据此决定直接执行、复用已有 Agent、创建一个新 lane，或在确实独立且 ownership 已隔离时并行。重复 utility pane、已有 idle/done Agent、已有 worktree 都作为“优先复用”的资源事实返回，清理仍由 planner 在确认任务完成后执行，不增加后台 cleanup daemon。

### GitHub PR / Auto-merge 新鲜状态

v0.4.5 继续通过现有 `herdr_call` 增加一个本地只读方法，不改变 workstation 的 18-tool contract：

```text
herdr_call(
  method="herdr_mcp.github.status",
  params={
    "project_root":"/path/to/project",
    "pr_number":284,
    "previous_fingerprint":"sha256:..."
  }
)
```

该方法每次都直接使用 workstation 已登录的 `gh` CLI 读取 GitHub，因此会显式返回 `source=local_gh_api`、`fresh=true` 和 `cache_policy=bypass_connector_cache`。在刚修改仓库 Auto-merge 等设置之后，planner 不必依赖可能仍有缓存延迟的 Connector 投影。`project_root` 必须是当前 Herdr live snapshot 里的受管 Git root，且 `origin` 必须位于 `github.com`。

传入 `pr_number` 后会返回 PR state、merge state、Auto-merge request、required checks，以及 Deno Deploy 等 supplemental status。每次结果都有确定性的 `fingerprint`；继续监控时把它作为 `previous_fingerprint` 传回，如果状态没有变化，下一次只返回精简 summary 和 `changed=false`，不会重新输出整张检查表。因此 planner 应优先使用该方法，而不是会反复打印完整 job snapshot 的 `gh run watch`。

## Connector 与 Automation 凭据

交互式 Connector 作为 Worker fleet principal 进行批准和撤销：

```bash
herdr-mcp connector approve <approval-request-id>
herdr-mcp connector revoke <client-id> --confirm
```

批准命令会交互式读取 6 位授权码，不要把授权码放进 argv 或 shell history。所有已登记 Device 都是平等的 Worker 管理通道，不存在 owner/member 设备层级。

GitLab CI 等无人值守调用方使用可独立 revoke 的 Automation Client：

```bash
herdr-mcp automation create --name "gitlab:group/project:prod"
herdr-mcp automation list
herdr-mcp automation rotate <svc_client_id> --confirm
herdr-mcp automation revoke <svc_client_id> --confirm
```

`create` 和 `rotate` 只显示一次 `client_secret`，应直接存进 CI secret manager；`list` 永远不返回 secret，只返回有限的签发统计。Automation Client 用 OAuth `client_credentials` 通过 `client_id + client_secret` 换短期 access token，只拥有普通 MCP 权限，不拥有 fleet-admin 权限。

本机静态 bearer、公网 OAuth Connector 与 Automation Client 是三套边界。`HERDR_MCP_TOKEN` 只服务本机 TCP runtime，不要复制进 ChatGPT 或 GitLab CI。

完整接入见 [ChatGPT Connector](chatgpt-connector.md)。

## 日志与健康检查

```bash
herdr-mcp status
herdr-mcp logs
herdr-mcp logs -f
```

排障时最好先记录状态再重启。

建议组合：

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
herdr-mcp status
herdr-mcp logs
```

即使是 loopback，`http://127.0.0.1:8772/mcp` 也故意保持鉴权。第一方本机客户端可以从受保护的本地状态自动取得 runtime credential，让用户无需手工粘贴；裸 `curl` 或第三方 TCP client 必须显式带 bearer。官方浏览器插件不使用这个 TCP token，而是通过 Chromium Native Messaging 进入 mode-`0600` 的 `extension.sock` trusted IPC；这条通道本身 tokenless，并会剥离网页传入的 `Authorization`。

本机 HTTP 返回 `200` 或 `401` 都说明 runtime 在监听；连接失败才说明进程/端口层有问题。

## Watchdog

macOS 可以安装本机 watchdog：

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

它用于发现 herdr-mcp runtime 自身失联并按受控策略恢复，不把每一次 Herdr TaskGroup/ExceptionGroup 控制面毛刺都当成 daemon 必须重启。

因此 watchdog 是 runtime 可用性保护，不是“看到任何错误就全家桶重启”。

## UI 语言

```bash
herdr-mcp lang en
herdr-mcp lang zh
herdr-mcp lang ja
```

用于项目本机管理界面/相关 UI 语言设置。浏览器扩展也支持 en / 简体中文 / 日本語。

## 浏览器 Native Messaging Host

浏览器扩展主链路需要本机 Native Messaging host：

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

安装后：

```text
Chrome extension
  ↓ Native Messaging
herdr extension host
  ↓ local Unix socket
herdr-mcp runtime
```

浏览器不需要保存 Herdr bearer。

安装与 HUD 使用见 [浏览器扩展](extension.md)。

## Cloudflare Edge 凭据

### 最小权限凭据

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
bin/herdr-cloudflare-token --zone example.com --rotate
```

用于创建/验证 Cloudflare Edge 所需的最小权限 deployment credential。具体权限和安全边界见 [Cloudflare Edge 凭据](cloudflare-edge-token.md)。

### Custom Domain

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

用于把已验证 Worker 绑定到 Custom Domain。它和普通 Worker code deployment 是两个独立操作。

### 旧 Tunnel/CNAME 迁移

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

只用于遗留架构从 CNAME/Tunnel 迁到 Worker Custom Domain。新安装不需要走这条路径。

详见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## Runtime A/B

Runtime generation manager：

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation <id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation <id>
bin/herdr-runtime-generation rollback
bin/herdr-runtime-generation remove --generation <id>
```

它负责的是**同一 contract epoch 内**的本机 runtime generation 注册、切流和回滚。

核心原则：candidate 先运行、先健康检查、先验证 contract，再激活；不要先杀旧 runtime 再祈祷新 runtime 能起来。

详见 [Runtime A/B](runtime-self-upgrade.md)。

## Self Update

```bash
bin/herdr-self-update
```

受监督自升级复用 generation 机制。它适合在同一 public contract 下更新实现，不用于偷偷跨 contract epoch。

如果 public tool catalog 发生不兼容变化，应走显式 contract migration，并在新的 ChatGPT conversation 验证新的 tool snapshot。

## Workstation Link

```text
bin/herdr-link
```

`herdr-link` 是工作站到 Cloudflare Edge 的出站 WSS sidecar。正常情况下由系统服务管理，不需要日常手工运行。

它负责：

- workstation identity；
- 持久出站 WSS；
- 当前 active runtime 路由；
- runtime generation/version heartbeat。

它不是 MCP runtime 本身。

## 常用环境变量

| 变量 | 默认 | 用途 |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | 本机 runtime HTTP 端口 |
| `HERDR_MCP_TOKEN` | 空 | 本机 curl/Cursor bearer；不给 ChatGPT |
| `HERDR_MCP_BASE_URL` | 空 | 公网 OAuth/MCP origin，不带 `/mcp` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr Socket API |
| `HERDR_MCP_READONLY` | 关 | 禁止 mutation |
| `HERDR_MCP_WRITE_ROOTS` | managed roots | 缩小允许写的项目范围 |
| `HERDR_MCP_AGENT_ALLOW` | 全部已发现 Agent | 可选地限制 inspect/since 展示哪些 Agent |
| `HERDR_MCP_ALL_TOOLS` | 关 | 打开高级/兼容工具；正常 ChatGPT 不需要 |
| `HERDR_SKILL_NETWORK` | 开 | `0` 时只使用 bundled skill |

## 命令怎么选

| 目标 | 命令 |
|---|---|
| 看本机 runtime 是否活着 | `herdr-mcp status` |
| 看错误 | `herdr-mcp logs -f` |
| 把当前源码 dogfood 为 DEV runtime | `herdr-mcp dev sync` |
| 查看 DEV/PROD provenance | `herdr-mcp dev status` |
| 从 DEV 回到固定 PROD | `herdr-mcp dev rollback` |
| 安装扩展本机桥 | `herdr-mcp native-host install` |
| 部署前检查 Cloudflare 权限 | `bin/herdr-cloudflare-token ... --dry-run` |
| 看 A/B 当前状态 | `bin/herdr-runtime-generation status` |
| runtime 出问题回滚 | `bin/herdr-runtime-generation rollback` |
| Custom Domain 状态 | `bin/herdr-cloudflare-domain status` |

如果你发现自己需要大量直接调用 Herdr workspace/pane/agent 命令，请回到 Herdr 官方 CLI 文档；herdr-mcp 不重复维护那一套控制面。
