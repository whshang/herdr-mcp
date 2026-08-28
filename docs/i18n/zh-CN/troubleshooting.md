# 故障排查：从本机到网页，一层一层找问题

herdr-mcp 的链路横跨本机 runtime、Herdr、workstation link、Cloudflare Edge、OAuth、MCP 和浏览器。最快的排障方式不是“把所有东西重启一遍”，而是先确认故障在哪一层。

推荐始终按这个顺序：

```text
Herdr
  ↓
local herdr-mcp runtime
  ↓
workstation link
  ↓
Cloudflare Edge
  ↓
OAuth / MCP
  ↓
ChatGPT tool snapshot
  ↓
browser continuity
```

前一层不通，就先不要猜后一层。

## 30 秒快速定位

### 1. 本机 HTTP 在吗

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` 或 `401` 都说明进程正在监听。连接失败才表示本地 runtime 没起来、端口不对或进程异常。

macOS LaunchAgent：

```bash
herdr-mcp status
herdr-mcp logs
```

### 2. Herdr 在吗

```bash
herdr --version
herdr api schema >/dev/null
```

如果本地 HTTP 正常但 `herdr_inspect` 看不到任何真实 workspace，继续检查 `HERDR_SOCKET_PATH` 和 Herdr daemon，而不是先重装 ChatGPT Connector。

### 3. Edge 能看到 workstation 吗

检查 Edge `/health` / workstation status。OAuth 可以在 workstation 离线时仍然成功，所以“登录成功”不能证明本机已经连上。

### 4. 新 ChatGPT 会话有当前工具吗

当前 production contract 是 **epoch 2 / 18 tools，包含 `herdr_skill`**。

旧聊天可能保留旧 `tools/list` 快照。确认服务端版本后，优先刷新 App/Connector actions（如果当前 ChatGPT UI 提供）并**新开会话**，不要先重装本机 runtime。

## 症状：Connector 添加失败或 OAuth 循环

优先检查：

- MCP URL 是否是 `https://<stable-origin>/mcp`；
- `HERDR_MCP_BASE_URL` / OAuth issuer 是否使用同一个 origin，且不带 `/mcp`；
- protected-resource / authorization-server / OpenID metadata 是否可访问；
- Cloudflare Worker 是否真的是你以为的那一份部署；
- Workspace 是否允许 Developer mode / custom MCP App。

如果 OAuth token 已成功签发但随后失败，问题通常已经从“认证”进入 MCP discovery / workstation routing，不要继续围绕登录页面打转。

详见 [ChatGPT Connector](chatgpt-connector.md)。

## 症状：Connector 显示已连接，但聊天里 0 tools

这说明“安装 Connector”和“当前聊天接受 tool catalog”不是一回事。

按顺序检查：

1. 新会话；
2. 服务端当前 contract/version；
3. `tools/list` 是否成功；
4. 是否有一个不兼容 `inputSchema` 导致整张 catalog 被客户端拒收；
5. 是否仍是老 conversation 的 tool snapshot。

如果新会话拿到 18 tools，而旧会话仍是 17 tools，服务端通常没有故障，是会话缓存边界。

## 症状：能看到 tools，但 `herdr_inspect` 报 workstation offline

这已经不是 ChatGPT tool schema 问题。

检查：

- 本机 runtime 是否在线；
- `herdr-link` 是否运行；
- workstation id 是否和 Edge 配置一致；
- 当前 active runtime generation 是否健康；
- Edge status 是否显示最近 heartbeat。

不要通过删除/重建 Connector 来修 workstation link。

## 症状：`herdr_inspect` 正常，但文件读写失败

常见原因：

- 路径不在当前 managed Git root；
- 文件名被 secret-path gate 拦截；
- `HERDR_MCP_READONLY=1`；
- `HERDR_MCP_WRITE_ROOTS` 没包含目标仓库；
- 文件已经 dirty，写工具要求明确确认；
- 同一项目有 Agent 正在工作，busy gate 阻止并发写入。

先看错误返回里的 gate/reason，不要换成 shell 绕过保护作为默认解决方案。

`herdr_exec` 是显式的高权限边界，不具备 `herdr_fs_*` 的 secret-path 过滤。

## 症状：Herdr 控制面偶发 TaskGroup / ExceptionGroup

表现通常是：

- Agent 明明仍在工作；
- 一次 inspect/snapshot/pane 操作失败；
- 几秒后重新观察又恢复。

这类瞬时控制面错误不等于仓库坏了。

herdr-mcp 对只读路径有若干退化策略：

- snapshot 失败时组合 list APIs；
- Git 在安全条件下直接读取本机仓库；
- exec 只有在命令**尚未投递**时才可能 fallback；
- 已投递 mutation 永远不因为控制面错误自动双跑。

正确处理：重新 `herdr_inspect` / `herdr_since` 获取当前事实。

## 症状：prompt / exec timeout，不知道任务到底有没有执行

最重要的规则：**mutation 不盲重试。**

### `herdr_prompt`

如果失败发生在提交后的状态等待阶段，Agent 很可能已经收到任务。先 inspect/since 看 Agent 状态和输出，再决定是否再次投递。重复意图应复用 `idempotency_key`。

### `herdr_exec`

如果工具明确表示已经向 pane 投递，不能因为响应超时再执行同一命令。先看 pane / Git /产生的文件 / 测试状态。

“客户端没收到成功回复”不等于“服务端没做”。

## 症状：本地 Agent 做完了，ChatGPT 不继续

这是最容易被误判成 MCP 故障的情况。

MCP 完成的是：

```text
ChatGPT → workstation
```

Agent 后续完成不会自动创建新的 ChatGPT turn。要实现：

```text
workstation → ChatGPT
```

需要浏览器扩展：

1. Native Messaging host 正常；
2. 当前网页 conversation 已绑定正确 workspace；
3. 相应 Auto scope 已开启，或用 HUD 手动继续/监控。

详见 [浏览器连续工作](browser-continuity.md)。

## 症状：HUD 显示了错误 workspace 名称

绑定身份以 `workspace_id` 为准，label 只是展示信息。

如果 ID 正确但名称陈旧，扩展应从实时 workspace catalog 更新 label。不要为了改一个展示名称就删除正确 binding；先确认扩展已经加载当前构建并刷新页面。

如果 ID 本身错了，再重新绑定。

## 症状：浏览器控制中心打不开、没有 workspace，或一直显示本机运行时不可用

先区分 **Side Panel UI 问题**、**Native Messaging 身份问题** 和 **runtime 问题**：

1. 点击 Herdr 工具栏图标，确认 Chrome 直接打开 Control Center Side Panel；不要把 `control-center.html` 当普通网页直接访问；
2. `herdr-mcp status` / `herdr-mcp doctor` 应该先证明本机 runtime 正常；
3. `herdr-mcp native-host status` 应显示 Native Messaging host 已注册；
4. Chrome 刚更新商店扩展后，刷新受影响网页（必要时重启 Chrome），让当前 content script 重新加载；
5. `herdr-mcp native-host status` 应显示官方 Store 扩展身份；若出现 origin mismatch，使用当前 runtime 重新执行 `herdr-mcp native-host install`。维护者的 unpacked identity 排障只放在 Store 开发 WIP，不放在最终用户指南；
6. 顶部如果显示“运行时正常 · 事件流正在重连”，说明已有 snapshot，但增量事件正在恢复，不等于整个 runtime 离线；可以先点刷新让 Side Panel 做一次权威 reconciliation。

Control Center 的“提示 Agent / 调整会话 / Herdr API / 终端输入”当前本来就是 Preview-only。按钮不执行 mutation 不是故障；当前可执行的是 `查看状态` 和有界的 `读取最近输出`。

详见 [浏览器控制中心](browser-control-center.md)。

## 症状：ChatGPT 回复一半停住、连接中断或显示发送超时

不要第一反应就重新提交原用户任务。工具 mutation 可能已经发生。

当前 continuity recovery 的策略是 evidence-first：

1. 尝试读取同源 conversation state；
2. 如果服务端已经比 DOM 更靠前，刷新页面同步；
3. 如果明确是请求未接受，才进行受限重试；
4. delivery 不确定则 fail closed；
5. 普通恢复耗尽后，才考虑 handoff/rollover。

如果自动恢复没有证据可用，可以人工刷新后用 HUD 的 **herdr监控** 先重新获取本地状态，再继续。

详见 [自动继续、恢复与接力](extension-wake.md)。

## 症状：ChatGPT “排队”后没有立即发送，或排队内容还留着

“排队”的设计目标就是**不立即发送**：assistant 仍在回复时，内容应该留在当前 conversation 的持久队列，等 turn settled 后再优先于通用 auto-continue 发送。

检查：

- 当前页面是 ChatGPT；其它站点目前没有同一套 Queue UI；
- assistant 是否仍处于 generating / tool / permission-card 状态；只要 turn 仍在进行，队列就不应强行发送；
- `turn-in-progress` 或提交未确认时，队列不会 ACK 删除，这是为了避免丢消息；
- composer 为空时再次点击“排队”可以尝试重发仍待交付的 batch；
- 右键“排队”会明确清空当前 conversation 队列；
- handoff 成功后，未发送内容会迁移到新 conversation，不应该在旧 conversation 重复发送。

如果队列在没有确认 delivery 的情况下消失，才属于可靠性问题；记录 conversation、当前 turn 状态和浏览器 console 后提 Issue。

## 症状：手动接力不可用

直接使用页面 **HUD 的“接力”**。如果入口不可用或被禁用，再确认：

- 当前站点/会话类型支持 handoff；
- workspace 已绑定；
- 当前作用域可以是 `自动 开` 或 `自动 关`；目标会话会继承源会话 Auto 状态；
- workspace 没有仍处于 `working` 的 Agent；
- 没有已经进行中的 transfer。

接力必须先生成 packet、建立新 conversation、确认 seed 存在，最后才迁移 binding。如果停在“恢复接力”，不要手工解绑旧 conversation；旧 binding 在 cutover 完成前是安全锚点。

## 症状：z.ai / DeepSeek 输出 JSON tool call 后停住

这属于 JSON→MCP bridge，不是 ChatGPT Connector。

检查：

- Native Messaging host；
- 本机 MCP catalog 是否可取；
- 当前 conversation identity 是否稳定；
- assistant 最后一条真实消息是否仍是 tool-call JSON；
- bridge round 是否有工具结果回填；
- 页面刷新后是否仍保留足够 bridge context 进行恢复。

不要把内部 tool-call JSON 当作最终自然语言答案。详见 [JSON → MCP Bridge](extension-bridge.md)。

## 症状：Chrome 提示“设备上的应用”/loopback 权限

Chrome/Chromium 对本机 loopback 网络可能要求单独授权。

进入扩展管理页面，检查该扩展的网站/本地设备访问权限。Native Messaging 是主链路，但某些诊断/兼容路径仍可能触发 loopback 权限提示。

如果 Options 的连接测试一直 pending，先排这个权限，而不是轮换 Herdr 凭据。

## 症状：Cloudflare 部署失败

先分清是哪一类：

- **凭据失败**：Cloudflare API 权限/identity；
- **构建失败**：Edge TypeScript/test；
- **Worker 部署成功但 health 失败**：配置/runtime link；
- **workers.dev 正常但 Custom Domain 失败**：domain/route/DNS 层。

最小权限凭据见 [Cloudflare Edge 凭据](cloudflare-edge-token.md)，部署结构见 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

不要为了一个 route/DNS 问题扩大长期 Worker token 为账号管理员。

## 症状：Runtime 升级后出问题

如果只是同一 contract epoch 内的新 runtime implementation：

```bash
bin/herdr-runtime-generation status
```

确认 active/previous generation，再按 [Runtime A/B](runtime-self-upgrade.md) 回滚。

如果 tool catalog / contract epoch 改了，这不是普通 A/B 问题，必须按 contract migration 单独处理。不要用 `herdr-self-update` 偷跨 epoch。

## 日志和 Issue 最有用的信息

比“不能用了”更有价值的是：

- 当前 `boot_id`；
- runtime version / contract epoch；
- workstation id；
- 出错的具体 tool；
- `failure` / `failure_phase` / `delivery_state`；
- mutation 前后是否看到 Git/pane/Agent 状态变化；
- Edge `/health` 是否看到 workstation；
- 新会话还是旧会话；
- 浏览器扩展是否绑定正确 workspace。

注意清理 token、OAuth JWT、Cloudflare secret 和项目敏感内容后再提交日志。

## 最后再考虑重启

有些错误当然可以通过重启恢复，但排障时最好先抓住事实：

1. 记录当前状态；
2. 确认故障层；
3. 再只重启相关组件；
4. 重启后重新验证这一层和下一层。

这样不会把“偶发好了”误认为“根因已经修复”。
