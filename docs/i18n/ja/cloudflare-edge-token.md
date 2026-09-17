# Cloudflare 資格情報

*最小権限、一時的な bootstrap、検証可能な状態。*

herdr-mcp Edge のデプロイには Cloudflare API 権限が必要ですが、長期運用はアカウント全体の管理者資格情報に依存すべきではありません。

このページでは、本プロジェクトの最小権限 Cloudflare Account API Token ワークフローを説明します。

## 資格情報の役割を分離する

デプロイ中に現れる資格情報の役割は二つあります。

1. **bootstrap credential** — 対象 token を作成するために一時的に使う、既存の高権限資格情報
2. **deployment credential** — 以降の herdr-mcp のデプロイ／カットオーバー作業で使う最小権限 token

目標は、bootstrap credential をできるだけ早く通常のワークフローから外すことです。

## 対象となる権限

本プロジェクトの helper が作成する token は次のスコープを持ちます。

- 対象 zone の **Workers Routes Write**
- 対応する account の **Workers Scripts Write**

既定では広範なアカウント管理者アクセスも R2 アクセスも要求しません。私有 R2 artifact relay は明示的な任意能力です。必要なときにだけ有効化し、オペレーターが Cloudflare の R2 サブスクリプション／支払い設定に同意したうえで、bucket provisioning には別途 R2 対応の deployment credential を使ってください。

### コアインストールと任意の artifact relay

製品が能力を分けているのと同じように、権限も分けます。

- **コアインストール**に必要なのは account の **Workers Scripts Write** だけです（手動で作成する token には Account Settings Read、Memberships Read、User Details Read も持たせてください）。コアデプロイは `workers.dev` 上の Workers + Durable Objects であり、R2 なし、支払いカードの紐付けなし、R2 権限なしの Workers Free で成功しなければなりません。
- **任意の artifact relay** は、ユーザーが artifact relay を明示的に有効化したときにだけ **Workers R2 Storage Write** を追加します。Cloudflare R2 の課金／サブスクリプションの手順も同時に伴います。先回りして要求しないでください。

active と確認できた token からの特定の呼び出しが 403 を返す場合、それは壊れた token ではなく権限の不足を意味します。`GET .../workers/subdomain` → Account Settings Read、Worker script deploy → Workers Scripts Write、R2 provisioning → 任意の Workers R2 Storage Write。不足している権限を名指しし、まさにそれだけを付与して再試行してください。これを一般的なデプロイ失敗として報告しないでください。

純粋な `workers.dev` デプロイでは、すべての操作に zone route が必要とは限りません。実際に使うデプロイ経路が要求する権限だけを付与してください。

## Helper コマンド

```text
bin/herdr-cloudflare-token
```

オプションの確認：

```bash
bin/herdr-cloudflare-token --help
```

よく使うモード：

```bash
# token を作成せずに identity を解決し、bootstrap 権限を検証する
bin/herdr-cloudflare-token --zone example.com --dry-run

# 最小権限の資格情報を作成して保存する
bin/herdr-cloudflare-token --zone example.com

# 保存済みの資格情報を検証する
bin/herdr-cloudflare-token --zone example.com --verify-only

# 保存済みの資格情報を明示的に置き換える
bin/herdr-cloudflare-token --zone example.com --rotate
```

bootstrap credential はプロセスの環境変数として渡します。

```bash
export CLOUDFLARE_API_TOKEN='<temporary-bootstrap-token>'
# または CF_API_TOKEN
```

実値をリポジトリのファイル、コミット、スクリーンショット、チャットの記録にコピーしないでください。

## ローカルの資格情報状態

helper の既定パス：

```text
~/.config/herdr-mcp/cloudflare-cutover.env
```

このファイルは制限されたローカル権限（**mode `0600`**）で書き込まれ、token の値は標準出力に表示されません。

また account/zone の identity も記録するため、後の検証で意図した Workers Scripts と Routes へのアクセスを確かめられます。

このファイルはプロジェクト設定ではなくローカルの資格情報状態です。コミットしてはいけません。

## なぜ最初に dry-run するのか

token の作成は mutation です。新しい Cloudflare アカウントや zone では、まず次から始めてください。

```bash
bin/herdr-cloudflare-token --zone <zone> --dry-run
```

これにより、zone identity の欠落／曖昧さ、bootstrap 権限の不足、permission group の問題を、新しい資格情報を生成する前に検出できます。

## なぜ rotation が明示的なのか

ローカルに資格情報がすでに存在する場合、helper はそれを黙って置き換えません。`--rotate` が必要です。

資格情報の rotation は、以前の token を使い続けている実行中のデプロイ、CI、その他のスクリプトに影響し得るため、置き換えには明示的な意図が要ります。

## 検証が意味するもの

`--verify-only` は token 文字列の存在だけを確認するのではありません。保存された資格情報が active で、期待する Cloudflare の account/zone API に対して使用可能であることを確認します。

よい検証では次を確認します。

- token が active であること
- account identity が正しいこと
- Workers Scripts へのアクセス
- 必要な場合の、対象 zone への Workers Routes アクセス
- 私有 artifact bucket provisioning のための Workers R2 Storage アクセス（任意の artifact relay が有効なときだけ検証します。コアインストールでは意図的に省略します）

デプロイが失敗したときは、資格情報の失敗と Worker/DO 設定の失敗を区別してください。

## ChatGPT にこの資格情報は不要

これらは別々の層です。

```text
Cloudflare API token
  purpose: deploy and maintain Edge

ChatGPT OAuth token
  purpose: ChatGPT accesses the deployed MCP Edge

HERDR_MCP_TOKEN
  purpose: local curl / Cursor / legacy local compatibility
```

一方を他方の層にコピーしないでください。特に、Cloudflare API token と `HERDR_MCP_TOKEN` は ChatGPT Connector UI に置くものではありません。

## 資格情報の衛生

推奨プラクティス：

- bootstrap credential は一時的なプロセス環境変数で渡す
- 最小権限の資格情報は、制限されたローカルファイルか適切な Secret Store にのみ保存する
- secret を `wrangler.toml`、README、サンプル、Git に置かない
- secret の値をターミナルログに echo しない
- ログには資格情報の中身ではなく readiness/status を記録する
- 権限を広げる前にアーキテクチャ／設定を調査する
- rotation の後は、古い token を外す前に新しい token を検証する

## Edge デプロイとの関係

新しいインストールでは、まず `workers.dev` 上で Worker、workstation link、OAuth、MCP を検証し、その後で Custom Domain/routes を別のステップとして追加してください。

アーキテクチャとデプロイについては [Cloudflare Edge デプロイ](cloudflare-edge-deployment.md)、最短のエンドツーエンド手順については [インストール](install.md) を参照してください。
