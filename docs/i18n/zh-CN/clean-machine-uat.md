# 干净机 UAT 清单（G18）

给**从未把本仓库当 runtime 安装源**的第二台机器用的命令清单。在已有 production Link 的开发机上跑通，**不能**封 G18。

仅完成此清单也**不要**打非 alpha 的 stable tag。产品仍处 alpha，直到 G1 + G18 + 其余 GA veto 通过。

## 测试平台

第一版 GA 建议：只正式测 **macOS Apple Silicon**。

- Windows Release 二进制：preview / 可选观察，不当第一 GA lifecycle 封板。
- Linux lifecycle：不宣称。

## 前置

- 全新用户账号或 VM，已按 <https://herdr.dev> 装好 Herdr。
- 本机 runtime 路径不要求事先有 `~/herdr-mcp` checkout。
- 能访问 GitHub Releases；（走 ChatGPT 时）能访问 Cloudflare。

## A. 仅本机 runtime

```bash
herdr --version
herdr api schema >/dev/null

# 从 GitHub Releases 下载当前平台二进制并放入 PATH，然后：
chmod +x "$(command -v herdr-mcp)"
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

期望：

- `doctor` 本机 Herdr / runtime / service 层 PASS
- PATH 上的 `herdr-mcp` 经 `~/.config/herdr-mcp/runtime/current` 解析
- 本机 runtime 不需要 `git clone` 或 `npm ci`

## B. 可选公网 ChatGPT 路径

按 [安装](install.md) / [Agent 辅助安装](agent-install.md)，只用 Release 二进制 + 临时 Edge bootstrap：

```bash
herdr-mcp doctor
# Edge + Link 配好后：
# doctor 应区分 Edge configured / reachable / OAuth metadata / MCP endpoint
# 且永不打印 token
```

然后在**新的** ChatGPT 会话：

1. Connector OAuth 成功
2. `tools/list` 为 epoch 2 / 18 tools
3. 一次只读工具调用（如 `herdr_inspect`）
4. 一次操作者选定的有界真实 mutation
5. 若本轮 UAT 含长任务 / streaming basics，再补一次烟雾

## C. 同机 update / rollback

```bash
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status
herdr-mcp doctor
# 仅当已有上一托管 generation 时做受控回退：
herdr-mcp rollback
herdr-mcp doctor
herdr-mcp status
```

## D. 本清单明确不做的事

- 不在这里实现 Browser Control Plane / true-steer（G16 post-GA boundary）。
- 不把从 git checkout 解压 `extension/` 当成已封板的 G15 路径。
- 不用 `target/*/herdr-mcp` 或指向仓库的 `~/.local/bin/herdr-mcp` 当生产证据。
- 不从本清单去改 dogfood Mac 上的 production Link。

## 需要记录的证据

采集（非秘密）输出：

```bash
herdr-mcp --version
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
```

干净机真实通过后，把这些附到 GA scorecard 的 G18 行。
