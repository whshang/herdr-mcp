# 最佳实践：让 Web AI 长期参与本地开发

herdr-mcp 的理想工作方式是：**Web 模型负责理解目标、做决策和持续编排；确定性的本地操作直接执行；只有真正需要独立推理或并行工作时才调度另一个 Agent。**

这样既利用 ChatGPT 的长上下文和推理能力，也保留 Herdr 对本地工作现场的持久管理。

## 1. 开始工作前先确认现场

新的 Web 会话不要根据上一段聊天里的旧状态直接修改代码。

推荐顺序：

1. `herdr_inspect`：确认 Herdr 连接、workspace、pane、Agent、managed project roots 和运行环境。
2. `herdr_skill`：每个会话读取一次当前操作策略。
3. `herdr_git status`：准备修改某个仓库前确认 live Git 状态。
4. 之后用 `herdr_since` 获取增量变化，减少重复读取整个工作区。

长对话接力后的第一轮同样执行这些检查。接力摘要用于恢复意图和已知事实，live state 决定接下来能不能安全 mutation。

## 2. 确定性工作直接做

读取文件、搜索代码、查看 diff、应用明确补丁、运行测试，这些工作不需要额外启动一个 Coding Agent。

优先使用：

- `herdr_fs_read` / `herdr_fs_list` / `herdr_fs_grep`
- `herdr_fs_patch` / `herdr_fs_edit`
- `herdr_git`
- `herdr_exec`，以及长任务用的 `herdr_exec_start` / `read` / `kill`

这条原则的价值主要是减少等待、减少状态转述，并让 Web planner 直接看到操作结果。Agent 数量应该由任务并行性和推理需要决定。

## 3. 什么时候调度本地 Agent

适合 `herdr_prompt` 的任务包括：

- 可以和主线独立并行的实现；
- 需要第二种思路的故障调查；
- 独立 code review / audit；
- 较长、边界明确、结果可以通过 Git 和测试验证的工作。

Pi、Cline、OpenCode 等 worker 适合执行实现；Grok、Droid 等可以承担独立审查。具体可见 Agent 由当前 Herdr 配置决定，不应在业务流程里假设某个 Agent 永远存在。

Web planner 自己保留总体任务拆分、优先级和最终验收权。一个 Agent 完成后，直接检查 Git diff、测试和运行结果，不依赖它的自然语言“已经完成”。

Herdr worker 不可用且本机安装了 DSH 时，可以用 `dsh --profile headless` 作为 CLI fallback。长任务通过 `herdr_exec_start` 运行，避免把正常的长推理误判成同步命令超时。详见 [Worker fallback](worker-fallbacks.md)。

## 4. 并行开发要隔离工作区

多个 Agent 同时改一个 checkout 很容易制造不可解释的 dirty state。较大的并行任务应使用独立 Git worktree，并让 Herdr workspace、pane cwd、Agent cwd 都指向同一个项目根。

每个并行任务应有清晰边界：

- 要解决的问题；
- 允许修改的范围；
- 验证命令；
- 是否允许提交 / push；
- 完成后如何合并和回收 workspace。

完成的临时 workspace 应及时关闭。存在未提交修改或仍在运行的 Agent 时先检查，再决定保留、合并或回收。

## 5. Mutation 失败后先查事实

远程控制最危险的错误之一，是网络或等待超时后把同一个 mutation 再执行一次。

`herdr_prompt` 建议带 `idempotency_key`。任何提交、push、部署、消息发送、创建资源等操作出现不确定结果时，先用 `herdr_since`、`herdr_inspect`、Git 或目标系统状态确认是否已经生效，再决定是否重试。

`herdr_exec` 也遵循同样原则：命令一旦已经投递，就不能因为返回链路失败而假设它没有运行。

## 6. Git 是开发结果的事实来源

Agent 状态只能说明“谁在工作”，Git 和验证命令说明“工作产生了什么”。

推荐在修改前后检查：

```text
herdr_git status
herdr_git diff
project tests / lint / typecheck / build
git diff --check
```

不要为了得到状态而让另一个 Agent 运行 `git status`。这些确定性事实直接读取更快，也更可靠。

## 7. 浏览器扩展负责连续工作

扩展解决的是 Web 会话自身不持续运行的问题。它负责把 Herdr 进度送回网页、恢复超时回复、按作用域自动继续，并在长对话接近容量边界时生成接力摘要并迁移到新会话。

自动化开启后，应该让扩展负责这些机械动作；HUD 负责网页 / Herdr 状态、Auto、三个预置推进动作和当前会话的手动接力。需要主动切换会话时，直接使用 **HUD 的“接力”**；自动接力仍由容量和恢复策略触发。

ChatGPT Project、普通 ChatGPT 会话、z.ai 和 DeepSeek 的作用域规则不同，具体以 [浏览器扩展](extension.md) 和 [自动继续与接力](extension-wake.md) 为准。

## 8. Edge 保持稳定，本地 Runtime 可以演进

ChatGPT 保存的是稳定的 MCP/OAuth identity。工作站只建立出站认证连接，不开放公网入站端口。

因此日常升级应保持 Edge origin 和 Connector 不变，在其后升级本地 runtime。跨公共工具契约的变更使用受控的 runtime generation / A-B 切换和回滚流程。详见 [Runtime 自升级](runtime-self-upgrade.md)。

## 9. 权限按工作站风险配置

允许 Shell 意味着远程模型可以执行该用户有权限执行的命令。用于真实开发机器时：

- Cloudflare 和 ChatGPT account 应开启可靠的账号保护；
- 使用 `HERDR_MCP_WRITE_ROOTS` 限定允许写入的项目；
- 只需要观察时使用 `HERDR_MCP_READONLY=1`；
- 不把本机 bearer、`.env` 或其它凭据复制到 Connector 配置和聊天内容；
- 高风险生产 mutation 继续使用项目自身的审批和权限机制。

## 一个典型工作流

```text
用户提出目标
  ↓
ChatGPT: inspect + skill + git status
  ↓
直接读取 / 搜索 / 修改 / 测试
  ↓
有独立并行任务？ ── yes ──► Herdr worker
  │                            ↓
  no                      since / Git / tests
  │                            ↓
  └──────────────► 汇总验证结果
                       ↓
                commit / push / deploy
                       ↓
                浏览器扩展继续监控
```

核心判断始终围绕三个问题：当前 live state 是什么；这个动作能否确定性直接完成；完成后用什么事实验证。这样 Web AI 才能稳定地参与几个小时甚至跨多个对话的开发工作。