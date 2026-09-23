# 手動インストール

*1 台の Herdr ワークステーションから、使える Web AI 開発環境へ。*

> **位置づけ：手動 / operator 向けリファレンス。** herdr-mcp の主要なインストールプロトコルは、実行する Agent に向けて直接書かれています。詳しくは [Agent install](agent-install.md) と [Agent installation](agent-install.md) を参照してください。このページは、手動での確認、トラブルシューティング、各段階の理解のために使用します。特定の coding agent にコピーする製品固有のプロンプトは、もう提供していません。

目的は、ソースコードと実際の実行をワークステーションに置いたまま、ローカルのワークステーションを ChatGPT / Web AI に接続することです。

## インストール前の確認

### 1. Herdr が利用できること

```bash
herdr --version
herdr api schema >/dev/null
```

Herdr が入っていない場合は、公式の stable インストーラーを使用してください。

macOS / Linux:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

その後、もう一度 `herdr --version` を実行します。Herdr 本体のインストール動作については <https://herdr.dev/docs/install/> が正式な情報源です。

### 2. 必要なクライアント経路を決める

- ChatGPT / その他の公開 Web AI → Cloudflare Edge + アウトバウンドの Herdr Link;
- ローカルの MCP クライアントのみ → Cloudflare なしで loopback runtime を使用できる;
- ブラウザ拡張 → ベースの Connector が動作した後の任意機能であり、初回インストールの前提条件ではない。

## サポート対象プラットフォームの境界

<https://github.com/whshang/herdr-mcp/releases> の GitHub `Latest` stable Release を使用してください。初回デバイスの `worker bootstrap` と既存 fleet の `worker connect` は、Apple Silicon macOS と、x86_64・ARM64 双方の native Debian 系 Linux で production-qualified です。Windows x86_64 と Windows ARM64 は 1.0 candidate として公開されます。Windows x86_64 は実機で install/recovery と実際の Connector 会話まで確認済みですが、最新 main の正確な candidate に対する #394 の完全な昇格記録が揃うまでは Candidate のままです。Windows ARM64 は現在も hosted runner の qualification のみです。Windows は Herdr named pipe、Windows Credential Manager、カレントユーザーの Startup フォルダー ショートカットによる autostart、デタッチされたユーザープロセスを使用します。この managed Windows runtime が起動すると、設定された Herdr API を調べ、必要であれば既にインストール済みの `herdr server` をベストエフォートで起動します。Herdr 自体のインストールや削除は行いません。Browser Native Messaging と製品レベルの reinstall / uninstall は、Windows 実機 UAT の主張には含まれません。テスト済み / 未テストの正確な境界は [platform support matrix](platform-support-matrix.md) を参照してください。

古いインストールについては [Runtime self-upgrade](runtime-self-upgrade.md) に従ってください。runtime はその場でアップグレードします。現在の Release に移行するためだけに、健全な Worker、デバイス関係、ChatGPT Connector を作り直さないでください。

## ステップ 1：ネイティブ herdr-mcp runtime をインストールする

<https://github.com/whshang/herdr-mcp/releases> から、このプラットフォーム向けの最新 stable `herdr-mcp` binary をダウンロードし、`PATH` に置いてから次を実行します。

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

`install` は `~/.config/herdr-mcp/runtime/` 配下に不変の generation を配置し、ユーザーの PATH エントリを `runtime/current/herdr-mcp` に向けます。一般ユーザーは git clone、`npm`、`cargo` でローカル runtime をインストールしません。

Debian では、マシンのアーキテクチャに一致する静的 musl Release アセットを使用します。x86_64 なら `x86_64-unknown-linux-musl`、ARM64 なら `aarch64-unknown-linux-musl` です。Windows では、マシンのアーキテクチャに合わせて `x86_64-pc-windows-msvc` または `aarch64-pc-windows-msvc` を使用します。Linux のインストーラーは `systemd --user` を優先し、user systemd manager が存在しない場合は managed user-process backend を使用します。macOS のサービスはユーザー LaunchAgent であり、Full Disk Access は `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker` にある安定した macOS 専用 broker にのみ付与されます。Linux と Windows はこの TCC/FDA パスを使用しません。`sudo` は macOS のプライバシー権限の代わりにはなりません。

macOS では `herdr-mcp permissions status` から始めます。`needs_setup` が返る場合は `herdr-mcp permissions setup` を実行し、**System Settings → Privacy & Security → Full Disk Access** で正確にその安定した broker を有効にしてから、`herdr-mcp permissions verify` を実行します。macOS のプロンプトを引き出すためだけに、setup の前に `~/Documents` を調べないでください。host-capable broker は、MCP の保護フォルダー向けファイル / Git ツールにおける安定した TCC クライアントです。そのリクエスト面は、保護フォルダー向けファイル / Git 操作の固定 allowlist（`fs_read`、`fs_list`、`fs_grep`、`fs_image`、`fs_edit`、`fs_write`、`fs_patch`、`git`）であり、任意の shell / exec 面ではありません。native Herdr の pane と Agent は、それぞれの実行ホストの TCC identity に従います。managed `herdr server` はこの host-capable broker を通して起動されるため、その配下の pane と Agent は broker の identity を再利用します。一方、この managed パスの外で起動された Herdr ホストは、そのホスト自身の TCC 境界を保ちます。したがって通常の初回インストールでは、プロセスごとに個別承認するのではなく、Full Disk Access の承認が 1 回必要です。通常の runtime 更新はこの broker を保持します。`permissions setup --upgrade-broker` は明示的な互換 migration にのみ使用してください。macOS は別途、安定した `herdr-mcp-credential-helper` による Keychain アクセスを 1 回だけ求めることがあります。この一度きりのプロンプトを承認し、更新をまたいで helper を安定させてください。プラットフォームの詳細は [CLI reference](cli-reference.md) と [Troubleshooting](troubleshooting.md) にあります。ローカルの doctor が不健全なうちは、公開 Edge を追加しないでください。

### 任意：高速セマンティック判断

ローカル runtime が正常になった後、高速 decision モデルを任意で設定できます。`herdr-mcp install` はこの機能を案内しますが、設定を必須にはしません。TypeSafe.ai は汎用 `decision` プロトコルを利用できるため推奨設定例ですが、製品依存ではありません。この例を使う場合は <https://typesafe.ai/> で登録して API Key を作成し、次の setup コマンドを実行します。

```bash
herdr-mcp semantic setup
herdr-mcp semantic status
```

setup は TypeSafe.ai / `jev-latest` を既定の参考値として使うだけで、`--name`、`--protocol`、`--url`、`--model` で上書きできます。API Key は非表示の端末入力から読み取り、typed-decision route は保存前に実際の検証を行います。ローカル route の資格情報は mode-`0600` の runtime 設定にだけ保存されます。semantic 設定を省略しても、従来の決定論的 runtime 経路は変わりません。

## ステップ 2：公開 Edge をデプロイする

Cloudflare Workers Free は Herdr にとって十分であり、支払い方法は必要ありません。Cloudflare アカウントがない場合は、サインインページで無料アカウントを作成します。Google サインインが最短の推奨経路です。

ChatGPT がインターネット経由でワークステーションに到達する必要がある場合は、Cloudflare Worker を安定した OAuth/MCP の入口として使用します。`workers.dev` はゼロドメインの bootstrap / 診断 origin として有効なままにしておきますが、選択した Cloudflare Account に適した active zone が既にある場合は、Connector の認可より前に `herdr-mcp.example.com` のような専用 Custom Domain を長期の OAuth/MCP identity として優先します。適した zone がない、またはユーザーが希望しない場合は、インストールを妨げることなく `workers.dev` で続行します。

自動インストールの場合、実行する Agent は [Agent install](agent-install.md) / [Agent installation](agent-install.md) に直接従います。Token のスコープ、Worker の命名、secret の注入、アカウントの選択、ネットワーク blocker の境界は、これらのプロトコルが担います。

手動 / operator によるデプロイでも、インストール済み runtime を使用します。

```bash
herdr-mcp worker bootstrap
```

このコマンドは、Worker の命名、Release artifact の検証、Cloudflare API への直接アップロード、secret、初回デバイスの enrollment、readiness の検証を担います。信頼済み DNS の回復を含む runtime では、新しい `workers.dev` hostname がローカルで解決できない場合、bootstrap は Cloudflare DNS、続いて Google DNS を試し、返されたアドレスを実際の TLS `/health` contract で検証し、そのうえで初めてその hostname の Herdr マーク付きマッピングをシステム hosts ファイルに永続化しようとします。Unix では `sudo` を要求することがあり、hosts ファイルが書き込み可能でない Windows では管理者権限のターミナルが必要です。マッピングの永続化に失敗しても、既に検証済みのプロセス内 bootstrap 経路は無効になりません。通常のインストールには、ソース checkout、Node.js、npm、Wrangler、`wrangler.user.toml` は不要です。

次の制約を守ってください。

- Cloudflare API Token は一時的なプロセス値であり、リポジトリやログの値ではありません;
- `workers.dev` はゼロドメインの bootstrap origin として維持してください。適した active zone がある場合は、汎用の DNS Write を使うのではなく、Connector の認可より前に Worker Custom Domain を確定させてください;
- `LINK_SHARED_SECRET` は Worker secret として保持してください;
- ワークステーションはアウトバウンドの認証済み WSS を確立し、公開ローカルポートを公開しません。

通常の bootstrap contract については [Agent-assisted installation](agent-install.md) を参照してください。[Cloudflare Edge deployment](cloudflare-edge-deployment.md) は、source / Wrangler のワークフローを maintainer と深い運用のリファレンスとしてのみ保持しています。


### v0.4.8 の `workers.dev` DNS 回復

v0.4.8 は、信頼済み DNS の自動的な直接永続化より前のものです。bootstrap がすでに Worker を作成していて、その `workers.dev` hostname がローカルで解決されないために失敗する場合は、再開可能な bootstrap を再実行する前に次を行ってください。初回の enrollment で公開 Relay に切り替えないでください。

```bash
EDGE_ORIGIN="https://<worker>.<account-subdomain>.workers.dev"
HOST="${EDGE_ORIGIN#https://}"; HOST="${HOST%%/*}"

# まず Cloudflare DoH を試す。その hostname もローカルで解決できない場合は、
# 同じリクエストに --resolve cloudflare-dns.com:443:1.1.1.1、次に 1.0.0.1 を付けて再試行する。
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://cloudflare-dns.com/dns-query?name=${HOST}&type=A"

# Cloudflare DoH が使えない場合は Google DoH を試す。同じ固定形式で
# dns.google:443:8.8.8.8、次に 8.8.4.4 を使用できる。
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://dns.google/resolve?name=${HOST}&type=A"
```

返された IPv4 の `A` アドレスを 1 つ `IP` として選び、hosts に触る前にそのアドレスを検証します。

```bash
curl --noproxy '*' -fsS --resolve "${HOST}:443:${IP}" "${EDGE_ORIGIN}/health"
```

TLS の hostname 検証が成功し、`/health` が期待どおりの Herdr Worker / contract を識別した場合にのみ続行します。まず `/etc/hosts` を確認してください。管理されていないエントリがすでにこの hostname を指している場合は、上書きせずに停止します。そうでない場合は、マーク付きのマッピングを正確に 1 つ追加し、`sudo` は自分で承認してから bootstrap を再実行します。

```bash
grep -n "${HOST}" /etc/hosts || true
printf '%s	%s	# herdr-mcp workers.dev %s
' "$IP" "$HOST" "$HOST" | sudo tee -a /etc/hosts >/dev/null
herdr-mcp worker bootstrap
```

これは 1 つの Worker hostname だけを変更します。システムの DNS server、プロキシ、ネットワークノード、OAuth issuer、MCP の公開 origin は変更しません。自動回復を含む後の runtime は、このマッピングが古くなったときに自身のマーク付きエントリを更新できます。

## ステップ 3：Herdr Link を検証する

```bash
herdr-mcp doctor
herdr-mcp link status
```

`workers.dev` への直接アクセスが失敗する場合は、まず `worker bootstrap` / `worker connect` に上記の検証済み単一 hostname マッピングで DNS を修復させてから、直接 Link を再試行します。直接 transport がまだ失敗する場合にのみ既存のローカルプロキシを再利用し、組み込みの署名付き共有 Relay は最後のフォールバックとして保持します。この hostname を修復するためだけに Worker を再デプロイしたり、システム DNS を変更したりしないでください。`doctor` と `link status` で検証します。

## ステップ 4：公開パスを検証する

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

認証されていない `/mcp` の応答が `401` であっても正しい場合があります。有用な確認項目は、ローカル runtime が健全であること、Link が接続されていること、Edge の `/health` に到達できること、OAuth metadata に到達できることです。

## ステップ 5：ChatGPT に herdr Connector を追加する

これは人手による手順です。coding agent は一旦停止してユーザーを案内する必要があります。

1. ChatGPT の **Settings → Account security & login** を開き、**Developer mode** を有効にします;
2. **Plugins → Browse plugins** を開き、右上の **+ → Create APP** から MCP App を作成します。推奨例の名前は `herdr` ですが、カスタム App 名も利用できます;
3. デプロイ済みの完全な `${MCP_URL}` を貼り付けます。末尾の `/mcp` も含めます;
4. ブラウザで OAuth を完了します。承認ページはブラウザの言語から中国語・英語・日本語を選択し、6 桁のコードを入力する前にターミナルの承認コマンドを実行するよう求めます;
5. **Project** を作成または開き、そこで作業します;
6. 新しいチャットでは毎回、最初のメッセージでコンポーザーの `+` ボタンから `herdr` を参照し、その会話でプラグインを有効にします。

次に読み取り専用のテストを行います。

```text
私の Herdr プロジェクトを確認してください。読み取り専用で、何も変更しないでください。
```

`herdr_inspect` が実際のワークステーションのデータを返せば、基本ループは使用可能です。

[ChatGPT Connector](chatgpt-connector.md) を参照してください。

## ステップ 6：continuity が必要な場合にのみブラウザ拡張を追加する

ブラウザ拡張は、Side Panel Control Center、workspace binding、長い会話の continuity、キューされた次ターンのメッセージを追加します。基本の MCP ループには必要ありません。

拡張には現在 3 つの identity があります。**STORE / STANDALONE / DEV** です。STORE は通常ユーザー向け、STANDALONE は固定 identity の GitHub / 手動配布向け、DEV はソース開発専用です。

- STORE：一般ユーザーのデフォルト経路。固定の Chrome Web Store identity と Store 経由の更新;
- STANDALONE：独立 / GitHub 配布向けの固定非 Store identity;
- DEV：ソース開発専用。リポジトリ / worktree の `extension/` から Load unpacked し、パス由来の identity を持つ。

サポートされているチャネルをインストール / 選択した後に次を実行します。

```bash
herdr-mcp native-host status
```

active channel、extension identity、Native Host runtime generation が意図したインストールと一致していることを確認してください。DEV を一般ユーザーのフォールバックとして使用しないでください。また、GitHub / 手動配布の固定 identity パッケージを "dev" と呼ばないでください。

[Browser extension](extension.md) と [Browser Control Center](browser-control-center.md) を参照してください。

## 「インストール済み」とは何を意味するか

最低限、次を満たすことです。

- `herdr --version` が動作する;
- `herdr-mcp doctor` が健全である;
- Herdr Link が接続されている;
- Edge の `/health` に到達できる;
- ChatGPT の OAuth が完了している;
- 新しい会話が実際のワークステーションに対して `herdr_inspect` を呼び出せる;
- 任意の拡張がインストールされている場合、`herdr-mcp native-host status` が健全で、Control Center が workspace を認識できる。

## 自動実行の入口

自動インストールの場合、Agent は [Agent install](agent-install.md) を直接読みます。権限、セキュリティ、失敗境界の完全な contract が必要な場合は [Agent installation](agent-install.md) を使用してください。

必要な場合にのみ深く読んでください。

- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge deployment](cloudflare-edge-deployment.md)

maintainer の UAT、GA gate、リリースエビデンスは、通常のユーザー向けインストールフローの外に意図的に置かれています。

## 修復・再インストール・アンインストール

launchd ファイルや runtime ディレクトリを手動で削除するのではなく、製品レベルの lifecycle コマンドを使用してください。

```bash
herdr-mcp reinstall
herdr-mcp uninstall
```

macOS では、`reinstall` は設定と資格情報を保持したまま、managed Rust runtime を修復 / 置換します。Linux では runtime の修復に `herdr-mcp install` を、明示的なサービス削除に `herdr-mcp service uninstall` を使用します。完全な product uninstall は macOS の lifecycle 統合のままです。macOS では generation は通常の service GC に従い、active / rollback-safe な集合が保持されます。`uninstall` は、強く ownership が確認されたローカルの herdr-mcp runtime / config 状態を削除します。デフォルトインスタンスは自身の service、所有する日次の auto-update scheduler、Link / watchdog、Native Messaging host、managed CLI link、config root を対象とし、名前付きインスタンスは自身の service / watchdog / config のみを削除します。製品の uninstall は、teardown の前に、config root の外にあるユーザー cache へ小さな durable な update-fence tombstone を配置するため、キューされたサイレント updater が config ディレクトリ消失後に削除済みサービスを復活させることはできません。この tombstone は、明示的で成功した install / reinstall によってのみクリアされます。また、Herdr 本体（`herdr`、Herdr の service / socket / config）や、ブラウザ / Cloudflare / Keychain / TCC が個別に管理する認可状態を意図的に保持します。これらの lifecycle mutation は、変更対象の service に依存する managed `herdr_exec` セッション内ではなく、独立したターミナルから実行してください。
