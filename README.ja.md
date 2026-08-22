# herdr-mcp

ウェブ LLM（とくに ChatGPT Connector）が本機の [Herdr](https://herdr.dev) を駆動するための MCP HTTP ゲートウェイ。ペイン / agent の確認、git 管理下のプロジェクト編集、シェル実行、安価なローカル worker への投入。Chrome 拡張が進捗 / 続行を、紐づけたウェブ会話へ打ち返す。

[Herdr](https://herdr.dev) はコーディング agent 向けの端末マルチプレクサ。このリポジトリはソケットもディスクも見えない**遠隔**クライアントの入口。Herdr の約 90 個のネイティブメソッドを MCP ツールに再包装しない。

**しないこと:** Herdr の代替。DeepSeek に偽の OAuth connector を付ける。拡張を公開トンネルへ出す（拡張は `127.0.0.1` のみ）。

**言語（herdr と同じ）:** [English](README.md)（GitHub 既定）· [简体中文](README.zh.md) · [日本語](README.ja.md)。  
CLI / ブラウザ拡張: 初回インストールはシステム言語（`en` / `zh` / `ja`）、不明なら英語。変更: `herdr-mcp lang`、または拡張の Options → Language。

## アーキテクチャ（あなた ↔ ウェブ ↔ MCP ↔ herdr、拡張が逆方向チャネル）

上から下へ: あなた → ウェブ会話 →（herdr-mcp と chrome-extension **同列**）→ Herdr panes → ローカル agents。  
Agents の進捗 / settled が拡張へ届き、拡張がウェブ会話へ書き込む。詳細: [docs/extension-wake.md](docs/extension-wake.md)（中国語）。

**オーケストレーション（ウェブが計画、本機は安価）:**

- `herdr_fs_*` / `herdr_git` / `herdr_exec` を優先（ローカル agent API 不要）。
- 推論が必要なら安い/高速 worker（`pi` / `flash` / `cline` / `opencode` / `anti`）または監査（`droid` / `grok`）へ直接 `herdr_prompt`。本機 Claude/OMP/main を中間マネージャにしない。
- `inspect`/`since` は既定で Claude/OMP/Codex をソフト非表示。既知 pane への prompt は可。`HERDR_MCP_AGENT_ALLOW=*` で全表示。
- セッション開始: `herdr_inspect` → `herdr_skill`（一度）→ 作業。

```mermaid
flowchart TB
  You[You]
  Web[Web chat<br/>e.g. ChatGPT]
  MCP[herdr-mcp]
  Ext[herdr-mcp-chrome-extension]
  Herdr[Herdr panes]
  Agents[Local cheap workers<br/>pi / flash · edit / test]

  You --> Web
  Web -->|call MCP| MCP
  MCP --- Ext
  MCP -->|reach herdr| Herdr
  Herdr -->|dispatch| Agents
  Agents -.->|progress / settled| Ext
  Ext -.->|type into web chat| Web
```

## 対応 OS と起動

**Node サーバ**は [herdr](https://herdr.dev) と同じく **macOS / Linux / Windows**（Node.js 20+）。インストールディレクトリは走査せず、API socket（既定 `~/.config/herdr/herdr.sock`、`HERDR_SOCKET_PATH` で上書き）と PATH 上の `herdr api schema` を使う。

起動方法は二つ:

| 経路 | 対象 | 方法 |
|---|---|---|
| 前景 | 任意 OS | 下記 `node dist/server.js` |
| `herdr-mcp` CLI | **macOS** LaunchAgent | `bin/herdr-mcp start` / `status` / `logs` / `watchdog` |

`npm` の `bin` は `dist/server.js` であり bash CLI ではない。macOS では `ln -sf …/bin/herdr-mcp ~/.local/bin/herdr-mcp` 可。systemd / Task Scheduler は範囲外。

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
export HERDR_MCP_PORT=8772
# 公開 Connector 用（/mcp サフィックスなし）:
# export HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
node dist/server.js
```

## インストール（ゼロから動くまで）

### 0. 前提

- [herdr](https://herdr.dev) がインストール済みかつ起動中
- Node.js 20+（`node -v`）
- ChatGPT 向け: `cloudflared` による公開 HTTPS（[Quick Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/)）または自前ドメイン

### 1. 取得とビルド

```bash
git clone https://github.com/whshang/herdr-mcp.git
cd herdr-mcp
npm install
npx tsc
mkdir -p ~/.config/herdr-mcp
```

### 2. ローカル MCP 起動

```bash
export HERDR_MCP_TOKEN="$(openssl rand -hex 16)"
echo "token=$HERDR_MCP_TOKEN"
node dist/server.js
```

### 3. ChatGPT 接続（推奨: 無料 Cloudflare）

別ターミナル:

```bash
cloudflared tunnel --url http://127.0.0.1:8772
```

その origin（**`/mcp` なし**）で MCP を再起動:

```bash
export HERDR_MCP_BASE_URL=https://xxxx.trycloudflare.com
export HERDR_MCP_TOKEN=...
node dist/server.js
```

#### ChatGPT **Web** で Connector を追加（チャット UI / デスクトップからは不可）

1. [Plugins 設定](https://chatgpt.com/#settings/Plugins) で Developer mode をオン
2. [Create connector](https://chatgpt.com/plugins#settings/Connectors?create-connector=true)
3. MCP URL は `https://xxxx.trycloudflare.com/mcp`
4. ログインしてリダイレクトを待つ（API key / token は貼らない）
5. 接続後は**新しい**チャットを開始

トラブル時は `HERDR_MCP_BASE_URL` と tunnel 稼働、`herdr-mcp status` を確認。詳細: [docs/chatgpt-connector.md](docs/chatgpt-connector.md)。

Quick Tunnel は `cloudflared` 再起動でホスト名が変わる。変わったら `HERDR_MCP_BASE_URL` を更新して MCP を再起動。

### 4. Cursor（任意・同一マシン）

`~/.cursor/mcp.json` — ローカルのみ:

```json
{
  "mcpServers": {
    "herdr-mcp-local": {
      "url": "http://127.0.0.1:8772/mcp",
      "headers": {
        "Authorization": "Bearer <paste: herdr-mcp token>"
      }
    }
  }
}
```

### 5. ブラウザ拡張（任意）

フォルダ: `extension/`（MV3）。Chrome 上の名前は **herdr → Web wake**。`chrome://extensions` で unpacked 読み込み。Options に `http://127.0.0.1:8772` と同一静的 token。

## エンドポイント

| 用途 | URL |
|---|---|
| ローカル MCP | `http://127.0.0.1:8772/mcp` |
| 公開 MCP | `{HERDR_MCP_BASE_URL}/mcp` |
| 拡張 SSE | `http://127.0.0.1:8772/push/events` |
| 拡張スナップショット | `http://127.0.0.1:8772/push/state` |

Connector 認証は **OAuth (DCR / CIMD)**。静的 Bearer はローカル curl / Cursor / 拡張用（`herdr-mcp token`）。ChatGPT の connector フォームに貼らない。

## CLI（macOS）

```bash
herdr-mcp              # メニュー
herdr-mcp status
herdr-mcp connector
herdr-mcp start | stop | restart   # LaunchAgent
herdr-mcp logs [-f]
herdr-mcp token | url
herdr-mcp lang [en|zh|ja]
herdr-mcp watchdog install
herdr-mcp watchdog status
```

コード変更後: `npx tsc && herdr-mcp restart`（または `node dist/server.js` を再起動）。

## 既定ツール（なぜこの 18）

herdr のネイティブ面は大きな Unix-socket API（`herdr api schema`）。herdr-mcp は全メソッドを MCP ツールに再包装しない。代わりに:

| 層 | MCP ツール | herdr との関係 |
|---|---|---|
| Skill | `herdr_skill` | 上流 Herdr `SKILL.md` の読み取り（このプロセスが GitHub を取る。ChatGPT は取らない） |
| Passthrough | `herdr_methods`, `herdr_call` | ネイティブ socket API への薄いゲート |
| リモート編成 | `herdr_inspect`, `herdr_since`, `herdr_prompt` | ウェブ向け小さなヘルパ |
| リモート workstation | `herdr_fs_*`, `herdr_exec` / `herdr_exec_*`, `herdr_git` | オフマシンクライアント向けのディスク/シェル面 |

ツール表の全文は [README.md](README.md)（英語）または [README.zh.md](README.zh.md)。

`HERDR_MCP_ALL_TOOLS=1` でライフサイクルツールを足し 30 個。書き込みは managed git root に限定。`HERDR_MCP_READONLY=1` / `HERDR_MCP_WRITE_ROOTS` でさらに絞れる。

## 環境変数（主要）

| 変数 | 既定 | 役割 |
|---|---|---|
| `HERDR_MCP_TOKEN` | 空 | `/mcp` と `/push` の静的 Bearer |
| `HERDR_MCP_PORT` | `8772` | 待受ポート |
| `HERDR_MCP_BASE_URL` | 空 | ChatGPT 用公開 origin（`/mcp` なし） |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | herdr API socket |
| `HERDR_MCP_READONLY` | off | mutation を遮断 |
| `HERDR_MCP_ALL_TOOLS` | off | 18 ではなく 30 ツール |
| `HERDR_SKILL_NETWORK` | on | `0` = 同梱 SKILL.md のみ |

一覧: [docs/architecture.md](docs/architecture.md#environment-variables)。

## ブラウザ拡張

フォルダ: `extension/`（MV3）。対応サイト: chatgpt.com / claude.ai / chat.deepseek.com / chat.z.ai。

1. **進捗ナッジ（利用可）** — 会話を herdr **workspace** にバインド。既定チェック間隔 **60 秒**、変化なし時のフォールバック **20 分**。ChatGPT のページ内「許可」カードは自動クリック。任意でターン後に小モデル判定（拡張 ≥0.1.20）。
2. **JSON → MCP（未完成）** — DeepSeek / z.ai で `{"tool":...}` を抽出できるが、ローカル `/mcp` 呼び出しと結果の書き戻しはまだない。

UI 言語: en / 简体中文 / 日本語。詳細: [docs/extension.md](docs/extension.md)。

## ドキュメント

| Doc | 内容 |
|---|---|
| [CHANGELOG.md](CHANGELOG.md) | バージョン |
| [docs/architecture.md](docs/architecture.md) | herdr vs MCP（中国語） |
| [docs/chatgpt-connector.md](docs/chatgpt-connector.md) | ChatGPT OAuth（中国語） |
| [docs/extension.md](docs/extension.md) | 拡張（中国語） |
| [README.md](README.md) | 英語（GitHub 既定・ツール表フル） |

## Ops

```bash
npx tsc
# node dist/server.js を動かしているプロセスを再起動
```

Sessions: `~/.config/herdr-mcp/sessions/`。
