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

以 `workspace_id` 为身份事实，label 只是展示缓存。当前扩展会用实时 `/push/events` / `/push/state` workspace catalog 覆盖历史 binding 里的陈旧 label，并自动修复持久化记录。若浮层已显示正确的 `herdr-mcp (w68)` 而底栏仍显示旧项目名，先确认已经加载当前 0.1.43 扩展并刷新页面；不要通过解绑/重绑来“修”同一个 workspace id。

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