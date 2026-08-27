# 干净机 UAT 清单（G18）

给**从未把本仓库当 runtime 安装源**的第二台 Mac / VM 用的命令清单。在已有 production Link / `:8772` / `dev.herdr-mcp.*` 的狗粮机上跑通，**不能**封 G18。

仅完成此清单也**不要**打非 alpha 的 stable tag。产品仍处 alpha，直到 G1 + G18 + 其余 GA veto 通过。

## 同机隔离诚实说明

`herdr-mcp` 的 service install 写死 LaunchAgent label `dev.herdr-mcp.server` 与 loopback 端口 `8772`。产品当前**没有**提供这两项身份的覆盖开关。

| 尝试 | 狗粮机上是否安全 | 原因 |
| --- | --- | --- |
| 仅 TMPHOME / `HERDR_MCP_CONFIG_DIR` | 只能做部分探针 | 路径可隔离，但同 uid 的 `launchctl` 仍看见狗粮服务；`status`/`doctor` 可能把狗粮 `:8772` 报成 healthy |
| 同用户 `herdr-mcp install` | **否** | 会改狗粮 LaunchAgents / service |
| 同主机第二个 macOS 用户 | **不能**做完整 install | label 按用户隔离，但 `:8772` 是整机共享；启动第二实例会撞狗粮 |
| 第二台 Mac 或 VM（`:8772` 空闲、无 `dev.herdr-mcp.*`） | **是** | 诚实 G18 PASS 所需 |

TMPHOME 证据只能标 PARTIAL（见 `docs/_wip/g18-clean-machine-sim-20260828.md` 与 alpha.15 复测）。**禁止**用「同 daemon 上的 TMPHOME」假装 G18 PASS。

## 测试平台

第一版 GA 建议：只正式测 **macOS Apple Silicon**。

- Windows Release 二进制：preview / 可选观察，不当第一 GA lifecycle 封板。
- Linux lifecycle：不宣称。

## 前置

- 全新 Mac / VM，已按 <https://herdr.dev> 装好 Herdr。
- 本机 runtime 路径不要求事先有 `herdr-mcp` checkout。
- 能访问 GitHub Releases；（走 ChatGPT 时）能访问 Cloudflare。
- 安装前确认 `launchctl list | awk '$3 ~ /herdr-mcp/'` 为空，且本机无人占用 `:8772`。

## 一键操作者引导（第二台 Mac）

若测更新的 prerelease，替换 `TAG`（下例是首个附带 extension zip 的 Release）：

```bash
TAG=v0.4.0-alpha.15
REPO=whshang/herdr-mcp
WORKDIR="${HOME}/herdr-mcp-clean-uat"
mkdir -p "$WORKDIR/bin" "$WORKDIR/dl" && cd "$WORKDIR"
gh release download "$TAG" -R "$REPO" -D dl \
  -p "herdr-mcp-*-aarch64-apple-darwin" \
  -p "release-manifest.json" \
  -p "herdr-mcp-extension-*.zip" \
  -p "herdr-mcp-extension-*.zip.sha256"
install -m 755 dl/herdr-mcp-*-aarch64-apple-darwin bin/herdr-mcp
export PATH="$WORKDIR/bin:$PATH"
herdr --version
herdr api schema >/dev/null
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
# 扩展（干净机上的 G15 残余验收，可选）：
shasum -a 256 -c dl/herdr-mcp-extension-*.zip.sha256
mkdir -p ~/.config/herdr-mcp/extension
unzip -o dl/herdr-mcp-extension-*.zip -d ~/.config/herdr-mcp/extension
# Chrome: Load unpacked -> ~/.config/herdr-mcp/extension
herdr-mcp native-host install
herdr-mcp native-host status
```

不要 `git clone`。runtime 路径不要 `npm ci`。

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
- `doctor` **不得**把另一台已在跑的 `:8772` 当成这台干净机的证据

## B. 公网 ChatGPT 路径（需操作者本人）

按 [安装](install.md) / [Agent 辅助安装](agent-install.md)，只用 Release 二进制 + 临时 Edge bootstrap，再跟 [ChatGPT Connector](chatgpt-connector.md)。

```bash
herdr-mcp doctor
# 本干净机配好 Edge + Link 后：
# 期望 Edge configured + edge-reachable + oauth-metadata + mcp-endpoint（401 auth=not-sent）
# 且永不打印 token
```

然后由**人类操作者**（非无人值守 agent）在**新的** ChatGPT 会话：

1. Settings → Connectors → 按安装文档加入自定义 MCP App / Connector（公网 Edge URL）
2. 浏览器完成 OAuth（绝不把 `HERDR_MCP_TOKEN` 贴进 ChatGPT）
3. OAuth 成功后开**新会话**，保证 `tools/list` 新鲜
4. 确认 epoch 2 / 18 tools
5. 一次只读工具调用（如 `herdr_inspect`）
6. 一次操作者选定的有界真实 mutation
7. 若本轮 UAT 含长任务 / streaming basics，再补一次烟雾

若本会话无法完成 OAuth，保持 G7/G18 公网路径开放，并记录卡在哪一步（加 Connector / authorize / tools/list / tool call）。

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
- 不从本清单去改狗粮机上的 production Link。

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
