# エコシステム比較

*なぜ Herdr + herdr-mcp なのか、アーキテクチャはどう違うのか、どんなときに他の選択肢が適するのか。*

ローカル開発マシンを本当に制御したい Web AI には、多くの形があります。この記事では、Herdr + herdr-mcp がそのエコシステムの中でどこに位置するのか、なぜこのアーキテクチャが存在するのか、そしてどんなときに別のツールのほうが本当に適するのかを説明します。これは境界の判断であり、機能数の競争ではありません。

短く答えると：**Web AI が planner に留まり、ワークステーションが持続的で、観察可能で、人間がいつでも引き継げる開発環境であるとき、Herdr + herdr-mcp が最も役に立ちます。** Web モデルは決定的な小さな作業を直接こなし、より大きな作業は差し替え可能なローカル Agent に委譲し、作業現場を会話をまたいで生かし続けることで、ユーザーがいつでも離れ、戻り、引き継げるようにします。

## 何を比較するのか

ChatGPT や Codex にローカル開発環境を操作させるプロジェクトは、三つの決定的な軸で異なります。

1. **タスクはどこから始まるか** — ChatGPT Web、Codex CLI、デスクトップアプリ、あるいは任意の MCP クライアント。
2. **誰が planning を担うか** — Web モデルが決定的なツールを直接呼ぶか、ローカルのコーディング Agent に委譲するか。
3. **ローカルにどんな永続状態があるか** — 単発の MCP リクエストか、永続的な session、task、PTY、Agent、ブラウザ会話、復旧証拠か。

これらの組み合わせがどの単一ツールよりも重要であるため、ほとんどのプロジェクトは無関係な機能の長い裾ではなく、いくつかのアーキテクチャ系統に収まります。

| 系統 | 代表的なプロジェクト | 主な入口 | Planning/実行 | 最適な用途 |
| --- | --- | --- | --- | --- |
| 汎用コーディング MCP runtime | coding-tools-mcp、MCPX、DevSpace Local Artifacts | 任意の MCP クライアント / モデル | クライアントのモデルが決定的なツールを呼ぶ | 既存の AI クライアントに安全なローカルコーディングツールを足す |
| ChatGPT → ワークステーション | AgenticGPT、gpt-webcodex、chatgpt-workspace-mcp、chatgpt-local-coder | ChatGPT Web | Web planner → ローカル runtime/worker | Web/モバイルから 1 台の開発マシンを操作する |
| ChatGPT → コーディング Agent | codex-from-chatgpt、codex-chatgpt-bridge | ChatGPT Web | Web → Codex → ローカルリポジトリ | 専用 CLI を coding executor にする |
| Codex → ChatGPT Web | codex-chatgpt-web | Codex CLI | Web の推論を使う Codex 駆動のループ | Codex harness を保ったまま Web モデルを使う |
| 二 Agent 協調 | codex-with-chatgpt | ChatGPT + Codex | Web planner/reviewer ↔ Codex executor | 明示的な plan–execute–review ワークフロー |
| 永続的な作業現場の制御プレーン | Herdr + herdr-mcp | ChatGPT / 任意の MCP クライアント | Web planner → 決定的ツール または 差し替え可能な Agent | 長命なワークステーション + ブラウザ continuity |

## アーキテクチャの系統

### コーディング MCP runtime：モデルがツールを直接駆動する

```text
ChatGPT / Claude / Grok / Cursor
              │ MCP
              ▼
      coding-tools-mcp / MCPX
              │
       files / Git / exec
```

この runtime はモデル中立です。クライアントのモデルが、何を検査し、変更し、実行するかを決めます。coding-tools-mcp は、workspace confinement と有界な結果を備えた、サーバー側で強制される安定した file/search/patch、Git、PTY/exec の面を強調しています。MCPX は、永続的なリモート session がその面に復旧セマンティクスを与える仕組みを示しています。問題全体が安全なローカルの file/Git/exec アクセスであるとき、これは最も単純な形です。そしてまさにその理由で Herdr-MCP はそれらのツールを再実装しません。

### リモートワークステーション製品

```text
ChatGPT Web → Secure MCP Tunnel / HTTPS → local runtime → workspace / process / tools
```

製品の面はツールから、インストール、トンネル管理、権限、バックグラウンド作業、復旧、ローカルライフサイクル管理へと広がります。AgenticGPT（managed job と任意の Hub を備えた Linux リモート worker）と gpt-webcodex（パッケージ化された Windows 製品）はここに属します。どちらも障害ドメインの分離と製品化の強い参照ですが、あらゆる操作を job/task システムに通す傾向があります。

### Codex ファーストのブリッジ

```text
ChatGPT Web → MCP → Codex bridge → Codex CLI → repository
```

codex-from-chatgpt と codex-chatgpt-bridge は Codex 自身のサンドボックスと Agent ループを再利用しますが、その代償として Codex が必須の実行ホップになります。逆向きの codex-chatgpt-web は、Codex をユーザーインターフェースとして保ちつつ、その背後のモデルを ChatGPT Web に差し替えます。Codex がすでにあなたの入口なら価値があります。

### 二 Agent 協調

```text
ChatGPT planner/reviewer ↔ Codex executor
```

codex-with-chatgpt は planning と実行を意図的に分離し、planner には読み取り専用の MCP ブリッジを、状態を運ぶ小さな制御チャネルを使います。明示的な二 Agent ループが目的のときには良いモデルです。

## Herdr と tmux、cmux、ACP

これら三つは、Herdr 層のより単純な置き換えとしてしばしば提案されます。しかし解く問題が異なります。

| 選択肢 | 主な抽象 | 強み | herdr-mcp にとっての主なギャップ |
| --- | --- | --- | --- |
| tmux | session / window / pane / PTY | 成熟、軽量、SSH に好適 | Agent/プロジェクトのセマンティクスや復旧モデルがない |
| cmux | AI 指向のデスクトップターミナル/workspace | 強い macOS UX とローカルでの操作感 | リモートからの Web 制御とクロスプラットフォームな runtime は中核ではない |
| ACP | client ↔ coding-agent プロトコル | 構造化された session、prompt、permission、イベント | ワークステーション、PTY、Git/プロセス状態、ブラウザ continuity を所有しない |
| Herdr | 永続的な workspace / pane / agent / event runtime | 長命な状態、Agent ステータス、人間による引き継ぎ、Socket API | 公開 MCP/OAuth と Web 向けツールには herdr-mcp が必要 |

- **tmux** は優れた基盤ですが抽象度が低すぎます。長時間動く Web planner には、プロジェクト/workspace の identity、セマンティックな Agent 状態、増分イベント、人間の引き継ぎ後の安全な再観察、ブラウザ binding も必要です。それらを tmux 上で作り直すと、徐々に Agent を理解する runtime になっていきます——それは Herdr がすでに所有しているものです。
- **cmux** は強力なローカル macOS フロントエンドですが、herdr-mcp が狙うのは、ユーザーが別のデバイスにいても開発マシンが数時間にわたり到達可能で、観察可能で、復旧可能でなければならない場合です。runtime の identity とイベントのセマンティクスが、デスクトップでの見せ方より先に来ます。
- **ACP** は client↔agent 間通信の自然な将来の互換層ですが、制御プレーンには依然として workspace、リポジトリ/worktree、PTY、プロセス、Git 状態、長い exec、runtime 世代、handoff が必要です。よりクリーンな境界は、Herdr が環境を所有し、任意の Agent アダプタの背後で ACP を使うことです。

## なぜ Web-planner モデルは作業を軽量に保つのか

```text
Web AI
  ├─ ファイルを読む / Git を確認する / テストを直接実行する
  ├─ 決定的な変更を加える
  └─ 別の推論 worker が助けになるときだけ委譲する
          ↓
       Herdr worker
```

小さな編集、調査、アーキテクチャの議論は軽量なままです。複雑な開発では複数のローカル worker を組み合わせられます。すべてのリクエストを別のコーディング Agent に通すと、Web モデルは二番目の planner の UI になり、レイテンシとコンテキストの翻訳が増えます。

## 閉じたループが差別化要因

ほとんどのコーディング MCP サーバーは下流方向だけを解きます。

```text
Web AI → MCP/OAuth → Edge → outbound link → herdr-mcp → files / Git / exec / Herdr Socket API
```

これは短いタスクには十分です。しかしユーザーが画面を離れている間に作業が何時間も走るなら、戻り経路が必要です。

```text
Herdr events → herdr-mcp → local IPC / Native Messaging → browser extension → Web conversation
```

ブラウザ拡張は最初のセットアップでは任意ですが、無人での長時間タスク、ページ復旧、会話をまたぐ handoff にとって欠けている第二のチャネルです。これがなければ、標準 MCP はローカル Agent が終了したときに、すでに settled した Web 会話で新しいターンを始めさせることができません。

## 取り込み、再利用し、避けるべきもの

エコシステム全体で、長持ちして移転可能な教訓は次のとおりです。

- **identity と復旧を最優先に。** transport の session と continuity/work identity を分離する。task、edit、operation、artifact、runtime 世代、ブラウザの page epoch、handoff checkpoint にはサーバー生成の id を使う。ログから identity を再構成してはいけない。
- **緑の `/healthz` ではなく、セマンティックな健全性。** READY はプロトコルのハンドシェイク、generation/schema の一致、実際の request/response プローブ、そしてブラウザ制御が必要な場合のブラウザ面の liveness を要求すべきです。
- **Browser Lease / Page Epoch。** 制御対象のすべてのタブ/会話には明示的な lease があるべきで、navigation、reload、discard、extension のリロード、handoff、generation の変更がそれを取り消し、observer、タイマー、保留中の作業をキャンセルします。
- **制御プレーンとデータプレーンの分離。** handoff と wake のメッセージは状態、identity、evidence の参照を運ぶ。ファイル、diff、ログ、テスト出力は必要時に取得する。
- **第一級の artifact。** ビルドレポート、スクリーンショット、テストレポート、添付、大きなログは、モデルのコンテキストに直列化するのではなく、id、hash、size、media type、source、有界な読み取りを伴う。
- **独立した障害ドメイン。** 中央のサービスはルーティングと調整だけを行う。各ワークステーションは中央の障害中も役に立つ。heartbeat、最近のアクティビティ、analytics は有界に保つ。

Herdr-MCP は workspace/pane/PTY、Agent のライフサイクルと状態、イベントストリーム、worktree、高度なネイティブ操作、人間の attach/focus/inspect を Herdr にゆだねて再利用します。本線からは外しています：二つ目の Agent runtime、別のターミナルマルチプレクサ、完全な Team/Task DAG/Lease システム、内部での ACP の必須化、単一のコーディング Agent ブランドへの依存、汎用のブラウザ自動化フレームワーク。タスクのセマンティクス（軽量な `work_id`、scope、acceptance criteria、evidence）は複雑な作業を助けられますが、一度きりの読み取りやコマンドの入場料になってはいけません。

## 推奨アーキテクチャ

```text
                    Web AI
                      │
                MCP + OAuth
                      │
                Stable Edge
                      │
               outbound WSS
                      │
              Rust herdr-mcp
             /        │        \
            /         │         \
       files/Git     exec       Herdr
                                │
                         workspace / pane
                         agent / event / PTY
                                │
                       Native Messaging
                                │
                        Browser continuity
```

責務は狭いままです。Web AI は planner、herdr-mcp は安全な遠隔操作と continuity の層、Herdr は runtime の真実の永続的な源、ローカル Agent は差し替え可能な worker、ブラウザ拡張は推論システムではなく戻りチャネルです。transport（Secure MCP Tunnel、Cloudflare Edge、あるいは別のもの）は差し替え可能なままで、canonical なワークステーション状態を所有しません。

## 別の選択肢が適する場合

- ローカルのターミナル多重化だけが必要 → tmux を使う。
- 洗練された macOS のデスクトップターミナル体験が欲しい → cmux を優先。
- client↔coding-agent の相互運用が必要 → ACP を優先。
- 独立した安全な file/Git/exec MCP が欲しい → coding-tools-mcp のほうが単純。
- Linux のリモート worker/Hub 配備が欲しい → AgenticGPT を評価。
- パッケージ化された Windows の ChatGPT コーディングデスクトップ製品が欲しい → gpt-webcodex。
- Web モデルの推論を使いつつ Codex をインターフェースとして好む → codex-chatgpt-web を見る。

Herdr + herdr-mcp が最も強いのは、**強い Web モデルを主要な思考者として保ちつつ、実際の開発ワークステーションに対する持続的で信頼でき、観察可能な制御を与え、ユーザーがいつでも離れ、戻り、引き継げるようにする**ことが目的のときです。

## 参考

この比較のために精査した主要プロジェクト：

- https://github.com/xyTom/coding-tools-mcp
- https://github.com/opentokenz/mcpx
- https://github.com/cooky-dance/devspace-local-artifacts
- https://github.com/slhaf/AgenticGPT
- https://github.com/3169657175/gpt-webcodex
- https://github.com/miuuyy/codex-chatgpt-web
- https://github.com/XiaoDuoYa/codex-with-chatgpt
- https://github.com/dxawdc/chatgpt-workspace-mcp
- https://github.com/alexcodeplace/chatgpt-mcp
- https://github.com/posavr/chatgpt-local-coder
- https://github.com/joseanu/codex-from-chatgpt
- https://github.com/Dalomeve/codex-chatgpt-bridge
- https://github.com/openai/tunnel-client

これらのプロジェクトは急速に変化するため、実装の詳細はそれぞれの最新リリースで確認してください。ChatGPT のプラン可用性、Developer Mode、Secure MCP Tunnel の挙動については、現在の OpenAI のドキュメントとワークスペースのポリシーを確認してください。
