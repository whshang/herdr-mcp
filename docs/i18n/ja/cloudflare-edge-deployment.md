# Cloudflare Edge

*プライベートなワークステーションに安定した公開エントリを与える。*

ChatGPT は公開インターネット上にあり、Herdr ワークステーションは通常 NAT、ファイアウォール、社内ネットワークの背後にあります。herdr-mcp は開発マシンで受信ポートを開くことを要求しません。ワークステーションが Cloudflare Edge へ、認証済みの送信接続を確立します。

```text
ChatGPT
   │ HTTPS / OAuth / MCP
   ▼
Cloudflare Worker + Durable Object
   ▲
   │ 認証済み WSS
herdr-link
   │
   ▼
ローカル herdr-mcp runtime
   │
   ▼
Herdr / Git / shell
```

このページでは、デプロイモデル、ドメイン不要の `workers.dev` bootstrap 経路、推奨される Custom Domain の本番経路、そして旧来の Tunnel/CNAME デプロイを安全に移行する方法を説明します。オープンソースのセットアップは**ユーザーがドメインを所有することを要求しません**。`workers.dev` は引き続き完全にサポートされますが、選択した Cloudflare Account にすでに active zone がある場合は、OAuth/MCP クライアントを登録する前に専用の Custom Domain を確定させてください。

## 覚えておくべき 3 つのルール

1. **新規インストールは `workers.dev` で bootstrap します。active zone が利用できる場合はクライアント登録前に Custom Domain を優先します。** ドメインは必須ではなく、Cloudflare が Custom Domain の DNS レコード/証明書を自動的に管理します。
2. **ワークステーションは送信接続のみを行います。** 公開インターネットから `127.0.0.1:8772` に直接到達することはできません。
3. **public origin を identity として扱います。** Connector URL、OAuth issuer、MCP resource は、一度検証したら安定させてください。

## Edge が所有するもの

Cloudflare 層は次を提供します。

- 安定した HTTPS MCP endpoint
- OAuth discovery/authorization/token フロー
- ワークステーション identity とルーティング
- 永続 WSS link の管理
- runtime の online/offline と generation/version の状態
- MCP request/response relay
- 任意の短命なプライベート R2 汎用 artifact relay（`/artifacts`、Worker 専用 bucket）

Edge はあなたの Git リポジトリを保存せず、Herdr を置き換えるものでもありません。コード、shell コマンド、Agent は依然としてワークステーション上で実行されます。R2 bucket は一時的な汎用 artifact relay であり、アセットライブラリではありません。

## Bootstrap デプロイ: workers.dev

通常の最初のデバイスのインストールでは、インストール済み runtime を使用します。

```bash
herdr-mcp worker bootstrap
```

ユーザーのコンピュータには、リポジトリの checkout、Node.js、npm、Wrangler のダウンロード、ローカルでの Worker ビルド、`wrangler.user.toml` のいずれも必要ありません。Release CI は、ゲート済みの Edge ソースから `herdr-edge-<version>.mjs` を一度だけビルドします。bootstrap は Release に正確に対応する manifest と bundle をダウンロードし、Release のソース commit が実行中の runtime と一致することを要求し、サイズ/SHA-256 と GitHub artifact attestation を検証したうえで、Cloudflare の Worker API を通じてモジュールを直接アップロードします。

### 有効な Worker name を生成する

bootstrap は、ローカルコンピュータ名から有界な DNS-label Worker name を内部で導出します。`workers.dev` の Worker name は canonical な `dev_<ULID>` デバイス identity とは分離されたままなので、最初のデバイスの enrollment に 2 回目の Worker デプロイは必要ありません。

### public origin

デプロイ後、Cloudflare は次のような origin を提供します。

```text
https://<worker>.<account-subdomain>.workers.dev
```

MCP endpoint:

```text
https://<worker>.<account-subdomain>.workers.dev/mcp
```

bootstrap 中、この hostname は Worker のコードが healthy であることを証明します。Custom Domain を使用する予定がある場合は、この時点で最終的な ChatGPT Connector を登録しないでください。OAuth issuer / `HERDR_MCP_BASE_URL` は、クライアントを接続する前に選択した canonical origin と一致している必要があります。

### デプロイ

`herdr-mcp worker bootstrap` は、Worker script、Durable Object binding と初回利用時の migration、非秘密変数、`workers.dev` の公開、cron trigger、bootstrap secret、最初の canonical デバイス enrollment、production Link の readiness を直接作成/更新します。コア経路には R2 binding が含まれず、R2 の subscription や支払い方法も必要ありません。任意のプライベート artifact relay のプロビジョニングは、コアのインストールが healthy になった後の別個のオペレーター操作のままです。

**既存の fleet** では、first-Worker bootstrap を再実行しないでください。`herdr-mcp worker update` を使用するか、通常の対話的な `herdr-mcp update` を実行し、Runtime の更新が成功した後に Edge を reconcile させてください。既存 fleet の更新は、Release に正確に対応する Edge bundle をダウンロードし、一時的な Cloudflare 認証の前にその digest と GitHub attestation を検証します。mutation の前に現在の Worker health identity と Cloudflare の設定を読み取り、実証済みのその script のみを更新します。既存の Durable Object と secret は保持し、既知の任意の `ARTIFACT_BUCKET` binding はそのまま引き継ぎ、設定済みの public OAuth origin（Custom Domain を含む）を保持し、routes/DNS、デバイス、Connector には手を触れません。未知のカスタム binding は破棄されるのではなく fail closed になります。正確に固定された epoch-2 rollback identity だけが fleet 管理 preflight で識別され、enrollment 済みの旧 Worker をその場で reconcile できます。任意/未知の Runtime execution contract は引き続き明示的な migration 境界であり、Link/install の admission window は広げません。`update auto` は決して Cloudflare 認証を開きません。Edge の reconcile がまだ必要な場合は `herdr-mcp worker update` を報告します。

Worker のデプロイ成功が証明するのは、公開コードが存在することだけです。クライアントを登録する前に最終的な public origin を選択してください。ワークステーションの link は依然として online である必要があります。

リポジトリの `wrangler.user.example.toml` と Wrangler コマンドは、メンテナ、Edge のコントリビュータ、深い運用リカバリのために引き続き利用できます。ただし、通常のインストール経路ではありません。

## OAuth/MCP クライアントを接続する前に public origin を確定する

選択した Cloudflare Account に active zone がある場合、`herdr-mcp.example.com` のような専用 Custom Domain が推奨される本番 identity です。Cloudflare は、`workers.dev` に依存するのではなく route や Custom Domain 上で本番 Worker を動かすことを推奨しています。Herdr Worker がこの hostname の origin であるため、別の origin の前段に置く Worker Route ではなく **Custom Domain** を使用してください。

Wrangler の場合:

```toml
[[routes]]
pattern = "herdr-mcp.example.com"
custom_domain = true
```

`OAUTH_ISSUER=https://herdr-mcp.example.com` を設定して再デプロイし、ChatGPT Connector を作成する前に、Custom Domain 上の `/health`、未認証の `/mcp`、OAuth discovery を検証してください。Cloudflare が必要な DNS レコードと証明書をあなたに代わって作成します。この hostname は active な Cloudflare zone に属している必要があり、既存の CNAME や互換性のない Worker/DNS の用途と競合してはいけません。

適切な zone がない場合、またはユーザーが使用しないことを選んだ場合は、`workers.dev` を canonical な public origin として維持してください。これはサポート対象の構成であり、インストールの失敗ではありません。

## ワークステーション Link

`herdr-link` は認証済みの送信 WSS 接続を作成します。

```text
workstation ── outbound WSS ──► Edge
```

これはワークステーション identity を運び、そのワークステーション宛てのリクエストを受け取り、active なローカル runtime generation にルーティングし、heartbeat で runtime の generation/version を報告します。

この分離により、ローカル runtime が再起動したり A/B で generation を切り替えたりしても、public Connector は安定したままになります。

OAuth と public な `/health` は動作するのに、tool call が `workstation offline` を報告する場合は、Connector を再インストールするのではなく link を調査してください。

現在の Edge は、最近接続していたワークステーションにまず短いインメモリの reconnect grace を与えます。validated Link が戻らない場合、MCP エラーは `retryable`、`delivery_state`、`retry_after_ms` と有界な読み取り専用 recovery policy を提示するため、Agent は replay が安全かどうかを推測する必要がありません。ワークステーション側の Link には独自の reconnect/backoff と長時間オフライン時の recycle 経路があります。この recovery はブラウザ拡張の状態を使用せず、request-led な Durable Object write/alarm も追加しません。正確な replay ルールは [トラブルシューティング](troubleshooting.md) を参照してください。

## 最初の検証シーケンス

層ごとに検証します。

```text
1. local runtime
2. herdr-link
3. Edge health
4. OAuth metadata/token
5. public MCP initialize/tools/list
6. real herdr_inspect
7. new ChatGPT conversation
```

これにより、Edge のデプロイ失敗とワークステーションへの到達失敗を素早く区別できます。詳しくは [トラブルシューティング](troubleshooting.md) を参照してください。

## なぜ Cloudflare Tunnel 直結がデフォルトではなくなったのか

Tunnel 直結のアーキテクチャは単純です。

```text
ChatGPT → Tunnel → local MCP
```

しかし、public endpoint を 1 つのローカルプロセスに密結合させてしまいます。runtime の再起動が公開経路に影響し、OAuth identity とマシンのライフサイクルが絡み合い、マルチワークステーションのルーティングや runtime A/B が扱いにくくなります。

推奨されるアーキテクチャは次のとおりです。

```text
ChatGPT → stable Edge ← persistent link ← workstation
```

Tunnel 直結はレガシー移行経路として残るものであり、新規インストールのアーキテクチャではありません。

## いつ Custom Domain を使うか

アカウントにすでに適切な active Cloudflare zone があり、hostname を Herdr 専用にできる場合は、デフォルトで Custom Domain を使用してください。たとえば次の hostname です。

```text
https://herdr.example.com
```

これにより、長期間維持できる命名、組織が所有する OAuth identity、チームのガバナンス、そして外部 URL を変えずに将来 implementation を移行できる余地が得られます。また、`workers.dev` がフィルタされてもユーザー自身の Cloudflare hostname には到達できる、というネットワーク経路の問題も避けられます。

これは依然として Herdr の技術的な前提条件ではありません。ドメインの所有を初回インストールの前提条件にしないでください。

## Custom Domain の操作

リポジトリでは、ドメイン操作を Worker コードのデプロイから分離しています。

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

初回インストールの推奨シーケンスは次のとおりです。

```text
workers.dev 上で bootstrap + 検証
      ↓
active zone を検出 / 専用 hostname を推奨
      ↓
attach
      ↓
OAuth issuer を設定 + health / OAuth / MCP を検証
      ↓
Connector を登録 + ワークステーションを検証
```

新しい Worker コードのデプロイと本番 hostname の変更は、それぞれ独立して元に戻せる操作のままにしておくべきです。

## 旧 CNAME / Tunnel デプロイの移行

この経路が必要なのは、既存のレガシーインストールだけです。

旧構成:

```text
herdr.example.com
  ↓ CNAME
Cloudflare Tunnel
  ↓
ローカル runtime
```

hostname にすでに競合する DNS レコードがある場合、Worker の Custom Domain は cutover を行わずに単純に置き換えることはできません。

安全な移行の原則:

1. 独立した `workers.dev` origin で新しい Worker を完全に検証する
2. 旧 DNS/Tunnel の rollback evidence を記録する
3. cutover 中は旧 Tunnel を online のまま保つ
4. 競合するレコードのみを削除する
5. Worker の Custom Domain を attach する
6. public health、ワークステーション、OAuth、現在の MCP contract、実際の読み取り専用 tool call を検証する
7. 新しい経路が安定してから旧 Tunnel を退役させる
8. いずれかの検証が失敗したら以前のエントリを復元する

トランザクショナルな helper:

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

すべてのリモート mutation と同様に、DNS/ドメインの delivery が不確実な場合は、リクエストを盲目的に繰り返すのではなく、実際の Cloudflare の状態を読み取ることで解決します。

レガシー CNAME の cutover は、DNS mutation が必要になる唯一の経路です。長期間有効な Edge デプロイ資格情報の権限を広げるのではなく、**one-shot で対象 zone のみに限定した `DNS Write` token** を使用してください。rollback の観察ウィンドウが閉じたら、それを revoke し、ローカル状態を削除してください。

```bash
bin/herdr-cloudflare-dns-token --verify-only
bin/herdr-cloudflare-dns-token --revoke
```

## Cloudflare API の資格情報

デプロイ資格情報は ChatGPT OAuth とは無関係です。

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
```

最小権限を使用し、レガシー cutover が本当に必要とする場合に限り、DNS Write は別個の短命な資格情報として保ってください。詳しくは [Cloudflare Edge の資格情報](cloudflare-edge-token.md) を参照してください。

## GitHub Actions によるデプロイ

リポジトリの production Edge workflow は、関連する Edge/contract の面をビルド/テストし、production Environment のゲートを通過し、Wrangler でデプロイし、デプロイ後の health チェックを実行します。

CI 資格情報はリポジトリのファイルではなく、GitHub Environment/Secrets に置きます。

通常の Worker コードのデプロイは、次を変更してはいけません。

- Custom Domain
- OAuth issuer
- workstation identity
- DNS
- ChatGPT Connector URL

これらの境界を分離しておくことで、コードのデプロイと本番エントリの変更をそれぞれ独立して元に戻せます。

## Edge と Runtime A/B は別々の Release プレーン

```text
Public plane
Cloudflare Edge / OAuth / Connector URL

Local plane
herdr-link → runtime generation A/B
```

ほとんどの runtime implementation の修正は、public Edge identity を変更せずにローカルの generation プレーンで出荷すべきです。同様に、Edge の relay/OAuth の更新に Herdr の再起動を要求すべきではありません。

[Runtime A/B](runtime-self-upgrade.md) を参照してください。

## セキュリティ境界

- ワークステーションに公開の受信ポートはない
- 認証済みのワークステーション WSS
- ChatGPT はローカルの static bearer ではなく OAuth を使用する
- Cloudflare API 資格情報と OAuth signing material を Git に入れない
- デプロイ資格情報は最小権限を使用する
- Worker のデプロイと Domain/DNS の mutation は別々の操作である
- Edge はリモート制御プレーンであり、実際のコードと実行はローカルに残る

## デプロイの選択肢

| 状況 | 推奨 |
|---|---|
| 初回インストール / 個人利用 | `workers.dev` |
| 長期間使う個人エンドポイント | `workers.dev` または安定した Custom Domain |
| チーム/本番環境 | Custom Domain + Environment secrets |
| 旧 Tunnel/CNAME | Worker を並行して検証してからトランザクショナルに cutover |
| ローカルの Cursor/curl のみ | Cloudflare Edge は不要 |

ChatGPT を動かすまでの最短経路を知りたい場合は [インストール](install.md) に戻ってください。このページは、public control plane を理解し運用するためのものです。
