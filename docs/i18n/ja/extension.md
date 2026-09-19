# ブラウザ拡張

*Continuity、Browser Control Center、実験的なローカルブリッジ。*

herdr-mcp のブラウザ拡張は、動作している MCP Connector の上に載る**任意のブラウザ層**です。第二の Agent runtime ではなく、最初のワークステーション接続にも必須ではありません。

ブラウザ側の次の三つの問題を担当します。

| 面 | 解決する問題 | 詳細ドキュメント |
| --- | --- | --- |
| Continuity | ローカルでの完了を、正しい Web 会話へどう戻し、復帰させ、引き継ぐか | [ブラウザ Continuity](browser-continuity.md) |
| Control Center | Chrome Side Panel が workspace、ペイン、Agent をどう観測し、binding / Pinned Target を管理するか | [ブラウザ Control Center](browser-control-center.md) |
| JSON → MCP bridge | ネイティブ MCP Connector を持たない Web AI が、境界付き JSON プロトコル経由でローカルツールを使う方法 | [JSON → MCP ブリッジ](browser-json-mcp-bridge.md) |

ChatGPT の composer 横の Queue はブラウザ操作のプリミティブです。現在の返信が終わるのを待ってから、明示的な次ターンのユーザー指示を送ります。生成を中断しません。

データの扱いと権限については [ブラウザ拡張のプライバシーポリシー](privacy.md) を参照してください。

## インストール identity：STORE / STANDALONE / DEV

拡張の identity は Runtime の DEV/PROD 面とは独立しています。

| チャネル | 用途 | Chromium identity |
| --- | --- | --- |
| **STORE** | 一般ユーザーの既定 | 固定の Chrome Web Store identity。ストア経由で更新される |
| **STANDALONE** | v0.4.3+ の GitHub / 手動による独立配布 | 固定の非 Store identity。インストールディレクトリを移動しても ID は変わらない |
| **DEV** | ソース開発 | repo/worktree の `extension/` から Load unpacked。ID はパスから派生する |

stable v0.4.2 の Native Host ownership は STORE / DEV のみです。STANDALONE には、その契約を実際に実装した v0.4.3+ runtime が必要です。パス派生の DEV ビルドが standalone を装ってはいけません。

既定では[公式の Herdr Chrome Web Store 拡張](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp)を使います。STORE 配布が適切でなく、インストール済み runtime が明示的に対応している場合にだけ STANDALONE を使ってください。DEV はソース開発専用です。

v0.4.3+ の runtime は、ソースツリーを clone せずに GitHub リポジトリから Load unpacked 用の STANDALONE コピーを生成できます。

```bash
herdr-mcp extension standalone install
herdr-mcp native-host use standalone
```

リリース版 runtime の既定は、コンパイル時に埋め込まれた不変の source commit です。検証可能な build commit を持たない開発ビルドだけが `main` にフォールバックします。開発で明示的にリポジトリ最新ソースが必要な場合は `herdr-mcp extension standalone install --ref main` を使ってください。

`--path` を提供する runtime は、Chrome に見せる Load unpacked パスを明示的に選べます。

```bash
herdr-mcp extension standalone install --ref extension-v0.1.91 --path ~/Documents/herdr-mcp/extension
```

`--path` は省略可能で、既定は `~/Documents/herdr-mcp/extension` です。カスタムパスはユーザーの HOME 配下に解決されなければなりません。管理対象コピーは常に `~/.config/herdr-mcp/extensions/standalone/current` に置かれ、選択したパスはそこへの安定したシンボリックリンクであるため、`--path` を変更しても standalone 拡張の identity は変わりません。明示的に指定されたパスがすでに使われている場合は上書きせず fail closed します。既定パスの衝突はそのまま残し、インストーラーは管理対象パスへフォールバックします。自動化が `chrome://extensions` → Developer mode → Load unpacked で選択すべき正確なディレクトリを必要とするときは、`herdr-mcp extension standalone status` を実行し、その `chrome.load_unpacked_path` 値を使ってください。今後の更新も同じパスを再利用します。この値は最後のインストールが記録した Chrome 向けパスで、`~/.config` state から読まれるため、macOS の権限で `~/Documents` を検査できない場合でも安定しています。`user_visible_path.status` はそのエイリアス検査を `unverified` として報告しますが、`chrome.load_unpacked_path` は変わりません。

インストーラーは要求された ref を不変の commit SHA に解決し、その commit の `extension/` ツリー配下にある Git 追跡ファイルだけをダウンロードします。`manifest.json` を除き、すべてのファイルはリポジトリのソースとバイト単位で同一のままです。`manifest.json` では、固定 STANDALONE 拡張 ID に必要な公開 `key` を Herdr が注入します。repo/worktree の DEV manifest は決して変更されません。`herdr-mcp extension standalone status` は、インストール済みの commit、バージョン、ID、パスを報告します。

拡張のパッケージ workflow（`.github/workflows/extension-store.yml`）は、Store ZIP と並んで手動用パッケージも生成します。`herdr-mcp-extension-X.Y.Z.zip` は Chrome Web Store へのアップロード用パッケージです。`herdr-mcp-extension-standalone-X.Y.Z.zip` とその `.sha256` サイドカーは手動の **Load unpacked** 用です。Native Messaging が要求する STANDALONE Chromium ID を維持する公開固定 manifest `key` を持つのは standalone ZIP だけなので、Store ZIP を手動でロードしてはいけません。`shasum -a 256 -c herdr-mcp-extension-standalone-X.Y.Z.zip.sha256` で検証し、古いバージョンの上に重ねるのではなくクリーンなディレクトリへ展開してください。

macOS では、STANDALONE のインストールまたは更新後に `herdr-mcp doctor` も実行してください。`standalone status` はディスク上の管理対象ファイルを説明します。`doctor` はさらに、Google Chrome の profile preferences にある Herdr 拡張の正確なエントリだけを読み、Chrome が現在 Load unpacked しているパスを上記の管理対象パスと比較します。管理対象の `~/.config/herdr-mcp/extensions/standalone/current` ディレクトリと、最後のインストールが記録した Chrome 向けパスのどちらも受け付けます。記録されたパスは `~/.config` state から読まれ、安定した文字列として比較されるため、`--path` の変更や macOS による `~/Documents` の読み取り拒否で誤った `drift` は発生しません。`WARN standalone-extension-load state=drift` が報告された場合、同じ固定拡張 ID が別のディレクトリからロードされています。多くの場合、古い Downloads のコピーや開発用コピーです。`chrome://extensions` を開き、Herdr 拡張を見つけて、`doctor` が報告する `expected` パスから Load unpacked / Reload してください。このパスの不一致を直すためだけに Native Host のチャネルを切り替えたり、拡張データを削除したり、資格情報をコピーしたりしないでください。このチェックは診断専用で、Chrome の設定を書き換えることも、タブを再読み込みすることもありません。

チャネルを選んだら検証します。

```bash
herdr-mcp native-host status
```

active なチャネル、拡張 identity、Native Host、現在の runtime generation が一致している必要があります。STORE は Chrome Web Store、STANDALONE は正式な独立パッケージ、DEV は開発者による明示的な Reload で更新されます。拡張の更新後は、長時間開いている Web ページを更新して現在の content script を受け取らせてください。

## 入口と状態オブジェクト

| 概念 / 入口 | 単一の責務 |
| --- | --- |
| ツールバーアイコン | Side Panel Control Center を開く |
| HUD | 現在のページの簡潔な状態、Auto、手動の continue / handoff |
| Control Center | workspace binding、Pinned Target、ローカルでの観測と人間による制御 |
| Queue | 現在の返信が終わった後に、次の明示的なユーザーメッセージを送る |
| Workspace Binding | この Project / conversation をどの長期 workspace が所有するか |
| Pinned Target | 次の人間による制御がどのペイン / Agent を明示的に対象とするか |
| Herdr Focus | 人がいま Herdr UI で見ているペイン。binding や pinned target を黙って置き換えてはならない |

これらの状態をなぜ分離するのか、復帰と引き継ぎがどう動くのかは、それぞれの SSOT である [ブラウザ Continuity](browser-continuity.md) と [ブラウザ Control Center](browser-control-center.md) に属します。この概要はそれらの実装詳細を意図的に繰り返しません。

## ローカルセキュリティ境界

拡張は Herdr bearer をページの JavaScript、service worker、ブラウザストレージのどこにも置きません。

```text
ページの content script / Side Panel
          ↓
Chrome Extension Service Worker
          ↓ Native Messaging
ローカル Host
          ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

ブラウザは操作と可視化を所有し、Native Host は信頼されたローカルブリッジであり、ツール schema、managed-root チェック、権限、mutation 境界は引き続き runtime が所有します。公開 OAuth/MCP とローカル Native Messaging は別々の信頼境界です。

## 初回利用

1. Runtime と ChatGPT Connector がすでに動作していることを確認します。
2. STORE / STANDALONE / DEV を選び、`herdr-mcp native-host status` を検証します。
3. 対応する Web ページと Side Panel を開きます。
4. そのページを意図した workspace にバインドします。
5. 状態、Pinned Target、手動操作を確認している間は Auto をオフのままにします。
6. 無人で長時間動く作業が本当に必要なときにだけ、スコープ付きの Continuity 自動化を有効にします。

semantic Auto の Provider 設定は意図的に小さく保たれています。TypeSafe/Jev と OpenAI 互換 LLM judge は、それぞれ endpoint、model、API key だけを公開します。通常の Auto は Jev -> LLM -> bounded script fallback の固定順序で動き、Goal-aware Auto では Jev を既存 LLM Goal Supervisor の advisory semantic prior として利用できます。どちらの API がなくても script fallback が基本 Auto を維持し、Work Memory/TODO evidence と deterministic safety guard は引き続き authoritative です。semantic policy、Jev の判定境界、judge prompt、completion token は製品側で管理し、ユーザー設定にはしません。

z.ai / DeepSeek の JSON → MCP 連携は実験的で、既定では無効です。Herdr の実験的設定で明示的に有効にしてください。

## リリースとメンテナンスの境界

STORE / STANDALONE / DEV の identity は共存できますが、管理対象の Native Messaging manifest の active owner は一つだけです。`contracts/browser-extension-store.json` は Store identity の機械可読な SSOT です。v0.4.3 は Standalone に `contracts/browser-extension-standalone.json` を使い、DEV はパス派生のままです。

`native-host use store` / `use standalone` / `use dev` の後は、すでに開いている対応ページを更新してください。拡張のバージョンは Rust runtime とは独立に進化します。Native Host の identity / channel 契約が新しくなる場合にだけ、対応する runtime 能力が必要です。

メンテナー向けのリリース詳細は `docs/_wip/browser-extension-development-and-store-release.md` と `AGENTS.md` にあります。
