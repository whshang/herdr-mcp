# アーキテクチャ

*Web モデルに、働き続けるワークステーションを与える。*

herdr-mcp は、Web AI と Herdr が管理するローカル開発環境の間にあるリモートコントロールプレーンです。

中心となる考えは「shell をインターネットに置く」ことではありません。責務を分離して保つことです:

- Web モデルがゴール、計画、ステップをまたぐ意思決定を所有する;
- Herdr が永続的な workspace、ペイン、Agent ライフサイクルを所有する;
- herdr-mcp がコンパクトで安全なリモートコントロールサーフェスを公開する;
- Cloudflare Edge が安定した公開 OAuth/MCP identity を提供する;
- ブラウザ拡張が Web 会話への戻り経路と、ライブなローカル workspace / pane 状態を観測する Side Panel を提供する。

```text
User
  ↓
ChatGPT / Web AI        ← browser continuity ← local Herdr events
  ↓ MCP + OAuth
Cloudflare Edge
  ↓ authenticated routing
persistent herdr-link
  ↓
active local runtime generation
  ↓
Herdr socket + managed Git workstations
  ├─ files
  ├─ Git
  ├─ shell
  └─ agents
```

## Web モデルが planner である

多くの場合、システム内で最も強いモデルが最も広いコンテキストを持ちます。ユーザーの意図、以前の会話、アーキテクチャの選択、タスクの優先度、受け入れ条件です。

したがって次を決めるべきなのはそのモデルです:

- 次に何を調べるか;
- 何が直接行うのに十分決定的か;
- 独立したローカル推論に委譲する価値がいつあるか;
- ある結果が完了しているか;
- テスト、失敗、レビュー所見の後に何をするか。

ローカル Agent はワーカーです。タスクが特にその恩恵を受ける場合を除き、第 2 の隠れたオーケストレーション階層になってはなりません。

## Herdr は永続的な作業場である

通常の Web ツール呼び出しは一時的です。実際の開発作業はそうではありません。

Herdr は耐久性のある作業領域を保持します:

```text
workspace
  ├─ coding pane
  ├─ test pane
  ├─ development server
  └─ review worker
```

この永続状態が重要になるのは次のような場合です:

- Agent がまだ作業中にブラウザのターンが終わるとき;
- コマンドが 1 つの MCP リクエストより長く動くとき;
- ブラウザがリロードするとき;
- 会話がロールオーバーするとき;
- リモート planner が runtime 再起動後に再接続するとき。

herdr-mcp はそのモデルを置き換えません。それをリモートに公開します。

## 公開 MCP サーフェスを小さく保つ理由

Herdr は、Web planner がすべての MCP ツールカタログに載せるべきネイティブ Socket API よりもはるかに大きな API を持っています。

そのため herdr-mcp は高頻度の機能とロングテールを分離します。

### 高頻度のリモートツール

固定された公開サーフェスがカバーするのは:

- 現在の状態: `herdr_inspect`、`herdr_since`;
- プロジェクトポリシー: `herdr_skill`;
- ファイル: `herdr_fs_*`;
- Git: `herdr_git`;
- shell: `herdr_exec*`;
- 委譲: `herdr_prompt`。

### Herdr ネイティブのロングテール

次を使います:

```text
herdr_methods
  ↓ discover live socket schema
herdr_call
  ↓ validated passthrough
native Herdr method
```

これにより、すべての Herdr メソッドを恒久的な公開 MCP ABI にすることなく、ネイティブの到達性を保ちます。

ワークステーションの Runtime Execution Contract は **epoch 4 / 18 tools** です。現在の first-party DEV/PROD 公開 Edge 契約は **epoch 7 / 19 actions** です。ワークステーションのツールカタログ変更は付随的な runtime 変更ではなく、明示的な契約移行のままです。Runtime epoch 2/3 と公開 Edge epoch-3 identity は、有界な rollback/互換ベースラインとしてのみ維持されます。

## Progressive skills と capability truth

凍結された 18-tool カタログは固定のままですが、planner ポリシーはもはや常時ロードされる 1 つの巨大なドキュメントである必要はありません。Rust runtime にはコンパクトなグローバル `AGENTS.md` と、オンデマンドの 8 モジュールが含まれます。workstation control、file search、file mutation、Git、execution、agent dispatch、development orchestration、engineering robustness/self-verification です。内部の `herdr_mcp.skill.list/describe/load` メソッドには既存の `herdr_call` から到達でき、19 番目の公開 MCP ツールを追加しません。

progressive 経路は capability truth から意図的に分離されています。ワーカーは、その製品名だけを理由に code-edit 可能、vision 可能、高推論、または特定の provider/model に紐づくとは扱われません。代わりに `herdr-mcp scan` が evidence を構築します:

```text
Herdr agent manifest
  + executable/version evidence
  + bounded agent-specific probe
  + live Herdr session state
        ↓
capability inventory
        ↓
capability resolver
        ↓
compact inspect / progressive summary
        ↓
safe dispatch decision
```

静的または半静的な evidence は信頼性状態データベースとは別に保存されるため、新しい capability スキーマが古い runtime の rollback を不可能にすることはありません。binary identity、manifest version、probe-adapter version はキャッシュされた evidence を無効化します。inventory は live な status、cwd、project、pane、workspace、session の事実を決して所有せず、それらは引き続き Herdr/EventCache から来ます。

unknown は**未検証**を意味し、false でも「おそらくサポートされている」でもありません。probe のサブプロセスは非対話で有界であり、継承された資格情報を受け取らず、信頼された自己記述アダプターが明示的に報告した特性だけを昇格させます。完全な probe evidence は診断データであり、モデルに見える progressive bootstrap はコンパクトなカウントと検証済みの worker 特性だけを受け取ります。

Modular Progressive Skills の実装は `HERDR_MCP_PROGRESSIVE_SKILLS` の背後で Rust runtime に同梱されています。capability-aware なマルチ Agent UAT がデフォルト有効化の移行に対する evidence を提供するまで、互換/デフォルト経路は legacy のままです。

## files、Git、shell が一級である理由

Web モデルは単独ではワークステーションのファイルシステムを見られません。これは Herdr ネイティブのペイン管理とは別の話です。

そこで herdr-mcp は決定的なワークステーションの事実とアクションを直接公開します:

```text
read/search image → herdr_fs_*
Git facts         → herdr_git
short command     → herdr_exec
long command      → herdr_exec_start/read/kill
```

これにより「diff を見せて」や「テストスイートを実行して」といったタスクに Agent 呼び出しを浪費せずに済みます。

## 2 つの通信方向

MCP は下向きの制御経路を解決します:

```text
Web AI → workstation
```

長く続く開発には逆方向も必要です:

```text
workstation → browser conversation
```

ブラウザ拡張は会話を Herdr workspace にバインドし、progress/settled シグナル、recovery 状態、handoff 制御をページへ戻すことができます。その Chrome Side Panel はライブな workspace / pane / Agent 状態、明示的なペインターゲット、有界な読み取り、そして信頼されたローカルコントロールプレーンを提示します。Agent Prompt は Rust の target fencing と durable な idempotency を伴って実行され、provider Steer は正確な capability outcome を報告し、terminal-only のペインは狭いフェンス付き `pane.send_input + Enter` コマンド経路を公開し、任意の Herdr メソッドは fail-closed/preview-only のままです。

その拡張は別の runtime ではありません。Continuity、Control Center、Queue、JSON → MCP は同じ信頼されたローカルブリッジ上のブラウザサーフェスであり、Herdr は runtime truth のままです。

ローカル coding Agent は同じ browser control plane を反対側から使います。`herdr-mcp` CLI を通じて、同じ consent、identity、idempotency、delivery ルールの下で、対応する WebChat 会話を作成し、継続し、dispatch し、観測し、handoff できます。それは control plane の第 2 の一級呼び出し元であり、第 2 の runtime でもメッセージバスでもありません。

[ローカル Agent による WebChat 操作](local-agent-webchat-control.md) と [ブラウザ Continuity](browser-continuity.md) を参照してください。

## ワークステーションが外向きに接続する理由

ローカル runtime は loopback にバインドします。公開インターネットがワークステーションへ直接接続することはありません。

代わりに:

```text
workstation
   └─ authenticated outbound WSS → Cloudflare Edge
```

これにより、インバウンドのワークステーションポートを開かずに安定した公開エンドポイントを作ります。

ローカル runtime が再起動したり A/B generation が変わっても、公開プレーンは安定したままでいられます。

## Edge と runtime は別々のリリースプレーンである

```text
Public plane
  Worker / Durable Object / OAuth / MCP endpoint

Local plane
  herdr-link / active runtime generation
```

ローカル実装の修正は通常、新しい Connector URL を必要としません。同様に OAuth リレーの修正がローカル runtime の置き換えを必要とすることもありません。

[Cloudflare Edge デプロイ](cloudflare-edge-deployment.md) と [Runtime A/B](runtime-self-upgrade.md) を参照してください。

## Runtime A/B

`herdr-link` は新しいリクエストをアクティブなローカル generation ポインターへルーティングします。

```text
          ┌─ runtime A :8772
herdr-link
          └─ runtime B :8773
```

候補は独立に起動し、ヘルスと契約のゲートを通過し、アクティブになった後も、古い generation を rollback 用に利用可能なまま残せます。

すでに dispatch された作業は、アクティブポインターが変わったというだけで重複してはなりません。

## Git-backed root と、正確に live と確認できる非 Git operational root がファイル境界である

リモートのファイル操作は、live な Herdr snapshot が知っているプロジェクトルートに制約されます。それには 2 種類あります:

- **Git-backed managed root** — 既存の境界: snapshot から導出される `managed && vcs == git` のプロジェクト。
- **非 Git operational root** — vcs を持たないディレクトリで、完全に live な workspace/pane cwd であり、canonical で存在するディレクトリに解決されるもの。`$HOME` 自体とその任意の祖先（macOS Data-volume の firmlink 表記 `/System/Volumes/Data/Users/<user>` と symlink エイリアスを含み、device+inode でマッチされます）は決して operational root にはならず、live トポロジーが証明しない sibling も同様です。

重要なゲートには次が含まれます:

- validated-root（Git-backed または operational）の検証;
- 読み取り専用モード;
- 任意の write-root allowlist;
- dirty ファイルの確認;
- busy プロジェクトの確認;
- `herdr_fs_*` における secret っぽいパスのフィルタリング。

operational root は Git 状態を捏造しません。`managed` ではなく、clean/dirty/status も持ちません。読み取り/実行サーフェス（`herdr_fs_read` / `herdr_fs_list` / `herdr_fs_grep` / `herdr_fs_image` / `herdr_exec`）はそれを受け入れます。`herdr_git` は Git-only の境界を保ち、mutation（`herdr_fs_edit` / `herdr_fs_write` / `herdr_fs_patch`）は安全性が Git-dirty の確認に依存するため、現在は `operational_root_mutation_unsupported` で fail closed します。

macOS では、`Documents` / `Desktop` / `Downloads` ルートは既存の TCC 経路を保ちます。ローテーションする runtime はそれらを直接読みません。`herdr_fs_*` はインストール済みの stable TCC broker（operational root では compat revision 4）が提供し、`herdr_exec` は委譲された utility pane が提供します。

`herdr_exec` は意図的に強い境界です。ワークステーションユーザーとして shell を実行するのであり、secret パスフィルター付きのファイル API と等価ではありません。

実際の sandbox が追加されない限り、shell アクセスを sandbox として説明しないでください。

## mutation の不確実性は一級の状態である

リモートシステムは不快な場所で失敗します:

```text
request sent
  ↓
mutation happened
  ↓
response lost
```

クライアントが盲目的にリトライすれば、mutation は 2 回起きるかもしれません。

したがって herdr-mcp は次を優先します:

- 利用可能な場合は idempotency key;
- 明示的な delivery evidence;
- post-submit の状態待ちから分離されたトランスポート障害;
- 不確実な mutation を再試行する前の再検査;
- デプロイ/cutover 操作に対する状態ベースの reconciliation。

この原則は agent prompt と shell 実行から、ブラウザ handoff と Cloudflare の変更にまで適用されます。

## コントロールプレーンの障害は自動的にプロジェクト障害ではない

Herdr の snapshot/pane 制御は、Git リポジトリとは独立に時折失敗することがあります。

読み取り専用の経路はより狭い証拠源へ縮退できます。例えば:

- 完全な snapshot の代わりに list API;
- 直接の Git 事実;
- 決定的なプロジェクトファイル読み取り。

Web planner は「現在 1 つのコントロールプレーンオブジェクトを検査できない」と「そのリポジトリでは作業できない」を区別すべきです。

## ブラウザのセキュリティ境界

ブラウザ拡張は、ページ JavaScript や service-worker ストレージに Herdr bearer を必要としません。

主要な経路:

```text
content script
  ↓
extension service worker
  ↓ Native Messaging
local host
  ↓ Unix socket (0600)
herdr-mcp runtime
```

公開 ChatGPT アクセスは Edge で OAuth を使います。ローカルのブラウザ continuity は信頼されたローカル IPC を使います。これらは意図的に別々の信頼境界です。

## このアーキテクチャが意図的に抑制されている理由

このシステムは重複する層を作ることを避けます:

- Herdr がすでに Agent とペインを管理しているので、herdr-mcp は別の Agent レジストリを作りません;
- Web AI がすでに計画しているので、herdr-mcp はワークフロー DSL を作りません;
- Git がすでにソースオブトゥルースの状態を提供しているので、Agent の文章は完了の証拠として扱われません;
- Cloudflare がすでに公開ルーティング/OAuth プリミティブを提供しているので、ワークステーションが自身を直接公開することはありません。

結果は、最も重要な特性が機能数ではなく所有権の明瞭さであるコントロールプレーンです。

## 典型的な修復ループ

```text
Inspect live workspace
  ↓
Read Git + relevant files
  ↓
Make deterministic edits directly
  ↓
Delegate one narrow task only if useful
  ↓
Run tests / long command
  ↓
Use since + Git evidence
  ↓
Review
  ↓
Commit / deploy
  ↓
Browser continuity resumes the Web planner when needed
```

それが実践におけるこのアーキテクチャです。Web の計画、永続的なローカル実行、明示的な evidence、そして独立に復旧可能な層です。

関連する読み物:

- [設計哲学](design-philosophy.md)
- [ベストプラクティス](best-practices.md)
- [ChatGPT Connector](chatgpt-connector.md)
- [ブラウザ Continuity](browser-continuity.md)
- [プラットフォームと互換性のサポートマトリックス](platform-support-matrix.md)
- [トラブルシューティング](troubleshooting.md)
