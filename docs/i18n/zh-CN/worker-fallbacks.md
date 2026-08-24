# Worker 备选

herdr-mcp 始终把网页模型当作 planner。本地 agent CLI 是执行 worker，不是第二层编排。

## 推荐顺序

| 优先级 | Worker | 调用 | 用途 |
|---|---|---|---|
| 1 | Herdr 原生便宜 worker，尤其 Pi | `herdr_prompt` | 常规编码 / 调研，本地推理有用时 |
| 2 | DeepSeek Harness 无头模式 | `herdr_exec_start` 运行 `dsh --profile headless "…"` | 有边界的兜底：窄而自包含的编码或 review，Pi/Herdr 原生 worker 不可用或需要第二种实现时；不要把大范围关键路径重构放在它后面 |
| 3 | Cline / OpenCode / Anti | Herdr pane 中存在时用 `herdr_prompt` | 替代的 Herdr 原生编码 worker |
| 人工兜底 | dsh-tui | 交互式终端 | 人工接管、检查、续跑、审批；不是默认的自动化 worker |
| 审计 | Droid / Grok | `herdr_prompt` | 实现之后的独立 review，默认不做主编辑 |

确定性的文件、Git 与 shell 工作应该在任何 agent 之前先用 `herdr_fs_*`、`herdr_git`、`herdr_exec`。

## DSH 冒烟证据 — 2026-08-23

本地实测环境：

```text
@deepseek-ai/dsh 0.1.1-rc.2
@deepseek-harness-tui/dsh-tui 0.9.0
Node v24.16.0
```

安装的 `dsh-tui` profile 组合了 DeepSeek Harness rc.8 插件包。因为 DeepSeek Harness 仍是 developer preview，launcher/profile/plugin 版本可能各自演进，升级后应重新检查兼容性。

### 无头回答

非交互接口适合自动化：

```bash
dsh --profile headless "Reply exactly DSH_OK and do not use tools."
```

返回：

```text
DSH_OK
```

### 受控代码修改

在 herdr-mcp 工作树之外创建了一个临时 Git 仓库，里面放了这个刻意制造的 bug：

```js
export function add(a,b){ return a-b; }
```

DSH 只接到一个任务：只改这一个文件，让 `add(a,b)` 返回 `a+b`。

实测结果：

- 修改成功完成；
- `add(2,3)` 求值为 `5`；
- 只有该文件被改动；
- 进程没有在远程同步 60 秒预算内打印最终的 assistant 总结。

这对编排很重要：**DSH 能成功改代码，但要把它当作长任务 worker。** 不要因为 60 秒内没有最终回答就判定失败，也不要在超时后盲重试——mutation 可能已经发生。

## 从 herdr-mcp 正确调用 DSH

优先使用后台 exec session：

```text
先解析 dsh 路径（后台 exec 的 PATH 可能比可见 utility 窗格的 PATH 更小）
  -> command -v dsh
  -> 否则检查 $HOME/.npm-global/bin/dsh 或安装特有的 bin 路径
herdr_exec_start(root=<project>, command='<resolved-dsh> --profile headless "<task>"')
  -> herdr_exec_read(...)
  -> 若 worker 超过预期预算，先 inspect Git/测试再重试
  -> herdr_exec_kill(...) 只在确实需要取消时调用
```

不要假设持久 utility 窗格里可见的命令也在 `herdr_exec_start` 后台 PATH 上。在 2026-08-23 的生产冒烟中，普通 `dsh` 在后台 session 里以 127 退出，而同一安装位于 `$HOME/.npm-global/bin/dsh`。派发前先解析可执行文件，不要把 127 当成 agent/模型失败。

推荐的任务契约：

- 指明确切的仓库与文件/功能边界；
- 告诉 DSH 不要修改无关文件；
- 要求测试或确定性的验证命令；
- 不要要求 DSH 派发其它 agent；
- 超时后先 `git status` / `git diff` 再重新提交。

与 `herdr_prompt` 不同，这条路径还没有 Herdr 原生的 agent 生命周期事件或 idempotency key。这也是 Pi 在可用时仍是默认 worker 的原因。

### 来自真实 herdr-mcp 工作的编排证据

用两个独立的无头 DSH 任务处理过真实的 herdr-mcp bug：

- **GitHub Pages 部署：** DSH 完成了任务并正确判断出：静态站点/workflow 文件本身已经足够，且 workflow 提交没有到达远端 `main`。接着网页 planner 解决了机器特有的 SSH host-key 问题，完成了部署。
- **浏览器 workspace/HUD 故障：** DSH 在没有 stdout、也没有目标文件 diff 的情况下活跃了约七分钟。它在编排预算处被取消，网页 planner 回退到确定性的浏览器、socket 与网络证据。那轮调查找到了真正的根因：每个历史 binding 各建一条长命 `/push/events` SSE，耗尽了 Chromium 对单 origin 的 HTTP 连接池，导致 `/push/state` 被饿死。

这就是设计中的兜底模型：DSH 是有能力的编码 worker，但不允许无限期卡住关键路径。给它与任务匹配的预算；若没有有用的输出或 diff，先检查进程/Git 状态再取消，然后回退到直接的 fs/exec/browser 证据或另一个 worker。

### 文档改版中的无头调度教训 — 2026-08-23

文档站改版提供了一个比临时仓库冒烟更严格的编排测试。几个约束渐强的无头任务被给了同一个隔离 worktree：先是宽泛的改版，然后固定文件范围，再是精确的导航图与仅执行指令。它们在分析/工具读取上花了约几分钟而没有产出被跟踪的 diff，而确定性的网页 planner 路径在同一 worktree 里实现并验证了改动。

还测试了按次调用（per-invocation）的无头配置覆盖来降低推理、选择更快的执行模型。它改善了琐碎的无工具冒烟，但没有让多文件编码或 P0/P1 review 任务在关键路径上可靠地足够快。因此可复用的规则针对的是任务形态与证据，而不是某一个 provider 或模型：

- 架构、信息架构与跨文件规划留在网页 planner；
- 给 DSH 一个窄任务，带明确的属主文件、期望验证和时间/diff 检查点；
- 若检查点既没有有用输出也没有相关 diff，检查一次 Git/进程状态，合适就取消，然后用确定性工具或 Herdr 原生 worker 继续；
- “现在就实现”之类的措辞不能替代编排预算与完成证据；
- 对自动化无头任务有用的模型/推理覆盖，应限定在无头调用/profile（例如通过临时 `--patch`），不要作为自动化副作用写进操作者的全局交互/TUI profile。

DSH 仍是有用的可选兜底和窄范围独立检查；它不是大范围多文件实现的默认属主。

## dsh-tui

安装的 TUI profile 在把本地凭证文档升级到当前 version-1 schema（`version: 1` 加 `refs:` 映射）后可以成功 compose。之后 `dsh --profile dsh-tui --dump-config` 能成功完成。

dsh-tui 用于：

- 人类操作者接管进行中的任务；
- 浏览/恢复 Harness session；
- 交互式审批与提问；
- 检查 model/reasoning/profile 配置。

**不要**把 dsh-tui 当作 ChatGPT 的常规自动化兜底。全屏或交互式终端没有干净的机器级“最终结果”契约，无头 profile 才有。

## 升级规则

因为 DSH 是快速演进的 developer preview，绝不要硬编码某个版本的假设。升级后在 worker 顺序里提升它之前，先验证：

1. `dsh --version`；
2. `dsh --profile headless --help`；
3. 一次无工具回答冒烟；
4. 一次临时仓库修改冒烟；
5. 如果装了 TUI，`dsh --profile dsh-tui --dump-config`。

herdr-mcp 项目 skill 只应在 DSH 二进制确实存在时把它宣传为可选安装兜底；选不选它由网页 planner 负责。