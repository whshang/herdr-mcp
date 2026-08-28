# Completed / Historical (G4 sealed 2026-08-28)

# 内部 GA UAT — 第二台 Mac Agent 协议（非终端用户安装）

> **受众：** 在第二台 Mac（pi）上跑 **G18 干净机 UAT** 的 herdr-mcp 维护者。  
> **不是普通用户文档。** 普通用户走 [install.md](../../i18n/zh-CN/install.md)（Release 二进制 → `herdr-mcp install` → `herdr-mcp doctor`）。在自己机器上让 Agent 代装，看 [agent-install.md](../../i18n/zh-CN/agent-install.md)。

---

## 粘贴给 pi 上的 coding Agent

```text
你收到了第二台 Mac GA UAT 协议。立即阅读本 URL 对应全文并开始执行 — 不要开场白、不要问「是否继续」，除非必须暂停等 Cloudflare API Token 或 Chrome Load unpacked。

目标：在 pi 完成 G4 默认实例干净机 UAT — 独立 Herdr、Release `v0.4.0` stable runtime、独立 Cloudflare Worker（仅 workers.dev）、独立 Link、扩展 + native-host；最后把 ChatGPT Connector URL 交给 owner 做 OAuth。

卡住时参考：agent-install.md、clean-machine-uat.md、chatgpt-connector.md
```

---

## 架构约束（简表）

| 对象 | pi | 禁止 |
| --- | --- | --- |
| `workstation_id` | 本机唯一，与 Edge `DEFAULT_WORKSTATION_ID` 相同 | 狗粮 `prod-real-runtime` 或任何 live dogfood ID |
| `LINK_SHARED_SECRET` | 本机新生成，Keychain + 本机 Worker secret | 狗粮 / `herdr-edge-prod` 密钥 |
| Cloudflare Worker | 本机专属 `name`，仅 `workers.dev` | `herdr-edge-prod`、Custom Domain、DNS、Tunnel |
| 本机 runtime | GitHub Release → `herdr-mcp install` | `git clone` 当 runtime、`target/*`、`npm ci` 装 runtime |
| Edge 源码 | 临时 shallow clone，部署后删除 | checkout 留在 PATH 或当 runtime |

**只暂停：** Cloudflare API Token（见下）；Chrome **Load unpacked**。ChatGPT OAuth 由 owner 在末尾完成。

---

## 执行协议

### 0. Agent 规则

1. 能自动化的 shell 直接执行；只在 Cloudflare Token / Account 选择或 Chrome Load unpacked 时暂停。
2. 禁止对无关 checkout 做 `reset --hard`、`clean -fd` 或覆盖用户修改。
3. 首次安装只用 `workers.dev` — 不创建 Custom Domain、DNS、Tunnel，不改 zone。
4. 禁止回显或持久化 `CLOUDFLARE_API_TOKEN`、`LINK_SHARED_SECRET`、`HERDR_MCP_TOKEN` 到仓库、`.env`、日志、截图或 shell history。仅在当前进程 `export CLOUDFLARE_API_TOKEN=...`。
5. 每个 mutation 后先验证再继续。

### 1. 启动前检查

6. GitHub Releases 最新 **stable**：`TAG=v0.4.0`（G4 干净安装）。G4 封板不要用 `alpha.17`/`alpha.19`。
7. macOS Apple Silicon；`launchctl list | awk '$3 ~ /herdr-mcp/'` 为空；`:8772` 空闲。默认实例 — 不是狗粮 `--instance uat`。
8. **不得**连接或配置 `herdr-edge-prod` / `wss://herdr-edge-prod.*.workers.dev/ws`。

### 2. Herdr

9. 若需要：`curl -fsSL https://herdr.dev/install.sh | sh`
10. `herdr api schema >/dev/null`；socket 缺失则后台启动 headless server（CI 模式，不用 TUI）。
11. `herdr workspace create --cwd "$HOME/herdr-uat-workspace" --label "uat" --focus`

### 3. 本机 runtime（仅 Release）

12. 下载 Release 到 `$HOME/herdr-mcp-clean-uat`（不要 `git clone` 当 runtime）：

    ```bash
    TAG=v0.4.0
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

13. `herdr-mcp install` → `doctor` → `status` → `update check`。`v0.4.0` 上预期 `update.channel=stable` 且 head 处 `available=false`。

### 4. 身份（仅内存）

14. 内存生成：`HERDR_MCP_TOKEN`（`openssl rand -hex 32`）、`LINK_SHARED_SECRET`、`WORKSTATION_ID`（唯一，`[A-Za-z0-9_.-]`，≤64 字符）、`HERDR_LINK_KEYCHAIN_SERVICE=herdr-edge-link-${WORKSTATION_ID}`。

### 5. 暂停 — Cloudflare API Token

**交给 owner 的说明（可复制）：**

```text
我需要 Cloudflare API Token 来部署你的私有 workers.dev Worker。请现在创建：

1. 打开 https://dash.cloudflare.com/profile/api-tokens
2. 点击「Create Token」
3. 模板选「Edit Cloudflare Workers」（推荐）
4. Account Resources：选「All accounts」或指定本次 UAT 用的那一个 Account
5. Zone Resources：选「All zones」或「All zones from an account」— 仅 workers.dev 部署不依赖具体 zone，关键是 Account 级 Workers 权限。不要加 DNS Write。
6. 继续 → Create Token → 复制密钥（只显示一次）

交付方式二选一：
  (A) 在本对话的私密输入框粘贴，或
  (B) 存入密码管理器，并告知你已在我会用的终端里 export CLOUDFLARE_API_TOKEN（勿写入 git、shell history 或截图）

日后查看或吊销：同一页面 https://dash.cloudflare.com/profile/api-tokens → Active tokens → Revoke。
```

15. Token 到达后：`export CLOUDFLARE_API_TOKEN=...`（禁止命令行字面量）。验证 `GET https://api.cloudflare.com/client/v4/user/tokens/verify`，再 `npx wrangler whoami`。单 Account 自动选；多 Account 只问 Account 名。

16. `export CLOUDFLARE_ACCOUNT_ID=...`。`GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain` → `ACCOUNT_SUBDOMAIN`。Origin：`https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`。

### 6. Edge 部署（临时 clone，用后删除）

17. `EDGE_TMP=$(mktemp -d)` → shallow clone → `cd "$EDGE_TMP/edge/cloudflare"`。
18. `WORKER_NAME="$(node "$EDGE_TMP/scripts/cloudflare-worker-name.mjs" "$(hostname)")"`。
19. `cp wrangler.user.example.toml wrangler.user.toml` — 填 `name`、`DEFAULT_WORKSTATION_ID`、`OAUTH_ISSUER`；保持 `workers_dev = true`、`routes = []`。
20. `npx wrangler deploy --config wrangler.user.toml`；`printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml`。
21. 验证 `/health`、OAuth discovery、`/mcp`（401 可接受）。`rm -rf "$EDGE_TMP"`。

### 7. Link（覆盖狗粮默认）

22. Keychain：`security add-generic-password -a "$USER" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET" -U`
23. `herdr-mcp link install` → patch plist 的 `HERDR_EDGE_URL`、`HERDR_WORKSTATION_ID`、`HERDR_LINK_KEYCHAIN_SERVICE` → `launchctl bootstrap`。**禁止**使用默认 `herdr-edge-prod` URL。
24. `herdr-mcp link status`；`herdr-mcp doctor`（edge-reachable，401 auth=not-sent）。pi 上不要跑 `link cutover` / `link seal --execute`。

### 8. 扩展 + native-host

25. 从 Release zip 解压扩展；**暂停** — 请 owner 打开 `chrome://extensions` → 开发者模式 → **Load unpacked** → `~/.config/herdr-mcp/extension`。
26. `herdr-mcp native-host install` → `native-host status` → `doctor`。

### 9. 闭环与报告

27. 验证本机 + Edge + Link；确认未创建 Custom Domain / DNS / Tunnel。
28. `unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID`；删临时 token 文件；提醒 owner 可在控制台吊销一次性 Token。
29. 输出非敏感报告（version、WORKER_NAME、workers.dev origin、WORKSTATION_ID、HERDR_EDGE_URL、MCP_URL、doctor/link 摘要）。
30. 交给 owner 的 Connector URL：`https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/mcp` — owner 在**新对话**完成 OAuth；绝不粘贴 `HERDR_MCP_TOKEN`。

## 明确不做

不做 multi-device Worker 实验、不打 stable tag、不改狗粮 live 状态、不把 pi 指向 `herdr-edge-prod`。
