# 既存の Herdr Worker に新しいコンピュータを接続する

これは**新しいコンピュータを既存の herdr-mcp Worker/Connector に接続する**ための権威ある Agent 実行契約です。**新規 Worker デプロイではありません**。

> **v0.4.3 では macOS のみ。** 安全な新規デバイス接続（ペアリング）は macOS Keychain の資格情報バックエンドを必要とします。Linux/Windows では `worker pair` / `worker connect` 経路は**利用不可で fail closed** です。runtime 自体はこれらのプラットフォームでもサポートされています。

## 始める前に

- この経路は **v0.4.3+** を必要とします。最新の stable Release のバージョン/能力を確認してください。stable がまだ `<0.4.3` の場合、またはインストール済み CLI が `herdr-mcp worker pair` / `herdr-mcp worker connect` を提供しない場合は、**停止してバージョン/能力の blocker を報告**してください。ユーザーが preview/source のテストを明示的に求めた場合を除き、prerelease/source build をインストールしないでください。
- 最新の stable PROD herdr-mcp を **GitHub Release** からインストールしてください。repo checkout からはインストールしないでください。source/dev build を通常インストールとして扱わないでください。
- これは**新規 Worker デプロイではありません**。Cloudflare Worker、Durable Object namespace、OAuth app/client、Connector を新規作成してはならず、旧来のグローバル `LINK_SHARED_SECRET` をコピーしてもいけません。ユーザーが既に持っている Worker に参加するだけです。

## ペアリングの仕組み

1. **推奨: 登録済みのデバイスからペアリングを作成します。** fleet 内の任意の登録済みコンピュータで次を実行します:

   ```bash
   herdr-mcp worker pair
   ```

   `worker pair` は device/operator が管理する fleet アクションであり、このマシンが対象 Worker に登録済みであることを証明する資格情報が必要です。Worker control plane で pairing を作成するため、既存ワークステーションがオンラインである必要はありません。結果には、**ペアリングアドレス**、単回使用の **6 桁コード**、**正確な有効期限**、およびコピー可能な `herdr-mcp worker connect "<pairing-address>"` コマンドをまとめて表示します。通常の最大 TTL は 600 秒です。

2. **新しいコンピュータ**で、Agent が次を実行します:

   ```bash
   herdr-mcp worker connect "<pairing-address>"
   ```

   CLI は**6 桁のコードの入力を求めます**。対話端末では入力した数字を通常どおり表示するため、打ち間違いを確認できます。コードは**コマンドライン引数にはならない**ため、shell history には残りません。

   デフォルトでは、参加するコンピュータの macOS **Computer Name** が device display name として登録されます。ユーザーが別名を明示的に希望する場合だけ `--name "<device-name>"` を指定してください。`worker pair --name ...` も明示的な上書きであり、参加側の自動検出名より優先されます。

   ペアリング消費後、`worker connect` はローカル `herdr-mcp` service を自動的にインストール/起動し、登録済み Rust production Link を作成してロードします。ローカル service が healthy で、`link-prod` が managed runtime と新しい device identity を使用していることを確認できた場合のみ成功を返します。失敗時は既存の revoke / Keychain / config 補償経路を使用します。

3. 成功すると、一時的なペアリングが既存の高エントロピー毎デバイス秘密情報と交換されます。最終的なデバイス秘密情報は**macOS Keychain のみ**に保存されます。ペアリングコード/セッションは即座に使用不能になります。参加デバイスでは、Cloudflare デプロイ資格情報も旧来の `LINK_SHARED_SECRET` も使用されません。

## セキュリティ規則

- 6 桁のコードは、意図された短時間有効なペアリング資格情報です。単回使用で、10 分で期限切れになり、**誤った試行が 5 回**を超えるとセッションは永久にロックされます。
- ペアリング id は高エントロピーで推測不可能であり、ペアリングアドレス（URL フラグメント）に含まれます。HTTP アクセスログのパスには含まれません。最終的なデバイス秘密情報はペアリングアドレスには決して含まれません。
- 最終的なデバイス資格情報は macOS Keychain に属します。印刷またはログ記録しないでください。

## 検証

接続成功後、次を検証してください:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

結果として得られる不変な `device_id`、Link の online/healthy、ローカルバインドの成功を確認してください。

後から現在の登録済みコンピュータを明示的に改名する場合だけ、次を実行します:

```bash
herdr-mcp worker rename "<new-device-name>"
```

`herdr-mcp device rename ...` も同じ操作です。rename が変更するのは人向けの表示名だけで、不変な `device_id`、workstation identity、資格情報、authorization、scheduling は変わりません。Link の再接続で明示的な rename が上書きされることもありません。default/legacy workstation も最初の登録時にローカル Computer Name を記録します。

別の登録済みデバイスの認可を恒久的に取り消す場合、登録済みの任意のワークステーションで実行します。まず `herdr_devices` で不変の `device_id` を確認し、次を実行します:

```bash
herdr-mcp worker revoke "<device-id>" --confirm
```

デバイス/オペレーターが fleet 管理を担当します。この操作は display name を受け付けず、不変の `device_id` を使用する必要があります。承認済み WebChat Connector は通常の MCP 権限のみで、デバイスを revoke できません。

revoke はそのデバイス identity と資格情報に対して恒久的です。稼働中の Link は切断され、古い資格情報では再接続できません。古い identity の復活を防ぐため内部には最小の revoked tombstone を保持しますが、通常のデバイス一覧には表示しません。後で同じコンピュータを再追加する場合は、新しい pairing で新しいデバイス identity として登録してください。

## 不確実な配信 / リカバリ

- いずれかの mutation が不確実な配信を報告した場合は、**盲目的に再試行せず**、まず現在の状態を確認してください。
- connect がサーバー側の消費後に失敗した場合は、組み込みの補償/revoke 動作（正確なリモート revoke-self + ローカル Keychain クリーンアップ + 以前の config の復元）に依存し、証拠を報告してください。手動の秘密情報処理を発明しないでください。
- コードを 5 回間違って入力すると、セッションは永久にロックされます。`herdr-mcp worker pair` で新しいペアリングを作成してください。

## 複数デバイスの検証

接続後は ChatGPT から `herdr_devices` を確認し、新しいデバイスに対する実際の read-only RPC を 1 回実行して、同じ Worker/Connector 経由で到達できることを確認してください。