# 故障排查

按症状优先的常见故障清单。拿不准时，重新连接后**新开一个会话**——很多“0 工具”报告其实是快照过期，不是服务挂了。

## ChatGPT 显示 0 工具，或工具数量是旧的

- 重新连接后**新开会话**；旧会话持有旧的工具快照。
- 确认 MCP URL 与 `OAUTH_ISSUER` 使用**同一源站**（环境变量里不要带 `/mcp` 后缀）。
- 检查 Edge 健康、`herdr-link` 连通性、OAuth discovery（`/.well-known/oauth-authorization-server`）。
- 当前生产契约是 epoch 2，共 **18 个工具（含 `herdr_skill`）**。如果 ChatGPT 仍显示 epoch-1 的 17 个工具，说明 Connector 缓存过期。运行时版本来自 `/.well-known/mcp.json` / `initialize.serverInfo.version`。

硬性要求与诊断方法：见 [连接 ChatGPT](chatgpt-connector.md)。

## MCP Connector 的授权卡片反复弹

确认扩展已加载、当前标签页是 `chatgpt.com`、Options 已勾选**启用项目自动**、当前 Project HUD 显示 **`自动 开`**。权限卡自动处理已并入 Project 自动化；全局手动或 Project `自动 关` 时扩展仍观察页面和 Herdr 状态，但不会点击权限卡。浏览器原生权限条也始终需要人工处理。见 [浏览器扩展](extension.md)。

## HUD 绑定的是 w68，但底栏显示了另一个项目名

以 `workspace_id` 为身份事实，label 只是展示缓存。当前扩展会用实时 `/push/events` / `/push/state` workspace catalog 覆盖历史 binding 里的陈旧 label，并自动修复持久化记录。若浮层已显示正确的 `herdr-mcp (w68)` 而底栏仍显示旧项目名，先确认已经加载当前 0.1.47 扩展并刷新页面；不要通过解绑/重绑来“修”同一个 workspace id。

## ChatGPT 回复了一半就停住，页面像缓存了旧状态

0.1.44 不再直接把这种现象等同于“模型卡死”。当前 Project 必须 `自动 开`，扩展才会做自动 stale-view 恢复。最近回合无页面进展约 30 秒后，它会 best-effort 比较 ChatGPT 同源 conversation snapshot 与当前 DOM：服务端明确领先页面时刷新一次；服务端自己仍未完成且至少 60 秒无进展时也可进入刷新（页面仍显示 streaming 时再多等 30 秒）。刷新后若页面追上服务端就停止恢复；如果仍是同一半截回复，10 秒后发送一次“浏览器恢复”消息激活当前会话。

如果内部 snapshot 接口不可达、返回错误或结构发生变化，freshness 状态为 `unknown`，扩展会 fail-closed，不凭“时间看起来很久”就盲目刷新。此时可以手动刷新页面，再用 HUD **手动继续** 或 **herdr监控** 激活。为避免重复 mutation，恢复消息会要求从实际停止处继续并重新核对实时 Herdr/runtime/Git 状态。

## “手动接力”按钮不可用，或一直显示“压缩中/接力中”

0.1.47 的 **手动接力**支持已绑定 ChatGPT Project 和已经落成稳定 `/c/<chat_id>` 的 z.ai 会话；z.ai 根页 `/`、普通 ChatGPT `/c/<id>`、Claude、DeepSeek 不显示该按钮。当前作用域必须先切到 `自动 关`；`自动 开` 时按钮会锁定，background 也会拒绝 `automation_enabled`。绑定 workspace 仍有 agent `working` 时同样拒绝开始，因为接力不能和 settled/wake 的投递目标迁移竞争。

如果 z.ai 已经进入 `/c/<chat_id>` 但 HUD 仍像根页，请确认 0.1.47 已加载并刷新一次。新聊天在 `/` 上临时建立的 binding/自动化偏好会在首次落成 `/c/<chat_id>` 时迁移一次；之后在已有 `/c/A`、`/c/B` 之间切换不会跟着迁移。z.ai 的 handoff summary/seed 使用 raw 通道，不会被 JSON→MCP bridge 改写成 coding-agent task。

若按钮显示 **压缩中… / 接力中…**，说明同一个 source conversation 已有活跃 transfer，扩展会锁住按钮避免创建第二个接力；若 seed 投递不确定则会显示 **恢复接力**，再次点击会先探测目标 conversation 是否已经收到 transfer marker，再决定完成 cutover 还是重试 seed。旧 workspace binding 在新 seed 被确认前始终保持权威，不要为了“解锁按钮”手工解绑。

## 本地服务器无响应

- `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/` 应返回 `200` 或 `401`，而不是连接错误。
- macOS 用 LaunchAgent 时：`herdr-mcp status`，再 `herdr-mcp logs [-f]`；`herdr-mcp watchdog install` 每 120s 自动重启掉线的 MCP。
- 确认服务器使用的 `HERDR_MCP_PORT` 与你探测的端口一致。

## 工具间歇性失败，agent 仍显示 working

这是瞬时控制面故障（Herdr 守护进程的 ExceptionGroup/TaskGroup 聚合）：一次请求失败，几秒后同样的请求又成功。不要把控制面波动当成仓库阻塞；用 `herdr_inspect` / `herdr_since` 复查。部分请求会降级为组合 list API，并带 `warnings`（如 `snapshot_failed_used_list_apis`）。见 [架构](architecture.md)。

## 本地 worker 不可用

Pi/Herdr worker 掉线时，`dsh --profile headless "任务"` 是实测可用的 CLI 备选——要通过 `herdr_exec_start` 长任务 session 跑，因为工具可能已改完代码却还没打印最终回复；重试前先看 Git/测试。`dsh-tui` 只作人工交互接管，不是默认自动化面。见 [worker fallbacks](worker-fallbacks.md)。

## 想回滚某个运行时发布

Runtime A/B 保留上一代际：`herdr-runtime-generation status`，然后 `rollback`（或 `activate --generation <上一代>`）。绝不用 `herdr-self-update` 跨契约代际。见 [Runtime A/B 自升级](runtime-self-upgrade.md)。

## Token 安全方面的坑

- 绝不要把静态 `HERDR_MCP_TOKEN` 粘给 ChatGPT——它在 Edge 走 OAuth。
- 绝不把 `~/.config/herdr-mcp/*.env`（Cloudflare cutover 凭据，权限 `0600`）提交进 Git。
- Cloudflare token 用最小权限；只有一次性遗留迁移才临时扩容。见 [Cloudflare Edge Token](cloudflare-edge-token.md)。

## 还是搞不定？

- 本地：`herdr-mcp logs -f` 或服务器 stdout；启动行会打印 `boot_id` 与监听端口。
- Edge：Worker 的 `/health` 端点与 OAuth discovery。
- 提 issue 时附上：`boot_id`、失败的工具与 `failure_phase`，以及出错前是否发生过 `commitAtomic` / `herdr_exec` 投递。