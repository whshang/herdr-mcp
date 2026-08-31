# 快速开始：完成安装后的第一次真实任务

> **职责：** 本页从“herdr-mcp 已安装并连接”开始，只验证第一次真实远程开发闭环。安装协议见 [Agent 安装](agent-install.md)；人工安装与运维步骤见 [安装](install.md)。

第一次体验不需要重新部署 Edge、重新安装 runtime，也不需要先装浏览器扩展。目标只有一个：证明 Web AI 真的到达了你的工作站，并能在真实仓库里完成一次可验证的工作。

## 1. 先做一次只读检查

在一个新的 ChatGPT 会话里发送：

```text
检查当前 Herdr workspace 和 Git 状态。只读，不要修改。
```

正常路径应当是：

```text
herdr_inspect
  ↓
选择真实 managed Git root
  ↓
herdr_git status
  ↓
herdr_fs_read / herdr_fs_grep
  ↓
基于本机事实回答
```

这一步验证的不是“Connector 显示已连接”，而是公网 MCP、workstation Link、runtime 与 Herdr 现场确实连成了一条链。

如果这里出现 `workstation_offline`、0 tools、OAuth 循环或 managed-root 拒绝，不要继续做写入测试，先按 [故障排查](troubleshooting.md) 定位对应层。

## 2. 做一次确定性小修改

选择一个安全、容易验证的改动，例如文档错字、测试夹具或小范围配置，然后要求：

```text
先检查 Git 状态，读取目标文件，完成这一个小修改，运行最相关的验证，最后给我 diff 和结果。不要调度本地 Agent。
```

理想流程是：

```text
inspect → read → patch → test → diff
```

这一步验证 Web planner 能直接完成确定性工作，而不是把所有事情再次包装成 Coding Agent 任务。

## 3. 再试一次真正值得委派的任务

只有任务需要独立推理、并行调查或较长执行时，再让 Web planner 调度本地 worker。例如：

```text
调查这个失败测试的根因并实现最小修复。保持无关文件不变，完成后由你重新检查 Git diff 和测试结果。
```

本地 Agent 的最终文字不是事实源。Web planner 应重新读取仓库、测试和运行状态再验收。worker 选择、超时和 fallback 规则见 [Worker 备选](worker-fallbacks.md)。

## 4. 需要长时间离开页面时，再加浏览器扩展

标准 MCP 已经足够完成文件、Git、Shell、Agent 和多 workspace 工作。浏览器扩展只在你需要这些能力时再加入：

- 本地 `working / progress / settled` 主动回到正确网页会话；
- 页面卡顿、刷新和长对话接力；
- Chrome Side Panel 观察 workspace / pane / Agent；
- 当前回复结束后发送的“排队”消息。

第一次启用扩展时保持 Auto 关闭，先确认 binding 与实时状态正确。扩展总览见 [浏览器扩展](extension.md)，连续工作机制见 [浏览器连续工作](browser-continuity.md)，Side Panel 操作见 [浏览器控制中心](browser-control-center.md)。

## 5. 什么才算第一次体验成功

最低验收不是“服务健康”，而是下面四件事同时成立：

1. Web AI 能读取真实 Herdr workspace 和真实仓库；
2. 一个小修改经过相关验证并能看到真实 diff；
3. 需要委派时，Web planner 能重新验收 worker 的实际结果；
4. 如果启用浏览器扩展，长任务完成后能安全回到正确会话，而不是重复执行原 mutation。

做到这里，安装阶段就结束了。日常工作方式继续看 [最佳实践](best-practices.md)；系统为什么这样设计看 [架构](architecture.md)。
