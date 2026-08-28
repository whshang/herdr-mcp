# 快速开始：从零体验第一次远程开发

目标：在已经安装 Herdr 的情况下，让 ChatGPT 等 Web AI 第一次真正连接自己的开发环境。

整个过程只需要理解一件事：

> Herdr 提供本机开发现场，herdr-mcp 把这个现场安全地连接给 Web AI。

## 1. 准备本机开发现场

herdr-mcp 不替代 Herdr。先安装并启动官方 Herdr：

```bash
herdr --version
herdr api schema >/dev/null
```

如果第一次接触 Herdr，先理解这些概念：

- workspace：一个长期存在的工作空间；
- tab：工作区中的标签页；
- pane：具体终端/Agent 工作面板；
- agent：正在执行任务的本地智能体；
- session：可以恢复的长期执行状态。

这些状态会成为 Web AI 后续观察和恢复工作的依据。

## 2. 安装 herdr-mcp

从 [GitHub Releases](https://github.com/whshang/herdr-mcp/releases) 下载当前平台的原生 `herdr-mcp` 二进制，放到 `PATH` 上，然后验证：

```bash
herdr-mcp doctor
herdr-mcp status
```

本机 MCP runtime **不需要** Node.js / npm。优先使用顶层 `doctor` / `status` / `update ...`；不要把 `herdr-mcp service install` 当成普通安装主路径。

确认本机 HTTP 在听：

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` 或 `401` 都说明本机 HTTP 进程已起来。

## 3. 选择你的第一次连接方式

### 路线 A：ChatGPT Connector

适合：希望用最强 Web 模型直接开发。

链路：

```text
ChatGPT
 ↓
Cloudflare Edge + OAuth
 ↓
herdr-mcp
 ↓
Herdr 工作站
```

继续阅读：

- [安装](install.md)
- [连接 ChatGPT](chatgpt-connector.md)

首次部署推荐使用 `workers.dev`。自定义域名属于长期生产优化，不是第一次运行的前置条件。

### 路线 B：浏览器连续工作

适合：希望网页会话持续观察本地任务。

浏览器扩展提供：

- workspace 绑定与 Agent 进度 / settled 回推；
- ChatGPT 页面恢复与长对话接力；
- Chrome Side Panel 浏览器控制中心：实时查看 workspace / pane / Agent；
- 明确 pin 一个 pane，读取状态与最近输出；
- ChatGPT **排队**：当前回复不中断，补充要求在下一轮优先发送；
- z.ai / DeepSeek 的 JSON → MCP 兼容桥。

第一次使用先保持 Auto 关闭，确认 binding、Control Center 实时状态和人工操作都符合预期，再按作用域开启自动化。

继续阅读：[浏览器扩展](extension.md) 和 [浏览器控制中心](browser-control-center.md)。

### 路线 C：本地 CLI / Agent 集成

适合：Cursor、Claude Code、Pi、其它本地 Agent。

它们可以直接使用本机 MCP，不经过公网 Edge。

## 4. 第一次让 ChatGPT 操作项目

推荐从一个小任务开始：

例如：

> 查看这个仓库最近一次测试失败原因，修复后运行相关测试。

一个正常流程应该类似：

1. ChatGPT 调 `herdr_inspect` 查看当前工作现场；
2. 使用 `herdr_git` 确认仓库状态；
3. 使用 `herdr_fs_read/grep` 定位代码；
4. 使用 patch 修改；
5. 使用 exec 运行测试；
6. 检查 diff 和测试结果。

需要并行分析时，再调度本地 Agent。

## 5. 验收连接是否真正成功

不要只检查服务有没有启动。真正可用需要验证：

- ChatGPT 能获取当前 MCP tools；
- 工具调用可以读取真实项目文件；
- Git 状态来自你的真实仓库；
- 浏览器扩展可以看到 workspace；
- 长任务完成后，状态可以回到网页会话。

## 6. 什么时候开启自动继续

自动化用于减少等待，不用于绕过确认。

建议：

- 第一次使用保持手动模式；
- 熟悉流程后，再针对稳定项目开启自动继续；
- 高风险操作（部署、权限修改、生产数据变化）保持人工确认。

## 下一步

- 深入理解：[设计思路](design-philosophy.md)
- 理解系统：[架构](architecture.md)
- 连接 ChatGPT：[ChatGPT Connector](chatgpt-connector.md)
- 学习日常流程：[最佳实践](best-practices.md)
