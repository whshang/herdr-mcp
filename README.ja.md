# herdr-mcp

**Web AI から Herdr を通じて実際のローカル開発ワークステーションを操作するためのリモート制御プレーンです。**

ChatGPT のような Web モデルは目標の理解や設計判断に強い一方、ブラウザだけではローカルのファイル、Git、Shell、長時間ジョブ、Herdr workspace を直接見ることができません。herdr-mcp は、その間を安全に接続します。

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: [English](README.md) · [简体中文](README.zh.md) · **日本語**

## できること

herdr-mcp は Web planner に次の能力を提供します。

- **永続的なローカル作業状態** — Herdr workspace / pane / agent lifecycle
- **決定的なワークステーション操作** — files / Git / images / shell
- **必要なときだけ local agent に委譲**
- **安定したリモート接続** — Cloudflare Edge 上の OAuth/MCP と、workstation からの outbound link
- **ブラウザ継続性** — ローカル進捗を Web 会話へ戻し、長い会話を安全に新しい会話へ handoff

```text
User
  ↓
ChatGPT / Web AI
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated routing
herdr-link
  ↓
local herdr-mcp runtime
  ↓
Herdr workspace
  ├─ files / Git / shell
  └─ local agents

Herdr progress
  ↓
browser extension
  ↓
Web conversation resumes
```

## 何ではないか

herdr-mcp は Herdr の代替ではなく、別の Agent runtime でもありません。また、Herdr の全 Socket API を個別 MCP tool に複製しません。

Web モデルが planner、Herdr が永続的なローカル作業環境、local agent が worker です。完了判定の根拠は agent の発言ではなく、Git・test・runtime の実状態です。

## 公開 tool surface

高頻度の遠隔操作だけを固定 MCP tool とし、Herdr の長尾 API は動的に参照します。

```text
frequent work
  → herdr_inspect / herdr_since / herdr_fs_* / herdr_git / herdr_exec* / herdr_prompt

native Herdr long tail
  → herdr_methods → herdr_call
```

現在の production public contract は **epoch 2 / 18 tools** で、read-only の `herdr_skill` を含みます。

## 最短セットアップ

前提：

- [Herdr](https://herdr.dev) がインストール済み・起動中
- Node.js 20+
- ChatGPT から公開接続する場合は Cloudflare account

ローカル Coding Agent にセットアップを任せる場合は、推測させず英語の正本を先に読ませてください。

```text
Install and deploy herdr-mcp for me. First read:
https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md

Follow that guide end to end. Use workers.dev for the first install and do not expose or commit secrets.
```

手動で構築する場合：

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm ci
npm run build

export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
node dist/server.js
```

ローカル runtime を確認します。

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
herdr --version
herdr api schema >/dev/null
```

ChatGPT から使う場合は、まず Cloudflare Worker を `workers.dev` にデプロイし、`herdr-link` を起動して、公開 `/mcp` URL を ChatGPT の custom MCP App/Connector に登録して OAuth を完了します。

`HERDR_MCP_TOKEN` や Cloudflare API Token を ChatGPT に貼らないでください。

詳細手順（英語）：

- [Quick start](docs/i18n/en/quick-start.md)
- [Installation](docs/i18n/en/install.md)
- [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md)

## 最初の実タスク

Connector 設定後は、新しい会話で read-only から確認してください。

```text
Inspect the current Herdr workspaces and Git status. Read only; do not modify anything.
```

正常な流れ：

```text
herdr_inspect
  ↓
herdr_skill
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
  ↓
answer
```

その後、小さな edit + test + diff を試します。local agent は独立推論が本当に役立つタスクだけに使います。

## Browser continuity

MCP は基本的に次の方向です。

```text
Web AI → workstation
```

ローカル Agent はブラウザの turn が終わった後も作業を続けることがあります。MV3 extension は逆方向を補います。

```text
workstation → Web conversation
```

主な機能：

- workspace binding
- progress / settled wake
- evidence-first recovery
- long-conversation handoff
- native custom MCP Connector を持たない z.ai / DeepSeek 向けの bounded JSON→MCP bridge

Native Messaging host をインストールします。

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

その後 `extension/` を Chrome/Chromium の unpacked extension として読み込みます。

詳細：[Browser continuity](docs/i18n/en/browser-continuity.md) · [Browser extension](docs/i18n/en/extension.md)

## セキュリティ境界

- local runtime は loopback に bind
- workstation は Edge へ **outbound** authenticated WSS を作る
- ChatGPT は public Edge で OAuth を使用
- browser continuity は Native Messaging + `0600` Unix socket
- `herdr_fs_*` は managed Git root / write / secret-path gate の対象
- `herdr_exec` はより強い shell 能力であり、sandbox ではない
- delivery が不確実な mutation は、再実行前に実状態を確認

詳細：[Architecture](docs/i18n/en/architecture.md) · [Best practices](docs/i18n/en/best-practices.md)

## Runtime A/B

公開 Edge とローカル runtime は別の release plane です。

```text
stable Edge / OAuth / MCP URL
        ↓
persistent herdr-link
        ↓
runtime generation A / B
```

同じ public contract epoch の中なら、新しい local runtime generation を検証・activate・rollback しても ChatGPT Connector URL を変更する必要はありません。

詳細：[Runtime A/B](docs/i18n/en/runtime-self-upgrade.md)

## Documentation map

- [Overview](docs/i18n/en/overview.md)
- [Design philosophy](docs/i18n/en/design-philosophy.md)
- [Quick start](docs/i18n/en/quick-start.md)
- [Installation](docs/i18n/en/install.md)
- [ChatGPT Connector](docs/i18n/en/chatgpt-connector.md)
- [Browser continuity](docs/i18n/en/browser-continuity.md)
- [Architecture](docs/i18n/en/architecture.md)
- [Best practices](docs/i18n/en/best-practices.md)
- [CLI reference](docs/i18n/en/cli-reference.md)
- [Cloudflare Edge deployment](docs/i18n/en/cloudflare-edge-deployment.md)
- [Runtime A/B](docs/i18n/en/runtime-self-upgrade.md)
- [Troubleshooting](docs/i18n/en/troubleshooting.md)

## Development checks

```bash
npm run build
npm test
npm run test:edge
npm run build:site
git diff --check
```
