# ChatGPT Connector

*MCP と OAuth を通して Web AI を手元のワークステーションに接続する。*

herdr-mcp は、マシンを直接公開することなく、ChatGPT に実際の開発環境へのアクセスを与えます。

```text
ChatGPT
  │ OAuth + MCP
  ▼
Cloudflare Edge
  │ authenticated WSS
  ▼
herdr-link / herdr-mcp
  │
  ├─ files / Git / shell
  └─ Herdr workspaces / panes / agents
```

このページでは ChatGPT 側、つまり接続、OAuth、ツールスナップショット、トラブルシューティングについて説明します。デプロイについては [Installation](install.md) を参照してください。全体アーキテクチャについては [Architecture](architecture.md) を参照してください。

## 「接続済み」が持つ三つの異なる意味

Connector は、ある層では接続できていても、別の層では失敗していることがあります。

### OAuth 成功

ChatGPT が MCP サービスの identity を認識し、認可を得ている状態です。

### MCP ハンドシェイク成功

ChatGPT が初期化を完了し、`tools/list` を受け取ります。

### ワークステーション成功

ツール呼び出しが Edge を通過し、実際の Herdr ワークステーションに到達できます。

Connector のステータスが緑であることは、一つの層を証明するだけです。信頼できる検証は、新しい会話で `herdr_inspect` を呼び出すことです。

## MCP Connector を追加する

ChatGPT の UI は変化し続けます。一般的な流れは次のとおりです。

1. Plugins の Developer mode を有効にします。
2. **Plugins → Browse plugins** を開き、`herdr` という名前のカスタムプラグインを追加します。
3. 完全な MCP URL を入力します。末尾の `/mcp` も含めます。

```text
https://<your-edge-origin>/mcp
```

4. ブラウザで OAuth を完了します。初回の認可時、Herdr はブラウザの言語から中国語・英語・日本語のいずれかを選択します。ステップ 1 では、ターミナル用に `herdr-mcp connector approve <approval-request-id>` をコピーします。このコマンドはローカルの `herdr-mcp` サービスと Herdr server を確認したうえで、ステップ 2 が 6 桁の承認コードを求めます。承認済みの WebChat Connector はあくまで通常の MCP にすぎず、別の Connector を承認することはできません。
5. ChatGPT の **Project** を作成または開きます。最初にワークステーション情報の取得や Herdr 操作が必要なメッセージで `herdr` を選択または `@herdr` します。同じ会話では以後も Herdr が継続して利用でき、毎回 mention する必要はありません。Herdr が生成する Auto、Agent 結果、recovery、handoff の turn は app reference を明示的に保持します。Edge、Link、runtime が正常なのに後から tool が消える場合は attachment の回帰として扱い、再度の `@herdr` は復旧 workaround に限ります。

`HERDR_MCP_TOKEN` を ChatGPT に貼り付けないでください。公開 ChatGPT アクセスは OAuth を使用します。静的な bearer は curl や Cursor などのローカルクライアント向けです。

組織のポリシーによっては、カスタムアプリに管理者の承認が必要です。herdr-mcp は ChatGPT の workspace ガバナンスを迂回しません。

## 安定した origin と OAuth issuer

公開 origin は、単なる URL ではなく identity です。

推奨:

```text
HERDR_MCP_BASE_URL=https://herdr-edge.example.workers.dev
MCP URL=https://herdr-edge.example.workers.dev/mcp
```

`HERDR_MCP_BASE_URL` に `/mcp` は含めません。

Edge origin は安定させてください。ローカルの runtime generation は、ChatGPT connector を変更することなく、その背後でアップグレードできます。

## OAuth フロー

OAuth の境界は Edge が処理します。Dynamic Client Registration（DCR）は client metadata を登録するだけであり、**認可ではありません**。v0.4.6 以降、新しい Connector は、enrolled device / operator の制御チャネルが明示的な承認を記録するまで token を交換できません。

```text
Connector
  │ metadata discovery + DCR
  │ authorize + PKCE
  ▼
Herdr pending approval page
  │ request id + short-lived 6-digit code
  └─ any enrolled computer:
       herdr-mcp connector approve <request-id>
  ▼
authorization code → token → MCP request
```

同じ Worker に enrollment されたデバイス間には、この制御プレーンにおける owner / member の階層はありません。Worker / operator の資格情報が fleet を管理し、承認された Connector は通常の MCP アクセスだけを受け取ります。他の Connector を承認・revoke することも、デバイスを pair・revoke することもできません。明示的な承認より前に発行された v0.4.6 より前の OAuth token は、operator がその client grant を明示的に revoke するまで、通常の MCP 互換アクセスとして引き続き使用できます。現在の各 Connector は、不変の `connector_id` で `herdr-mcp connector list` を使って確認します。このインベントリは、安定した Connector 名、認可時刻、スロットリングされた最後の実 MCP 利用時刻を保持します。1 つのインスタンスを独立して revoke するには `herdr-mcp connector revoke <connector-id> --confirm` を使います。Connector-instance の記録より前から存在する legacy client は、互換 grant tombstone を通じて引き続き revoke できます。

承認コードは単一用途で、有効期限が短く、試行回数に上限があり、CLI の argv で受け付けるのではなく対話的に入力します。2026-09-08 の実 ChatGPT UAT により、ChatGPT でカスタム Connector を削除 / Disconnect しても、現在は信頼できる RFC 7009 の revocation リクエストが Herdr に送られないことが確認されています。したがって、provider 側の Disconnect はサーバー側の revocation シグナルとして扱われません。operator はいつでも特定の `conn_*` を手動で revoke でき、Worker は Connector の活動が 30 日間ない active な Connector インスタンスを自動的に revoke します。自動クリーンアップが削除するのはそのインスタンスの active な access / refresh 資格情報だけで、Connector 名、認可時刻、最終利用時刻、revocation 時刻、`inactive_30d` 理由を含む非機密の監査行は保持されます。実 MCP の利用は、不要な Durable Object 書き込みを避けるため、`last_used_at` を 24 時間に最大 1 回だけ更新します。

トラブルシューティング:

- 公開 origin の一貫性;
- OAuth issuer の設定;
- 認可 / token 交換;
- audience / resource の一致;
- 認証後のワークステーション link の可用性。

OAuth の成功は、ワークステーションがオンラインであることの証明にはなりません。

## ツールスナップショットと新しい会話

ChatGPT は、レビュー済みで凍結された MCP action 定義のスナップショットを使用します。runtime や Edge をデプロイしても、すでに承認済みの workspace app に新しい action が自動的に有効になるわけではありません。

Herdr 0.4.3 は、二つの contract を意図的に分離しました。

**public ChatGPT contract（first-party DEV/PROD）：epoch 7 / 19 actions。workstation runtime execution contract：epoch 4 / 18 tools。** 追加された公開 action は Edge-local の `herdr_devices` であり、ワークステーションに転送されることはありません。Runtime epoch 2/3 と public Edge epoch 3 identity は、有界な rollback / compatibility のベースラインとしてのみ保持されます。

例:

```text
Server: public epoch 7 / 19 actions

Refreshed action set   ✓ can expose epoch 7
Old/frozen action set  → may remain on an older action set
```

runtime のアップグレード後:

1. Edge / runtime のバージョンを確認します。現在の Release は `herdr-mcp update check|status` を通じて Worker drift を公開します。対話的な `herdr-mcp update` は Runtime の activation 後に安全な同一 contract の既存 Worker を reconcile し、`herdr-mcp worker update` が直接の修復経路です。
2. Runtime または既存 Worker がその場でアップグレードされたというだけで Connector を disconnect / 削除 / 再追加**しないでください**。Worker の reconcile は public OAuth origin と Connector 資格情報を保持します。
3. Herdr の公開 action catalog が変わった場合は、そのアカウントで利用できる workspace の管理機能から app action を更新・レビュー・公開し、必要に応じて新しい action を明示的に有効にします。
4. action snapshot が変わった後は新しい会話を使用します。新しい Edge 提供の description を得るには Worker コードの更新が必要ですが、すでにレビュー済みの ChatGPT action snapshot は、app action を更新するまで凍結されたままになり得ます。

古い tool snapshot のためにワークステーションを再インストールしないでください。既存の v0.4.2 runtime は epoch-2 の 18-tool workstation contract を引き続き実行します。単に、アップグレードするまで v0.4.3 のマルチデバイス runtime 機能を得られないだけです。

## カタログを意図的に小さくしている理由

Herdr は多数の native Socket API メソッドを公開しています。すべてのメソッドを MCP tool として登録すると context を消費し、選択が難しくなります。

公開カタログは一般的なリモート作業に焦点を当てています。

- `herdr_inspect`
- `herdr_since`
- `herdr_fs_*`
- `herdr_git`
- `herdr_exec*`
- `herdr_prompt`

高度な native 機能は、動的な discovery を通じて引き続き利用できます。

## 最初の検証リクエスト

安全な最初のリクエストを使用してください。

```text
現在の Herdr workspace と Git state を確認してください。読み取り専用で、何も変更しないでください。
```

期待される結果:

1. `herdr_inspect` が実際のワークステーションのデータを返す;
2. `herdr_skill` が現在のガイダンスを提供できる;
3. managed Git root が見える;
4. ファイル / Git 操作が動作する。

## 権限の確認

ChatGPT は action に対して確認 UI を表示することがあります。これらのコントロールは ChatGPT の安全レイヤーに属します。

ブラウザ拡張は、厳格な条件下で明確に識別できるページレベルの Allow action を処理できますが、workspace ポリシーやブラウザ / システムの権限ダイアログを迂回することはできません。

[Browser continuity](browser-continuity.md) と [Extension wake](browser-continuity.md) を参照してください。

## ブラウザ continuity が存在する理由

MCP はリクエスト駆動です。ChatGPT がタスクをローカル Agent に送った後、Agent が後から完了しても、ブラウザの会話は自動的に wake しません。

```text
ChatGPT → MCP → Herdr Agent

Agent が後から完了する

（別のチャネルがなければ、ブラウザ側のターンは自動では発生しない）
```

拡張は逆方向を提供します。

```text
Herdr events → browser → ChatGPT conversation
```

Connector は Web AI がワークステーションに到達する問題を解決します。ブラウザ continuity はワークステーションが会話に到達する問題を解決します。

## トラブルシューティングマップ

| 症状 | 確認項目 |
|---|---|
| Connector を追加できない | public URL、OAuth metadata、workspace ポリシー |
| OAuth は成功するがツールがない | tools/list、schema、connector の更新、古い会話の snapshot |
| ツールはあるがワークステーションがオフライン | herdr-link、runtime health、identity |
| ファイル操作が失敗する | managed root、権限、gate |
| Agent は完了したがブラウザが止まる | extension binding と continuity 設定 |

## 最低限の受け入れ条件

実際の ChatGPT 連携は次を満たすべきです。

- OAuth が完了する;
- 新しい会話が現在のカタログを受け取る;
- `herdr_inspect` がワークステーションを見つける;
- managed project を読み取れる;
- 安全なコマンドを実行できる;
- 権限が期待どおりに動作する;
- インストールされている場合、長時間タスクでブラウザ continuity が動作する。
