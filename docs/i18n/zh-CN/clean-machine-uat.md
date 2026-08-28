# 干净机 UAT 清单（G18）

给**从 GitHub Release 二进制安装**（不把本仓库当 runtime 安装源）用的命令清单。

仅完成此清单也**不要**打非 alpha 的 stable tag。产品仍处 alpha，直到 G1 + G18 + 其余 GA veto 通过。

## 同机隔离诚实说明

默认生产身份保持不变：

- LaunchAgent `dev.herdr-mcp.server`
- loopback 端口 `8772`
- 配置根 `~/.config/herdr-mcp`
- 用户 CLI `~/.local/bin/herdr-mcp`

| 尝试 | 狗粮机上是否安全 | 原因 |
| --- | --- | --- |
| 仅 TMPHOME / `HERDR_MCP_CONFIG_DIR` | 只能做部分探针 | 路径可隔离，但默认 label/port 仍冲突；`status`/`doctor` 可能把狗粮 `:8772` 报成 healthy |
| 同用户默认 `herdr-mcp install` | **否** | 会改狗粮 LaunchAgents / `~/.local/bin/herdr-mcp` |
| 同用户**命名实例**（`--instance uat` / `HERDR_MCP_INSTANCE=uat`） | **本机 runtime UAT 可以** | 独立 label `dev.herdr-mcp.uat.server`、非 8772 端口、`~/.config/herdr-mcp-uat`；不改写默认 user CLI |
| 第二台 Mac 或 VM（`:8772` 空闲、无默认 `dev.herdr-mcp.*`） | **是** | 完整默认实例 G18（含 native-host / 公网 ChatGPT） |

命名实例证据推进狗粮机上的 G18 本机 install/doctor/status。它**不能**替代第二台 Mac 上默认实例的 native-host + 公网 OAuth 封板。

需要已包含实例隔离的 Release（首个：`v0.4.0-alpha.16` 或更新）。不要用 `v0.4.0-alpha.15` 宣称命名实例 UAT。

## 测试平台

第一版 GA 建议：只正式测 **macOS Apple Silicon**。

- Windows Release 二进制：preview / 可选观察，不当第一 GA lifecycle 封板。
- Linux lifecycle：不宣称。

## 前置

- 已按 <https://herdr.dev> 装好 Herdr。
- 本机 runtime 路径不要求事先有 `herdr-mcp` checkout。
- 能访问 GitHub Releases；（走 ChatGPT 时）能访问 Cloudflare。
- **默认实例**安装前：确认 `launchctl list | awk '$3 ~ /herdr-mcp/'` 为空，且本机无人占用 `:8772`。
- **命名实例**在狗粮机上：不要动默认 `dev.herdr-mcp.server` / `:8772`；不要用 UAT 二进制对狗粮 Chrome 跑 Link cutover / `native-host install`。

## 第二台 Mac 需要独立的 Edge Worker

第二台 Mac 走公网 ChatGPT 路径时，不能复用 dogfood 的 Edge URL 和 Link 密钥。Edge 按 Worker 单租户路由：

- **`DEFAULT_WORKSTATION_ID` 路由：** 每个已部署 Worker 把公网 `/mcp`（及 OAuth discovery）绑定到一个 `DEFAULT_WORKSTATION_ID`。无显式 workstation 头的请求会落到该 ID。一个 Worker 对应公网路径上的一个逻辑 workstation。
- **每个 `workstation_id` 仅一条活跃 Link：** Link 连 `/ws/{workstation_id}`。Edge Durable Object 每个 ID 只保留一条活跃 Link；新的 `hello` 会把旧连接标为非活跃并关闭（`superseded by newer workstation link`）。
- **pi 不能复用 dogfood 身份：** 让 UAT 机指向 `herdr-edge-prod` + `prod-real-runtime`（或任何 live dogfood Worker / `DEFAULT_WORKSTATION_ID`）会踢掉 dogfood 生产 Link，或把 ChatGPT 工具调用路由到错误机器。**G18 封板禁止。**

走 B 节（公网 ChatGPT）前，先部署**机器专属** Worker：唯一 Worker `name`、唯一 `DEFAULT_WORKSTATION_ID`（如 `pi-uat-<date>`）、`OAUTH_ISSUER` 对应该 Worker URL；Link 侧 `HERDR_WORKSTATION_ID` 与之相同。步骤见 [Agent 协助安装](agent-install.md) §6（Edge 部署 + `LINK_SHARED_SECRET`）；UAT 保持 `workers_dev = true` 与 `routes = []`。

**仅内部 GA UAT（非终端用户安装）：** 把 [第二台 Mac GA UAT Agent 协议](../../_wip/zh-CN/second-mac-ga-uat-agent-prompt.md) 复制给 pi 上的 coding Agent（Agent 优先协议；含 Cloudflare Token 暂停、Link env 覆盖、最终报告模板）。普通用户走 [install.md](install.md) 或 [agent-install.md](agent-install.md)。

## 一键操作者引导（第二台 Mac，默认实例）

若测更新的 prerelease，替换 `TAG`：

```bash
TAG=v0.4.0-alpha.16
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

## 同机命名实例（没有第二台 Mac 时）

只用下载的 Release 二进制（禁止 `target/*/herdr-mcp`）。狗粮保持默认实例。

```bash
TAG=v0.4.0-alpha.16
REPO=whshang/herdr-mcp
WORKDIR="${HOME}/herdr-mcp-clean-uat"
mkdir -p "$WORKDIR/bin" "$WORKDIR/dl" && cd "$WORKDIR"
gh release download "$TAG" -R "$REPO" -D dl \
  -p "herdr-mcp-*-aarch64-apple-darwin" \
  -p "release-manifest.json"
install -m 755 dl/herdr-mcp-*-aarch64-apple-darwin bin/herdr-mcp
export PATH="$WORKDIR/bin:$PATH"
export HERDR_MCP_INSTANCE=uat

# 预检：狗粮不得被改动
readlink "$HOME/.config/herdr-mcp/runtime/current"
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'

herdr-mcp --version
herdr-mcp --instance uat install
herdr-mcp --instance uat doctor
herdr-mcp --instance uat status
herdr-mcp --instance uat update check
herdr-mcp --instance uat service status

# 期望独立身份
test -x "$HOME/.config/herdr-mcp-uat/runtime/current/herdr-mcp"
launchctl list | awk -v label='dev.herdr-mcp.uat.server' '$3 == label { print $1, $2, $3 }'
# 狗粮仍是默认：
readlink "$HOME/.config/herdr-mcp/runtime/current"
ls -l "$HOME/.local/bin/herdr-mcp"
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
```

### 命名实例扩展 (同机, 仅静态)

将 Release 扩展 zip 解压到 **uat** 配置根 (不要写入狗粮 `~/.config/herdr-mcp/extension`):

```bash
export HERDR_MCP_INSTANCE=uat
gh release download "$TAG" -R "$REPO" -D "$WORKDIR/dl" \
  -p "herdr-mcp-extension-*.zip" \
  -p "herdr-mcp-extension-*.zip.sha256"
shasum -a 256 -c "$WORKDIR/dl"/herdr-mcp-extension-*.zip.sha256
mkdir -p "$HOME/.config/herdr-mcp-uat/extension"
unzip -o "$WORKDIR/dl"/herdr-mcp-extension-*.zip -d "$HOME/.config/herdr-mcp-uat/extension"
herdr-mcp --instance uat doctor
# 预期: local-ipc PASS; native-messaging absent (Chrome 主机名 dev.herdr.mcp 单例)
```

静态 smoke (无需 Chrome; 在 checkout 中运行):

```bash
node tests/manual/extension_smoke.mjs
node tests/manual/background_bind_test.mjs
node --test tests/queued-insert.test.mjs
```

**不要**在狗粮机上跑 `herdr-mcp --instance uat native-host install`: 会覆盖生产 Chrome 清单 `NativeMessagingHosts/dev.herdr.mcp.json`。完整 G15 native-host + Load unpacked 封板需第二台 Mac 默认实例, 或在狗粮默认实例的 owner 维护窗进行。

结束后清理（不动狗粮）：

```bash
export HERDR_MCP_INSTANCE=uat
herdr-mcp --instance uat uninstall
```

### 狗粮机上命名实例的非目标

- 不要对狗粮 Chrome 跑 `native-host install` / Load unpacked。
- 不要对 UAT 实例跑 `link install` / `link cutover` / seal 变更。
- 公网 ChatGPT OAuth 仍是 **owner** 步骤；卡在 OAuth 时记下确切停在哪一步。

## A. 仅本机 runtime

```bash
herdr --version
herdr api schema >/dev/null

# 从 GitHub Releases 下载当前平台二进制并放入 PATH，然后：
chmod +x "$(command -v herdr-mcp)"
herdr-mcp install          # 干净机默认实例
# 或：herdr-mcp --instance uat install   # 与狗粮同机
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

期望：

- 对该实例的 Herdr / runtime / service 层 `doctor` PASS
- 默认实例：PATH `herdr-mcp` 可指向 `~/.config/herdr-mcp/runtime/current`
- 命名实例：始终带 `--instance` / `HERDR_MCP_INSTANCE`，或用隔离的 `runtime/current`；`~/.local/bin/herdr-mcp` 仍指向狗粮
- runtime 路径不要求 `git clone` / `npm ci`
- 命名实例的 `doctor` 必须报告 UAT 的 label/port/config，而不是狗粮 `:8772`

## B. 公网 ChatGPT 路径（owner 动作）

按 [安装](install.md) / [Agent 协助安装](agent-install.md) 用 Release 二进制 + 临时 Edge bootstrap，再按 [ChatGPT Connector](chatgpt-connector.md)。

本节优先用**第二台 Mac / 默认实例**。同机命名实例 UAT 在会改动狗粮 Link/Edge 之前停下。

```bash
herdr-mcp doctor
# 在本机配好 Edge + Link 后：
# expect Edge configured + edge-reachable + oauth-metadata + mcp-endpoint (401 auth=not-sent)
# 不打印 token
```

然后由**人类操作者**（不是无人值守 agent）在**新的** ChatGPT 会话中：

1. Settings → Connectors → 按安装文档加入自定义 MCP App / Connector（公网 Edge URL）
2. 浏览器完成 OAuth（绝不把 `HERDR_MCP_TOKEN` 贴进 ChatGPT）
3. OAuth 成功后新开对话，刷新 `tools/list`
4. 确认 epoch 2 / 18 tools
5. 一次只读工具调用（`herdr_inspect` 或等价）
6. 一次操作者选定的有界写操作
7. 若本轮 UAT 覆盖，再做一次长任务 / streaming 基础冒烟

若本会话无法完成 OAuth，保持 G7/G18 公网路径开放，并记录卡在哪一步（加 Connector / authorize / tools/list / tool call）。

## C. 更新 / 回滚（同一干净机 / 同一实例）

```bash
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status
herdr-mcp doctor
# 仅在已有受管上一 generation 时做受控回滚：
herdr-mcp rollback
herdr-mcp doctor
herdr-mcp status
```

命名实例下每条命令都要带 `HERDR_MCP_INSTANCE=uat`（或 `--instance uat`）。

## D. 本清单明确不做

- 不在这里做 Browser Control Plane / true-steer（G16 后 GA 边界）。
- 不把 git checkout 里的未打包 `extension/` 当成已封的 G15 路径。
- 不用 `target/*/herdr-mcp` 或仓库软链的 `~/.local/bin/herdr-mcp` 当生产证据。
- 命名实例路径不要改狗粮机上的 production Link。

## 需要记录的证据

采集（非密钥）输出：

```bash
herdr-mcp --version
# 默认：
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
# 命名实例：
readlink "$HOME/.config/herdr-mcp-uat/runtime/current" || true
launchctl list | awk -v label='dev.herdr-mcp.uat.server' '$3 == label { print $1, $2, $3 }'
herdr-mcp --instance uat status
herdr-mcp --instance uat doctor
herdr-mcp --instance uat update check
ls -l "$HOME/.local/bin/herdr-mcp"
```

附到 GA scorecard 的 G18 行。同机命名实例证据记为本机 runtime 进展，不要写成完整公网路径 PASS。
