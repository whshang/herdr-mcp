# 自動化

*ドキュメント、Edge、ローカル Runtime を独立にデプロイします。*

このメンテナー向けページは、**CI/CD がどのように実行されるか**を説明します。リリースモデルを再定義するものではありません。

リリース境界についての長期的な SSOT は 1 つだけです: [`docs/release-model.md`](../../release-model.md)。このドキュメントは Runtime、Browser Extension、Contract Compatibility をバージョン/互換性のプレーンとして定義しています。一方、本ページは自動化が日常的に行使する**デプロイ面**を説明します:

```text
ドキュメント公開          パブリック Edge デプロイ    ローカル Runtime 活性化
GitHub Pages              Cloudflare Worker           runtime generation A/B
         │                           │                         │
人間 + agent 向け文書     OAuth / MCP / WSS           fs / Git / shell / Herdr
```

これらの操作は 1 つのリポジトリを共有しますが、資格情報、rollback 境界、障害ドメインを共有すべきではありません。Browser Extension の配布と contract migration には独自のフローがあり、本ページはそれらが CI とどう相互作用するかを説明するだけです。

## 自動化を切り離しておかなければならない理由

通常の修正が、OAuth identity、Cloudflare routing、ローカル runtime generation、ブラウザ拡張の identity、ChatGPT tool contract を同時に気軽に変えてよいわけではありません。

運用ルールは次のとおりです: **そのタスクに必要なデプロイ操作だけをトリガーし、他の release/compatibility プレーンは安定させたままにする。**

例:

- ドキュメントの修正 → Pages のみ公開;
- Edge relay の修正 → Worker のみデプロイ;
- runtime implementation の修正 → ローカル generation のみ検証して切り替え;
- public tool catalog の変更 → 明示的な contract-compatibility migration を使う;
- 拡張のみの UI 修正 → Runtime Release を公開せずに拡張の配布パスを使う。

## GitHub Pages

Workflow:

```text
.github/workflows/pages.yml
```

サイト:

```text
https://whshang.github.io/herdr-mcp/
```

ビルドの入口:

```bash
npm run build:site
```

サイトジェネレーターは、論理ドキュメントモデル、locale の完全性、ナビゲーション、生成ページを検証します。

Pages は次の両方を配信します:

```text
人間向けドキュメント

リモート planner ポリシー
```

`herdr_skill` は公開された skill ソースを利用でき、ネットワークアクセスが利用できない場合はバンドルされた Release のコピーにフォールバックします。`HERDR_SKILL_NETWORK=0` はオフライン動作を強制します。

## CI

Workflow:

```text
.github/workflows/ci.yml
```

CI は、ある commit が他のプレーンを壊していないことを証明します。典型的なゲートには次が含まれます:

- dependency install;
- TypeScript build;
- ドキュメントサイトの build;
- runtime tests;
- Edge/frozen-contract tests;
- extension smoke tests;
- shell syntax checks;
- package dry-run;
- `git diff --check`.

パブリック Edge contract は、意図的に runtime implementation よりも安定しています。現在の first-party DEV/PROD パブリック contract は **epoch 7 / 19 actions** であり、ワークステーションの Runtime Execution Contract は **epoch 4 / 18 tools** です。追加のパブリック action である `herdr_devices` は Edge で実行され、ワークステーションへ転送されることはありません。Runtime epoch 2/3 とパブリック Edge epoch-3 identity は、現在の DEV/PROD contract ではなく、境界付きの rollback/compatibility ベースラインとしてのみ保持されています。それらのベースライン向けの互換性テストは存在しますが、通常の runtime 変更がどちらの contract も暗黙に変えてはなりません。

### GitLab CI とその他の無人 MCP 呼び出し元

`HERDR_MCP_TOKEN`、`STATIC_MCP_BEARER_SECRET`、または 1 つの fleet 全体共有 access token を CI に置かないでください。これらの資格情報は、pipeline 単位の identity も個別の revoke も提供しません。

代わりに、登録済みの任意のワークステーションから **Automation Client** をプロビジョニングしてください:

```bash
herdr-mcp automation create --name "gitlab:group/project:prod" --device <device-id-or-unique-name>
herdr-mcp automation list
herdr-mcp automation rotate <svc_client_id> --confirm
herdr-mcp automation revoke <svc_client_id> --confirm
```

意味のある信頼境界ごとに個別の client を使い、通常は少なくとも GitLab プロジェクトと environment 単位で分けます。すべての Automation Client はちょうど 1 つの登録済みデバイスにバインドされます。`--device` はその不変の `device_id` または一意なデバイス名を受け付け、Worker は解決された不変 id を保存します。`create` は長期有効な `client_secret` を一度だけ返し、`rotate` はその置き換えを一度だけ返します。Worker が保存するのは verifier だけです。これらの値を masked/protected な GitLab variables に置いてください:

```text
HERDR_MCP_URL
HERDR_MCP_CLIENT_ID
HERDR_MCP_CLIENT_SECRET
```

job の開始時に、Worker の `/oauth/token` エンドポイントで `grant_type=client_credentials` を指定して client 資格情報を交換します。結果は最大有効期間 1 時間の短期 MCP access token で、refresh token はありません。Token の発行は、MCP リクエストごとに Durable Object レコードを 1 件書き込む代わりに、境界付きのインベントリメタデータ（`last_token_issued_at_ms` と `token_issue_count`）を更新します。

Automation Client は通常の MCP 権限のみを持ち、バインドされたデバイスにスコープされます。呼び出しで `device` を省略した場合、Worker はそのバインドされたデバイスへルーティングします。別のデバイスを選択したり参照したりすると fail closed になります。これらは fleet を管理できず、デバイスの pair/revoke、Connector の approve/revoke、別の Automation Client の作成、fleet の残りの検出もできません。Revoke は不変の `client_id` をキーとし、将来の token 発行をブロックし、すでに発行済みの access token を Worker の検証時に遮断します。

Automation の inventory/rotate/revoke は、登録済みデバイス/operator の control-plane アクションです。承認済みの WebChat Connector はこれらの管理メソッドを受け取りません。長期有効な client secret は意図的に inventory から返されず、チャットのトランスクリプトへコピーすべきではありません。登録済みワークステーションの CLI から create/rotate し、CI の secret manager に直接保存してください。

## Cloudflare Edge のデプロイ

Workflow:

```text
.github/workflows/cloudflare-edge.yml
```

Edge の自動化はパブリック control plane を管理します:

- Worker / Durable Object;
- OAuth;
- MCP relay;
- workstation routing;
- デプロイ後のヘルスチェック。

デプロイの secret は GitHub Environment/Secrets に置きます。

通常の Worker デプロイが自動的に変更すべきではないもの:

- Custom Domain;
- DNS;
- 旧来の Tunnel state;
- OAuth issuer;
- workstation identity;
- ローカル runtime generation。

Domain と DNS の mutation は、それぞれ独立した rollback evidence を持つ別個の操作です。

[Cloudflare Edge のデプロイ](cloudflare-edge-deployment.md) と [Cloudflare Edge の資格情報](cloudflare-edge-token.md) を参照してください。

## ローカル runtime の自動化

ローカル Release は、稼働中のプロセスをその場で置き換えるのではなく、runtime generation を使います:

```text
stable A
  ↓
candidate B
  ↓
health + contract ゲート
  ↓
activate
  ↓
rollback ターゲットを保持
```

よく使う入口:

```bash
bin/herdr-runtime-generation status
bin/herdr-self-update status
bin/herdr-self-update check
```

`herdr-self-update` は candidate の build、validation、activation、observation を自動化します。次は担当しません:

- contract epoch migration;
- Edge deployment;
- OAuth issuer migration;
- DNS / Custom Domain の変更。

[Runtime A/B](runtime-self-upgrade.md) を参照してください。

## Contract epoch migration

public MCP tool surface の変更は、runtime implementation の更新とは別物です。

```text
runtime implementation のアップグレード
                   ≠
public MCP contract migration
```

contract migration は ChatGPT の tool snapshot に影響し、ローカル runtime、Link identity、パブリック Edge、新しい会話での検証にわたる明示的なエビデンスを必要とします。

## ブラウザ拡張の Release

拡張は continuity 層です。リポジトリのバージョニングは共有しますが、信頼境界は分離したままです。

検証には次が含まれます:

- manifest と JavaScript の互換性;
- Native Messaging host;
- workspace binding;
- Auto ゲート;
- progress/settled の挙動;
- recovery/handoff;
- JSON → MCP bridge。

Node のテストではページの挙動を証明できないため、実際のブラウザでの UAT が依然として必要です。

## `herdr_skill`

`herdr_skill` は次を組み合わせます:

1. herdr-mcp プロジェクトポリシー;
2. runtime / contract / generation のコンテキスト;
3. 一致する Herdr ガイダンス。

これは Web planner の挙動を導きますが、CI、デプロイスクリプト、runtime 管理を置き換えるものではありません。

`herdr_methods` は、インストール済み Herdr Socket API schema の権威であり続けます。

## Release 判断表

| 変更 | Release プレーン |
|---|---|
| ドキュメント、ナビゲーション、チュートリアル | Pages |
| Worker/OAuth/relay | Edge |
| ローカル実装 | Runtime A/B |
| ブラウザ continuity | Extension + compatibility validation |
| Tool catalog/schema ABI | Contract epoch migration |
| Custom Domain/DNS | Domain cutover |

## 完了とは何か

workflow がグリーンであることは、最終的な証明ではありません。

対応する runtime のエビデンスが必要です:

- Pages: 生成されたページとリンクが機能する;
- Edge: health + workstation + OAuth/MCP が機能する;
- Runtime: active generation + 実際の tool call + rollback target;
- Extension: 実際のサイトでの binding/Auto/recovery smoke;
- Contract: 新しい会話が期待どおりの tool snapshot を受け取る。

自動化が価値を持つのは、検証と rollback の境界を固定するからであり、すべての人間の判断を排除するからではありません。
