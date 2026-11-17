# Agent インストール

*コーディング Agent の最小実行契約。詳細は[手動インストールガイド](install.md)、障害は[トラブルシューティング](troubleshooting.md)。*

> **実行ロール：Agent。** このページを読み、そのまま実行する。ブロッカー時だけ詳細ガイドを開く。ユーザーが担当するのはサインイン、システム承認、Cloudflare Token の作成 / アカウントやドメインの選択、ChatGPT OAuth。

## 1. 実行ルール

1. **ツールを呼ぶ前に計画する。** 最初の mutation 前に、マシン / fleet 状態、人の境界、依存関係、最終受け入れを確定する。独立した読み取りは一度に発行し、同じ安全境界の決定的な shell / Git 作業は一つの有界な実行呼び出しにまとめる。再計画は、結果が判断を変えるときだけ。
2. **既存作業を保つ。** `reset --hard`、`clean -fd`、無関係な dirty ファイルの上書き、fleet の再構築は禁止。
3. **Stable GitHub Release を使う。** 明示的なソース開発以外は clone、`npm`、`cargo` ビルドをしない。
4. **秘密は一時的に保つ。** Cloudflare Token を echo せず、Git、ログ、shell history、ChatGPT に入れない。環境または CLI の隠し入力だけで渡す。`HERDR_MCP_TOKEN` も同様。
5. **ユーザーのネットワーク環境を変更しない。** プロキシ、システム DNS、汎用プロキシを変更しない。例外は §6 の検証済み単一 Worker hosts。
6. **人の境界でだけ止まる。** 自動化できる手順は続ける。Cloudflare ログイン、macOS 権限、Account / zone 選択、ChatGPT OAuth が必要なときだけ尋ねる。

## 2. 最初に判断する：最初の Worker か既存 fleet か

この順で判断し、反復質問しない。

- pairing アドレスがすでに提示されている：このマシンは**既存の Herdr Worker に参加する**。§5 へ進みます。
- このマシンがすでに enrollment 済み：既存 fleet を保ち、別の Worker を作らずに修復 / 検証します。
- それ以外は**最初の Worker**。Token が使えたら `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts` を列挙し、Herdr Worker があれば新規 mutation を止め、enrollment 済みデバイスからの pairing を要求して §5 へ。
- 現在インストール中のコンピュータで、fleet 検出の探査として `herdr-mcp worker pair` を実行してはいけません。
- ユーザーが fleet の意図を変えない限り、ランダムサフィックス付きの Worker、二つ目の R2 バケット、二つ目の Connector にフォールバックしない。

## 3. ローカルインストール段階

`herdr` がない、または Server が未起動という理由だけで止めない。<https://github.com/whshang/herdr-mcp/releases> から **Latest stable** の herdr-mcp バイナリを `PATH`（通常 `~/.local/bin/herdr-mcp`）に置き、次を実行する。

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

`herdr-mcp install` が Herdr 依存関係の復旧を担当する：`HERDR_BIN`、安定パス、Homebrew、PATH を解決し、既存 Herdr を検証・更新、なければ <https://herdr.dev/tui/> の installer を実行して Server/API を確認する。失敗報告がない限り別手順を作らない。「Herdr」は Herdr TUI（`herdr`）で、Herdr app や herdr-core（`hc`）は入れない（v1.1 非対応）。Windows UAT は exact candidate artifact。

`~/.local/bin/herdr-mcp` はあるのに shell が解決できない場合は `installed_but_not_on_shell_path` と分類し、PATH を修復して検証する。再インストールや二つ目の PATH owner は作らない。

macOS は Cloudflare の作業の前に `herdr-mcp permissions status` を実行。`needs_setup` の時だけ安定 broker に Full Disk Access を付与し `herdr-mcp permissions verify`。保護パスを探査せず `sudo` も使わない。broker は MCP ファイル/Git の TCC のみ。Linux は user-service / process backend、Windows は Startup フォルダ + Credential Manager。Node.js 等は不要。

### 任意：高速セマンティック判断

コア runtime が正常になった後、高速 decision モデルは任意であることを案内する。推奨例は [TypeSafe.ai](https://typesafe.ai/)（必須ではなく、インストールを妨げない）。ユーザーは API Key を作成し、次を実行できる。

```bash
herdr-mcp semantic setup
herdr-mcp semantic status
```

`semantic setup` は API Key を非表示入力から読む。他の Provider は `--name`、`--protocol`、`--url`、`--model` で設定可能。省略しても決定論的動作を維持する。

## 4. 最初の Worker：Cloudflare + bootstrap

Herdr は Cloudflare Workers Free で十分、支払い方法は不要。アカウントがなければ Google サインインを勧める。Token は <https://dash.cloudflare.com/profile/api-tokens> の **Edit Cloudflare Workers** を選択 Account に。カスタム Token には **Account Settings → Read** と **Workers Scripts → Write/Edit**。`workers/subdomain` が 403 なら不足権限を報告し、Token のスコープを広げない。**コアインストールに R2 は不要**（Workers R2 Storage は artifact relay 時のみ追加）。

Token は `CLOUDFLARE_API_TOKEN` または `worker bootstrap` の隠し入力だけで渡し、コマンド、設定、ログに書かない。

実行：

```bash
herdr-mcp worker bootstrap
```

このコマンドは Cloudflare API 事前チェック、subdomain 解決、既存 Worker 検出、release manifest と `herdr-edge-<version>.mjs` artifact attestation、Worker/DO bootstrap、デバイス enrollment、資格情報、Link 整合を担う。Wrangler もソース checkout も不要。

公開 origin は Connector 作成前に一度選ぶ。
- 適した active zone が一つなら `https://herdr-mcp.example.com/mcp` の Custom Domain。
- 複数なら一度だけユーザーに選択を求める。
- 不適切・拒否・衝突なら `workers.dev`（例 `https://herdr-edge-device.username.workers.dev/mcp`）。

Custom Domain に汎用 DNS Write は不要。復旧で OAuth/MCP 公開 origin を変更しない。

## 5. 既存の Worker に参加する

enrollment 済みデバイスが pairing を作り、新マシンは消費する。

```bash
herdr-mcp worker connect "<pairing-address>"
```

6 桁の検証コードは CLI が要求したときだけ尋ねる。表示名は既定でコンピュータ名。`--name` はユーザーが明示したときだけ。この経路は別 Worker をデプロイせず、別 Connector を作らず、fleet の長期秘密を新マシンへコピーしない。

enrollment 後の identity は不変の `device_id`（`dev_` + 26 文字 ULID、例 `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`）。表示名とは別管理。hostname から `WORKSTATION_ID` をでっち上げてはいけません。

詳細は[既存 fleet への参加](existing-worker-connect.md)。

## 6. Link とネットワーク

`worker bootstrap` / `worker connect` はデバイス資格情報と production Link を整合する。Agent は次を検証する。

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Custom Domain がなければ `workers.dev` を試す。DNS 失敗時は Cloudflare DNS、Google DNS の順で照会し、TLS `/health` を通過した候補だけが hosts エントリになる。Unix は `sudo`、Windows は管理者ターミナルが必要なことがある。次に Link 直接、既存プロキシ、最後に共有 Relay。システム DNS、OAuth issuer、公開 MCP origin は変えない。

## 7. 最終受け入れ

読み取り専用チェックを最後にまとめ、次の全項目を証明して完了とする。

- `herdr-mcp status` / `doctor` が healthy である。
- `herdr-mcp link status` が enrollment 済み production Link のオンラインを示す。
- canonical な公開 origin が `/health` と OAuth discovery を正しく提供する。
- マシンが canonical な `dev_<ULID>` デバイス identity を持つ。
- 認証済みの実際の MCP リクエストが、公開 origin からこのワークステーションへ往復して完了する。

ChatGPT の **Settings → Apps & Connectors**（旧 **Plugins → Browse plugins**）で **Developer mode** を有効にし、**+** ボタン → **Create APP** から `https://…workers.dev/mcp` で App を作り OAuth を完了する。推奨名 `herdr`。ChatGPT の Project 内で作業し、新しいチャットの最初のメッセージで `+` ボタンから App を選ぶ。

Chrome 拡張 / Native Messaging は任意。Control Center、次ターン予約、引き継ぎ、マルチ WebChat（`ChatGPT`、`Claude`、`Grok`、`Doubao`）、`browser-page`、ブラウザスキルが必要な場合だけ、[拡張ガイド](extension.md)と[WebChat ガイド](local-agent-webchat-control.md)に従いストア導入または `herdr-mcp extension standalone install`。

## 8. クリーンアップ、更新と報告

`CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID` を unset し一時資格情報を削除。秘密でない事実（runtime version、device id、Worker 名、origin、Link 状態、OAuth、E2E 結果）のみ報告。v1.1 の更新は `herdr-mcp update`（[自己更新ガイド](runtime-self-upgrade.md)）。

権限、ネットワーク、OAuth、Worker ownership、配信の不確実性で止まる場合は、[手動インストールガイド](install.md)または[トラブルシューティング](troubleshooting.md)の該当節だけを開く。
