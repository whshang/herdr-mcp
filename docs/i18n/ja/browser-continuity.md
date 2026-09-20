# ブラウザ continuity

*MCP だけでは長期的な Web 作業に足りない理由。*

MCP は Web AI に workstation 上の tool を呼び出す手段を与えます。しかし、長いローカルタスクが終わった後もブラウザの会話が動き続けることは保証しません。

この区別は Herdr のワークフローで重要になります。

```text
MCP direction
Web AI ─────────────► workstation

Continuity direction
workstation ────────► browser conversation
```

持続可能な AI 開発ワークフローには、両方向が必要です。

## 長いタスクの隙間

典型的なタスクを考えます：

1. ChatGPT がリポジトリを inspect する。
2. 焦点を絞った実装タスクを Herdr agent へ dispatch する。
3. tool call はすぐに返る。ジョブは受け付けられた。
4. ブラウザの turn が終わる。
5. ローカル agent はさらに 20 分間作業を続ける。
6. agent が完了する。

手順 6 の時点で、標準的な request-driven な MCP は新しい ChatGPT turn を自動で開始しません。ローカルマシンは作業が終わったことを知っていますが、ブラウザは知りません。

これが拡張の埋める隙間です。

## Continuity は別の agent ではない

ブラウザ拡張は ChatGPT、Herdr、ローカル worker を置き換えません。プロジェクト計画やコード生成ポリシーを所有しません。

その役割はより狭いものです：

- ブラウザの scope を Herdr workspace に binding する——利用可能なら安定した ChatGPT Project、そうでなければ具体的な conversation。
- ローカルの進捗と settled 状態を観測する。
- 有用な進捗を正しい conversation へ戻す。
- 停止した、または応答の一部を失ったブラウザビューを復旧する。
- 非常に長い conversation を新しい conversation へ安全に引き継ぐ。
- 正しいブラウザ conversation を、そのローカル作業に長期にわたって接続し続ける。

Web model は planner のままです。Herdr は runtime の真実のままです。拡張は二つの側を長期にわたって接続し続けます。

この境界が、拡張が存在する主な理由です。ChatGPT の tool round は Agent を dispatch してから終了できます。その assistant 応答が送信された後、Web planner はポーリングを続けられず、Herdr も新しい model turn を直接開始できません。そのため拡張は、ローカル状態の変化を継続のシグナルとして扱います。意味のある出力の変化は有界な progress wake を生むことがあり、working → idle/done/blocked の遷移は一度だけ wake し、reconnect はブラウザ/runtime のリンクが使えなかった間に起きた settle を復旧できます。wake は Web planner に対し、live な Herdr/Git 状態を読み直し、Agent の結果と diff を inspect し、分離された完了作業を必要に応じて review/cherry-pick または merge し、そのタスクの acceptance check を実行するよう伝えます。blocked/failed/timeout 的な結果は診断すべき作業のままです。wake 自体は完了の evidence にはなりません。

この挙動は Rust/runtime の変更をまたぐ互換契約です。ローカル runtime や event transport のコードを変更するたびに、リグレッションカバレッジは browser binding、one-shot な settle セマンティクス、progress の重複排除、古い source の拒否、Native Messaging の信頼境界、handoff/queue の移行、extension version の整合性を保たなければなりません。

## binding が agent ではなく workspace である理由

実際の作業はしばしば複数の pane にまたがります：

```text
workspace: my-project
  ├─ pane: pi implementing a fix
  ├─ pane: test suite
  ├─ pane: local server
  └─ pane: grok reviewing the diff
```

ブラウザを一つの agent に binding すると、プロジェクト状態の残りを失います。完全なローカル作業領域を表す workspace のほうが、continuity の単位として適しています。

binding は安定した workspace identity を保存します。label は表示データにすぎず、live な workspace catalog から更新できます。ChatGPT ではもう一段階あります。Project binding は安定した `project_id` をキーとし、`active_conv_key` はメッセージを受け取るべき具体的な conversation を指します。これにより、チャットを作成する前に Project ホームから binding でき、rollover でも binding 自体を安定に保てます。

## progress と settled イベント

長い作業の間、拡張は意味のある新しいローカル出力があるときに progress update を送信できます。binding された workspace が settled すると、Web conversation を一度 wake し、planner が結果を inspect し、完了した Agent 作業を必要に応じて統合し、acceptance check を実行し、次に何をするかを決められるようにします。

重要な規則は、continuity を通知スパムに変えないことです。progress のチェックと progress の送信は別の概念です。拡張は頻繁にチェックしてよいものの、新しい情報があるか、設定された fallback interval が経過したときにのみ送信します。

## ブラウザの復旧

サーバー側の conversation がすでに進んでいるのに、Web AI の turn がブラウザ側で失敗することもあります。例：

- 明示的な send-timeout エラー。
- 切断された応答ストリーム。
- 古い部分的な回答しか表示していないページ。
- サーバーが現在の DOM より新しい assistant メッセージを持っていること。

tool の mutation がすでに発生している可能性があるため、ユーザータスクを盲目的に再送するのは危険です。

したがって復旧ポリシーは evidence-first です：

```text
error / stale view
      ↓
inspect same-origin conversation state when possible
      ↓
server already advanced? → refresh view
request clearly not accepted? → bounded retry
unknown delivery? → fail closed
      ↓
only after recovery is exhausted consider handoff
```

目標は最大限の自動化ではありません。通常のブラウザ障害から復旧しつつ、重複作業を防ぐことです。

## Conversation の rollover

生産的な Herdr セッションは、一つの ChatGPT conversation より長く生き残ることがあります。tool call、project instruction、可視テキストはいずれも context を消費し、ブラウザは古いメッセージを virtualization して、履歴全体を DOM に保持しなくなることがあります。

0.4.2 以降、binding された conversation の finalized な user/assistant turn は、Native Messaging 経由で Rust の `state.db` Continuity Journal に増分的に追記されます。ID のみの rollover の前に、拡張は現在の `continuity_id` について live な Rust resolve を実行します。その resolve が成功すると、新しい conversation はコンパクトな continuity 参照だけを受け取り、既存の `herdr_call(method="continuity.resume", ...)` 経路を呼んで有界な直近の作業 tail を復旧し、その後 mutation の前に Herdr、Git、関連サービスを再 inspect します。

同じ binding 済み ChatGPT Project の中でユーザーが **手動で** 別の conversation を開始する場合、`continuity_id` を覚えたり入力したりする必要はありません。新しい conversation で最初に受け入れられた user turn（たとえば「続けて」）は、既存の Project binding を通じて同じ continuity chain に journal されます。Web planner が「続けて」「resume」「どこまで進んだか」といった prior-work の意図を見た場合、ユーザーに内部 ID を尋ねる前に `continuity.resolve` / `continuity.search` で検索します。自動的な `continuity.resume` が許可されるのは、安定した conversation/project/workspace identity がちょうど一つの active chain を導く場合だけです。テキストのみの一致は候補が 1 つでもユーザーの確認が必要です（confirmation-required のままです）。「continue」のような一般的な語は検索のトリガーであり、選択の evidence ではありません。曖昧な結果はユーザー確認のために有界な title/workspace/update-time と最近の turn の evidence のみを提示し、Herdr が newest-or-most-similar のヒューリスティックで選ぶことは決してありません。

Handoff は一つの durable な復旧 payload を持ちます。`herdr_mcp.browser_handoff.prepare` は既存の `continuity_id` を再利用し、正確な古い ChatGPT URL を `source_url` として保持し、自動配送と Copy Prompt フォールバックの両方に一つのコンパクトな continuation メッセージを返します。ブラウザ自動化の無いユーザーは、新しい Web-AI conversation を開いて同じメッセージを貼り付けることができます。古い URL は単独の復旧アンカーとしても残ります。URL しか得られない場合、planner は完全な URL を `conversation_url` として `continuity.resume` を呼べます。有界な `continuity.search` にフォールバックするのは `continuity_ambiguous` の場合だけです。この復旧は ego-browser や、事前の endpoint/account/Project/generation/device の列挙に依存しません。ローカル journal に取り込まれたことのない private な ChatGPT conversation は、MCP 経由でその URL から本文を取得できません。MCP 接続自体がユーザーの private な ChatGPT session body を運ばないためです。

continuity id は conversation をまたぐ一つの安定した work chain を識別します。ChatGPT Projects では、Project/workspace binding と continuity id はその場に留まり、確認済みの active conversation target だけが変わります。

ローカル journal が利用できない、または live な Rust resolve が失敗する場合、新しい ChatGPT handoff は fail closed し、source conversation と完全な source URL を保持します。source の WebChat やフォールバック LLM に handoff サマリーを生成させることはもうありません。`HERDR_HANDOFF_V1` は、すでに存在する legacy transfer と、z.ai のような provider 固有の legacy contract のための read/recovery 互換としてのみ残り、新しい ChatGPT handoff はそれを作成しません。

これにより、消えゆく source ページを、作業状態の唯一の復旧可能なコピーの責任者にすることなく、continuity を保てます。

### 1.0 Work Memory と continuity compaction

1.0 runtime は Continuity Journal の上に Project Work Memory と verified rolling checkpoint を実装しています。finalized raw turn は durable な continuity evidence として残り、`work_memory.checkpoint.put` は checkpoint revision CAS の下で compact な structured checkpoint を書き込み、同じ Work Memory partition に属する実在の message/evidence anchor を要求します。

`work_memory.resume` は正確な `project_ref + repo_id + work_chain_id` partition を解決し、最新の verified checkpoint、bounded recent raw tail、ローカル evidence を返します。これにより長期 work chain の復旧 context を bounded に保ちながら、task-state authority は一つのままです。browser handoff は同じ `continuity_id` を引き続き再利用し、Provider 固有の Memory は復旧 authority になりません。

古い raw body を回収できるのは、replacement checkpoint の evidence が生成・検証された後だけです。browser memory、長い DOM のコスト、main-thread/render の負荷、model context pressure は rollover 判断に利用できますが、Work Memory authority や replay safety は変更しません。checkpoint mutation は Reliability Kernel の generation、idempotency、delivery、uncertain-result rules に引き続き保護されます。元の設計 provenance は [Rust Native Rearchitecture Phase 8](../../history/architecture/rust-native-rearchitecture.md#phase-8continuity-20) と [1.0 Alpha 2 Work Memory design](../../history/architecture/v1.0-alpha2-work-memory.md) に保持されています。

## 手動制御と自動制御

自動化の scope は意図的に限定されています。

ChatGPT Projects では、グローバルの Project 権限が有効なとき、自動化を Project レベルで共有できます。通常の ChatGPT conversation、z.ai、DeepSeek は、サポートされている場合に conversation レベルの設定を使います。

新しい scope は既定で Auto オフです。HUD の三つのプリセット progression アクションは自動 progression と排他的で、同じ conversation が二つの経路で同時に進められることはありません。Handoff は意図的な例外で、**HUD** に一つの UI エントリを持ちます。サポートされている場合、Auto オン/オフのいずれでも開始でき、transfer 中は source からの自動 wake を一時停止し、target に source の Auto 状態を継承させます。

手動での引き継ぎは依然として重要です。ユーザーは Auto をオフにして手動で続けるか、Herdr の状態を抽出するか、HUD から軽量な LLM judge を実行できます。明示的な handoff は、サポートされている場合 Control Center から開始します。

## ローカル Native Messaging を使う理由

拡張は workstation の bearer をブラウザストレージに持つ必要があるべきではありません。

主要な経路は：

```text
content script
  ↓
extension service worker
  ↓ Chrome Native Messaging
native host
  ↓ local Unix socket (0600)
herdr-mcp runtime
```

ブラウザは continuity のトラフィックを公開 Cloudflare Edge 経由で流しません。これにより、公開 OAuth identity とローカル拡張の信頼が別々のセキュリティ境界として保たれます。

## Continuity は拡張の一つの面であり、ブラウザ製品全体ではない

このページは **Web continuity** に焦点を当てます。workspace binding、progress / settled の push-back、stale-view の復旧、handoff / rollover です。

拡張には今、責任の異なる二つの別のプロダクト面があります：

- [Browser Control Center](browser-control-center.md) — Chrome Side Panel での live な workspace / pane / agent の観測、明示的な Pinned Target、有界な read。
- [JSON → MCP bridge](extension.md) — サイトがネイティブの MCP Connector を公開していない場合の、z.ai / DeepSeek 向けローカル tool 互換経路。

これらは Native Messaging とローカル IPC を共有しますが、一つの状態機械ではありません。Continuity は Web conversation がどう持続するかを決め、Control Center はローカルの真実と明示的な人間によるターゲティングを提示し、JSON → MCP は tool protocol を適応させます。

## メンタルモデル

Herdr を持続する作業場、MCP を遠隔操作用のケーブル、browser continuity を作業場が変わったことを Web planner に伝える戻り信号と考えてください。

三つが揃えば、長いコーディングタスクを一つの同期的なブラウザ turn に収める必要はもうありません。

次に読む：

- [ブラウザ拡張](extension.md) — インストールとブラウザ製品全体の概要
- [Browser Control Center](browser-control-center.md) — Side Panel の live な状態と明示的なターゲティング
- [wake、復旧、handoff](browser-continuity.md) — continuity 状態機械と現在の挙動
- [JSON → MCP bridge](extension.md) — z.ai / DeepSeek 互換経路

## Continuity の実装と復旧の詳細

> **役割：** ブラウザ continuity 状態機械の上級リファレンス。ほとんどのユーザーに必要なのは [Browser Extension](extension.md) と [Browser Control Center](browser-control-center.md) だけです。

このページではブラウザ continuity の状態機械を説明します。ブラウザの turn が終わった後も Herdr の作業が続くとき、拡張が正しい Web conversation をどう wake するか、停止した ChatGPT のビューをどう復旧するか、mutation を重複させることなく長い conversation を新しいものへどう引き継ぐかです。

アーキテクチャ上の動機については、まず [Browser Continuity](browser-continuity.md) を読んでください。インストールと HUD の使い方は [Browser Extension](extension.md) で扱います。

## Project または conversation を workspace に binding する

Continuity はブラウザの scope を、単一の agent ではなく Herdr の **workspace** に binding します。通常の conversation では、その scope は conversation 自体です。ChatGPT Projects では安定した `project_id` です。

```text
ChatGPT conversation
        │ binding
        ▼
Herdr workspace
  ├─ implementation agent
  ├─ test process
  ├─ server/log pane
  └─ review agent
```

`workspace_id` が安定した identity です。label は表示用メタデータで、live な catalog から更新されます。

0.1.59 以降、conversation が存在する前に `https://chatgpt.com/g/<project>/project` から直接 ChatGPT workspace を binding できます。Project binding は独立して永続し、具体的な active な `/c/<id>` はその `active_conv_key` 配送ターゲットにすぎなくなります。`https://chatgpt.com/` は tab スコープの pending binding を保持することもでき、その tab が最初に Project または conversation へ入ったときに一度だけ移行します。root ページと Project ホームページは binding コントロールを公開しますが、conversation 専用の Continue/LLM/recovery/rollover アクションは実行しません。

## working、progress、settled

拡張は信頼された Native Messaging 経路を通じて、ローカルの `/push/events` を観測します。

### working

binding された workspace 内の関連する agent が working になると、拡張は progress 観測を armed にします。

### progress

`progressTickSec` は progress を **チェックする** 頻度を制御するもので、メッセージが必ず送信される頻度を制御するものではありません。

progress メッセージは、意味のある新しい出力があるとき、または設定された fallback interval の経過後に送信されます。最後に送信したサマリーとタイムスタンプは永続化されるため、Service Worker の再起動で通知が繰り返されることはありません。

### settled

workspace 内の他の agent がまだ working なのに一つの pane が settled した場合、それは部分的な progress です。workspace が settled と見なされるのは、関連する working set が空になったときだけです。

settled イベントは Web planner を wake し、Git、テスト、agent の出力を inspect できるようにします。イベント自体は、ビジネス上の acceptance criteria が満たされた証拠ではありません。

## HUD の手動アクション

HUD は次を公開できます：

- **Continue** — 現在の Web conversation へ単純な継続を送信する。
- **Herdr monitor** — 続ける前に binding された workspace を inspect する。
- **LLM analysis** — 設定済みの小モデルに、最新の返信が明らかに未完了かを尋ねる。

HUD は Continue / Check Herdr / LLM decide に加えて Handoff を公開します。最初の三つの page-scoped な progression アクションは Auto がオンの間ロックされ、引き継ぎは安全ゲートを通過したときに利用可能なままです。workspace binding とローカル Herdr コントロールは Side Panel に留まります。Handoff は **HUD** に単一の UI エントリを持ち、transfer 中は source の自動 wake を一時停止し、target に source の Auto 状態を継承させます。

## Queue：明示的な次 turn のユーザー意図は auto-continue より優先される

ChatGPT composer の横の **Queue** は、HUD の手動 Continue アクションとは異なります。

Queue は、assistant がまだ返信中だがユーザーが次の指示をすでに決めている場合のためのものです。クリックしても live な turn は中断されず、現在の composer テキストをその conversation のために永続化します。

turn が settled したときの順序は：

```text
current assistant turn ends
       ↓
queued content? ── yes ──► merge and send the next user message
       │ no
       ▼
then consider semantic Auto: Jev -> LLM -> bounded script fallback
```

この優先順位は意図的です。**明示的な次 turn のユーザー指示は、続けるかどうかをモデル自身が決めることより優先されます。**

キューは次の境界にも従います：

- エントリは挿入順を保ち、空行で merge されます。
- `turn-in-progress` やその他のブロックされた delivery は、内容を ACK も破棄もしません。
- 削除されるのは、配送が確認された batch だけです。
- エントリ数、エントリごとの長さ、merge 後の長さには上限があります。
- Queue を右クリックすると、現在の conversation キューをクリアします。
- composer が空の状態でクリックすると、まだ pending な batch を再試行します。
- handoff の cutover が確認された後、pending の内容は同じ順序で target conversation へ移行します。

Queue は Herdr tool を実行せず、workspace binding も変更しません。**次のユーザーメッセージ** を保存して配送するだけです。

## 自動化の scope

### ChatGPT Projects

Project の自動化には両方が必要です：

1. Options での ChatGPT Project 自動化のグローバル権限。
2. 現在の Project の HUD が Auto オンに設定されていること。

設定は安定した `project_id` をキーとするため、同じ Project 内の handoff 後の conversation は Project の自動化設定を継承できます。

Herdr tool の permission card は、Auto に従わない唯一の例外です。サポートされ、明示的にラベル付けされた Herdr tool の permission card は、Auto がオフでも自動的に受け入れられます。それは Herdr connector 自身が要求した tool 呼び出しを unblock するだけで、fail-closed な card 検出器を再利用するためです。Auto は引き続き wake/progress/rollover の挙動を統制し、オフにしても tool call が card の後ろで止まったままになることはありません。

### 通常の ChatGPT / z.ai / DeepSeek

サポートされている場合、これらは conversation スコープの Auto を使います。

z.ai と DeepSeek の Auto は Herdr の progress/settled wake 挙動だけを行います。ChatGPT 固有の stale-view 復旧、permission-card の扱い、turn 終了時の意味判定、自動 rollover は、汎用の能力として扱われません。

## turn 終了時の意味判定

ChatGPT の返信は、構文的には終わっていても意味的には未完了であることがあります。たとえば、まだテストを実行する必要がある、次の手順は Git を inspect することだ、といった内容です。

通常の Auto は、次の固定された段階的な順序で判断します：

```text
deterministic safety / scope gates
        |
        v
TypeSafe Jev / System One（設定済みの場合）
        |
        v uncertain / unavailable
OpenAI-compatible LLM judge（設定済みの場合）
        |
        v ambiguous / unavailable
bounded mechanical script fallback
```

Jev は最初に狭い意味判定を行い、通常の Auto では高信頼の continue/done を最終結果として扱います。Jev が確定できない場合のみ LLM に進み、LLM も確定できない場合のみ精度の低い機械的な script fallback を使います。この fallback により Jev/LLM API を持たないユーザーでも基本 Auto を利用できますが、script が Jev/LLM の結果を上書きすることはありません。

ブラウザ拡張は Provider の endpoint、model、API key を設定せず、ローカル Herdr Runtime の統一 semantic service だけを呼び出します。Provider 設定は 2 層だけで、単一マシンでは mode-`0600` の `config.json`、全体共有では認証済み Cloudflare Worker route pool を使います。同じ typed/chat capability にローカル route があればローカルを優先します。両方の層で `name / capability / protocol / url / model / api_key` の共通 route JSON object を使い、shell/process environment は Provider 設定には使いません。Jev と LLM route は同じ rotation、deadline、cooldown、bounded failover を共有します。

Goal-aware automation ではさらに強い境界を維持します。Jev は既存の LLM Goal Supervisor に、`can_continue`、`needs_human`、`waiting_external`、`task_completed`、`needs_handoff` の 5 つの有界 semantic prior を一度に提供できます。これらの確率は advisory にすぎず、完了・待機・handoff・人間の判断境界・uncertain delivery については Work Memory/TODO evidence と deterministic runtime guard が引き続き authoritative です。

## 復旧は evidence-first

ブラウザが停止していることは、サーバーがリクエストを決して受け付けなかったことを意味しません。

user message、tool mutation、assistant 応答は、DOM が古いままでもサーバー側ではすでに進行している可能性があります。

したがって復旧の順序は：

```text
browser appears stalled
        │
        ▼
best-effort same-origin conversation snapshot
        │
        ├─ server ahead ───────► safe reload
        ├─ request not accepted ► bounded Retry
        ├─ server stalled ─────► wait, then one reload
        └─ unknown ────────────► fail closed
```

delivery が不明であることは、元のタスクを盲目的に再送する正当化には決してなりません。

## stale view：server が DOM より先に進んでいる

拡張は、最後に可視の assistant メッセージと、同じ origin の conversation snapshot を比較できます。message identity、テキスト長、完了状態、更新時刻です。

- **server ahead** — サーバーに、より新しいかより長いメッセージがある。ビューを同期するために一度 reload する。
- **server stalled** — サーバー自体がまだ進捗の無い未完了の assistant turn を示している。保守的に待ってから一度 reload する。
- **synced** — サーバーと DOM が一致している。復旧アクションは行わない。
- **unknown** — snapshot が利用できないか曖昧。fail closed する。

reload は既存のサーバー側 turn を明らかにするためのもので、ユーザーのタスクを再 submit するためのものではありません。

## 明示的な send-timeout エラー

ChatGPT が send-timeout/thread-error の card を表示した場合、拡張はまずサーバー側の conversation state を確認します。

- `current_node` がすでに assistant メッセージへ移っている場合、リクエストは受け入れられています。Retry は tool 作業を重複させ得るため、より安全なアクションはビューの reload です。
- `current_node` がまだ user メッセージである場合、ChatGPT 自身の Retry を一度だけ使えます。
- delivery を判定できない場合、別の user turn を作るよりも、有界なビューの同期を優先してください。

Retry と reload の予算は有限です。復旧を使い切った場合は無限ループではなく、明示的な failure/rollover の推奨になります。

## 中断されたレスポンスストリーム

assistant が返信を始めたのに、ページが切断されたストリームを報告する場合、エラーのプレースホルダ自体は progress ではありません。progress の時計を進めるのは、assistant テキストの増加か signature の変化だけです。

保守的な stall window の後、かつページがそれ以外は安全な場合にのみ、拡張は既存のサーバー側 turn を再同期するために一度 reload することがあります。元のタスクを再送することはありません。

## ページ健康の自己復旧：stall、メモリ、429

バージョン 0.1.63 は、以前は診断専用だった UI-pressure meter を、厳密に有界なページ健康復旧レイヤーへ接続します。ChatGPT/React が所有する履歴 DOM を削除することはありません。React の背後でそれらのノードを削除すると、フレームワークツリー、イベントハンドラ、virtualization 状態、実際の DOM が同期ずれを起こし得ます。ページ runtime 全体を回収する必要があるときは、document、React tree、JS heap をまとめて再構築する制御された reload のほうが安全です。

固定ウィンドウの O(1) シグナルは、MutationObserver callback rate、watcher tick rate、timer drift、Long Task、そして Chromium が公開する場合は JS heap 使用量です。単一のスパイクは観測のみです。

- active な turn が reload 対象になるのは、持続的なページ負荷と実際の assistant stall が続き、かつ同じ origin の conversation snapshot が `current_node` を **finished assistant** と証明した場合だけです。この証明があって初めて、古い Stop/streaming の UI ビットを live な作業ではなく renderer の問題として無視できます。
- critical な heap 負荷が対象になるのは、静止期間の後だけです。手動の composer テキスト、tool 実行、permission card、不確実な delivery は常に reload をブロックします。
- レベル 1 は最大で一度の durable な `location.reload()` です。その refresh を経ても同じ健康障害が残る場合、レベル 2 は最大で一度の sender-scoped な `chrome.tabs.reload(tabId)` です。background worker は、実際の `sender.tab`、同じ conversation、Auto 有効、一致する durable な pending レコードだけを受け入れ、ナビゲーションの前に executed-at を永続化するため、MV3 worker の再起動が reload loop を作ることはありません。
- 両方のレベルを使い切ると、以降の reload を停止し、代わりに制御された conversation rollover を推奨します。

HTTP 429 は逆の種類のシグナルです。**429 は backoff 専用で、Retry/reload のトリガーには決してなりません。** 可視の rate-limit エラーや Resource Timing の 429 は、`30s → 60s → 120s` の上限付き cooldown に入ります。自動復旧はその cooldown の間に追加のページ/API/attachment トラフィックを生まず、rate-limit の増幅ループを避けます。

## context pressure と自動 rollover

長い Herdr セッションは、可視テキスト、MCP payload、Project instruction、隠れた system context を蓄積し得ます。ChatGPT は古い DOM ノードも virtualization するため、現在の DOM が短いことは conversation が短い証拠にはなりません。

拡張は保守的な pressure シグナルを使います：

- 可視の user/assistant token の概算。
- まだ観測できる `conversation-turn-N` index の最大絶対値。
- 永続化された単調増加の message-count floor。
- ページに可視でない Project/system/tool payload 用に確保された余裕。

高 pressure は rollover を対象にするだけです。自動 handoff には依然として安全な境界が必要です。Project の Auto オン、binding された workspace が存在する場合は working でないこと、stream/tool/permission card が無いこと、未送信の手動ドラフトが無いこと、不確実な delivery が無いこと、他に進行中の handoff が無いことです。handoff 自体は workspace binding を必要とせず、durable continuity と現在の対応 conversation identity があれば開始できます。

## fail-closed な handoff

```text
old Project conversation
        │ generate compact packet with transfer id
        ▼
new conversation in the same Project
        │ submit seed packet
        ▼
verify new conversation id + seed marker
        │
        └── only then switch Project active_conv_key
```

Project/workspace binding と `continuity_id` は安定したままです。新しい conversation が検証されるまで、古い active conversation が権威であり続けます。z.ai は依然として conversation スコープなので、その binding は対象の seed が確認された後にのみ移動します。

新しいタブを開くだけでは不十分です。seed の送信を試みるだけでも不十分です。seed の delivery が不確実な場合、古い binding はその場に留まり、transfer は復旧可能なままです。

## handoff packet に含めるもの

有用な packet は次を保持します：

- 現在の目標。
- 完了した作業。
- 重要な決定。
- 未完了の作業。
- 既知の workspace/path/branch/commit/task の識別子。
- 安全上の制約。
- 推奨する次のアクション。

runtime や Git の状態がまだ最新であることを **保証しません**。新しい conversation は mutation の前に、live な Herdr/Git/runtime の状態を再 inspect しなければなりません。

## handoff（引き継ぎ）

Handoff は、現在の conversation の管理が難しくなる前の自然な作業境界で有用です。**ページ内 HUD** に一つのプロダクトアクションがあり、Side Panel はそれを複製しません。同じ canonical package は、WebChat の self-handoff と、ユーザーがプロンプトを新しく開いた conversation へコピーする使い方も支えます。

これは binding された ChatGPT Project の conversation と、安定した z.ai の `/c/<chat_id>` conversation でサポートされます。Handoff は Auto オン/オフのいずれでも開始でき、対象の conversation は source の Auto 状態を継承します。transfer が active な間は、source からの自動 wake が一時停止します。ChatGPT では cutover が Project binding の active conversation target だけを変更し、z.ai では conversation スコープの binding を移行します。workspace に active な working agent がいてはならず、settled/wake の delivery が cutover と競合しないようにします。

ChatGPT の場合、Rust はまず durable な Continuity Journal を解決し、読み取り専用の `herdr_mcp.browser_handoff.prepare` を呼びます。返される package には、既存の `continuity_id`、正確な `source_url`、任意の `work_chain_id`、最小限の target context、一つのコンパクトなメッセージが含まれます。`automatic_delivery.params.message` と `manual_delivery.copy_prompt` はバイト単位で同一のメッセージです。自動配送はそれを変更せずに `browser_session.create` へ渡し、新しい conversation は `continuity.resume` を呼ぶことから始め、その後 live な workspace / Git / runtime の状態を再確認します。ブラウザ制御が利用できないと確認された場合、HUD はすでに準備済みの package から **Copy Prompt** を公開します。`seed_uncertain` / 不確実な mutation outcome は、reconciliation が最初の delivery が適用されなかったと証明するまで Copy Prompt を公開しません。これにより Herdr が推測で二つ目の conversation を作ることはできません。自動配送が Herdr の execution/result フィールドを一切返さない場合、その outcome は workstation 実行が行われた証拠を何も提供しません。すでに準備済みの Copy Prompt を使うか、dispatch が存在する場合はその exact dispatch を再観測してください。Herdr が報告する不確実な delivery は reconciliation 専用のままで、自動でリプレイされることはありません。既存の provider 固有の legacy transfer は互換の復旧契約を保ちますが、新しい durable な ChatGPT handoff には二つ目の prompt builder がありません。

z.ai のサマリー/seed 制御メッセージは raw channel を使うため、JSON→MCP の coding task として再度ラップされることはありません。

## Auto が既定でオフである理由

Continuity はメッセージを能動的に submit し、一部のページアクションを処理し、conversation identity を変更し得ます。そのため新しい scope は既定で Auto オフです。

意図する進行は **まず観察し、次に自動化する** ことです。

## 実際のタスクで continuity を検証する

意味のある UAT では次を検証すべきです：

1. 意図したブラウザ scope が意図した workspace に binding されていること。ChatGPT Projects では、安定した Project binding とその active conversation target を別々に検証します。
2. 正しい scope で Auto が有効であること。
3. 実際の agent タスクが working に入ること。
4. 新しい出力がスパムなしで progress wake を生むこと。
5. workspace の settle が Web planner を wake すること。
6. reload/ブラウザ再起動後も正しい binding が保たれること。
7. 明示的な handoff が、新しい seed が検証された後にのみ ChatGPT Project の active target を変更すること（conversation スコープのサイトでは確認後に binding を移行します）。確認された delivery 失敗は同じ canonical な Copy Prompt を公開し、不確実な delivery は公開しません。

実装履歴は [CHANGELOG](../../../CHANGELOG.md) にあります。このページは現在の挙動を説明します。
