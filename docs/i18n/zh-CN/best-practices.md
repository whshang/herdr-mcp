# 最佳实践

让系统保持快速、安全、可观测的工作流。这些是项目遵循的运行规则，请当作默认做法，而不是唯一做法。

## Web 规划，本地干活

编排发生在网页会话里，重活交给廉价的本地 worker。

- 优先用 `herdr_fs_*` / `herdr_git` / `herdr_exec`——不经过本地 agent API。
- 确实需要推理时，优先用 `herdr_prompt` 发给廉价/快速 worker（`pi`、`flash`、`cline`、`opencode`、`anti`）或审计者（`droid`、`grok`）。不要把规划或委派交给本地 Claude/OMP/main。
- Pi/Herdr worker 不可用时，`dsh --profile headless "任务"` 是实测可用的 CLI 备选——要通过 `herdr_exec_start` 长任务 session 跑，不要用 60 秒同步 shell。见 [worker fallbacks](worker-fallbacks.md)。

## 每次会话的固定开场

1. `herdr_inspect`——连接健康、工作区、窗格、agent。
2. `herdr_skill`——每个会话一次，加载项目策略与匹配版本的 upstream Herdr 指导。
3. 然后开始干活。之后用 `herdr_since <cursor>` 续接，而不是重新倾倒全量状态。

详见 [架构](architecture.md)（工具面、epoch 2）。

## 变更纪律

- `herdr_prompt` 默认 fire-and-forget，务必带 `idempotency_key`；用 `herdr_since` / `herdr_inspect` 追踪投递。
- 任何传输失败后先查状态再重试——绝不对非幂等变更盲目重试。
- `herdr_exec`：控制面在投递前失败时，本地 fallback 可以运行；已投递则返回结构化错误——绝不重复执行。
- 失败的 `commitAtomic` 会清理本次尝试新增的文件；写入始终受 managed-root / dirty / busy / readonly 闸门约束。

## 让 Edge 成为唯一的对外面

- 工作站只建立出站的已认证 WSS（`herdr-link`）。不存在公网入站端口；除遗留迁移外，不要直接把本地 MCP 服务器暴露到公网。
- MCP URL 与 `OAUTH_ISSUER` 必须在**同一源站**上；`OAUTH_ISSUER` 不要带 `/mcp` 后缀。源站不一致是 Connector 最常见的失败原因。
- Cloudflare token 用最小权限（Workers Routes Write + Workers Scripts Write）——见 [Cloudflare Edge Token](cloudflare-edge-token.md)——并且绝不把 `~/.config/herdr-mcp/*.env` 提交进 Git。

## 升级用 A/B，不要一刀切

运行时发布在稳定的 Edge/Link 后面切换：用冻结的工具契约验证新代际、原子激活、排空，需要时回滚——ChatGPT Connector 完全不用改。绝不用 `herdr-self-update` 跨契约代际。见 [Runtime A/B 自升级](runtime-self-upgrade.md)。

## 端到端示例

1. ChatGPT 连上 Edge MCP 端点并完成 OAuth（见 [安装](install.md)）。
2. 新开会话；模型先调 `herdr_inspect` 看工作站，再调一次 `herdr_skill`。
3. 你要求修改某个 git 管理的项目：模型用 `herdr_fs_read` / `herdr_git status` 读、用 `herdr_fs_patch` 改、用 `herdr_exec` 跑测试，并在 managed root 下通过原子 Git 助手提交。
4. 在 MV3 扩展里把网页会话绑定到实际工作的 workspace。Options 选择**全局手动 / 项目自动**；ChatGPT Project 只有在 HUD 显式开启 `自动 开` 后，才会执行进度/收工、LLM 判断、回复恢复和安全自动接力；z.ai / DeepSeek 则提供更窄的会话级开关，只自动回推 progress/settled。需要主动 **手动接力** 时先切到 `自动 关`；已绑定 ChatGPT Project 和持久 z.ai `/c/<chat_id>` 会话可用。扩展与 Herdr 的通信保持在本机：浏览器通过 Native Messaging 交给 host，再由权限为 `0600` 的 Unix Socket 进入 runtime，浏览器侧不持有 Herdr bearer。见 [extension-wake](extension-wake.md)。
5. 某个 worker 掉线时，模型改走 `dsh --profile headless`（通过 `herdr_exec_start` 长会话）而不是卡住。

结果：一个稳定的公共契约、廉价的本地算力，加上一条让网页会话保持鲜活的回流通道。