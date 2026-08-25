# Worker 备选：什么时候应该派 Agent，什么时候不应该

herdr-mcp 把 Web AI 视为高层 planner。本地 Agent 是可以被调用的执行 worker，不是第二层总调度器。

这篇文档回答两个问题：

1. 什么任务值得交给本地 worker？
2. 当首选 worker 不可用、卡住或超时时，怎样切换而不重复已经发生的 mutation？

## 先决定：这个任务真的需要 Agent 吗

很多开发动作不需要额外模型推理：

```text
读文件      → herdr_fs_read
搜索代码    → herdr_fs_grep
看 Git      → herdr_git
精确修改    → herdr_fs_edit / patch
跑命令      → herdr_exec
长测试      → herdr_exec_start/read
```

只有当任务包含明显的独立推理价值时，才值得派 worker，例如：

- 阅读一组陌生代码并提出实现方案；
- 独立实现一个边界清晰的功能；
- 并行调查另一个假设；
- 对已经完成的 diff 做独立 review；
- 在多个方案间做技术比较。

原则：

> 能确定性完成的工作直接做；需要独立思考的工作才委派。

## 推荐 worker 顺序

| 优先级 | Worker 类型 | 典型入口 | 适合任务 |
|---|---|---|---|
| 1 | Herdr-native coding worker | `herdr_prompt` | 窄范围编码、调研、review |
| 2 | 其它 Herdr pane 中的可用 worker | `herdr_prompt` | 替代实现、并行验证 |
| 3 | 外部无头 Coding Agent CLI | `herdr_exec_start` | 首选 worker 不可用时的边界明确任务 |
| 人工 | 交互式 TUI / shell | 人工接管 | 审批、恢复、复杂诊断 |

具体品牌或模型不是长期规则。哪个 worker 排在前面，取决于它是否：

- 能无头稳定执行；
- 有清晰的任务边界；
- 能观察状态；
- 能验证输出；
- timeout 后能判断是否已经修改过代码。

## 为什么优先 Herdr-native worker

Herdr-native worker 已经处于受管理的 workspace/pane 生命周期里，所以 Web planner 可以获得：

- working / idle / done 状态；
- pane 输出；
- cwd；
- workspace 归属；
- prompt delivery evidence；
- `idempotency_key`。

典型调用：

```text
herdr_prompt
  ↓
herdr_since / herdr_inspect
  ↓
Git / tests 验证
```

这比启动一个完全独立的 CLI 进程更容易长期编排。

## 外部 CLI worker 的正确位置

有些 Coding Agent 提供 headless CLI，可以作为备用 worker。

正确模型：

```text
Web planner
  ↓
herdr_exec_start
  ↓
external coding CLI
  ↓
Git / tests
```

不要把外部 CLI 当成新的 planner，再让它去决定如何派其它 Agent。任务应该由 Web planner 预先收窄。

推荐任务契约包含：

- 明确仓库；
- 明确文件或功能边界；
- 不修改无关内容；
- 明确完成标准；
- 明确验证命令；
- 不再继续转派其它 Agent。

## 为什么外部 Coding Agent 应使用长命令 session

Coding Agent 的最终自然语言回复可能比实际代码修改晚很多。

如果用同步 `herdr_exec`：

```text
CLI 修改已经完成
      ↓
等待模型总结
      ↓
客户端 timeout
      ↓
误以为任务失败
      ↓
再次提交同一个任务  ← 危险
```

因此外部无头 Agent 应优先使用：

```text
herdr_exec_start
  ↓
herdr_exec_read
  ↓
Git / tests 检查
  ↓
需要时 herdr_exec_kill
```

进程 timeout 和任务失败不是同一个概念。

## Timeout 后先看事实，不要先重试

任何 coding worker 超时后，都按下面顺序：

```text
1. 看 worker/pane/process 状态
2. git status
3. git diff
4. 看目标文件
5. 跑相关测试
6. 再决定继续、修正、取消还是重试
```

如果目标 diff 已经出现，说明 mutation 至少部分发生了。

这时正确动作通常是：

- 验证结果；
- 提示 worker 收尾；
- 或直接由 Web planner 修剩余小问题。

不是重新提交整份原任务。

## 怎样判断一个 worker 已经卡住

不要只根据“几分钟没最终回答”。更有价值的信号是：

- pane/process 仍 active，但输出长期不变；
- Git 没有任何相关 diff；
- CPU/process 状态显示不再推进；
- worker 一直重复同样的读取；
- 已超过任务本身合理预算；
- 关键路径被它独占，却没有新证据。

这时可以：

1. 再读一次当前状态；
2. 如果没有 mutation evidence，取消；
3. 换 deterministic path 或另一个 worker。

不要为了“已经等了很久”继续无限等。

## 并行 worker 什么时候有价值

并行不是越多越好。

适合并行：

```text
worker A → 实现
worker B → 独立 review
```

或：

```text
worker A → 调查浏览器问题
worker B → 调查 server 问题
```

不适合：

```text
worker A、B、C 同时修改同一个文件
```

除非它们各自在独立 worktree 中，并且 Web planner 明确负责最后合并。

## 用 worktree 隔离真正的并行开发

当两个 worker 都需要修改代码：

```text
main worktree
    │
    ├─ worktree A → implementation
    └─ worktree B → alternative / review fix
```

这样可以避免：

- dirty file gate；
- 互相覆盖修改；
- 无法判断 diff 属于谁；
- 一个 worker 的 reset/format 影响另一个。

Web planner 最后比较 Git diff 和测试结果，再决定 merge/cherry-pick/手工整合。

## Review worker 不应该默认成为主编辑者

审计/review worker 的价值在于独立性。

推荐：

1. 主路径实现；
2. 跑确定性测试；
3. 把 diff 交给独立 review worker；
4. Web planner 判断问题是否成立；
5. 小修直接处理，大修再委派。

如果 review worker 一开始就拥有整份实现，它就失去了独立检查的价值。

## 外部 worker 升级后的验证

外部 Agent CLI 通常更新很快，不应在长期文档里硬编码具体版本行为。

升级后重新验证：

1. `--version`；
2. headless/non-interactive help；
3. 一次无工具回答；
4. 一个临时 Git 仓库里的小修改；
5. timeout 后是否能通过 Git 判断 mutation；
6. 如果依赖 profile/plugin，验证配置能够正常加载。

只有这些通过，才把它放回自动化关键路径。

## Human takeover 是正式能力，不是失败

有些任务天然适合人类接管：

- OAuth / 登录；
- 安全审批；
- TUI 交互；
- 需要视觉判断的复杂状态；
- 高风险外部 mutation；
- worker 行为已经不可预测。

Herdr 的 workspace/pane 可见性让人类可以随时观察并接管，而不是只能等一个黑盒 Agent 返回结果。

## 推荐的委派流程

```text
Inspect
  ↓
能否 deterministic 完成？
  ├─ yes → fs/git/exec
  └─ no
       ↓
   选择一个窄 worker 任务
       ↓
   dispatch
       ↓
   since / process read
       ↓
   Git + tests
       ↓
   必要时独立 review
       ↓
   Web planner 决定下一步
```

## 最终原则

Worker 是**可替换的执行资源**，项目状态才是事实。

因此完成标准永远不是：

> “Agent 说它完成了。”

而是：

- diff 正确；
- tests 通过；
- runtime 状态符合预期；
- 没有遗漏的副作用。

这让 herdr-mcp 可以接入不同 Agent，而不把整个编排架构绑死在某一个模型或 CLI 上。
