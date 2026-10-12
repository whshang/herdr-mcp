# Runtime のアップグレード

*A/B でローカル runtime をアップグレードし、リモート開発を壊さない。*

herdr-mcp は公開接続プレーンとローカル runtime プレーンを分離します。

ChatGPT 側では、Edge の origin、OAuth identity、MCP URL は安定したままにすべきです。ワークステーション側では、herdr-mcp runtime をアップグレード・検証・切り替え・rollback できます。Connector を再接続する必要はありません。

```text
ChatGPT
  │ 安定した MCP/OAuth origin
  ▼
Cloudflare Edge
  │ 永続的なワークステーション WSS
  ▼
herdr-link
  │ active generation ポインタ
  ├───────────────┐
  ▼               ▼
runtime A       runtime B
127.0.0.1:8772 127.0.0.1:8773
```

## 解決する課題

分離しない場合、ローカルのアップグレードは公開経路、OAuth identity、実行中の tool call、ChatGPT セッションに同時に影響し得ます。

A/B デプロイは次を分離します。

- candidate runtime のビルド
- その runtime の検証
- 新しいトラフィックをそこへ送ること
- 以前の runtime の drain

## Runtime generation とは

generation とは、独立してアドレス指定できるローカル herdr-mcp runtime の 1 つです。

```text
generation A
  現在の stable runtime

generation B
  candidate runtime
```

`herdr-link` が active generation のポインタを所有します。Edge はローカルのポート変更を知る必要がありません。

したがって次のようになります。

```text
runtime の切り替え != Connector の切り替え
```

## DEV / PROD は generation 上の provenance プレーン

現在の runtime は、同じ generation の仕組みの上に明示的なソース開発プレーンを提供します。3 つ目の恒久的な runtime 環境を作るものではありません。

```text
PROD
  公開済み / 検証済みのインストール済み binary
  固定された recovery source

DEV
  source provenance を持つ repo/worktree build
  managed generation として activate
```

通常のソース dogfooding では次を使用します。

```bash
herdr-mcp dev status
herdr-mcp dev sync
herdr-mcp dev rollback
```

`dev sync` は、repo ビルドの DEV generation を activate する前に現在の PROD binary/checksum を固定し、server/Link の generation reconcile が完了した後にのみ切り替えを受け入れます。`dev rollback` は、ソースを再ビルドしたり「どの previous generation が安定だったか」を推測したりするのではなく、その固定された PROD source に戻ります。低レベルの `bin/herdr-runtime-generation ...` コマンドは実装/UAT 作業では引き続き有用ですが、ソース dogfooding における通常のメンテナ向け入口ではありません。

Runtime の DEV/PROD は、ブラウザ拡張の DEV/STANDALONE/STORE identity とは無関係です。

## A/B はプロセスマネージャではない

generation manager は任意のコマンドを起動するものではありません。推奨フローは次のとおりです。

1. candidate をビルドする
2. 新しい loopback endpoint で candidate を起動する
3. generation を登録する
4. health チェックと contract チェックを実行する
5. activate する
6. 観察する
7. 後で古い generation を削除する

プロセスの生成とトラフィックの activate は分離したままにします。

## CLI

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation candidate-<id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation candidate-<id>

bin/herdr-runtime-generation rollback

bin/herdr-runtime-generation remove --generation candidate-<id>
```

アップグレードや rollback の前には、必ず `status` から始めてください。古いデプロイログから active な runtime を推測してはいけません。

lifecycle mutation では決して `launchctl submit` を使ってはいけません。推測された launchd job はコマンド終了後に replay される可能性があるため、破壊的な rollback/update が複数回実行され得ます。独立したプロセスから managed lifecycle コマンドを直接実行するか、`RunAtLoad=true` と `KeepAlive=false` を設定した明示的な one-shot plist を使用してください。

## Activation gate

candidate は検証を通過した後にのみ active になります。

1. endpoint に到達できる
2. health/discovery が動作する
3. 実際の `tools/list` が成功する
4. public tool contract が現在の contract epoch と一致する
5. 任意の runtime version チェックを通過する
6. 必要な場合、observation チェックが healthy なままである

現在の first-party DEV/PROD の public Edge contract は **epoch 8 / 19 actions**、ワークステーションの Runtime Execution Contract は **epoch 5 / 18 tools** です。直前の凍結された epoch-4 contract と凍結された epoch-2 catalog は、有界な rollback/compatibility ベースラインとしてのみ残ります。epoch 3 は historical only であり、public Edge の epoch-3 identity は非 DEV/PROD の有界なベースラインとして残ります。正確な build hash は Release の証拠であり、長期にわたるドキュメント上の事実ではありません。activation は現在の凍結された contract 定義に従います。

## Runtime のアップグレードと contract migration は別物

Runtime A/B が意味するのは次のことです。

> 公開 contract を維持したまま実装を置き換えること。

例を示します。

- filesystem の挙動を修正する
- snapshot fallback を改善する
- 実行の信頼性を改善する
- tools を変更せずに内部の relay 挙動を変更する

tool surface の変更は別物です。

```text
runtime implementation upgrade
    !=
public MCP contract migration
```

contract migration は ChatGPT の tool snapshot、Edge compatibility、Link の期待値に影響します。明示的な epoch migration プロセスが必要です。

### 1 つの epoch をまたぐ（N → N+1）

Release の選択は 2 つの独立した段階、discovery と admission から成ります。

**Discovery** は「このワークステーションがどの Release を見るべきか」だけを決めます。GitHub releases インデックスを読み、非 draft の semver であり、release manifest を公開しており、かつ設定された update チャネルが受理するもののうち、最も高い 1 つを選びます。Discovery は contract identity を読まないため、より古い Release に差し替えることも、選択を中間 bridge に書き換えることもありません。チャネル内で適格な最も高い Release がそのまま選択された Release です。

**Admission** は、何かをダウンロードする前にその選択された manifest を検証します。公開済みのすべてのアップデータは admission を「完全一致」としてのみ実装しています。manifest の `contract` identity `{epoch, hash, tool_count}` が実行中 Runtime の contract identity と一致しなければ、`release manifest contract identity mismatch` で fail closed します。したがって公開済みの v1.0.x（epoch 4）は、どのチャネルでも epoch 5 の Release へ直接 update できず、その結果は拒否であって別の Release へのフォールバックではありません。

前方にちょうど 1 つの epoch をまたぐことは**計画機能**であり、公開済みのどのアップデータにもまだ実装されていません。実装されると、`runtime-manifest.json` 内で次のように宣言されます:

```json
"contract": { "epoch": 5, "hash": "sha256:<epoch-5>", "tool_count": 18 },
"contract_migration": {
  "from": { "epoch": 4, "hash": "sha256:<epoch-4>", "tool_count": 18 },
  "state_schema_from": 15
}
```

この宣言は manifest のバイト列の一部であり、（実装後は）既存の manifest digest に対する GitHub artifact attestation の対象に含まれます。

計画中の受理規則（`L` をローカルの contract identity、`M` を manifest の `contract` とします）:

1. `M == L` — 通常の同一 contract 更新。挙動は変わりません。
2. `M != L` — 次がすべて成り立つ場合のみ受理します。`M.epoch == L.epoch + 1`、`contract_migration` が存在し形式が正しい、`contract_migration.from` が `L` と完全に一致（epoch, hash, tool_count）、`contract_migration.state_schema_from` がローカルの state schema と一致、`manifest.state_schema` がローカルの state schema と一致。
3. それ以外は何もインストールする前に拒否し、ローカルと対象の epoch を明示したエラーを返します。複数 epoch のスキップ、より低い epoch、`from` の hash/tool_count 不一致、宣言の欠落または不正な形式、`contract_migration` 内の未知フィールドが該当します。

その他のゲートはすべて変更なく実行されます。manifest attestation、tag/source_ref identity、asset sha256、candidate probe、health をゲートにした activation、update fence、チャネル規則です。cross-epoch 更新では、ダウンロードした candidate は `L` ではなく対象 contract `M` に対して検証されます。

`update auto` は対話コマンドと同じ discovery と同じ admission チェックを使うため、まったく同じ規則で（計画上の）1 つの epoch をまたぐことができ、PROD + Stable 限定のままです。このプロトコルは state schema の移行を許可しません。schema 変更は引き続き明示的な major migration 経路です。

### v1.0.4 bridge は計画中の Release であり、`herdr-mcp update` では到達できない

通常の v1.0.x → v1.1 経路は 2 段階を予定しています。まず `herdr-mcp update` で v1.0.4 epoch-4 bridge（contract identity が同一なので通常の同一 contract 更新）へ進み、次に `herdr-mcp update` で `contract_migration.from = epoch 4` を manifest で宣言した最初の v1.1 Release（予定は v1.1.0-rc.2）へ進みます。ただし現時点では 2 つの事実がこの計画を制限します:

- v1.0.4 は**計画中**の bridge Release であり、確立された公開済み Release ではありません。現時点でどの v1.0.x ワークステーションもこれをインストールできず、v1.1.0-rc.1 はどの経路でも v1.0.x から到達できません。
- v1.0.4 が公開された後でも、公開済みの v1.0.0–v1.0.3 runtime 上の通常の `herdr-mcp update` はダウンロード後に失敗します。これらのアップデータはダウンロード後の candidate probe を文字列 `contract epoch 2 / 18 tools` にハードコードしています。probe はダウンロードした candidate 自身の `herdr-mcp version` 出力を読み、`staged update binary identity does not match release manifest` で拒否します。v1.0.x や v1.0.4 の candidate は実際の epoch 4 を報告するためです。通常の update で bridge をインストールするには、後続のアップデータがこの probe を修正する必要があります。このハードコードされた probe は現在の 1.1.0-rc.1 ソースにも残っているため、その修正は v1.0.x だけの歴史的な問題ではなく、計画上の経路の前提条件です。

したがって現在の v1.0.4 への主要な経路は、v1.0.4 release binary の**認証済み手動インストール**です:

1. 対象ターゲットの公開された `runtime-manifest.json` を読み、asset 名と sha256 を取得する。
2. Release からその asset をダウンロードし、sha256 を検証する。
3. Release の artifact attestation を検証する。例: `gh attestation verify <asset> --repo whshang/herdr-mcp`
4. 検証済み binary 自身のインストーラーを実行する:

```bash
<binary> install
```

手動インストールも同じ service lifecycle を通じて不変な generation を配置します。contract identity、state schema、attestation、health の各ゲートを迂回するものではありません。

### epoch をまたいだ後の rollback

`herdr-mcp rollback` は直前のインストール済み generation を再アクティブ化します（例: 1.1 → v1.0.4）。これは service rollback なのでプラットフォーム境界が重要です:

- macOS では、成功した install/activation が ready な service rollback（以前の service 定義と以前の generation）を記録し、`herdr-mcp rollback` がそれを再検証して再アクティブ化します;
- Linux では `herdr-mcp rollback` は公開操作として提供されておらず、成功後の service rollback レコードも書き込まれません。代わりに、インストール失敗時は直前の generation をトランザクション的に復元します。したがって、すでに成功した更新に対する cross-epoch rollback は macOS 限定です。

両側とも state schema 15 なのでデータベースの復元は不要であり、更新後に書き込まれた durable state は巻き戻りません。依拠する前に実際の service/state を確認してください。その共有状態にある epoch 5 の 2 つの書き込みに注意してください:

- `browser_dispatches.message_canonical_digest` は nullable な前向き追加列であり、番号付き migration ではありません。schema 15 の epoch 4 binary は同じデータベースを開き、この列を無視します。以前に書き込まれた行は `NULL` のままで raw のみで照合され、これは偽の canonical 一致ではなく fail-closed です。
- epoch 5 の browser endpoint 登録書き込みは `webchat_control_allowed=1` を保存し、再登録は以前の `0` を上書きします。rollback 後の epoch 4 binary はその保存値を読み、WebChat 制御を許可済みとして扱います。つまり rollback は 1.1 が下した同意判断を巻き戻しません。

## トラフィックの切り替え

activation はローカルのルーティングポインタを変更します。

```text
切り替え前
新規リクエスト → A

activate B

切り替え後
新規リクエスト → B
既存リクエスト → A は drain
```

望ましい結果は次のとおりです。

- persistent WSS は接続されたまま
- Edge URL は変わらない
- OAuth identity は変わらない
- すでに delivery された作業は二重実行されない

## Rollback

rollback は新しいリクエストを以前の known-good generation に戻します。

```bash
bin/herdr-runtime-generation status
bin/herdr-runtime-generation rollback
```

Runtime rollback はビジネス rollback ではありません。以前の runtime がすでに実行した Git の変更、ファイル、リモートサービス、Agent の副作用を巻き戻すものではありません。

## ローカル制御状態

generation の状態が保存するものは次のとおりです。

- generation specification
- desired active generation
- observed active generation
- previous/last-good generation
- activation observation

これはワークステーションの制御状態であり、リポジトリのソースではありません。bearer 資格情報も保存しません。

## Heartbeat と Edge の状態

ワークステーションの link は、heartbeat データを通じて active な runtime identity を報告します。

したがって Edge は次を観察できます。

```text
workstation online
active generation changed
runtime version changed
```

heartbeat の状態が収束するまでの短い遅延は、必ずしも activation の失敗を意味しません。ローカルの generation 状態と、その後の heartbeat 更新を確認してください。

## 推奨アップグレードフロー

```text
現在の状態を確認
  ↓
candidate をビルド
  ↓
candidate を起動
  ↓
generation を登録
  ↓
health と contract を検証
  ↓
activate
  ↓
実際の使用を観察
  ↓
後で古い generation を削除
```

いずれかの手順が失敗した場合の対応は次のとおりです。

- activation 前: candidate を修正する
- activation 後: rollback を評価する
- mutation の delivery が不確実: 先に確認し、盲目的に繰り返さない

## 既存 v0.4.8 から 1.0 へのワンコマンドアップグレード

通常の enrolled user が実行するのは 1 コマンドだけです。

```bash
herdr-mcp update
```

ユーザーが 1.0 binary を手動ダウンロードしたり、`update major-apply` を直接実行したり、Worker を作り直したり、device を再 pair したり、ChatGPT Connector を追加し直す必要はありません。immutable な v0.4.8 updater は、schema 5 / Runtime epoch 2 の identity を保つ v0.4.9 migration bridge を最初に発見します。bridge は元の update job の detached worker としてだけ動き、成功経路では v0.4.9 を production service として install しません。互換性のある schema 15 / Runtime Contract epoch 4 Runtime を download/attest し、正確な v0.4.8 service が source のまま qualified major migration を実行した後、新 Runtime が既存 Cloudflare Worker を in-place reconcile します。

Worker name と public origin は変わらず、Durable Objects、secrets、既知の optional binding、enrolled devices、OAuth issuer、Connector records を保持します。同じ update job の中で Cloudflare authorization が browser に開く場合があります。複数の accessible Cloudflare account に同名 Worker がある場合は fail-closed し、non-secret の `CLOUDFLARE_ACCOUNT_ID` で明示的に disambiguate できます。v0.4.8 foreground updater の bounded watch が先に終了しても、authorization 中の detached migration は継続するため、2 回目の update command は不要です。

migration 前には exact v0.4.8 binary と schema-5 database snapshot を N-1 rollback material として保存します。必要な場合の recovery command は `herdr-mcp update major-rollback` です。`update major-apply` は bridge 内部および maintainer recovery/UAT 用 primitive として残りますが、通常の v0.4.8 user の手順ではありません。

この 1 コマンドの経路は schema-5 の v0.4.8 → 1.0 境界だけを対象とします。公開済み v1.0.x を計画中の v1.0.4 epoch-4 bridge へ移行するのは別の境界であり、通常の `herdr-mcp update` では現在完了できません。前述の節を参照してください。

## `herdr-self-update`

`bin/herdr-self-update` は generation の仕組みを使用します。

次のような用途に適しています。

- 同じ contract epoch 内の更新
- candidate の検証と制御された切り替え

次のための近道ではありません。

- public contract の変更
- Edge/OAuth の移行
- Domain/DNS の変更
- 無関係な Herdr daemon のアップグレード

これらは別々の Release プレーンです。

## Release プレーン

```text
Public Edge plane
Worker / Durable Object / OAuth / public MCP relay

Local runtime plane
herdr-link / runtime generation
```

これらを分離しておくと、Release の爆発半径（blast radius）を小さくできます。

## セキュリティルール

- candidate は loopback endpoint 上に留まらなければならない
- contract の不一致は active になれない
- delivery 済みの mutation は切り替えによって二重化されない
- 古い generation は削除前に drain する
- 資格情報は generation の状態の外に置く
- rollback の前後に実際の Git/Agent/サービスの状態を確認する
- contract migration と domain の mutation は通常の self-update 操作ではない

## 受け入れ基準

A/B アップグレードの成功が証明するものは次のとおりです。

- candidate が healthy
- contract gate を通過
- active generation が変更された
- Edge heartbeat が収束した
- 新しいリクエストが candidate を使用する
- Connector/OAuth identity が安定したままだった
- 実際の MCP call が成功した
- 観察中も rollback 先が利用可能だった

関連:

- [Cloudflare Edge デプロイ](cloudflare-edge-deployment.md)
- [CLI リファレンス](cli-reference.md)
- [トラブルシューティング](troubleshooting.md)
- [アーキテクチャ](architecture.md)
