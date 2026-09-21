# ブラウザコントロールセンター

*Chrome Side Panel から実際の Herdr 現場を観測する。*

ブラウザコントロールセンターは、Herdr の workspace、ペイン、ターミナル、Agent の状態を Chrome Side Panel に持ち込みます。

これはブラウザに無制限の shell アクセスを与えるための近道ではありません。より根本的な問題を解決します:

> Web AI、ローカル Agent、テスト、ターミナルが同時に動いているとき、実際のワークステーション状態をどう見える状態に保ち、次の人間の操作対象を明示するか？

現在のコントロールセンターはコンパクトなレイアウトを採用しています。ヘッダーは Herdr の接続/実行状態と少数のグローバル操作だけを保持し、対応するアクティブページは `ChatGPT · 3 bound` のような 1 行に縮約され、workspace と pane の行が UI の主体になります。ペインをクリックするとそのアクションがインラインで展開されます。`Send instruction`（指示を送信）は Herdr `agent.prompt` を通じて実行され、`Adjust current task`（現在のタスクを調整）は native steer が告知されている場合にのみ表示され、作業中の Agent は `Stop task`（タスク停止）を公開し、terminal-only のペインは `pane.send_input + Enter` を通じたフェンス付き `Run command`（コマンド実行）アクションを公開します。

## ブラウザ Continuity との違い

拡張には現在、関連しているが役割の異なる 2 つの操作サーフェスがあります:

| サーフェス | 主な問い | 入口 |
|---|---|---|
| HUD / Continuity | このページは何をしていて、Herdr は何をしていて、Auto と 3 つのプリセット会話アクションのどれを実行すべきか | 対応する Web AI ページ内 |
| Control Center | どの Project / conversation がアクティブタブで、何にバインドされ、ローカルで何が起きていて、どのペインが明示的なターゲットか | Chrome Side Panel |
| Options | 低頻度の timing / 言語 / integration 設定は何を適用すべきか | Control Center の Settings（設定） |

HUD は意図的に**第 2 のコントロールパネルではありません**。no drawer（引き出し）も workspace picker も binding エディタも timing フォームもローカル Herdr mutation コントロールも持ちません。表示するのは Web 状態、Herdr 状態、コンパクトな binding バッジ 1 つ（`🔗N`）、Auto、3 つのプリセット進行アクション、そして Manual handoff です。これらのアクションが現在の Web 会話に対して作用するからです。ペインと Agent の詳細は Control Center 側に留まります。

Control Center は**アクティブページ identity、binding / unbinding、詳細なワークステーション状態、明示的なローカルターゲット選択**を所有します。Manual handoff（手動引き継ぎ）は意図的にページ内 HUD が所有します。

両者は同じ Native Messaging / ローカル IPC 信頼経路を共有しますが、1 つの状態機械ではありません。

## コントロールセンターを開く

ブラウザツールバーの Herdr 拡張アイコンをクリックします。Chrome は **Browser Control Center** の Side Panel を直接開きます。中間の拡張 Popup はありません。

パネルは Options とページ内 HUD と同じ拡張言語設定に従います。現在の UI ロケールは:

- English;
- 簡体字中国語;
- 日本語。

## Worker デバイス

Control Center はコンパクトな読み取り専用の Worker デバイス一覧も表示します。デバイスの認可、接続、ヘルスを分離して表示し、runtime のバージョン / generation と last-seen の鮮度も含みます。ローカルで接続中のコンピュータは明示的にマークされ、runtime が証明できる場合はローカル Link の generation 不一致も表示されます。

このビューは Chrome に Worker credential を与えません。Side Panel は extension service worker に問い合わせ、service worker が既存の Native Messaging / ローカル IPC 経路を使います。ローカル runtime が現在登録済みのデバイス credential で Worker を読み取り、サニタイズ済みのデバイス要約だけを返します。登録済みデバイスに owner/member 階層はありません。このマシンに利用可能な fleet credential がない場合、パネルはブラウザにより広い権限を与えるのではなく、明示的な権限の説明を表示します。

デバイス一覧は Control Center を開いたとき、明示的な Refresh（更新）時、および古い非表示パネルが再び表示されたときに更新されます。固定のポーリングループは追加しません。

## Current page はアクティブなブラウザタブに追従する

最上部の **Current page**（現在のページ）カードは、ブラウザコンテキストとローカル状態をつなぐ橋渡しです。既存の binding 権威にアクティブな Chrome タブを問い合わせ、次を表示します:

- 対応しているサイト;
- 存在する場合の ChatGPT Project identity;
- 存在する場合の conversation identity;
- その Project / conversation に現在バインドされている workspace の数;

Bind / unbind は Current page カード内に chip と selector の形で二重化されなくなりました。唯一の経路は**下の各 workspace 行にある binding toggle** です。これによりライブ状態とページ binding が同じ場所で読み取られ、変更されます。

Chrome タブの切り替えやアクティブタブのナビゲーションは、tab activation / navigation イベントからこのカードを更新します。固定のポーリングループはありません。バインド済み workspace はローカル workspace 一覧の先頭に移動し、ハイライトされたままになります。

これは明示的な Pinned Target を**再ターゲットしません**。アクティブページ binding が答えるのは「どのローカル workspace がこの Web コンテキストに属するか」であり、Pinned Target が答えるのは「将来の人間による操作がどのペインを対象にするか」です。

## ライブ状態の取得元

Control Center は固定間隔でページ DOM をポーリングしません。

現在のデータ経路:

```text
Herdr workspace / pane / agent state
        ↓
herdr-mcp Rust runtime
        ↓ local IPC / push events
Extension service worker
        ↓ one snapshot + incremental events
Chrome Side Panel
```

最初の表示または再接続時に権威ある snapshot を 1 回取得し、その後は次のような増分ライフサイクルイベントを消費します:

- `workspace_upsert` / `workspace_removed`;
- `pane_upsert` / `pane_removed`;
- Agent の working / settled の変化。

Side Panel が非表示の間はレンダリング作業が削減されます。再び表示されたとき、またはイベントストリームが再接続したときに状態が reconciliation されます。

したがって実際のペインの作成/クローズは、定期的な UI ポーリングを待つのではなく、ライフサイクル更新として現れるはずです。

## Workspace Binding、Pinned Target、Herdr Focus はそれぞれ別の identity である

この区別はコントロールセンターの最も重要なインタラクション契約です。

| 概念 | 意味 | Herdr focus に自動追従するか |
|---|---|---|
| Workspace Binding | Web の Project / conversation がどのローカル作業コンテキストに属するか | いいえ |
| Pinned Target | 次の Control Center アクションが明示的にどのペイン / Agent を対象にするか | **いいえ** |
| Herdr Focus | 人間が Herdr で現在見ているペイン | はい |

例えば、ChatGPT Project が `wD7` にバインドされていても、Control Center は明示的に `wD7:p2` を pin しているかもしれません。

その後、人間が Herdr で `wD7:p3` にフォーカスしても、Control Center が黙って `p2` から `p3` に再ターゲットしてはなりません。

これは危険な種類のミスを防ぎます: **ユーザーはアクションが A を対象にしていると思っているのに、フォーカス変化によって B を対象にしてしまう。**

## Workspace 状態と current-page binding は 1 つのリストを共有する

Control Center は「workspace 状態」と「current-page binding」を別々の UI モジュールに分割しなくなりました。各 workspace 行は次を表示します:

- workspace ラベル / id;
- workspace の集約状態ドット;
- ペイン数と working 数;
- アクティブページがこの workspace にバインドされているか;
- 唯一の **Bind / Bound** toggle。

アクティブページにすでにバインドされている workspace は先頭に移動し、ハイライトされたままになります。workspace 本体のクリックはペインの展開/折りたたみだけを行い、binding toggle のクリックは bind / unbind だけを行うため、2 つの操作が互いをトリガーしません。binding mutation は UI 内で直列化され、連続クリックによる曖昧な中間状態を避けます。

binding は ChatGPT の Project identity とは独立したローカルプロジェクト identity も運びます。Git workspace の場合、runtime が Git common-dir メタデータから導出するため、メイン checkout と linked worktree は同じローカルプロジェクトに属します。非 Git workspace の場合、canonical なローカルフォルダーがフォールバック identity になります。1 つの workspace がバインドされた後は、同じローカルプロジェクト identity を持つ新しく開かれた Herdr workspace が、その同じブラウザスコープを自動的に継承します。ChatGPT Project では Project スコープのままになり、通常の `/c/<id>` チャットでは conversation スコープのままで、別のチャットへ漏れません。

正確な `workspace_removed` ライフサイクルイベントは、クローズされた workspace の binding を即座に削除します。空でない権威ある workspace カタログは、MV3 worker がサスペンドされている間に見逃したイベントに対する補償的な reconciliation も行います。空の、または一時的なカタログは、すべての workspace がクローズされた証拠として扱われません。クローズ済みの履歴 workspace はオフライン行として合成されません。自動バインドされたローカルプロジェクトグループの 1 メンバーを unbind すると、そのグループは現在のブラウザスコープから削除され、即座の再継承を防ぎます。

展開された行は引き続きペイン単位の詳細を表示します:

- pane id;
- Agent 名、または terminal-only 状態;
- working / idle / done / blocked ステータス;
- 現在の Herdr focus マーカー;
- cwd / project root;
- Agent の経過時間;
- 最近のアクティビティ;
- 有界の直近サマリーまたは terminal title。

初期展開は有界なので、多数のプロジェクトがあるワークステーションでも読めない行の壁として開かれません。ブラウザタブの切り替えはバインディング順序とハイライトを更新しますが、Pinned Target を再ターゲットしません。

## 明示的なターゲットを pin する

ペイン行をクリックして pin します。

下部パネルは次のような明示的な identity を表示します:

```text
Pinned target
wD7 / wD7:p2 / pi
working · revision ...
```

pin されたターゲットは拡張のローカル状態に永続化され、snapshot と再接続の後に再検証されます。

### ターゲットが stale になる理由

pin は例えば次の場合に fail closed になります:

- ペインが削除された;
- 同じ pane id が新しい Agent session に属するようになった;
- ターゲット revision が、同じ実行ターゲットとして安全に扱えない形で変化した。

パネルは代替を推測しません。読み取りやアクションプレビューを継続する前に、ユーザーが再びペインを選択する必要があります。

## 現在実際に実行されるアクション

パネルには現在、1 つの包括的な「preview-only」ルールではなく、4 種類の挙動があります:

| モード | 現在の挙動 | delivery セマンティクス |
|---|---|---|
| Details | 有界な読み取りを実行する | 読み取り専用 |
| Recent output | 有界な terminal-tail 読み取りを実行する | 読み取り専用 |
| Send instruction | 信頼された extension-only のローカルアクション経路と、既存の Herdr `agent.prompt` 信頼性カーネルを通じて**実行する** | `submitted`、`queued`、`rejected`、`uncertain`、`failed`。可能な場合は operation id / evidence を伴う |
| Steer current task | runtime が pin された provider の native steer を明示的に告知している場合にのみ表示される | 現在の active turn を停止せずにアクティブタスクをリダイレクトする。Agent Prompt へは決してフォールバックしない |
| Stop task | 現在 working の Agent にのみ利用できる。そのペインへリテラルな `Ctrl+C` を送る前に確認する | 現在の CLI turn/process を停止する。provider interrupt としては決して提示しない |
| Herdr API | Preview only（プレビューのみ） | この UI から任意の Herdr mutation は実行されない |
| Run command | terminal-only のペインに対してのみ、`pane.send_input` と `Enter` を通じて**実行する** | mutation の前に `target_revision` を再検証し、不確実な delivery を自動リトライしない |

### Inspect state（状態の検査）

`Inspect state` は構造化されたペイン状態を表示しつつ、潜在的に大きな最近の出力を有界にします。

### Read output tail（出力末尾の読み取り）

`Read output tail` はローカル runtime に有界な terminal tail を要求します。このリクエストは意図的に制限されており（おおよそ 40 行 / 4096 文字）、何時間も動いているターミナルが無制限の履歴を Side Panel に吐き出せないようにします。

### Send instruction: ターミナル注入ではなく、信頼できる Agent Prompt

`Send instruction` はインラインアクション領域を開いているペインを対象とし、次の経路だけを通ります:

```text
Side Panel
  → extension service worker
  → Chrome Native Messaging
  → mode-0600 herdr-mcp Unix socket
  → POST /extension/control/action
  → existing durable agent.prompt operation
```

この HTTP route は通常の TCP では意図的に使用できません。標準の herdr-mcp bearer を持つ呼び出し元でも `403` を受け取ります。これによりブラウザ制御の mutation サーフェスが公開のワークステーション API になることを防ぎます。

すべてのアクションは runtime が生成した `target_revision` を運びます。Rust は mutation の直前に live pane を読み直します。ペインが消えていた場合、ペイン背後の Agent/session が変わっていた場合、または runtime generation が変わっていた場合、リクエストは何も submit せずに `stale_target` を返します。

Prompt はまた、ブラウザ専用のリトライロジックを発明するのではなく、既存の `agent.prompt` 永続 idempotency レコードを再利用します。Side Panel は idempotency key を生成し、不確実な delivery を明示的に提示します。結果が不確実な場合は、再試行する前にライブ状態を確認してください。盲目的に再送しないでください。

### Steer current task: true steer を決して偽装しない

`Steer current task` は Prompt より意図的に厳格です。UI は `control_capabilities.steer.available` が true の場合にのみこれを表示します。`agent.prompt` に**フォールバックせず**、その結果を steer とラベル付けすることもありません。

Codex の場合、provider-native の同一ターン `turn/steer` には、pin された Herdr ペインからアクティブな app-server control endpoint、`threadId`、現在の `expectedTurnId` への権威あるマッピングが必要です。現在の Herdr pane/session メタデータはそのマッピングを公開していません。したがって working な Codex ペインは現在 `session_not_resolved` を報告し、idle な Codex ペインは `no_active_turn` を報告し、他の provider は `unsupported_provider` を報告できます。

ローカルの `~/.codex/ipc/ipc.sock` ファイルだけでは十分な証拠になりません。socket は stale かもしれず、別の client/session に属するかもしれず、対象の thread や expected active turn を識別しません。provider-native steer は、それらの identity がエンドツーエンドで証明できる場合にのみ有効化されます。

`Stop task` は別の、より狭いローカル制御経路です。`working` 状態の Agent にのみ有効で、確認を求め、`pane.send_keys(["C-c"])` を送ります。provider レベルの interrupt セマンティクスを主張せず、不確実な delivery の後に自動リトライもされません。別の stop を送る前にターゲット状態を確認してください。

### Run command: terminal-only かつフェンス付き

terminal-only のペインは展開時に `Run command` を公開します。これは任意の Herdr API アクセスではなく、ターゲット選択も迂回しません。Side Panel はペインの `target_revision` を運び、Rust は mutation の直前に live pane を読み直し、その後にのみ呼び出します:

```text
pane.send_input({ pane_id, text, keys: ["Enter"] })
```

ペインが消えていた、置き換えられていた、または Agent ペインになっていた場合、アクションは `stale_target` または `rejected` を返します。IPC/ネットワークの曖昧さは `uncertain` を返し、UI はコマンドを自動再送しません。この経路は隔離された実ターミナル UAT でカバーされており、選択したペインでテキストと `Enter` が実行されることを検証しています。

これが元の Issue #57 の曖昧さに対する直接の解決です: **キュー済み/prompt 済みの作業と同一ターンの steering は別々の outcome であり、決して別名ではありません。**

## Reliability kernel: メモリ、リクエスト圧力、タイムアウト復旧、reload ループ

Browser Control Plane の信頼性はアクションの delivery に限られません。拡張は Side Panel の作業と並行して計画されていたページ/runtime の保護をすでに備えています:

- **Side Panel に固定のポーリングループはありません。** パネルは snapshot を 1 回取得し、増分の workspace/pane イベントを消費します。
- **共有された 1 つの Herdr イベントストリーム。** workspace の観測が binding ごとにネットワークストリームを作ることはありません。
- **状態取得の重複排除。** 同時に走る freshness リクエストは合流し、`/push/state` トラフィックを増やしません。
- **MutationObserver / render の合流。** DOM のバーストは有界な UI 作業に畳み込まれ、mutation ごとに render/action をトリガーしません。
- **非表示ページのサスペンド。** 高コストな UI 作業はサーフェスが非表示の間は延期され、再び表示されたときに reconciliation されます。
- **保持出力の有界化。** terminal/output tail はクリップされ、長時間動くペインがブラウザ側で無制限の履歴を蓄積しません。
- **UI 圧力 / heap シグナル。** 復旧層は mutation rate、timer drift、ブラウザが公開する場合は JS heap pressure を観測します。
- **429 は backoff のみ。** レート制限応答はネットワークの backoff を延長し、ページリロードの嵐を引き起こしません。
- **evidence-first の返信復旧。** 送信タイムアウトや切断ストリームからの復旧は、リクエストを再試行すべきかビューを更新すべきかを決める前に、same-origin/server 状態を確認します。
- **強制リロードは最後の有界な復旧段階。** リロード要求は sender-scoped で、automation gate 付きで、ナビゲーション前に永続化され、durable cooldown/budget で保護され、同時要求は 1 つの winner を選出します。
- **reload loop はありません。** cooldown 中の繰り返し要求は拒否され、Auto がオフの会話はバックグラウンドリロードを強制できません。

これらのメカニズムは continuity と Browser Control Plane が共有する 1 つの信頼性層です。アクション経路は別のポーリングループ、heartbeat、リトライデーモンを追加しません。

## 任意の Herdr メソッドが依然 preview-only にとどまる理由

ブラウザ制御の難しい部分は、ターミナルにバイトを書き込むことではありません。難しいのは次の問いへの答えを保つことです:

- ターゲットはまだユーザーが選択した正確なオブジェクトか？
- 失敗したリクエストは配信されたのか？
- 再試行すると mutation が重複しないか？
- ペイン背後の Agent session は変わっていないか？
- delivery phase はブラウザリロードや MV3 service-worker の再起動をまたいで生き残れるか？

Prompt は Rust の target fencing と `agent.prompt` idempotency を再利用することでこれらの契約を満たします。ターミナル制御は現在、同じ target fencing を持ち、不確実な delivery の後に自動リトライしない、狭い 1 つの `terminal_input -> pane.send_input + Enter` 操作だけを公開します。任意の Herdr メソッド呼び出しは依然としてはるかに広い影響面を持つため、fail-closed / preview-only のままです。

## Runtime とイベントストリームの状態

上部のステータスは次を区別します:

- **Runtime unavailable** — 信頼できる現在の runtime snapshot が存在しない;
- **Runtime healthy · event stream reconnecting** — ライブイベント経路が再接続している間、既存の状態を表示できる。

再接続後、パネルは権威ある snapshot を再取得し、増分イベントを再開します。

## Control Center、HUD、Queue を一緒に使う

3 つのサーフェスを別々の層として扱ってください:

```text
HUD
  current web conversation: status, Auto, Continue / Check Herdr / LLM decide
  ↓
Control Center · Current page + Workspaces
  current page ↔ workspace binding plus live workspace / pane / agent truth
  ↓
Control Center · Local Herdr target
  explicit pinned pane and local Herdr reads / action preview
  ↓
Queue (ChatGPT composer)
  add the next user intent without interrupting the current reply
```

典型的なフローは次のとおりです:

1. Control Center を開き、実際の workspace と working なペインを確認する。
2. 必要ならペインを pin し、その最近の出力を調べる。
3. ChatGPT に戻り、Web planner が MCP / Herdr ツールを通じて実際の制御を行うようにする。
4. ChatGPT がまだ返信している最中に新しい要件が生じたら、ライブターンを中断する代わりに Queue を使う。
5. キューされた内容は、現在の返信が settle した後に次のユーザーターンになる。
6. 長時間の作業では、ブラウザ continuity engine が progress / settled / recovery / automatic handoff を維持する。HUD はページスコープの status、Auto、3 つのプリセット進行アクション、Manual handoff を公開し、binding とローカル Herdr 制御は Side Panel に留まる。

## ローカルセキュリティモデル

Control Center は既存の信頼されたローカル経路を使います:

```text
Side Panel
   ↓ Extension service worker
Chrome Native Messaging host
   ↓ Unix socket (0600)
herdr-mcp Rust runtime
```

パネルを開いても:

- Herdr bearer が Web ページに公開されない;
- 公開のワークステーションポートが開かない;
- 任意のページが無制限の shell になることがない;
- Herdr focus が明示的な target identity の代わりになることがない;
- stale なターゲットに対して mutation が継続しない。

## 現在のプロダクト境界

Control Center が現在含むもの:

- 一級の Chrome Side Panel エントリーポイント;
- 固定ポーリングなしのライブ workspace / pane ライフサイクル;
- Agent ステータスの提示;
- 明示的な pinned target;
- runtime-authoritative な `target_revision` と stale-target の fail-closed 挙動;
- 有界な状態 / 出力読み取り;
- durable な idempotency/outcome evidence を伴う、実行可能で信頼された `Prompt Agent`;
- 正直な capability outcome を返し、**Prompt のなりすましを行わない**、実行可能な provider `Steer Session` リクエスト;
- preview-only の任意 Herdr API / raw terminal コントロール;
- 共有されたメモリ/リクエスト圧力/タイムアウト/reload ループ保護;
- en / zh / ja UI。

Codex の true same-turn steer は、検証可能な pane → app-server endpoint → `threadId` → active `expectedTurnId` のマッピングを条件として引き続きゲートされています。その primitive が存在するまで、隠れたフォールバックではなく `session_not_resolved` が正しい outcome です。

## 関連ドキュメント

- [ブラウザ拡張の概要](extension.md)
- [ブラウザ Continuity](browser-continuity.md)
- [Auto-continue、リカバリ、handoff](browser-continuity.md)
- [JSON → MCP bridge](extension.md)
- [トラブルシューティング](troubleshooting.md)
