# 既存の Herdr Worker に新しいコンピュータを接続する

これは**新しいコンピュータを既存の herdr-mcp Worker/Connector に接続する**ための権威ある Agent 実行契約です。**新規 Worker デプロイではありません**。

> **v0.4.3 では macOS のみ。** 安全な新規デバイス接続（ペアリング）は macOS Keychain の資格情報バックエンドを必要とします。Linux/Windows では `worker pair` / `worker connect` 経路は**利用不可で fail closed** です。runtime 自体はこれらのプラットフォームでもサポートされています。

## 始める前に

- この経路は **v0.4.3+** を必要とします。最新の stable Release のバージョン/能力を確認してください。stable がまだ `<0.4.3` の場合、またはインストール済み CLI が `herdr-mcp worker pair` / `herdr-mcp worker connect` を提供しない場合は、**停止してバージョン/能力の blocker を報告**してください。ユーザーが preview/source のテストを明示的に求めた場合を除き、prerelease/source build をインストールしないでください。
- 最新の stable PROD herdr-mcp を **GitHub Release** からインストールしてください。repo checkout からはインストールしないでください。source/dev build を通常インストールとして扱わないでください。
- これは**新規 Worker デプロイではありません**。Cloudflare Worker、Durable Object namespace、OAuth app/client、Connector を新規作成してはならず、旧来のグローバル `LINK_SHARED_SECRET` をコピーしてもいけません。ユーザーが既に持っている Worker に参加するだけです。

## ペアリングの仕組み

1. **認可済みの既存 macOS コンピュータ**で、オーナーが次を実行します:

   ```bash
   herdr-mcp worker pair
   ```

   これにより短時間有効なペアリングセッション（デフォルト **10 分**、単回使用）が作成され、以下が表示されます:
   - **ペアリングアドレス**（Worker origin と、URL フラグメント内の高エントロピーなペアリング id）、および
   - **6 桁の検証コード**（`123 456` 形式）。

2. **新しいコンピュータ**で、Agent が次を実行します:

   ```bash
   herdr-mcp worker connect "<pairing-address>" --name "<device-name>"
   ```

   CLI は**6 桁のコードの入力を求めます**（エコーバックなしの TTY プロンプト、または非対話時はエコーなしの stdin 1 行）。コードは**決してコマンドライン引数にはならず**、**決してエコーまたはログ記録されません**。

   ペアリング消費後、`worker connect` はローカル `herdr-mcp` service を自動的にインストール/起動し、登録済み Rust production Link を作成してロードします。ローカル service が healthy で、`link-prod` が managed runtime と新しい device identity を使用していることを確認できた場合のみ成功を返します。失敗時は既存の revoke / Keychain / config 補償経路を使用します。

3. 成功すると、一時的なペアリングが既存の高エントロピー毎デバイス秘密情報と交換されます。最終的なデバイス秘密情報は**macOS Keychain のみ**に保存されます。ペアリングコード/セッションは即座に使用不能になります。参加デバイスでは、Cloudflare デプロイ資格情報も旧来の `LINK_SHARED_SECRET` も使用されません。

## セキュリティ規則

- 6 桁のコードは、意図された短時間有効なペアリング資格情報です。単回使用で、10 分で期限切れになり、**誤った試行が 5 回**を超えるとセッションは永久にロックされます。
- ペアリング id は高エントロピーで推測不可能であり、ペアリングアドレス（URL フラグメント）に含まれます。HTTP アクセスログのパスには含まれません。最終的なデバイス秘密情報はペアリングアドレスには決して含まれません。
- コードを argv、シェル履歴、ログ、トランスクリプトに**決して**入れないでください。`echo 123456 | ...` や、コードをシェル履歴に残すシェルリテラルは**使用しないでください**。
- 最終的なデバイス資格情報は macOS Keychain に属します。印刷またはログ記録しないでください。

## 検証

接続成功後、次を検証してください:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

結果として得られる不変な `device_id`、Link の online/healthy、ローカルバインドの成功を確認してください。

## 不確実な配信 / リカバリ

- いずれかの mutation が不確実な配信を報告した場合は、**盲目的に再試行せず**、まず現在の状態を確認してください。
- connect がサーバー側の消費後に失敗した場合は、組み込みの補償/revoke 動作（正確なリモート revoke-self + ローカル Keychain クリーンアップ + 以前の config の復元）に依存し、証拠を報告してください。手動の秘密情報処理を発明しないでください。
- コードを 5 回間違って入力すると、セッションは永久にロックされます。`herdr-mcp worker pair` で新しいペアリングを作成してください。

## 2 台デバイス UAT

正式な 2 台デバイスの GA/UAT はまだ通過していません。これは v0.4.3 の期待される動作であり、リリース/UAT 待ちです。