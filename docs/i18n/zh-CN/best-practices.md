# 最佳实践

*让 Web AI 长期参与本地开发。*

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

只有任务需要**独立推理、真正并行或独立审查**时才委派；已知文件上的确定性编辑、Git 查询和测试不要再包装成 Agent 任务。

一次好的委派应该有明确的问题边界、工作目录、允许修改范围、验收证据和停止条件。Web planner 保留总体拆分、优先级和最终验收权，并在 worker 完成后重新检查 Git、测试和运行状态。

具体 worker 怎么选、首选 worker 不可用怎么办、长任务如何运行、timeout 后如何避免重复 mutation，统一见 [Worker 备选](worker-fallbacks.md)。本页不维护第二套 worker 顺序和 fallback 规则。

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

## 7. 浏览器扩展只负责“时间上的连续”，不替代 planner

当任务会跨越当前网页回合时，扩展负责把本机进度、恢复和接力重新接到正确会话；它不负责重新规划任务，也不能因为“继续”而跳过 live-state 验证。

日常原则只有两条：第一次使用保持 Auto 关闭；任何恢复/接力后的 mutation 都重新检查 Herdr、Git 和 runtime。HUD、作用域、handoff、歧义确认、429/页面恢复等机制统一以 [浏览器连续工作](browser-continuity.md) 为 SSOT；Side Panel 操作见 [浏览器控制中心](browser-control-center.md)。本页不重复浏览器状态机细节。

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
