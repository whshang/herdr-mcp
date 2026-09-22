# プラットフォームと互換性のサポートマトリクス

*プロダクションサポートとは、テスト済みのセキュリティ境界であり、バイナリがたまたま起動するという主張ではありません。*

このページは Herdr-MCP 1.0 の公開互換性契約です。あるプラットフォームがプロダクションサポートされるのは、次の表が **Production** と示している場合だけです。Candidate または Unsupported と記されたものは、等価なプロダクション用ワークステーション対象として提示してはいけません。

## プロトコルと runtime の互換性

Herdr-MCP には互いに独立した複数のプロトコル層があります。それらは意図的に一つのバージョン番号を共有していません。

| 層 | 現在のプロダクション identity | 有界な互換性 | fail-closed ルール |
| --- | --- | --- | --- |
| 公開 MCP クライアントプロトコル | `2025-11-25`、`2025-06-18`、`2025-03-26`、`2024-11-05`、`2024-10-07` が `initialize` ネゴシエーションで受け入れられます | ChatGPT/OpenAI discovery は probe バージョン `2026-07-28` を advertise する場合がありますが、その probe 値は追加の `initialize` プロトコルではありません | 欠落または未認識の `initialize` プロトコル値は、明示的な legacy baseline `2025-11-25` までネゴシエートダウンします。リストにないプロトコルがサポート済みとして advertise されることはありません |
| 公開 Edge ツール契約 | epoch **7**、**19** actions | クライアント会話が古いツールスナップショットを保持している場合がありますが、Edge は現在の DEV/PROD 公開 catalog を publish します。epoch 3 identity は、保守的な非 DEV/PROD フォールバックおよび rollback ベースラインとしてのみ残ります | 現在の公開契約の外にあるツールは拒否されます |
| Workstation Runtime Execution Contract | epoch **4**、**18** tools | 直前の凍結された epoch **3**、**18** tools が有界な rollback ベースラインとして、また凍結された epoch **2**、**18** ツール catalog とともに受け入れられます | それ以外の epoch/hash の組み合わせは、ワークステーション実行の前に拒否されます |
| Relay wire protocol | 数値の `protocol_version = 1` | なし | 欠落、文字列値、または未知のプロトコルバージョンは、Relay 状態が書き込まれる前に拒否されます |
| 永続 runtime 状態 | 現在のバイナリ schema。Release は自動アクティベーションのために同じ rollback 互換の状態 schema を宣言しなければなりません | 古い store は append-only マイグレーションを通じてトランザクショナルに前方移行します | 実行中のバイナリより新しい store は拒否されます。自動 updater は、状態 schema または runtime contract identity が自身の rollback-safe なアクティベーション要件と正確に一致しない Release を拒否します |

現在の ChatGPT 経路はステートレスな Streamable HTTP クライアントとしてテストされています。`openai-mcp` の `initialize` と `tools/list` の応答は必須の SSE framing を使用し、`notifications/initialized` が受け入れられ、Connector の再接続は古い `Mcp-Session-Id` の保持に依存しません。ワークステーションの再接続、Durable Object の rehydration、heartbeat hibernation、reconnect grace はそれぞれ独立にカバーされているため、ブラウザが再接続できたことはワークステーションの mutation を安全に再生してよい証明とは扱われません。

## N、N-1、N+1 ポリシー

`N` は、現在デプロイされている Edge 契約と、現在 qualified な runtime generation を意味します。

- **N → N** は通常のプロダクション経路です。Candidate のアクティベーションは、トラフィックを移す前にヘルスと正確な runtime execution contract を確認します。Release 更新のアクティベーションではさらに、release manifest がローカルの durable-state schema と一致することが要求されます。
- **N-1 runtime execution** は有界な rollback 経路であり、一般的な互換性の約束ではありません。現時点では、直前の凍結された epoch-3 runtime contract、および現場にまだ存在する凍結された epoch-2 catalog が、現在の epoch 4 と並んで受け入れられます。この組は epoch と hash によって検証され、任意の古い catalog が受け入れられるわけではありません。
- **N+1 runtime または control plane** は推測されません。将来の epoch、relay protocol、durable-state schema は、対応する Edge/runtime マイグレーションが明示的に出荷され qualified になるまで互換ではありません。現在のコードは、ベストエフォートのダウングレードを試みる代わりに、未知の contract pair と将来の durable-state schema を拒否します。
- **durable state の移行はアップグレード時に一方向です。** 各 SQLite migration は append-only かつトランザクショナルです。rollback が安全なのは、先行するバイナリがその結果の状態をまだ読める場合だけであり、これが自動 Release アクティベーションで宣言された state schema の rollback 互換維持を要求する理由です。runtime の rollback は実行の所有権を変えるものであり、ツール呼び出しによってすでに行われた Git/ファイル/サービスへの副作用を取り消すものではありません。

contract migration と実装の更新は別の操作です。patch は `tools/list` を変えずに runtime の実装を変更できますが、モデルに見えるツールの追加/削除には明示的な contract epoch のマイグレーションとクライアント/ツールスナップショットの qualification が必要です。

## プラットフォームサポート

| プラットフォーム | ステータス | ファイルシステム境界 | プロセス / 資格情報境界 | ネットワーク境界 | qualification に関する注記 |
| --- | --- | --- | --- | --- | --- |
| macOS（Apple Silicon） | **Production** | リモートのファイル/Git mutation は live な managed Git root に限定され、read-only/write-root、dirty/busy、secret-path のゲートが適用されます。shell 実行はログインユーザーの shell のままであり、**sandbox ではありません**。TCC に関わるネイティブアクセスは、長期間にわたる Herdr-MCP broker responsibility boundary を使用します。 | ユーザーの launchd が managed runtime と production Link を所有し、immutable generation は `runtime/current` を通じて切り替わります。ローカルブラウザ IPC は所有権を持つ mode-0600 の Unix socket を使用し、device/runtime 資格情報は macOS の資格情報境界を使用します。 | runtime MCP はローカル bearer の背後で loopback に留まります。ワークステーションは認証済みの外向き Link を Edge に対して開きます。ワークステーションの受信リスナーが公開されることはありません。 | **物理的に qualified。** Apple Silicon macOS が主要なプロダクション経路です。TCC broker/process、generation rollback、OAuth/Connector、実際の ChatGPT とブラウザ拡張の経路には専用の regression/UAT カバレッジがあります。 |
| Linux x86_64（ネイティブ Debian 系ホスト） | **Production** | macOS と同じ managed-Git-root と mutation のゲートを、通常の Unix ファイル所有権/権限で強制します。macOS TCC に相当するものはなく、shell はワークステーションユーザーの非 sandbox shell のままです。 | `systemd --user` を優先し、user systemd manager が利用できない場合は managed current-user process backend が qualified なフォールバックです。device 資格情報と所有権を持つ service/control ファイルは、ユーザーごとの非公開ストレージ/権限を使用します。 | 同じ loopback runtime + 認証済み外向き Link モデルです。 | **物理的に qualified。** ネイティブ Debian の existing-fleet インストール、Link/service のライフサイクル、updater の適用が qualified です。release 成果物は static `x86_64-unknown-linux-musl` です。 |
| Linux ARM64 / aarch64（ネイティブ Debian 系ホスト） | **Production** | Linux x86_64 と同じ managed-root と Unix ownership の境界です。 | 同じ `systemd --user` / managed-process service model と同じ非公開 credential store です。 | 同じ loopback runtime + 認証済み外向き Link モデルです。 | **NanoPi R5C / Debian 11 ARM64 で物理的に qualified。** exact-source release build、immutable-generation install、保持された enrolled device identity、systemd-user service/Link の reconciliation、Edge `herdr_inspect` がすべて通りました。Release qualification では、ネイティブ GitHub ARM64 runner 上で static `aarch64-unknown-linux-musl` のビルドと smoke も行います。 |
| Windows x86_64 | **Candidate — not production** | 実機 UAT でネイティブの managed-root/path 境界は実際に確認されていますが、1.0 の Production 主張には、最新 main の正確な candidate に対する [#394](https://github.com/whshang/herdr-mcp/issues/394) の完全な昇格記録が引き続き必要です。 | [#364](https://github.com/whshang/herdr-mcp/pull/364) のネイティブ実装は `main` にあり、current-user プロセス、Windows Credential Manager、Startup フォルダのログインエントリを使用し、process ownership fencing を備えています。実機 UAT では install、service/Link 復旧、シミュレートログイン復旧、device/Connector の保持、実際の Connector 会話を確認し、[#514](https://github.com/whshang/herdr-mcp/pull/514) で qualification 中に見つかった Windows HOME/PATH 問題を修正しました。 | Candidate は同じ loopback + 認証済み外向き Link モデルに従います。 | **Windows x86_64 の実機 UAT は実施済みですが、#394 の Production 昇格はまだ open です。** 最終昇格記録は最新 main の正確な candidate で再実行・記録し、device/Connector identity の保持、シミュレートログイン復旧、最終 read-only inspect/workspace/pane アクセス、managed-root path の証拠を含める必要があります。明示的な fixture がない限り UNC/network path はサポート対象外です。 |
| Windows ARM64 / aarch64 | **Candidate — not production** | Windows x86_64 と同じ candidate の filesystem/path 契約です。実機でのファイルシステム主張はまだありません。 | 同じネイティブ Windows の current-user プロセス、Credential Manager、Startup フォルダの ownership モデルです。 | 同じ candidate の loopback + 認証済み外向き Link モデルです。 | **ネイティブ GitHub `windows-11-arm` の release build/qualification はテスト済みで、Windows ARM64 の物理 UAT はまだありません。** publish される target は `aarch64-pc-windows-msvc` です。Production へのプロモーションは、実機でのライフサイクル、資格情報、パス、エンドツーエンド Link の UAT を条件とします。 |
| WSL | **Unsupported** | WSL の host/guest ファイルシステム、Windows ドライブマウント、symlink、権限モデルがネイティブ Linux と等価であることは qualified されていません。 | WSL 固有の service manager、資格情報、ブラウザ IPC、host/guest プロセス所有権の境界は主張しません。 | WSL/NAT/Windows-host のネットワーキングは、qualified なプロダクション Link 境界ではありません。 | Linux バイナリが WSL 内で起動することはサポートの証拠になりません。別個の qualification がこれらの境界を明示的に定義するまで、セキュリティ上重要な Herdr-MCP ワークステーション対象として WSL を使用しないでください。 |

## 「Production」が意味するものと意味しないもの

プロダクションサポートとは、Herdr-MCP が所有する境界が明示的かつテスト済みであることを意味します。ワークステーションをコンテナ sandbox に変えるものではありません:

- `herdr_fs_*` と managed Git 操作は project-root でゲートされます。
- `herdr_exec*` は意図的にワークステーションユーザーとして実行され、そのユーザーがアクセスできるものにアクセスできます。
- Herdr の pane/agent は、同じワークステーションセキュリティモデル下の永続的なローカルプロセスです。
- Edge OAuth、device authorization、外向き Link 認証はリモートからの入口を保護しますが、ローカルユーザーの権限を下げることはありません。

あるプラットフォームが等価な境界を提供できない場合、正しいステータスは Candidate または Unsupported であり、契約を黙って弱めることではありません。

## プロモーションチェックリスト

Candidate のプラットフォームが Production に移行できるのは、マージ対象の正確な実装で以下がすべて真になった後だけです:

1. service/process のライフサイクルが、インストール、再起動/ログインからの復旧、失敗したアクティベーションを経ても、無関係なプロセスを引き継ぐことなく存続する。
2. 資格情報が再インストール/復旧を通じて非公開かつ device に束縛されたままである。
3. managed-root のファイルシステム操作がネイティブのパスセマンティクス（プラットフォーム固有の絶対/相対パスの挙動、および主張する UNC/ネットワークファイルシステムの挙動を含む）をカバーする。
4. runtime/Link がワークステーション境界で loopback/outbound-only のままである。
5. `initialize`、`tools/list`、reconnect、そして実際の読み取り専用 Web-AI → Edge → Link → ワークステーション要求が通る。
6. その OS に必要な clean-machine CI が実行され、hosted CI では証明できない境界を物理 UAT がカバーする。
7. 未サポートまたは未 qualified のサブ環境は、より広いプラットフォームラベルを継承せず、明示的に unsupported のままである。

関連資料: [アーキテクチャ](architecture.md)、[Runtime アップグレード](runtime-self-upgrade.md)、[トラブルシューティング](troubleshooting.md)、およびコントリビューター向けの [release model](../../release-model.md)。
