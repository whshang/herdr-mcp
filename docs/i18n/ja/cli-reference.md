# CLI リファレンス

ネイティブ Rust CLI の現在の操作ガイドです。コマンド名はすべて英語です。

## 日常の操作と出力

~~~bash
herdr-mcp --help
herdr-mcp status
herdr-mcp status --json
herdr-mcp status --details
herdr-mcp doctor
herdr-mcp doctor --json
herdr-mcp doctor --details
herdr-mcp help agent
herdr-mcp help advanced
herdr-mcp --help-all
~~~

`status` はバージョン、全体の状態、Herdr 接続、Link・クラウドの設定、更新チャネルとスケジュールを表示します。「設定済み」はローカルの設定情報です。`doctor` はサービスとランタイム、Herdr、対象システムの権限、ブラウザーとローカル連携、設定済みのクラウド、ローカル MCP 認証を検査し、次の操作を表示します。

既定は簡潔な人向け出力です。doctor はブラウザーと IPC を `browser_integration`、Edge/OAuth/公開 MCP を `cloud_health` にまとめ、対象外の権限行を省略します。個別の検査は JSON と開発者向け詳細に残ります。status はデバイス登録を推測しません。一覧には `herdr-mcp device list` を使います。`--details` は保守用のレイヤー、出自、世代、パスなどを追加します。`--json` は固定の英語キーと状態コードを持つ意味を持つフィールドだけの簡潔な単一 JSON 文書を出力します。`DOCTOR_JSON` 接頭辞はありません。`--json` と `--details` は同時指定できません。三つの形式は同じ収集結果を使用します。

doctor の終了コード **0** は検査済みレイヤーに既知の障害がないこと、**2** は既知の障害があることを示します。公開 Edge/OAuth/MCP の検査はコネクター認証情報を送信しません。`authenticated_remote_mcp: "not_probed"` と `remote_reason: "no_connector_oauth_credential"` はリモート認証が未検証であることを示し、失敗には数えません。`overall: "pass"` は doctor が扱う検査で既知の障害が見つからなかったことだけを示し、認証済みのエンドツーエンド遠隔経路が証明済みという意味ではありません。既知の障害は `overall: "fail"` です。`next_step` を参照し、認証済み MCP クライアントでリモートツール呼び出しを実行してください。Native Messaging が未対応のプラットフォームではブラウザー/IPC の事実は `not_applicable` となり、人向け既定出力ではブラウザー連携行を省略します。Agent/自動処理は翻訳済みテキストを解析してはいけません。JSON は `details` 配列、パスや出自、`scheduler_state` を含みません。調査には `--details` を使います。

General は一般ユーザー、`help agent` は Agent/自動化、`help advanced` は開発者と UAT 向けです。`--help-all` は三つすべてを含みます。`scan` は Agent ヘルプにのみ表示します。Agent は固定の英語 JSON キー・状態コード・エラーコードを使い、翻訳済みテキストを解析してはいけません。

~~~bash
herdr-mcp help agent
herdr-mcp status --json
herdr-mcp doctor --json
herdr-mcp scan --json [--refresh] [--probe]
herdr-mcp device list
~~~

## 言語

~~~bash
herdr-mcp lang
herdr-mcp lang en
herdr-mcp lang zh-CN
herdr-mcp lang ja
herdr-mcp lang auto
herdr-mcp --lang ja doctor
HERDR_MCP_LANG=zh-CN herdr-mcp status
~~~

優先順位はグローバル `--lang` > `HERDR_MCP_LANG` > `~/.config/herdr-mcp/ui.json` の保存設定 > 最初の空でない `LC_ALL` / `LC_MESSAGES` / `LANG` > POSIX がない場合の時間制限付き macOS 言語照会 > 英語です。空でない `HERDR_MCP_LANG` は優先順位を確定し、未対応の値は下位の保存設定やシステム言語へフォールスルーせず英語を選びます。`zh` は `zh-CN` の旧別名として使用できます。`ja_JP.UTF-8` などのシステム形式も認識します。最初の空でない POSIX 値が優先され、`C` や `de_DE` は macOS を照会せず英語になります。上位設定がすべてない場合だけ `/usr/bin/defaults read -g AppleLanguages` を一度実行します。待機上限は 300 ms、終了猶予は最大 250 ms、出力保持は最大 8 KiB で、子プロセスは回収します。失敗・タイムアウト・未対応リストは英語に戻ります。リスト順に `zh-Hans` / `zh-Hant` / `zh-*` を `zh-CN`、`ja-*` を `ja`、`en-*` を `en` に対応させます。依存ライブラリや常駐プロセスは追加しません。

`lang` は有効な言語を表示し、`lang auto` は保存した言語だけを削除します。Rust が言語ポリシーを管理し、旧 Bash と共有する JSON の他のフィールドを保持します。中国語は互換性のため `zh` として保存します。旧 `HERDR_MCP_UI_CFG` によるファイル指定も有効です。`--lang ja` と `--lang=ja` はコマンドの前後に指定でき、保存設定は変更しません。ヘルプと既定の status/doctor を翻訳し、技術診断やその他のコマンドは英語のままの場合があります。拡張機能の言語は別設定です。

## 詳細ヘルプと保守

~~~bash
herdr-mcp worker --help
herdr-mcp connector --help
herdr-mcp automation --help
herdr-mcp instance --help
herdr-mcp qualification --help
~~~

既定ヘルプはセットアップと診断、デバイス、コネクター、自動化、更新と修復をまとめます。UAT、インスタンス削除、qualification、Link cutover/seal/migration、native-host 開発、candidate は高度な保守ヘルプにあります。

ライフサイクル操作は独立したシェルから実行します。管理サービスは `runtime/current` を使用し、インストール済み世代を書き換えません。macOS の製品修復は `reinstall`、Linux は `install`、Linux のサービス削除は `service uninstall` です。詳細は[英語版](../en/cli-reference.md)を参照してください。
