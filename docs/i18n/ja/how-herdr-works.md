# Herdr の仕組み

*1 ページ：各コンポーネントの責務、呼び出しの流れ、永続データの所在。*

詳細な設計は [アーキテクチャ](architecture.md) を参照。このページは最初に確認する地図です。

## 構成要素

```text
ChatGPT / Claude / Grok 会話              ローカル coding agent(pi、claude、codex など)
        │  MCP + OAuth(Connector)                    │  herdr-mcp CLI(信頼ローカル IPC)
        ▼                                            ▼
                  Cloudflare Edge ──────── 自分の Worker
                  OAuth、ツールカタログ、              (runtime と release 一致)
                  配送証拠、ルーティング
                                │
              認証済みアウトバウンド WSS(Link)
                                │
        ┌───────────────────────┼────────────────────────────┐
        │           ワークステーション(受信ポート不要)          │
        │   herdr-mcp runtime(Rust、generation A/B)          │
        │     ├── ガバナンス:管理ルート、配送、冪等性          │
        │     ├── 信頼ローカル IPC socket(拡張、CLI)          │
        │     └── loopback HTTP MCP(127.0.0.1:8772)          │
        │   Herdr(TUI):workspace / tab / pane / agent        │
        │     └── 実際の PTY、git checkout、実行中のテスト      │
        └────────────────────────────────────────────────────┘

        Chrome 拡張(任意の戻り経路)
          Native Messaging → mode-0600 socket → 同一 runtime
          Side Panel:ライブ workspace / pane / バインド状態
          ページ側:バインド済み会話の wake + handoff
```

## 各コンポーネントの責務

| コンポーネント | 担当 | 担当しないもの |
| --- | --- | --- |
| Web AI(ChatGPT/Claude/Grok) | 目標、計画、結果の検証 | ローカル状態。runtime が返すものだけを受け取る |
| Cloudflare Edge | 公開 OAuth/MCP 識別子、`device_id` へのルーティング、リクエストごとの配送証拠 | ワークステーション状態。単なる中継で、第 2 の真実源ではない |
| Link | ワークステーションのアウトバウンド WSS。再接続と配送の報告 | あらゆる状態。純粋なトランスポート |
| herdr-mcp runtime | 契約、ガバナンス(管理ルート、冪等性、配送状態)、Continuity、Work Memory、agent 委譲記録 | 実際の PTY/workspace ライフサイクル |
| Herdr(TUI) | workspace、tab、pane、agent、PTY、セッション | リモート通信。Edge と直接話すことはない |
| ブラウザ拡張 | ブラウザ側実行、会話バインド、wake、観測 | ファイル転送、agent runtime、汎用ブラウザ自動化 |
| ローカル coding agent | 自分のタスク。他のローカルクライアント同様 `herdr-mcp` CLI を呼ぶ | タスク状態の第 2 権威 |

## 1 回の呼び出しの全経路

```text
ChatGPT ツール呼び出し(herdr_inspect,device=macbook-main)
  → Edge が OAuth を検証し device_id を決定
  → Link はその Worker への常設 WSS を保持済み
  → runtime が契約と管理ルートを検証し、Herdr socket を呼ぶ
  → Herdr がライブ状態で応答
  → runtime が結果と配送証拠を返す
```

誠実さを保つ 2 つのルール：

- **配送証拠、楽観しない。** すべてのミューテーションは `delivery_state`(`applied`、`not_delivered`、`uncertain` など)を報告する。タイムアウトや切断は「不明」であって「即リトライ」ではない。リプレイ可能なのは `not_delivered` だけで、同じ idempotency key を再利用する。
- **参照は観測されたものだけ。** `device_id`、`session_ref`、`endpoint_ref`、`continuity_id`、pane/workspace id はすべて以前の呼び出しか CLI から得る。ラベルやホスト名から推測しない。

## デバイス識別とルーティング

登録済みの各ワークステーションは fleet の **device** で、不変の `dev_*` `device_id` と独自の credential を持つ。`herdr_devices`(Edge ローカル action)が fleet を一覧し、`herdr-mcp device list` が同じ非秘匿インベントリをローカルに表示する。

```text
1 台目      :herdr-mcp worker bootstrap(Worker をデプロイし device #1 を登録)
2 台目以降  :登録済みデバイス → herdr-mcp worker pair
             新しいコンピュータ → herdr-mcp worker connect "<pairing-address>"
ChatGPT    :デバイスを明示するか、Edge が device_ambiguous で推測を拒否
```

裸の `w1:p1` は 1 つの Herdr server/session 内でしか一意でない。マシンをまたぐ pane/workspace 参照には必ず `device_id`(または `herdr_ref_*` アフィニティ)を付ける。[マルチデバイス制御](existing-worker-connect.md)と [デュアルパス制御](multi-machine-control.md) を参照。

## WebChat ライフサイクル(ローカル agent → ブラウザ)

ローカル agent は同一 runtime 経由で対応済み Web 会話を操作する。拡張、信頼 IPC、識別と配送ルールは MCP 経路と同じ：

```text
発見      herdr-mcp webchat endpoints → resources → inspect(観測された ref と observation_generation)
作成      herdr-mcp webchat create --source-url URL --message MESSAGE --idempotency-key KEY
送信      herdr-mcp webchat send   --session-ref REF --message MESSAGE --expected-generation N --idempotency-key KEY → dispatch_id + delivery_state
観測      herdr-mcp webchat dispatch-status <dispatch_id>
開く/閉じる herdr-mcp webchat open | archive | archive-status
handoff  herdr-mcp webchat handoff --continuity-id hc:... --source-url URL
         (canonical packet;automatic_delivery.completed=true のみが「配送済み」)
```

## 真実の所在

| レイヤー | 内容 | 寿命 |
| --- | --- | --- |
| ライブ状態 | workspace、pane、agent、Git、PTY | Herdr が所有。クエリのみ、上位にキャッシュしない |
| Continuity | 1 つの `continuity_id` の永続タスクジャーナル | 会話、ブラウザリロード、runtime 再起動をまたいで存続 |
| Work Memory | 履歴の限定パーティション(`project_ref` + `repo_id` + `work_chain_id`) | work chain ごと |
| registry/state db | device、grant、dispatch、generation | runtime ローカル |
| ページ/会話 | レンダリングのみ | 交換可能。状態の権威ではない |

## 異常が見えたとき

リクエストが実際に通る順序で診断する。上の壊れたレイヤーを直すために健全なレイヤーを再起動しない：

```bash
herdr-mcp status            # runtime, Herdr, TCC, one-line health
herdr-mcp doctor            # per-layer evidence; default output is concise
herdr-mcp doctor --local    # read-only local snapshot: allow-listed processes,
                            # loopback port 8772, config-file metadata only
herdr-mcp link status       # Link transport and Edge routing state
```

`doctor --local` はファイル内容、環境変数、`state.db` を一切読まない。既定以外のポートで動く名前付きインスタンスが 8772 で `listening:false` を出すのは設計通り。完全な故障語彙と回復手順は [トラブルシューティング](troubleshooting.md) を参照。
