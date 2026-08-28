# 第二台 Mac（pi）UAT — 本地 Agent 执行协议

把下面整段复制给第二台 Mac 上的 coding Agent（Codex、Cursor、Claude Code 等）。这是**可执行协议**，不是给人类逐条敲命令的教程。

**目标：** 在 pi 上完成 G18 默认实例干净机 UAT — 独立 Herdr、独立 `herdr-mcp` Release runtime、独立 Cloudflare Worker（仅 `workers.dev`）、独立 Link、扩展 + native-host，最后把 ChatGPT Connector URL 交给 owner 做 OAuth。

**权威参考：** [Agent 协助安装](agent-install.md) · [干净机 UAT](clean-machine-uat.md) · [ChatGPT Connector](chatgpt-connector.md)

---

## 架构约束（必须原样遵守）

| 对象 | pi 要求 | 禁止 |
| --- | --- | --- |
| `workstation_id` | 本机唯一，与 Edge `DEFAULT_WORKSTATION_ID` 相同 | 复用狗粮 `prod-real-runtime` 或任何 live dogfood ID |
| `LINK_SHARED_SECRET` | 本机新生成，写入本机 Keychain + 本机 Worker secret | 复用狗粮 / `herdr-edge-prod` 密钥 |
| Cloudflare Worker | 本机专属 `name`，仅 `workers.dev` | 连接 `herdr-edge-prod`、Custom Domain、DNS、Tunnel |
| ChatGPT Connector | 指向 pi Worker 的 `/mcp` | 复用狗粮 Connector URL |
| 本机 runtime | GitHub Release 二进制 → `herdr-mcp install` | `git clone` 当 runtime、`target/*`、`npm ci` 装 runtime |
| Edge 源码 | 临时 shallow clone，部署后删除 | 把 checkout 留在 PATH 或当 runtime |
| Multi-device Worker | 不在本轮实现或验证 | 任何「一 Worker 多机」实验 |

狗粮示例：`herdr-edge-prod.<account>.workers.dev`  
pi 示例：`herdr-edge-macbook-uat.<account>.workers.dev`（实际名由 hostname helper 决定）

**你只暂停两件事：** Cloudflare 登录 / API Token / Account 选择；Chrome **Load unpacked**（扩展目录路径由你准备好）。ChatGPT OAuth 由 owner 在末尾步骤完成，不是你代点。

---

## 执行协议（34 条）

### 0. Agent 合同

1. 能自动化的 shell 步骤直接执行；只在 Cloudflare 交互登录 / API Token 创建、多 Account 选择、Chrome Load unpacked 时暂停。
2. 不破坏已有工作。禁止对无关 checkout 做 `reset --hard`、`clean -fd` 或覆盖用户修改。
3. 首次安装只用 `workers.dev`。不要创建 Custom Domain、DNS、Cloudflare Tunnel，也不要改已有 zone。
4. Cloudflare Token、`LINK_SHARED_SECRET`、`HERDR_MCP_TOKEN` 是高敏凭据。禁止回显或写入仓库、`.env`、普通日志、截图、shell history。优先进程环境注入；若必须落临时文件，用 mode `0600` 并在用后立刻删除。
5. 每个 mutation 后先验证再继续。出错时先判断 mutation 是否已提交，再决定是否重试。
6. **不要**用 clone 本仓库或 `npm`/`cargo` 安装本机 MCP runtime（Edge 临时部署除外）。

### 1. 启动前检查 Release 与机器身份

7. 查询 GitHub Releases 最新 alpha tag（当前基线 `v0.4.0-alpha.16`；有更新则用最新 prerelease）。记录 `TAG` 与 `herdr-mcp --version` 将报告的版本。
8. 确认本机为 **macOS Apple Silicon**，且 `launchctl list | awk '$3 ~ /herdr-mcp/'` 为空、`:8772` 未被占用。这是默认实例干净机，不是狗粮机上的 `--instance uat`。
9. 确认本任务**不得**连接、探测或配置狗粮 Worker `herdr-edge-prod` / `wss://herdr-edge-prod.*.workers.dev/ws`。

### 2. Herdr

10. 若 `herdr --version` 失败：用官方脚本安装 Herdr：

    ```bash
    curl -fsSL https://herdr.dev/install.sh | sh
    ```

11. 验证 `herdr api schema >/dev/null`。若 socket 不存在或 server 未运行，在后台启动 headless server（参考 CI 模式，不要用 TUI）：

    ```bash
  SOCKET="${HERDR_SOCKET_PATH:-$HOME/.config/herdr/herdr.sock}"
  mkdir -p "$(dirname "$SOCKET")"
  HERDR_SOCKET_PATH="$SOCKET" herdr server </dev/null >>"$HOME/.config/herdr/headless-server.log" 2>&1 &
  # 轮询至多 60s，直到 herdr status server --json 报告 running:true
    ```

12. 创建并聚焦 UAT workspace（目录自定，例如 `~/herdr-uat-workspace`）：

    ```bash
  herdr workspace create --cwd "$HOME/herdr-uat-workspace" --label "uat" --focus
    ```

### 3. 本机 runtime（仅 Release 二进制）

13. 在临时工作目录下载 Release 资产（**不要** `git clone` 当 runtime）：

    ```bash
  TAG=v0.4.0-alpha.16   # 替换为步骤 7 确认的最新 alpha
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
    ```

14. 安装并验证默认实例：

    ```bash
  herdr-mcp --version
  herdr-mcp install
  herdr-mcp doctor
  herdr-mcp status
  herdr-mcp update check
    ```

15. Alpha 期保持 `update.channel = "preview"`：alpha 二进制在 `config.toml` 缺失时通常已默认为 preview；若已有 config 且为 `stable`，改为 preview 或删除该字段后重跑 `update check`。

### 4. 在内存中生成本机身份（禁止打印秘密）

16. 生成并仅在内存中保留：
    - `HERDR_MCP_TOKEN` — `openssl rand -hex 32`（`install` 会写入 server plist；不要回显）
    - `LINK_SHARED_SECRET` — `openssl rand -hex 32`
    - `WORKSTATION_ID` — 本机唯一，匹配 `[A-Za-z0-9_.-]`、≤64 字符（建议 `pi-uat-$(date +%Y%m%d)` 或 hostname 派生）
    - `HERDR_LINK_KEYCHAIN_SERVICE` — `herdr-edge-link-${WORKSTATION_ID}`

17. **暂停 — Cloudflare API Token（唯一凭据暂停点之一）**

    打开 <https://dash.cloudflare.com/profile/api-tokens>（有浏览器控制时自行打开，否则把 URL 交给 owner）。

    - 推荐模板：**Edit Cloudflare Workers**，限定单个 Account
    - **不要**加 DNS Write / Zone 权限
    - 自定义 token 至少：Account → Workers Scripts Write/Edit；Account Settings Read；Memberships Read；User Details Read
    - 告知 owner：Token 只显示一次，只粘贴到当前 Agent 会话的密输通道；**禁止**你在日志里 echo

18. Token 到达后：仅以 `export CLOUDFLARE_API_TOKEN=...` 注入（不要命令行字面量参数）。验证 `GET https://api.cloudflare.com/client/v4/user/tokens/verify`，再 `npx wrangler whoami`。单 Account 自动选；多 Account 只问 Account 名；失败则停止 mutation。

19. 取得 `CLOUDFLARE_ACCOUNT_ID` 后 `export` 到当前 shell。`GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain` 取得 `ACCOUNT_SUBDOMAIN`（复用已有，永不改名）。Worker 公网 origin： `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`。

### 5. Edge 部署（临时 shallow clone，部署后删除）

20. 临时获取 Edge 源码（**仅**用于 deploy，不得成为 runtime PATH）：

    ```bash
  EDGE_TMP="$(mktemp -d)"
  git clone --depth 1 https://github.com/whshang/herdr-mcp.git "$EDGE_TMP"
  cd "$EDGE_TMP/edge/cloudflare"
    ```

21. 用仓库 helper 生成 Worker 名（**禁止**自造 slug）：

    ```bash
  WORKER_NAME="$(node "$EDGE_TMP/scripts/cloudflare-worker-name.mjs" "$(hostname)")"
    ```

22. 从示例生成本地 Wrangler 配置并填入本机值：

    ```bash
  cp wrangler.user.example.toml wrangler.user.toml
    ```

    编辑 `wrangler.user.toml`：
    - `name = "<WORKER_NAME>"`
    - `DEFAULT_WORKSTATION_ID = "<WORKSTATION_ID>"`（与步骤 16 相同）
    - `OAUTH_ISSUER = "https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev"`
    - 保持 `workers_dev = true`、`routes = []`

23. 部署并写入 Link secret：

    ```bash
  npx wrangler deploy --config wrangler.user.toml
  printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
    ```

    记录：`EDGE_ORIGIN="https://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev"`，`HERDR_EDGE_URL="wss://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev/ws"`，`MCP_URL="${EDGE_ORIGIN}/mcp"`。

24. 验证 Edge（无 token）：`curl -fsS "${EDGE_ORIGIN}/health"`；OAuth discovery 与 `/mcp` 可达（401 可接受）。然后 **删除** `$EDGE_TMP`（`rm -rf "$EDGE_TMP"`）。不要把 checkout 留在 `~/Documents/herdr-mcp` 当安装源。

### 6. 本机 Link（必须覆盖 dogfood 默认）

`herdr-mcp link install` 写入的候选 plist 默认指向狗粮 `herdr-edge-prod`；**pi 必须在安装后改写 plist**，不得使用默认 Edge URL。

25. 把 `LINK_SHARED_SECRET` 写入 Keychain（服务名与步骤 16 一致）：

    ```bash
  security add-generic-password -a "$USER" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET" -U
    ```

26. 安装 Link 候选 LaunchAgent，再 patch 环境变量并重启：

    ```bash
  herdr-mcp link install
  PLIST="$HOME/Library/LaunchAgents/dev.herdr-mcp.link-rust-candidate.plist"
  launchctl bootout "gui/$(id -u)/dev.herdr-mcp.link-rust-candidate" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_EDGE_URL ${HERDR_EDGE_URL}" "$PLIST"
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_WORKSTATION_ID ${WORKSTATION_ID}" "$PLIST"
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_LINK_KEYCHAIN_SERVICE ${HERDR_LINK_KEYCHAIN_SERVICE}" "$PLIST"
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
    ```

27. 验证 Link：`herdr-mcp link status`；`herdr-mcp doctor` 应显示 edge-reachable / oauth-metadata / mcp-endpoint（401 auth=not-sent）。**禁止**对 pi 跑 `link cutover` / `link seal --execute`（那是狗粮封板动作）。

### 7. 扩展 + native-host

28. 从 Release zip 安装扩展（已下载于步骤 13）：

    ```bash
  shasum -a 256 -c "$WORKDIR/dl"/herdr-mcp-extension-*.zip.sha256
  mkdir -p "$HOME/.config/herdr-mcp/extension"
  unzip -o "$WORKDIR/dl"/herdr-mcp-extension-*.zip -d "$HOME/.config/herdr-mcp/extension"
    ```

29. **暂停 — Chrome Load unpacked（唯一 UI 暂停点）**

    请 owner：打开 `chrome://extensions` → 开启开发者模式 → **Load unpacked** → 选择 `~/.config/herdr-mcp/extension`。你不要代替点击 OAuth 或 ChatGPT 设置。

30. 安装 native-host 并验证：

    ```bash
  herdr-mcp native-host install
  herdr-mcp native-host status
  herdr-mcp doctor
    ```

### 8. 闭环验证与清理

31. 验证闭环（均不得打印 token）：
    - 本机：`herdr-mcp status`、`herdr-mcp doctor`（Herdr / runtime / service / link / edge 层）
    - Edge：`${EDGE_ORIGIN}/health`、OAuth metadata、`${MCP_URL}`（401）
    - Link：`herdr-mcp link status` 显示本机 `HERDR_EDGE_URL` 与 `WORKSTATION_ID`，**不是** `herdr-edge-prod`
    - 确认未创建 Custom Domain / DNS / Tunnel

32. 清理 bootstrap 凭据：`unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID`；删除任何临时 token 文件；建议 owner 吊销一次性 Token。

### 9. 最终报告（仅非敏感字段）

33. 向 owner 输出以下模板（**不得**包含 `HERDR_MCP_TOKEN`、`LINK_SHARED_SECRET`、Cloudflare Token）：

    ```text
    === pi UAT 安装报告 ===
    herdr-mcp version:
    runtime generation:
    launchd server label: dev.herdr-mcp.server
    loopback port: 8772
    config root: ~/.config/herdr-mcp
    Herdr workspace label: uat
    Cloudflare account (name + shortened id):
    WORKER_NAME:
    workers.dev origin:
    WORKSTATION_ID:
    HERDR_EDGE_URL: (wss://... 完整 URL)
    MCP_URL: (https://.../mcp)
    /health:
    herdr-mcp doctor summary:
    herdr-mcp link status summary:
    native-host status:
    extension path: ~/.config/herdr-mcp/extension
    update check:
    ```

34. **交给 owner 的 ChatGPT Connector URL（owner 自行 OAuth）**

    ```text
    ChatGPT → Settings → Connectors → Add MCP App
    MCP URL: https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/mcp
    ```

    提醒 owner：
    - 开启 Developer mode；**新开对话**验收 `tools/list`（epoch 2 / 18 tools）
    - **绝不**把 `HERDR_MCP_TOKEN` 贴进 ChatGPT
    - 验收读操作：`herdr_inspect`；再做一次有界写操作与长任务 `herdr_exec_start` → `herdr_exec_read`
    - 详见 [ChatGPT Connector](chatgpt-connector.md) 与 [干净机 UAT §B](clean-machine-uat.md)

---

## 明确不做

- 不实现或验证 multi-device Worker 控制面
- 不打 stable tag
- 不修改狗粮机 live 状态
- 不在脏 worktree 上 `reset --hard` / `clean -fd`
- 不把 pi 指向 `herdr-edge-prod` 或狗粮 `workstation_id`
