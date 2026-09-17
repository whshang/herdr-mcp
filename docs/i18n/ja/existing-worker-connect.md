# マルチデバイス制御

*一つの Herdr Worker と一つの ChatGPT 接続で、enrollment 済みの複数のコンピュータを制御する。*

Herdr の fleet は、一つの公開 Worker/Connector と、その背後にある独立した identity を持つ複数のコンピュータで構成されます。ChatGPT は fleet を検出し、タスクのためにデバイスを選び、後続の操作をそのデバイスに結び付けたままにできます。新しいコンピュータは短命な pairing を通じて既存の Worker に参加します。別の Worker をデプロイしたり、共有のグローバル秘密を受け取ったりはしません。

Herdr 0.9.1 には、マルチマシン TUI 用の独自の保存済み SSH マシン層と、ネイティブの `herdr --machine <label-or-id> <command>` 転送もあります。この層は、同じ物理コンピュータ上であってもこの Edge fleet と共存できますが、`device_id` ルーティングを置き換えるものではありません。identity、ルーティング、フェイルオーバーの規則は [Herdr 0.9.1 のマルチマシンとデュアルパス制御](multi-machine-control.md)を参照してください。

> v0.4.8 は macOS と x86_64 Linux/Debian で安全な新規デバイス pairing をサポートします。macOS は最終的なデバイス資格情報を Keychain に保持します。Linux は `0700` のディレクトリと `0600` の通常の資格情報ファイルを使う、ユーザー専用の資格情報ストアを使います。Windows の pairing は引き続き利用できず、fail closed です。

## ChatGPT から fleet を見る

`herdr_devices` を使って、Worker が把握しているデバイスを列挙します。結果には、安定したデバイス identity に加えて、現在の認可、接続、スケジューリング、ヘルスの情報が含まれます。

有用なプロンプトの例です。

```text
私の Herdr デバイスを一覧し、どれがオンラインかを示してください。バックエンドのタスクには macbook-main を、独立したテストタスクには macbook-lab を使ってください。両者の working tree は分離したまま保ち、完了を報告する前に両方の結果を検証してください。
```

ルーティングは意図的に保守的です。

- 明示的に指名されたデバイスは、その操作に使われます。
- 後続の参照と再試行は、元のデバイス identity を保ちます。
- ルーティング可能なデバイスが一つだけなら、Herdr はそれを自動的に選べます。
- mutation に対して複数のデバイスが有効な候補で、対象が指定されていない場合、Herdr は推測せず `device_ambiguous` を返します。

enrollment 済みの各コンピュータは、独自の資格情報と不変の `device_id` を持ちます。デバイス名は人が使いやすいセレクタであり、基盤となる identity は安定したままです。

## 新しいコンピュータを追加する

### 1. enrollment 済みデバイスから pairing を作成する

すでに fleet に enrollment されている任意のコンピュータで、次を実行します。

```bash
herdr-mcp worker pair
```

`worker pair` はデバイス / オペレーターによる fleet アクションです。このマシンが対象 Worker にすでに enrollment されていることを証明する資格情報を必要とします。操作をワークステーション経由でルーティングせず、Worker の control plane で pairing を作成します。応答は次の情報をまとめて示すはずです。

- 高エントロピーの pairing id を含む pairing アドレス。
- 単回使用の 6 桁検証コード。
- 正確な有効期限。
- コピー可能な `herdr-mcp worker connect "<pairing-address>"` コマンド。

通常の最大 TTL は 600 秒です。pairing は永続的な招待として扱わず、ただちに使ってください。検出の探査として、新しいコンピュータで `worker pair` を実行してはいけません。これが最初の Herdr Worker で、enrollment 済みデバイスがまだ存在しない場合は、pairing の前に最初の Worker の Cloudflare bootstrap を完了してください。

### 2. 新しいコンピュータを接続する

新しいコンピュータでは、Agent が次を実行します。

```bash
herdr-mcp worker connect "<pairing-address>"
```

このコンピュータが同じ Worker にまだ enrollment されていない場合、CLI は通常の可視のターミナル入力として 6 桁コードを求めます。入力した内容を確認できるようにするためです。このコードは通常のコマンドライン引数としては意図的に受け付けられないため、shell history には残りません。

`worker connect` は、**同じ Worker** にすでに enrollment されたデバイスに対して冪等です。ローカルの永続設定がその Worker の既存の `device_id` を特定し、Worker の inventory がそのデバイスを今も `active` と確認できる場合、Herdr はその enrollment を再利用します。6 桁コードを求めず、新しい pairing を消費せず、デバイス identity を作成または上書きしません。ローカルに同じ Worker の enrollment があるが遠隔ではすでに active でない場合、コマンドは黙って二つ目の identity を作らずに fail closed します。別の Worker 向けの pairing は、引き続き明示的な pairing 経路に従います。

既定では、参加するコンピュータがプラットフォームの報告するコンピュータ名 / hostname をデバイス表示名として登録します。ユーザーが明示的に別の初期名を望むときだけ `--name "<device-name>"` を使ってください。pairing の作成者が指定した `worker pair --name ...` の値も明示的な上書きであり、優先されます。

pairing が消費された後、`worker connect` はローカルサービスをインストール / 起動し、enrollment 済みの Rust production Link を整合させます。macOS は launchd を使います。Linux は `systemd --user` を優先し、ユーザーの systemd manager がない場合は管理対象のユーザープロセス backend を使います。コマンドが成功するのは、ローカルサービスと Link が healthy になった後だけです。起動に失敗すると、未完了の enrollment を取り消し、ローカルの資格情報 / 設定の状態を復元します。

Agent 支援のセットアップでは、新しいコンピュータにこの一文を貼り付けてください。

```text
このコンピュータを私の既存の Herdr fleet に接続してください。手順は https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/ja/existing-worker-connect.md に従ってください。pairing アドレスは <pairing-address> です。6 桁の検証コードは CLI が要求したときだけ私に尋ね、その後このデバイスが同じ Worker でオンラインになっていることを確認してください。
```

### 3. 新しいデバイスを検証する

接続が成功した後で、次を実行します。

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

そして ChatGPT に `herdr_devices` を呼ばせ、新しいデバイスが同じ Worker の下でオンラインであることを確認します。

このコンピュータが `workers.dev` に直接到達できない場合も、同じ enrollment を保ってください。`link status` は、対応しているローカルプロキシまたは共有 Relay の経路を示すことがあります。関連する結果は Link が healthy であることです。Link が healthy にならないときだけ[トラブルシューティング](troubleshooting.md)を参照してください。

現在 enrollment されているコンピュータを後から明示的に改名するには、次を実行します。

```bash
herdr-mcp worker rename "<new-device-name>"
```

`herdr-mcp device rename ...` は同等のエイリアスです。改名が変更するのは人向けの表示名だけであり、不変の `device_id`、ワークステーション identity、資格情報、認可、スケジューリング状態は変わりません。Link の再接続が明示的な改名を上書きすることはありません。既定 / 旧来のワークステーションも、最初の登録時にローカルの Computer Name を記録します。

別の enrollment 済みデバイスから認可を恒久的に削除するには、任意の enrollment 済みワークステーションで次を実行します。まず `herdr_devices` から不変の `device_id` を取得し、その後に実行してください。

```bash
herdr-mcp worker revoke "<device-id>" --confirm
```

fleet の管理はデバイス / オペレーターが所有します。この操作は表示名を決して受け付けません。不変の `device_id` を使う必要があります。承認された WebChat Connector は通常の MCP のみであり、デバイスを revoke できません。enrollment 済みのデバイスはすべて、単一のオペレーターが所有する control plane の下でのピアです。デバイス間に owner / member の階層はありません。

revoke は、そのデバイス identity と資格情報に対して恒久的です。live な Link は切断され、古い資格情報は二度と再接続できません。復活を防ぐため、revoked tombstone は内部に保持されます。revoked tombstone は通常の fleet / デバイス一覧からは隠されます。後でそのコンピュータを再度追加するには、新しい pairing を作成し、新しいデバイス identity として enrollment してください。

## pairing が変えるもの

短命な pairing は、新しいデバイス単位の資格情報と交換されます。macOS は最終的な資格情報を Keychain に保存し、Linux は上記のユーザー専用の資格情報ストアに保存します。Worker はそのデバイスの認証に必要な verifier だけを保存します。pairing セッションは消費に成功した後は使用できなくなります。

参加するコンピュータは次を**必要としません**。

- Cloudflare のデプロイ資格情報
- 新しい Worker や Durable Object のデプロイ
- 新しい ChatGPT Connector / OAuth クライアント
- 旧来のグローバル `LINK_SHARED_SECRET`

## pairing のセキュリティ

- 6 桁コードは単回使用で、有効期間が短い。
- 誤ったコード入力が 5 回でその pairing セッションは恒久的にロックされます。無期限に再試行せず、新しいものを作成してください。
- pairing id は高エントロピーで、通常の HTTP access log のパスに置かれないよう URL フラグメントに留まります。
- 最終的なデバイス単位の資格情報を表示またはコピーしないでください。それは OS の資格情報ストアに属します。

## 復旧

mutation が不確実な配信を報告した場合は、再試行の前に現在の状態を確認してください。配信がすでに起きているかもしれない操作を、盲目的に繰り返さないでください。

サーバーが pairing を消費した後に接続が失敗した場合は、組み込みの compensation / revoke の挙動に頼り、結果として得られた状態を確認してください。新しい pairing は、前回の試行が使用できないと確認できた後にだけ作成します。
