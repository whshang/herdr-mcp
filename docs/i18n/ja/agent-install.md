# Agent インストール

*コーディング Agent 向けの最小のエンドツーエンド実行契約。人向けの詳細は[手動インストールガイド](install.md)、障害診断は[トラブルシューティング](troubleshooting.md)にあります。*

> **実行ロール：Agent。** このページを一度読み、そのまま実行してください。リンク先のドキュメントをあらかじめ再帰的に読み込まないでください。詳細ガイドは、その blocker が実際に発生したときだけ開きます。人が本当に必要な手順はユーザーが担当します。サインイン、システム承認、Cloudflare Token の作成 / アカウントやドメインの選択、ChatGPT OAuth です。

## 1. 実行ルール

1. **ツールを呼ぶ前に計画する。** 最初の mutation の前に、現在のマシン / fleet の状態、人にしかできない境界、手順間の依存関係、最終受け入れチェックを内部で確定してください。現時点で判明している独立した読み取りは一度に発行します。同じプロジェクトと同じ安全境界を共有する決定的な shell / Git 作業は、その間に必要なローカルチェックも含めて一つの有界な実行呼び出しにまとめてください。再計画は、結果が次の引数や安全判断を変えるとき、ユーザーの操作が必要なとき、mutation の配信が不確実なときだけです。コマンドごとに `status` を実行したり、何も変わっていないことを証明するためだけにポーリングしたりしないでください。
2. **既存の作業を保つ。** `reset --hard`、`clean -fd`、無関係な dirty ファイルの上書き、既存 fleet の再構築をインストールの近道として行ってはいけません。
3. **通常のインストールは現在の Stable GitHub Release だけを使う。** ユーザーが明示的にソース開発を求めない限り、このリポジトリを clone したり、`npm` / `cargo` でワークステーション runtime をビルドしたりしないでください。
4. **秘密は一時的に保つ。** Cloudflare Token を echo したり、Git、通常のログ、shell history に書いたりしないでください。現在のプロセス環境か、CLI の隠しプロンプト経由でのみ渡します。ローカルの `HERDR_MCP_TOKEN` や Cloudflare Token を ChatGPT に入れないでください。
5. **ユーザーのネットワーク環境を変更しない。** プロキシを切り替えたり、システム DNS を書き換えたり、汎用プロキシを作ったりしないでください。唯一の復旧例外は §6 に示す検証済みの単一 Worker hosts エントリです。
6. **人の境界でだけ止まる。** 安全に判断・自動化できる手順はすべて続けてください。Cloudflare のログイン / Token 作成、macOS の権限承認、曖昧な Account / zone の選択、ChatGPT OAuth が実際にユーザーを必要とするときだけ一度尋ねます。

## 2. 最初に判断する：最初の Worker か既存 fleet か

ユーザーに何度も尋ねず、この順で確定してください。

- pairing アドレスがすでに提示されている：このマシンは**既存の Herdr Worker に参加する**。§5 へ進みます。
- このマシンがすでに enrollment 済み：既存 fleet を保ち、別の Worker を作らずに修復 / 検証します。
- それ以外は**最初の Worker** の準備です。Cloudflare Token が使えるようになったら `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts` を列挙し、その Account にすでに Herdr Worker がある場合は新しい Worker の mutation を止め、enrollment 済みデバイスからの pairing を要求して §5 へ進みます。
- 現在インストール中のコンピュータで、fleet 検出の探査として `herdr-mcp worker pair` を実行してはいけません。
- ユーザーが fleet の意図を明示的に変えない限り、ランダムサフィックス付きの Worker、二つ目の R2 バケット、二つ目の Connector にフォールバックしてはいけません。

## 3. ローカルインストール段階

`herdr` が見つからない、または Herdr Server がまだ起動していないという理由だけでインストールを止めないでください。<https://github.com/whshang/herdr-mcp/releases> から **Latest stable** の herdr-mcp プラットフォームバイナリをダウンロードし、ユーザーの `PATH`（通常は `~/.local/bin/herdr-mcp`）に置いてから、次を実行します。

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

`herdr-mcp install` が Herdr 依存関係の復旧を担当します。`HERDR_BIN`、ユーザーの安定パス、Homebrew/`/usr/local`、PATH の順で解決し、既存 Herdr を検証して自動更新を試み、完全に見つからない場合は <https://herdr.dev/> の公式 installer を実行し、最後に Herdr Server/API が到達可能であることを確認します。dependency-recovery の明示的な失敗が報告されない限り、Agent は二つ目の手動 Herdr インストール手順を作らないでください。Windows Candidate UAT は引き続き exact herdr-mcp candidate artifact を使用します。

`~/.local/bin/herdr-mcp` は存在するのにインタラクティブ shell が解決できない場合は、`installed_but_not_on_shell_path` と分類し、ユーザーの PATH を修復して新しい shell で検証してください。再インストールしたり、二つ目の PATH owner を作ったりしないでください。PATH の修復が必要なときだけ[トラブルシューティング](troubleshooting.md)を使ってください。

macOS では Cloudflare の作業の前に `herdr-mcp permissions status` を実行します。`needs_setup` を報告した場合は、安定した Herdr-MCP broker に Full Disk Access を一度付与してから `herdr-mcp permissions verify` を実行してください。先に保護パスを探査したり、`sudo` を使ったりしないでください。broker が担うのは MCP のファイル / Git TCC であり、ペインの shell ではありません。ペインはその実行ホストの TCC に従います。Linux は user-service / process backend を使います。Windows は昇格なしで Startup フォルダのショートカット、ユーザープロセス、Credential Manager を使い、必要ならインストール済みの `herdr server` を起動できます。通常のインストールに Node.js、Wrangler、npm、Cargo は不要です。

### 任意：高速セマンティック判断

コア runtime が正常になった後、高速 decision モデルは任意であることをユーザーに案内します。推奨設定例は [TypeSafe.ai](https://typesafe.ai/) ですが、必須 Provider ではなく、インストールを妨げてはいけません。ユーザーはそこで API Key を作成し、次を実行できます。

```bash
herdr-mcp semantic setup
herdr-mcp semantic status
```

`semantic setup` は API Key を非表示の端末入力から読み取り、argv では受け取りません。他の互換 Provider は `--name`、`--protocol`、`--url`、`--model` で設定できます。この手順を省略しても Herdr は従来の決定論的動作を維持し、インストールは通常どおり続行します。

## 4. 最初の Worker：Cloudflare + bootstrap

Herdr には Cloudflare Workers Free で十分で、支払い方法は不要です。ユーザーにアカウントがない場合は、登録が無料であることを伝え、Google サインインを勧めてください。Token が必要なときは <https://dash.cloudflare.com/profile/api-tokens> を開きます。選択した Account には **Edit Cloudflare Workers** を推奨します。カスタム Token には **Account Settings → Read** と **Workers Scripts → Write/Edit** が必要です。`workers/subdomain` が 403 を返す場合は、不足している権限を報告し、Token のスコープを広げないでください。**コアインストールに R2 は不要です**。Workers R2 Storage は artifact relay のためにだけ追加します。

Token は現在のプロセス内に `CLOUDFLARE_API_TOKEN` として置くか、`worker bootstrap` の隠し入力で渡してください。Token をコマンドラインのリテラル、リポジトリの設定、通常のログに入れないでください。

次を実行します。

```bash
herdr-mcp worker bootstrap
```

このコマンドは、Cloudflare API の事前チェック、Account と `workers.dev` subdomain の解決、既存 Herdr Worker の検出、release manifest と `herdr-edge-<version>.mjs` artifact attestation、Worker/DO bootstrap、最初の canonical デバイス enrollment、資格情報の保存、production Link の整合を担います。通常のユーザー経路は Wrangler を実行せず、ソース checkout も必要としません。

公開 origin は Connector の作成前に一度だけ選びます。

- Account に明らかに適した active zone が一つだけなら、`https://herdr-mcp.example.com/mcp` のような専用 Custom Domain を優先します。
- 実質的に異なる zone が複数適している場合は、どれを使うかを一度だけユーザーに尋ねます。
- 適した zone がない、ユーザーが望まない、または hostname が衝突する場合は `workers.dev` のままにします。例えば `https://herdr-edge-device.username.workers.dev/mcp` です。

通常の Custom Domain 経路に汎用 DNS Write は不要です。その後のネットワーク復旧が、選択済みの OAuth/MCP 公開 origin を黙って変更してはいけません。

## 5. 既存の Worker に参加する

enrollment 済みデバイスが pairing を作成し、新しいマシンはそれを消費するだけです。

```bash
herdr-mcp worker connect "<pairing-address>"
```

6 桁の検証コードは、CLI が要求したときだけ尋ねてください。表示名は既定でコンピュータ名から作られます。ユーザーが明示的に別名を望むときだけ `--name` を渡します。この経路は別の Worker をデプロイせず、別の Connector を作成せず、fleet 全体の長期秘密を新しいマシンへコピーしません。

enrollment 後の identity は不変の `device_id` です。例えば `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV` は、`dev_` と 26 文字の ULID です。表示名は identity とは別に扱ってください。hostname から `WORKSTATION_ID` を**でっち上げてはいけません**。

詳細は[既存 fleet への参加](existing-worker-connect.md)を参照してください。

## 6. Link とネットワーク

`worker bootstrap` / `worker connect` はデバイス単位の資格情報を作成し、production Link を整合させます。Agent が検証すべきは次のとおりです。

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Custom Domain がない場合は、まず `workers.dev` への直接接続を試します。DNS 失敗時、この復旧を備えた runtime は Cloudflare DNS、続いて Google DNS を照会します。実際の TLS `/health` による Herdr チェックを通過した候補だけが、マーク付きの単一ホストの system-hosts エントリになれます。v0.4.8 では、Agent が手動インストールガイドと同じ検証済み hosts 復旧を行ってから bootstrap を再実行します。Unix では対話的な `sudo` が必要なことがあり、Windows では管理者ターミナルが必要なことがあります。次に Link の直接接続を再試行し、その後に既存のローカルプロキシ、最後に署名付き共有 Relay を検討します。システム DNS、ネットワークノード、OAuth issuer、公開 MCP origin を変更してはいけません。

## 7. 最終受け入れ

読み取り専用のチェックは、インストール手順ごとに同じ確認を繰り返すのではなく、最後の検証 wave に一つにまとめてください。インストールは次のすべてが証明されたときにだけ完了です。

- `herdr-mcp status` / `doctor` が healthy である。
- `herdr-mcp link status` が enrollment 済み production Link のオンラインを示す。
- canonical な公開 origin が `/health` と OAuth discovery を正しく提供する。
- マシンが canonical な `dev_<ULID>` デバイス identity を持つ。
- 認証済みの実際の MCP リクエストが、公開 origin からこのワークステーションへ往復して完了する。

その後、ChatGPT の **Settings → Account security & login** を開いて **Developer mode** を有効にします。**Plugins → Browse plugins** で右上の **+ → Create APP** を選び、完全な `https://…workers.dev/mcp` を使って MCP App を作成し、OAuth を完了します。推奨例の App 名は `herdr` ですが、カスタム名も利用できます。ChatGPT の Project 内で作業し、新しいチャットの最初のメッセージでワークステーションへアクセスするとき、その App を `+` ボタンから選択または参照します。拡張が実際の provider-owned App keyword を学習し、後続の Herdr turn で再利用します。

Chrome 拡張 / Native Messaging の経路は任意であり、コア Connector の前提条件ではありません。ユーザーがブラウザの連続性、引き継ぎ、Control Center を望むときにだけ、[拡張のガイド](extension.md)からインストールしてください。拡張の配布 / 開発の詳細はそのガイドに残します。

## 8. クリーンアップと報告

`CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID` を unset し、一時的な資格情報ファイルを削除します。報告するのは秘密でない事実だけです。runtime version、デバイス名 / id（必要なら表示用に短縮）、Worker 名、最終的な公開 origin、Link 状態、`/health`、OAuth、MCP E2E の結果です。

権限、ネットワーク、OAuth、既存 Worker の ownership、mutation の配信の不確実性が進行を妨げる場合は、[手動インストールガイド](install.md)または[トラブルシューティング](troubleshooting.md)の該当セクションを開いてください。メンテナー用の runbook 全体を事前に実行コンテキストへコピーしないでください。