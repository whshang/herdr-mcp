# CLI リファレンス

*herdr-mcp のローカル管理と運用。*

本ページが扱うのは **herdr-mcp** のコマンドだけです。Herdr 本体の workspace、ペイン、agent、session のコマンドについては公式の [Herdr CLI reference](https://herdr.dev/docs/cli-reference/) を参照してください。

## 日常の runtime 管理

通常のユーザー経路は、GitHub Release からインストールしたネイティブ Rust CLI です:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp permissions status
herdr-mcp update
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update major-apply
herdr-mcp update major-rollback
herdr-mcp update auto
herdr-mcp update status
herdr-mcp worker update
herdr-mcp profile check --file ./workstation-profile.json
herdr-mcp rollback
herdr-mcp reinstall
herdr-mcp uninstall
```

v0.4.3 以降、`update` は通常のワンステップのユーザーアップグレードであり、`update apply` と等価です。インストール済みの v0.4.2 binary は、引数なしの `herdr-mcp update` を依然として読み取り専用チェックとして扱い、`herdr-mcp update apply` を `next_action` として報告します。そのため v0.4.2 から跨ぐ際は一度だけ `herdr-mcp update apply` を使用してください。`update check` は、明示的に読み取り専用の可用性/provenance チェックを行いたい場合にのみ使用します。

対話的な `update` / `update apply` では、CLI は最終的な機械可読結果を stdout に残し、人間向けの進捗を stderr に書き出します。進捗には Release/provenance の検出、artifact attestation、上限付きのダウンロード進捗（パーセント表示）、candidate の検証、installer の state、health gate を通過した最終結果が含まれます。ダウンロードされた candidate の activation は引き続き detached update worker が実行し、フォアグラウンドの CLI は自ら lifecycle mutation を行うのではなく、上限付きの時間だけ durable update job を観測します。同 contract の Runtime 更新がコミットされた後、対話コマンドはユーザーが設定した Cloudflare Worker も確認します。その `edgeVersion` が検証済み Release より遅れている場合、Herdr は一時的な Cloudflare 認可を取得し、health で証明済みのまさにその Worker をその場で更新します。このとき Worker identity、Durable Objects、secrets、public OAuth origin、登録済みデバイス、Connectors、既知の任意の artifact R2 binding は維持され、DNS は決して変更しません。Cloudflare 認可または Worker の reconcile を完了できない場合でも、Runtime は更新済みのままとなり、結果は `next_action: herdr-mcp worker update` を報告します。Runtime がすでに最新の状態で `herdr-mcp update` を再度実行した場合、Runtime を再インストールするのではなく、この保留中の Worker reconcile だけを実行します。インストールが正規の処理として上限付きのフォアグラウンド観測区間を超えた場合、コマンドは `update_queued` の結果を `next_action: herdr-mcp update status` とともに返します。新しい Runtime が有効になると、`update status` が Worker drift を報告します。`update auto` は引き続き非対話で、Cloudflare 認可を開くことはありません。そのため Edge が古い場合、次のアクションとして `herdr-mcp worker update` を報告します。不変の v0.4.2 binary はこの進捗 UI より前のものであるため、一度だけの `update apply` はネットワーク処理を行っている間も無出力のままになることがあります。

`herdr-mcp worker update` は、既存 fleet に対する直接的な Edge reconcile コマンドです。通常のユーザー経路では enrollment 済み default instance の PROD Runtime と一時的な Cloudflare 認可が必要です。fleet 管理 preflight は、正確に固定された hash の epoch-2 rollback contract を識別できるため、すでに enrollment 済みの旧 Worker をその場で更新できます。一方、Link/install のデータプレーン admission は current/N-1 のままで、Worker の更新完了までは fail closed を維持します。正確な DEV/UAT candidate では、`HERDR_MCP_EDGE_BUNDLE_PATH` が同じ qualified artifact 内の Edge bundle を明示的に指す場合にのみ実行できます。Edge downgrade や未知の contract hash は拒否し、未知/カスタムの Worker binding を捨てることなく fail closed します。2 つ目の Worker を作成したり、デバイス/Connector の資格情報をローテーションしたり、OAuth/DNS identity を変更したり、ChatGPT Connector の削除と再追加を要求したりしません。

`update apply` は互換性のない durable-state schema を意図的に拒否します。v0.4.8（schema 5）→ v1.0（schema 15）の移行では、独立したターミナルから、検証済み v1.0 binary の `update major-apply` コマンドを使用します。このコマンドはまず実行中の移行元サービスを停止して、最終スナップショットにシャットダウン中にコミットされた書き込みが含まれるようにし、次にプライベートで一貫性のある schema-5 SQLite バックアップと、正確に同じ旧 runtime binary のプライベートな実行可能コピーを作成し、両方の rollback artifact をハッシュ化してから、通常のトランザクショナルな service install を再利用して移行と activation を行います。バックアップに失敗した場合は移行元サービスの再起動を試み、再起動の失敗があれば報告します。また、既存の rollback 材料は停止の前に拒否されるため、リトライが以前のバックアップを上書きすることはありません。`update major-rollback` は rollback 記録と移行元 binary を検証し、稼働中のサービスを停止してから、データベースバックアップのハッシュを検証してアップグレード前のまさにそのデータベースを復元し、バックアップされた旧 runtime を再インストールします。プライベートな binary コピーにより、rollback はその後の runtime generation のガベージコレクションから独立します。rollback は 5 から 15 の間のどの schema で中断された移行でも受け入れ、その後 v1.0 移行以降に作成された state を削除します。どちらのコマンドも managed `herdr_exec` セッション内での実行を拒否します。

`profile check --file` は読み取り専用のポータビリティ / drift ゲートです。Profile JSON は `schema_version: 1` を使用し、`secrets_policy: "reauthorize"` を必須とし、プロジェクトの `{id, remote, path}` エントリに加えて、`can_run_headless`、`supports_code_edit`、`supports_shell`、`supports_vision` などの agent 能力要件を宣言できます。このコマンドはそれらの宣言を現在のファイルシステム/Git origin および既存の Capability Inventory と比較するだけで、ツールのインストール、ファイルのコピー、資格情報の持ち込みは決して行いません。未知/未 probe の agent trait は推測されず drift のままとなります。レポートで inventory が利用不可と表示された場合は `herdr-mcp scan --probe` を実行してください。

`update auto` はスケジューラのエントリポイントです。デフォルトの macOS production instance では、`service install` が所有権を明確にした `dev.herdr-mcp.auto-update` LaunchAgent を reconcile します。これはロード時に実行され、その後は毎日実行されます。自動インストールは意図的に **PROD runtime + Stable Release のみ**です。コンパイルされた DEV runtime、`[update] check = false`、named instance、`preview` はいずれもネットワークアクセスの前にスキップされます。厳密に新しい Stable Release が存在する場合、コマンドは通常の provenance 検証済み detached update トランザクションを再利用し、2 つ目のダウンローダを持ち込んだり rollback ゲートを迂回したりしません。`service uninstall` はまず所有権を明確にした durable update fence を設定してスケジューラを削除し、detached worker は activation の前にその fence を再確認するため、キューに入ったサイレント更新によってサービスの削除が取り消されることはありません。明示的な成功を伴う install が fence を解除します。

macOS では、`reinstall` が製品の修復/置き換え経路です。Linux では runtime の修復に `herdr-mcp install`、明示的なサービス削除に `herdr-mcp service uninstall` を使用します。以下で説明する ownership チェック付きの完全な product-uninstall トランザクションは macOS の lifecycle 統合です。`reinstall` は設定と資格情報を保持したまま managed Rust service lifecycle を再適用し、runtime generation は引き続き通常の service GC に従って active/rollback-safe な集合を保持します。`uninstall` は**強く所有権を確認した herdr-mcp runtime/config state** の完全なクリーンアップを行います。default instance は自身の service、所有権を明確にした auto-update scheduler、Link/watchdog、Native Messaging host、managed user CLI、config root を対象とし、named instance は意図的に自身の service/watchdog/config に限定され、default の scheduler/Link/Native Host/user-CLI state の所有権を取得することはありません。default の product uninstall は、config 削除後にユーザー cache へ小さな update-fence tombstone を 1 つ意図的に残し、以前に detached された updater が service を復活させられないようにします。これを解除するのは明示的に成功した install/reinstall だけです。どちらも独立した `herdr` executable、Herdr service/socket/config、ブラウザ拡張のアカウント state、Cloudflare リソース、macOS Keychain のエントリ、TCC 認可をアンインストールまたは変更することを意図的に**行いません**。`service uninstall` はより限定された高度な service primitive のままです。

`service ...`、`link ...`、`native-host ...`、`candidate` は高度/内部コマンドです。`dev` は以下で説明する高度な**ソース開発**サーフェスです。通常の runtime インストール経路としてリポジトリの checkout、Node.js、npm、`service install` を使用しないでください。

## ソース開発 runtime: DEV / PROD

v0.4.3+ には、安定した復旧元を失わずに herdr-mcp ソースを dogfood するための明示的な経路が 1 つだけあります:

```bash
herdr-mcp dev status
herdr-mcp dev sync --dry-run
herdr-mcp dev sync
herdr-mcp dev rollback
```

- `dev status` は読み取り専用です。現在の runtime channel、active/dev/prod generation、source repo/branch/commit/dirty provenance、`runtime/current` が記録された state と一致するか、固定された PROD snapshot が検証を通過するか、および最新の実際の `dev sync` の `last_transaction` を報告します。この durable な記録には transaction ID、phase、対象ソース、expected/final generation、終端エラー、利用可能な場合の activation evidence が含まれるため、呼び出し元は self-restart の後に sync を再生することなく結果を確定できます。
- `dev sync --dry-run` は、ビルドや runtime state の切り替えを行わずに、意図されたトランザクションを表示します。
- `dev sync` はデフォルトでクリーンな source checkout を要求し、`<version>-dev` のような DEV identity をビルドし、既存の PROD binary と SHA-256 evidence を `~/.config/herdr-mcp/runtime/channels/prod/` の下に固定し、時間がかかる可能性のあるビルドの前にそのトランザクションを既存の channel state に永続化します。その後、通常のトランザクショナルな service install 経路を再利用します。Server、Native Host、`dev.herdr-mcp.link-prod` が同じ managed generation に reconcile されて初めて、トランザクションは `succeeded` として記録されます。
- `dev sync --allow-dirty` は、意図的なローカル実験のための明示的な provenance override です。これをデフォルトにしないでください。
- `dev rollback` は固定された PROD binary を検証して再インストールします。DEV sync を繰り返しても、その固定された PROD 復旧元は保持され、直前の DEV generation が PROD として扱われることはありません。

DEV/PROD の切り替えはローカルの runtime lifecycle のみです。Cloudflare Edge のデプロイ、DNS/OAuth の変更、3 つ目の永続的な test 環境の作成は行いません。Runtime の DEV/PROD は拡張の DEV/STANDALONE/STORE identity とは独立しています。

## macOS の権限

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

`status` は `granted`、`denied`、`needs_setup`、`unknown`、`timeout` のいずれかです。`setup` は可能な場合に **Full Disk Access** ペインを開きますが、アクセスを付与したとは主張しません。macOS は引き続きユーザーによる明示的な承認を必要とします。`verify` は安定した broker を介して保護されたパスを確認します。新規の対話的 `herdr-mcp install` は、サービスが開始する前にその broker を準備し、認可がまだ必要な場合は同じ Full Disk Access ペインを開きます。file/git ツールが `macos_tcc_access_blocked` を返す場合は、`herdr-mcp-broker` に Full Disk Access を付与してから再度 verify してください。

## 能力検出: `scan`

`doctor` が答えるのは **「このインストールは健全か？」** です。デフォルト出力は、人向けの短い要約としてローカルの健全性、Edge 到達性、未実行のリモート Connector 認証、対応が必要な項目だけを表示します。完全なレイヤー診断は `herdr-mcp doctor --verbose`、機械可読出力は `herdr-mcp doctor --json` を使用します。macOS の JSON `standalone_extension` オブジェクトは managed STANDALONE 拡張の状態を報告します。`state=drift` は、Google Chrome が固定の standalone extension ID を `~/.config/herdr-mcp/extensions/standalone/current` 以外のパスから読み込んでいることを意味し、そのオブジェクトには `expected_path`、実際に読み込まれたパス/profile、Chromium の `location`、`drift_count` が含まれます。ブラウザ拡張は任意であるため、この advisory が、それ以外は健全なコア MCP/service readiness を失敗に変えることはありません。`scan` が答えるのは **「このワークステーション上で実際に証拠付けられているローカル agent 能力はどれか？」** です。

```bash
herdr-mcp scan
herdr-mcp scan --json
herdr-mcp scan --probe
herdr-mcp scan --refresh --probe
```

scan は Herdr の live-agent detection を意図的に**再実装しません**。インストールされた Herdr/runtime スタックが所有する 3 つの evidence ソースを組み合わせます:

- `agent.list` は live agent インスタンス、status、ペイン/workspace、cwd の権威です;
- `server.agent_manifests` は Herdr が読み込んだ detection manifest の権威です;
- インストールされた `herdr agent start --help` の宣言は、この Herdr ビルドが起動可能と述べている agent kind を発見するために使用されます。

herdr-mcp はそれらの kind の有界な和集合を取り、対応する実行ファイルを `PATH` 上で探し、`herdr_startable`、`executable_available`、および派生した `available_for_start` を記録します。kind が新しい委譲に利用可能と見なされるのは、インストールされた Herdr がそれを起動可能と宣言して**かつ**実行ファイルが存在する場合だけです。これにより、古くなったハードコードの Agent kind リストを herdr-mcp にコピーすることなく、ワークステーションローカルの可用性を記録できます。

デフォルトの scan は、副作用の smoke テストを通過し明示的に allowlist された self-description adapter についてのみ、有界な `--version` evidence を記録します。`--probe` はさらに、agent 自身の CLI から証明できる能力について、有界で非対話の `--help` adapter を実行します。発見された binary に信頼できる adapter がなければ、インストール済みだが未 probe のままとなります。`--refresh` は Herdr の agent manifest を明示的に再読み込みし、cache された probe evidence を迂回します。

probe のサブプロセスは stdin を受け取らず、3 秒の timeout と有界な出力キャプチャを持ち、クリアされた環境から開始して非機密の runtime 変数のみを復元します。API key、bearer token、provider credential は継承されません。未対応または曖昧な trait は unknown のままで、herdr-mcp は agent 名から provider、model、vision、reasoning quality、code-edit 対応を推測しません。

静的 evidence は herdr-mcp config ディレクトリ配下の有界な capability inventory に保持されます。live status、cwd、project、ペイン、workspace、session state は常に Herdr/EventCache から取得され、inventory に置き換えられることはありません。`herdr_inspect.capability_inventory.available_agents` はデフォルトで、ローカルで利用可能な発見済み kind をすべて公開します。より狭いビューが必要な場合、`HERDR_MCP_AGENT_ALLOW` は明示的なオペレーター制限です。可用性が role を割り当てたり委譲を要求したりすることはありません。Web planner がタスク構造、live load、検証済み能力、リソース state から判断し、未知の quality/cost/latency trait は unknown のままです。

### Web planner 向けの動的プランニング助言

v0.4.3+ はワークステーションの Runtime Execution Contract を 18 tools に保ち、専用の planning tool を追加しません。`herdr_devices` が Edge ローカルであるため、公開 Edge contract は 19 actions です。progressive な `herdr_skill` bootstrap は、既存の `herdr_call` tool を経由する読み取り専用のローカルメソッドを告知します:

```text
herdr_call(
  method="herdr_mcp.planning.advise",
  params={
    "project_root":"/path/to/project",
    "requires_code_edit":true,
    "requires_shell":true,
    "independent_units":2,
    "ownership_isolated":true
  }
)
```

結果は evidence と判断を分離します: live な compatible/rejected workers、scan で証明された起動可能だが稼働していない Agent kind、直接的な deterministic option、並列化の機会、および workspace/ペイン/worktree/utility ペインのリソース事実です。このメソッドは Agent の起動、worktree の作成、worker の自動選択を決して行いません。ユーザーが明示した target は保持され、必要な能力は evidence が欠けていれば fail closed し、任意の quality/cost/latency trait は検証されなければ unknown のままです。

Web planner はその後、直接実行、既存 Agent の再利用、新しい lane の 1 つ作成、または作業が真に独立で mutation の所有権が分離されている場合にのみ並列化を選べます。既存の idle/done Agent、worktree、重複した utility ペインは再利用シグナルとして公開され、クリーンアップは完了後に planner が所有するままで、バックグラウンドのクリーンアップ daemon にはなりません。

### GitHub PR / Auto-merge の最新 status

v0.4.5 は、18-tool のワークステーション contract を変えることなく、既存の `herdr_call` tool を通じて別の読み取り専用ローカルメソッドを追加します:

```text
herdr_call(
  method="herdr_mcp.github.status",
  params={
    "project_root":"/path/to/project",
    "pr_number":284,
    "previous_fingerprint":"sha256:..."
  }
)
```

このメソッドは呼び出しのたびに、ワークステーションの認証済み `gh` CLI を通じて GitHub を読み取ります。したがって、リポジトリの Auto-merge 設定や PR/check state について、設定 mutation の直後に Connector の projection に頼るのではなく、明示的な `source=local_gh_api`、`fresh=true`、`cache_policy=bypass_connector_cache` の境界を提供します。project root は Herdr が管理する live な Git root でなければならず、その `origin` は `github.com` 上にある必要があります。

`pr_number` が指定された場合、結果には PR state、merge state、Auto-merge request、required checks、および外部デプロイなどの補足的な status が含まれます。すべての応答は決定論的な state `fingerprint` を持ちます。PR を監視している間はその値を `previous_fingerprint` として返してください。関連する変更がなければ、次の呼び出しは完全な status テーブルを再送するのではなく、簡潔な summary カウントと `changed=false` だけを返します。実行中の CI を監視する場合は `previous_fingerprint` と一緒に `wait_ms: 20000` を渡します。ワークステーション側で有界待機した後、同じ呼び出しの中で GitHub を一度だけ再確認します。変化があれば新しい状態を返し、変化がなければ `wait_timeout=true` を伴う簡潔な `changed=false` を返します。20 秒の上限は Edge の通常の request deadline と GitHub probe のための余裕を残し、planner 側の sleep + status の往復や `gh run watch` を避けます。

## Connector と Automation の資格情報

対話的な Connector は、enrollment 済みデバイス/オペレーターの制御チャネルから承認/取り消しされます。承認は通常の MCP アクセスを付与しますが、Connector を fleet principal にするものではありません:

```bash
herdr-mcp connector approve <approval-request-id>
herdr-mcp connector list
herdr-mcp connector revoke <connector-id> --confirm
```

承認コマンドは 6 桁のコードを対話的に読み取ります。そのコードを argv や shell history に置かないでください。enrollment 済みのデバイスは等価な Worker 管理チャネルであり、owner/member のデバイス階層はありません。

GitLab CI のような無人呼び出し元は、独立に revoke できる Automation Client を使用します:

```bash
herdr-mcp automation create --name "gitlab:group/project:prod" --device <device-id-or-unique-name>
herdr-mcp automation list
herdr-mcp automation rotate <svc_client_id> --confirm
herdr-mcp automation revoke <svc_client_id> --confirm
```

`create` は明示的な対象デバイスを要求し、解決された不変の `device_id` を保存します。fleet の中から暗黙に選ぶことはありません。`create` と `rotate` は `client_secret` を一度だけ表示します。CI の secret manager に直接保存してください。`list` は secret を決して返さず、バインドされたデバイスと有界な発行メタデータを含みます。Automation Client は OAuth `client_credentials` で `client_id` + `client_secret` を短期 access token と交換し、バインドされたデバイス上での通常の MCP 権限のみを持ち、fleet-admin 権限は決して持ちません。

ローカルの静的 bearer 資格情報、公開 OAuth Connector、Automation Client は別々の境界です。`HERDR_MCP_TOKEN` はローカル TCP runtime の bearer にすぎず、ChatGPT や GitLab CI に属するものではありません。

[ChatGPT Connector](chatgpt-connector.md) を参照してください。

## ヘルスとログ

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
herdr-mcp status
herdr-mcp logs
```

ローカルの HTTP `200` または `401` は runtime がリッスンしている証拠です。接続エラーはプロセス/ポートの問題を示します。

`http://127.0.0.1:8772/mcp` は loopback であっても意図的に認証されます。first-party のローカルクライアントは保護されたローカル state から runtime credential を取得できるため、ユーザーが手で貼り付ける必要はありません。生の curl/サードパーティの TCP クライアントは bearer を明示的に送信する必要があります。公式のブラウザ拡張はこの TCP credential を**使用しません**。Chromium Native Messaging は mode-`0600` の `extension.sock` trusted IPC 経路に到達し、これは tokenless で、ブラウザが渡した `Authorization` を除去します。

## Watchdog

macOS では runtime watchdog をインストールできます:

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

watchdog は herdr-mcp の可用性を保護します。一時的な Herdr TaskGroup/ExceptionGroup エラーのすべてを Herdr daemon を再起動すべき理由として扱うことはありません。

## UI 言語

```bash
herdr-mcp lang en
herdr-mcp lang zh
herdr-mcp lang ja
```

ブラウザ拡張も英語、簡体字中国語、日本語をサポートします。

## ブラウザ Native Messaging host

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

主要な経路:

```text
Chrome extension
  ↓ Native Messaging
native host
  ↓ local Unix socket
herdr-mcp runtime
```

ブラウザは Herdr bearer を保存する必要がありません。[ブラウザ拡張](extension.md) を参照してください。

## Cloudflare Edge の資格情報

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
bin/herdr-cloudflare-token --zone example.com --rotate
```

最小権限での扱いについては [Cloudflare Edge の資格情報](cloudflare-edge-token.md) を参照してください。

## Custom Domain の操作

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

従来の CNAME/Tunnel 移行のみ:

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

新しいインストールに cutover 経路は不要です。[Cloudflare Edge のデプロイ](cloudflare-edge-deployment.md) を参照してください。

## Runtime A/B

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation <id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation <id>
bin/herdr-runtime-generation rollback
bin/herdr-runtime-generation remove --generation <id>
```

Generation 管理は、同じ public contract epoch 内の実装変更のためのものです: candidate を起動 → health/contract を検証 → activate → 旧 generation を drain。[Runtime A/B](runtime-self-upgrade.md) を参照してください。

## セルフアップデート

```bash
bin/herdr-self-update
```

監視付き updater は generation の activation を再利用し、public contract epoch を黙って跨るために使用してはなりません。

## ワークステーションの link

`bin/herdr-link` は、ワークステーションから Edge への送信方向の認証済み WSS 接続を維持します。ワークステーション identity を伝搬し、リクエストを現在の active runtime generation にルーティングします。通常は手動で実行するのではなく、サービスによって管理されます。

## 共通の環境変数

| 変数 | デフォルト | 目的 |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | ローカル runtime の HTTP ポート |
| `HERDR_MCP_TOKEN` | empty | ローカル curl/Cursor 用 bearer。ChatGPT には使用しない |
| `HERDR_MCP_BASE_URL` | empty | 公開 OAuth/MCP origin。`/mcp` を含めない |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr Socket API |
| `HERDR_MCP_READONLY` | off | mutation を無効化 |
| `HERDR_MCP_WRITE_ROOTS` | managed roots | 書き込み可能なプロジェクトを制限 |
| `HERDR_MCP_AGENT_ALLOW` | all discovered agents | inspect/since での agent 可視性を任意で制限 |
| `HERDR_MCP_ALL_TOOLS` | off | 高度/互換ツールを公開 |
| `HERDR_SKILL_NETWORK` | on | `0` の場合はバンドルされた skill のみを使用 |

## クイックコマンドマップ

| 目的 | コマンド |
|---|---|
| runtime の status | `herdr-mcp status` |
| ログを追う | `herdr-mcp logs -f` |
| 現在のソースを DEV runtime として dogfood する | `herdr-mcp dev sync` |
| DEV/PROD provenance を確認する | `herdr-mcp dev status` |
| DEV から固定 PROD に戻す | `herdr-mcp dev rollback` |
| ブラウザブリッジをインストールする | `herdr-mcp native-host install` |
| Cloudflare 権限を事前確認する | `bin/herdr-cloudflare-token ... --dry-run` |
| A/B state を確認する | `bin/herdr-runtime-generation status` |
| runtime を rollback する | `bin/herdr-runtime-generation rollback` |
| Custom Domain を確認する | `bin/herdr-cloudflare-domain status` |
