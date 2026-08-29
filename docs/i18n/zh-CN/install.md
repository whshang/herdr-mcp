# 安装：从一台 Herdr 工作站到可用的 Web AI 开发环境

> **推荐给普通用户：** 不需要从这里手抄命令。把 [快速 Agent 安装](quick-agent-install.md) 里的一句话发给 Cursor / Codex / Claude Code 等本地 Coding Agent，让它自动安装 Herdr、herdr-mcp、Edge 和 Link，只在 Cloudflare 与 ChatGPT 必须人工操作时暂停。本页保留为手动安装和排障参考。

目标是把一台本地工作站接到 ChatGPT / Web AI，同时保持代码和真实执行环境留在自己的机器上。

## 安装前确认

### 1. Herdr 已经可用

```bash
herdr --version
herdr api schema >/dev/null
```

如果 Herdr 未安装，推荐直接使用 Herdr 官方 stable 安装器：

```bash
# macOS / Linux
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows：

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

安装后重新执行 `herdr --version`。Herdr 本体的详细安装行为以 <https://herdr.dev/docs/install/> 为准。

### 2. 明确要连接的客户端

- ChatGPT / 其它公网 Web AI → 需要 Cloudflare Edge + 出站 Herdr Link；
- 只在本机用 MCP 客户端 → 可以直接连接 loopback runtime，不需要 Cloudflare；
- 浏览器扩展 → 是基础 Connector 连通后的可选能力，不是第一次安装前置条件。

## 支持平台

当前 stable runtime 是 **`v0.4.2`**。最充分的 clean-machine qualification 证据仍来自 `v0.4.0` 的 **macOS Apple Silicon** 验收。Windows x64 Release binary 已提供，但 Windows 端到端 UAT 仍在继续；Linux runtime 暂不作为当前 stable 的正式支持面承诺。

## 第一步：安装原生 herdr-mcp runtime

从 <https://github.com/whshang/herdr-mcp/releases> 下载当前平台的最新 stable `herdr-mcp` binary，放到 `PATH`，然后执行：

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

`install` 会把不可变 generation 放到 `~/.config/herdr-mcp/runtime/` 并让用户 PATH 入口指向 `runtime/current/herdr-mcp`。普通用户不要用 git clone、`npm` 或 `cargo` 安装本机 runtime。

## 第二步：先把本地 runtime 跑通

至少确认：

```bash
herdr-mcp doctor
herdr-mcp status
```

如果本地 doctor 不健康，不要继续部署公网 Edge。先解决本机 runtime / Herdr 问题。

## 第三步：部署稳定公网 Edge

如果 ChatGPT 需要从公网访问工作站，使用 Cloudflare Worker 提供稳定 OAuth/MCP 入口。首次安装优先 `workers.dev`，除非你明确需要自有域名。

推荐让 Coding Agent 按 [快速 Agent 安装](quick-agent-install.md) 自动完成这段，因为里面包含 Token 最小权限、Worker 命名、secret 注入、Account 选择和代理判断。

手动执行时，至少遵守：

- Cloudflare API Token 只作为临时进程环境变量；
- 不把 Token 写进仓库、日志、截图或 shell history；
- 默认 `workers_dev = true`、`routes = []`；
- Worker 名使用仓库 helper：

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

- `LINK_SHARED_SECRET` 作为 Worker secret 保存；
- 工作站只主动建立出站 WSS，不暴露本机公网端口。

详细手动协议见 [Agent 协助安装](agent-install.md) 与 [Cloudflare Edge 部署](cloudflare-edge-deployment.md)。

## 第四步：安装并验证 Herdr Link

```bash
herdr-mcp link install
herdr-mcp link status
```

如果 `workers.dev` 在当前网络不可达，优先复用已有代理：

```text
HERDR_LINK_PROXY
HTTPS_PROXY / https_proxy
HTTP_PROXY / http_proxy
ALL_PROXY / all_proxy
```

macOS 也会读取 `scutil --proxy`。不要还没验证代理就把网络问题扩大成 DNS / Custom Domain 变更。

## 第五步：验证公网路径

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

未带 OAuth 的 `/mcp` 返回 `401` 可以是正确结果。真正的成功标准是：runtime 健康、Link 已连接、Edge `/health` 正常、OAuth metadata 可访问。

## 第六步：在 ChatGPT 添加 herdr Connector

这一步需要用户本人操作。让 Coding Agent 暂停并指导：

1. 打开 ChatGPT 设置中的 Apps / Connectors；
2. 当前 UI 需要时开启 Developer mode；
3. 添加自定义 MCP Connector，名称建议 `herdr`；
4. URL 填部署后的 `${MCP_URL}`，必须以 `/mcp` 结尾；
5. 完成浏览器 OAuth；
6. 在新会话或 Project 中启用该 Connector。

然后先做只读验证：

```text
分析我的 Herdr 里有哪些项目。只读，不要修改。
```

如果 `herdr_inspect` 能返回真实工作站数据，基础闭环就已经可用。

详见 [ChatGPT Connector](chatgpt-connector.md)。

## 第七步：需要浏览器连续工作时再装扩展

浏览器扩展用于 Side Panel 控制中心、workspace binding、长对话连续性和“排队”下一轮消息。它不是基础 MCP 闭环的必需项。

最终用户只从 **Chrome Web Store** 安装：

1. 打开 <https://chromewebstore.google.com/>；
2. 搜索 `Herdr`，选择 Herdr 官方扩展；
3. 点击 **添加至 Chrome / Add to Chrome**；
4. Chrome Web Store listing 尚未正式上线时，直接跳过扩展，不改用本地开发版；
5. 安装完成后运行：

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

以后扩展版本通过 Chrome Web Store 正常更新机制分发；普通用户不需要重复下载 zip、覆盖本地目录或手工 Reload 扩展。

详见 [浏览器扩展](extension.md) 与 [浏览器控制中心](browser-control-center.md)。

## 什么叫“装好了”

至少满足：

- `herdr --version` 正常；
- `herdr-mcp doctor` 健康；
- Herdr Link 已连接；
- Edge `/health` 正常；
- ChatGPT OAuth 完成；
- 新会话能调用 `herdr_inspect` 读取真实工作站；
- 可选扩展如果已安装，`herdr-mcp native-host status` 正常且 Side Panel 能看到 workspace。

## 手动安装之外

如果目标只是“尽快用起来”，回到 [快速 Agent 安装](quick-agent-install.md)，让本地 Coding Agent 按协议自动执行。

更深入的内容按需查看：

- [故障排查](troubleshooting.md)
- [架构](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge 部署](cloudflare-edge-deployment.md)

维护者 UAT、GA gate 和发布证据不属于普通用户安装流程。
