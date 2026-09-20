# ローカル Agent による WebChat 操作

*ローカルの coding Agent が herdr-mcp 経由で、対応する WebChat セッションを作成・継続・引き継ぎ・観測する方法。*

このページは**ローカル Agent** 側、つまり Pi、Codex、Claude など、この開発マシン上で動き、ChatGPT/WebChat に作業の一部を任せたい coding Agent 向けです。答える問いは一つです。

> 私はローカル Agent です。どういうとき herdr-mcp 経由で Web 会話を操作すべきで、実際に何ができて、どう進めれば identity・冪等性・delivery evidence を壊さないのか。

Web AI は Herdr のブラウザ能力の唯一の呼び出し元ではありません。このページは、ユーザーが個別の API 名を教えなくてもローカル Agent が正しい能力に到達できるようにするためにあります。

## 1. これは何か

Herdr-MCP には二つの方向があり、契約は別物です。

```text
Web AI（planner）                      ローカル coding Agent（planner/executor）
      │                                          │
      │ MCP + OAuth Connector                    │ herdr-mcp CLI
      ▼                                          ▼
                    herdr-mcp（制御と状態の境界）
                        ├─ Continuity / Work Memory
                        └─ Browser / WebChat control
                                    │ ローカルの信頼経路
                                    │（Native Messaging → 0600 socket）
                                    ▼
                        Chrome extension / 登録済み browser endpoint
                                    │
                                    ▼
                            ChatGPT / 対応 WebChat
```

- **Web AI → herdr-mcp → 開発マシン**：Web AI が計画し、herdr-mcp がこのマシンのツールを与えます。
- **ローカル Agent → herdr-mcp → WebChat/browser**：ローカル Agent が計画と実行を担い、herdr-mcp が Web 会話への制御された経路を与えます。

どちらも同じ runtime、同じ Continuity journal、同じ browser registry に着地します。新しいメッセージバスは導入されず、第二のタスク状態権威もありません。ローカル Agent は既存の Continuity journal、Work Memory パーティション、browser リソース、browser control plane を再利用します。

| コンポーネント | 役割 |
| --- | --- |
| ローカル coding Agent | *なぜ*・*いつ* Web 会話を使うかを判断し、タスクを組み立て、結果を検証する |
| herdr-mcp runtime | 制御と状態の境界：identity、consent/能力ゲート、冪等性、delivery evidence、Continuity、Work Memory |
| Chrome extension | ブラウザ側の実行・binding・wake・観測の境界。実際のページ操作を行い、観測結果を返す |
| Browser endpoint | 登録・同意済みで制御対象になり得るブラウザ |
| ChatGPT / 対応 WebChat | リモートの会話そのもの。ローカル shell でも端末でも**ありません** |

Web 会話はターン単位の遠隔コラボレータです。Herdr-MCP は境界付きのメッセージを送り、それが配送されたかを記録し、永続タスク状態は Continuity に置きます。ページではありません。

## 2. いつ使うか

ローカル Agent の典型的な用途：

- 同じ ChatGPT Project 内で新しい会話を開始し、作業を一箇所に保つ。
- すでに binding された WebChat 会話で作業を続ける。
- 既存の binding 済み session に次のメッセージを dispatch する。
- 依存する前に browser/WebChat session が生きているかを確認する。
- ローカル文脈が満杯に近いとき、または作業を Web AI 側へ渡すときに canonical handoff を行う。
- Continuity/Work Memory で境界付きに履歴を復元し、その後会話で続ける。
- ローカル Agent のコード変更後、Web AI planner に引き継ぐ。

複数 WebChat / 複数アカウントの範囲は、返された browser registry と同じ広さです。検査した endpoint について `herdr-mcp webchat resources` が返した `session_ref` だけが操作対象で、返らないものは addressable ではありません。明記がない限り実験的機能として扱うものはありません（[現在の境界](#11-現在の境界) を参照）。

## 3. 使わない場合

- 一般の Web サイトのスクレイピング、対応 WebChat 以外のページ。
- ファイルのダウンロードやマシン間のデータ移送（extension はファイル転送路ではありません）。
- Playwright / Selenium / AppleScript の汎用代替としての利用。
- アカウント認可、consent プロンプト、extension の制御スイッチの回避。
- Project / アカウント / 会話 / session 識別子の推測。
- ユーザーの非公開 ChatGPT 履歴本文の読み出し。ローカル journal に取り込まれていない会話は MCP だけでは取得できません。
- 無関係な作業でユーザーの通常ブラウジングを操作すること。

## 4. 前提条件

以下はすべて実際の前提であり、形式要件ではありません。

1. **このマシンの herdr-mcp runtime が健全であること。**
   ```bash
   herdr-mcp status
   herdr-mcp doctor
   ```
   ソース開発 runtime の場合、まず `herdr-mcp dev status` でチャネルを確認し、どの runtime バイナリと話しているかを把握してください。CLI は常に*現在有効な* runtime だけと通信し、リポジトリのビルド成果物は使いません。
2. **browser extension がインストールされ、その runtime に接続されていること。**
   ```bash
   herdr-mcp native-host status
   herdr-mcp extension standalone status
   ```
   ブラウザ操作は extension が**省略できない**場面の一つです。extension がブラウザ側の実行境界です（Web AI → 開発マシンの主要経路は extension なしでも動作しますが、こちらは違います）。
3. **WebChat 制御に同意した登録済み endpoint。**
   ```bash
   herdr-mcp webchat endpoints
   ```
   endpoint が `consent.webchat_control: true` を報告している必要があります。`consent.tool_bridge` と `consent.tool_bridge_workstation_mutation` は別機能（ページ内 Web AI ツール呼び出し）の独立スイッチで、ここでは不要です。
4. **extension が観測した、サインイン済み provider アカウント。** アカウント、ChatGPT Project（`space` リソース）、会話（`session` リソース）はこの endpoint 配下のリソースとして現れます。
5. **資格情報を手で扱わないこと。** `herdr-mcp` CLI は first-party のローカルクライアントであり、信頼されたローカル呼び出し grant を自身で付与します。runtime bearer、ブラウザ cookie、Connector 資格情報を表示・コピー・要求してはいけません。

ChatGPT Connector（OAuth MCP）は**もう一方の方向**です。ローカル Agent が binding 済み WebChat を操作するのに Connector は不要で、Connector を入れてもブラウザ操作権が自動で得られるわけではありません。

`HERDR_ENV` はこの面とは無関係です。ネイティブ Herdr の pane/tab/workspace 操作を制御するもので、`webchat`・`continuity`・`memory` には影響しません。

## 5. 能力の検出

ハードコードされた API 名や記憶の中の `*_ref` から始めないでください。まず現在の能力と identity 集合を検出します。

```bash
# 1. 登録・同意済みのブラウザはどれか
herdr-mcp webchat endpoints --limit 5

# 2. その endpoint 配下に何があるか
herdr-mcp webchat resources --kind account
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF

# 3. このオブジェクトは何で、いま操作可能か
herdr-mcp webchat inspect SESSION_REF
```

読み方：

| フィールド | 意味 |
| --- | --- |
| `endpoint_ref` | 同意済みの browser endpoint。ref は不透明なハッシュで、合成・改変してはいけません |
| `resource_ref` | 正確なアカウント / `space`（Project）/ `session`（会話）の identity |
| `parent_ref` | 階層：account → space → session |
| `observation_generation` | extension が報告した generation。`--expected-generation` に渡します |
| `consent.webchat_control` | この endpoint を操作してよいか |
| `actuation_available` / `actuation_reason`（`inspect` 由来） | *その inspect 呼び出し*の能力であり、汎用の変更前チェックではありません。変更の真実は操作が返す delivery state です |

`herdr-mcp webchat resources` は `actuation_evaluated: false` と `actuation_reason: "not_evaluated_by_list"` を返します。リスト結果は健全性や consent の signal ではありません。

私有メソッド `herdr_mcp.browser_endpoint.inspect` は extension が宣言した能力操作（`capabilities.operations`）と入力契約（テキストのみ、サイズ上限、添付非対応）も公開します。機械可読な能力詳細が必要なときに使い、リソース単位の結論には `herdr-mcp webchat inspect` を使います。

**現在サポートされる browser 変更操作**（それ以外は fail closed で `code: "unsupported"` を返します）：

| 操作 | CLI | 状態 |
| --- | --- | --- |
| `browser_session.create`（ChatGPT、reasoning effort なし、required apps なし） | `herdr-mcp webchat create` | 対応 |
| `browser_dispatch.submit`（通常メッセージ） | `herdr-mcp webchat send` | 対応 |
| `browser_dispatch.status` | `herdr-mcp webchat dispatch-status` | 対応（読み取り専用） |
| `browser_session.archive` | `herdr-mcp webchat archive` | 対応 |
| `browser_session.archive_status` | `herdr-mcp webchat archive-status` | 対応（読み取り専用の provider archive 状態照合） |
| `browser_session.open` | `herdr-mcp webchat open` | 対応 |
| `browser_dispatch.stop` | — | 対応する私有メソッド、CLI ラッパーなし |
| `browser_message.append` | — | 非対応 |
| `browser_composer.set_reasoning` / `set_apps` | — | 非対応 |
| `browser_space.create` / `browser_space.open` | — | 非対応 |
| `reasoning_effort` または `required_apps` 付き `dispatch.submit` | — | 非対応（通常呼び出しは対応） |
| `herdr_mcp.browser_handoff.prepare` | `herdr-mcp webchat handoff` | 対応（canonical packet、任意で自動配送） |

能力が無い場合は正直に報告し、別の自動化スタックへ黙って切り替えないでください。

## 6. 基本ワークフロー

### ワークフロー A —— 既存の WebChat/browser session を確認する

```bash
herdr-mcp webchat endpoints --limit 5
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF --limit 10
herdr-mcp webchat inspect SESSION_REF
```

実際に使った identity チェーン（endpoint → account → Project（`space`）→ session）と `observation_generation` を報告してください。session ref / Project ref はこのマシンに束縛された値（`br_*`/`bep_*`）で、可搬な名前ではなく、ラベルから推測してはいけません。

関連する Work Memory パーティションがある場合、その locator（`project_ref`、`repo_id`、`work_chain_id`）は `herdr-mcp memory` / Continuity の結果から得ます。browser ref から得ることも、リポジトリパスから合成することもありません。

### ワークフロー B —— 既存 Project 内に新しい会話を作る

```bash
herdr-mcp webchat create \
  --endpoint-ref ENDPOINT_REF \
  --provider chatgpt \
  --account-ref ACCOUNT_REF \
  --space-ref SPACE_REF \
  --display-label "短いタスクラベル" \
  --message "新しい会話の最初のメッセージ" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]
```

補足：

- `--space-ref` は ChatGPT Project です。アカウント既定の会話を本当に意図する場合のみ省略します。
- `--display-label` はラベルであり identity ではありません。identity は返された session ref です。
- `--expected-generation` は対象スコープで観測した generation を渡します。古い generation は誤った対象に適用されず拒否されます。
- `--work-chain-id` は既存の Work Memory チェーンへの紐付けです。捏造しないでください。
- 一つの意図 = 一つの idempotency key。送信前に記録し、再試行判断でも同じ key を使います。
- 現在非対応の能力（`browser_space.create` 経由の Project 作成、composer の reasoning/apps 選択など）を別ツールで模擬してはいけません。

成功時は `ok: true` と作成されたリソース／delivery state が返ります。自分で組み立てた URL ではなく、返された `session_ref` を報告してください。

### ワークフロー C —— メッセージを dispatch し結果を観測する

```bash
herdr-mcp webchat send \
  --session-ref SESSION_REF \
  --message "次の指示" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]

herdr-mcp webchat dispatch-status DISPATCH_ID
```

`send` は dispatch オブジェクトを返します。

| フィールド | 意味 |
| --- | --- |
| `dispatch.dispatch_id` | 永続的な操作 identity。以降の読み取りはすべてこれを使います |
| `dispatch.delivery_state` | `applied`、`not_applied`、`uncertain`、`rejected`、`browser_offline`、`resource_unavailable`、`stopped` |
| `dispatch.execution_state` | 対象ターンがまだ実行中か |
| `dispatch.result.settled` / `assistant_message_ref` / `evidence_id` | そのターンの永続的な settlement 証拠 |
| `replayed` | 同じ idempotency key で記録済み dispatch を返した場合 `true` |

タイムアウト後の「再開」は再送**ではありません**。同じ `dispatch_id` で `dispatch-status` を読み、`uncertain` なら `herdr-mcp webchat inspect SESSION_REF` と実際のページを再観測してから判断します。`browser_offline` / `resource_unavailable` は対象が現在到達不能という意味です。待って再観測し、生存確認のために idempotency key を変えないでください。`browser_session.create` では、返された証拠が `message_submitted=false` かつ `retry_safe=true` を明示的に証明する一時的な `resource_unavailable` に限り、再観測後に同じ key で再 actuation できます。Runtime はその reservation を `not_applied` として永続化します。`uncertain` は reconciliation 専用のままで、`source_session_unavailable` が自動的に別の対象へ切り替わることもありません。

実行中ターンの停止は別の私有操作（`herdr_mcp.browser_dispatch.stop`）で、現在 CLI ラッパーはありません。CLI しか無い場合はその境界を正直に報告し、停止したふりをしないでください。

### ワークフロー D —— Canonical handoff

canonical handoff の準備は読み取り専用の私有メソッド `herdr_mcp.browser_handoff.prepare` です。

```text
continuity_id（永続タスク状態）
   └─ herdr_mcp.browser_handoff.prepare { continuity_id, source_url, [objective], [work_chain_id], [handoff_id] }
        ├─ handoff.message                                   （唯一の canonical メッセージ）
        ├─ automatic_delivery.params = { source_url, message, work_chain_id }
        ├─ manual_delivery.copy_prompt  == handoff.message    （バイト単位で同一）
        └─ herdr_mcp.browser_session.create（automatic_delivery.params をそのまま渡す）
             └─ 対象会話の最初の手順は continuity.resume <continuity_id>
```

安全を担保する規則：

- `prepare` は**読み取り専用**で、登録済みのソース会話から対象ルート（account/Project）を導出します。endpoint、account、Project、generation、device id を先に列挙しません。
- 自動配送と手動の **Copy Prompt** は*同じ* canonical メッセージを使います。二つ目の handoff メッセージを書いたり、エンコード・難読化したり、transport を変えたり、拒否された payload を再帰的に包んだりしてはいけません。
- 対象会話は**既存の** `continuity_id` を `continuity.resume` で再開することから始めます。handoff は二つ目の Continuity チェーンを作らず、ページはタスク状態権威になりません。
- 再開後、対象は live な workspace / Git / runtime を再確認します。journal は履歴であり、live な真実ではありません。
- 自動配送が Herdr の execution/result フィールドを一切返さない場合、その結果だけからワークステーション実行を推測してはいけません。準備済みの Copy Prompt を使うか、明示的な dispatch が存在する場合はその dispatch を再観測します。
- delivery が不確実な間は、reconciliation で最初の試行が適用されていないと証明されるまで Copy Prompt 経路を開きません。したがって推測で二つ目の会話を作ることはできません。

**正式な入口**：

```bash
herdr-mcp webchat handoff \
  --continuity-id hc:... \
  --source-url 'https://chatgpt.com/g/g-p-.../c/...' \
  [--objective TEXT] [--work-chain-id ID] [--handoff-id ID] \
  [--idempotency-key KEY] [--prepare-only]
```

`webchat handoff` は上記 canonical 実装の薄いローカルラッパーであり、二つ目の handoff 実装ではありません。

- Web planner と extension HUD と同じ経路で runtime に canonical packet（`herdr_mcp.browser_handoff.prepare`）を要求します。
- その packet 自身の `automatic_delivery.params` を**そのまま** source ベースの `browser_session.create` に渡します。CLI がメッセージを組み立てたり、書き換えたり、再エンコードしたりすることはありません。
- `--source-url` は監査アンカーであり唯一のルート入力です。runtime が登録済み会話から既存 WebChat ルート（endpoint / account / Project）を解決するため、コマンドラインに routing id は不要です。
- `--objective` / `--work-chain-id` / `--handoff-id` は同じ `prepare` 入力に対応します。`--handoff-id` は packet に安定した identity を与えます。

出力は canonical packet と配送証拠です。

| フィールド | 意味 |
| --- | --- |
| `handoff` | canonical packet（continuity_id、source_url、message、work_chain_id、target_context） |
| `automatic_delivery.params` | canonical な作成パラメータ。packet とバイト単位で一致 |
| `automatic_delivery.attempted` / `completed` | 配送を試みたか、`applied` に到達したか |
| `automatic_delivery.delivery_state` / `reason` / `replayed` | runtime 自身の delivery 語彙と replay フラグ |
| `automatic_delivery.session_ref` / `dispatch_id` / `result` | 作成された会話と dispatch 証拠 |
| `manual_delivery.copy_prompt` | 同じ canonical メッセージ（手動継続用） |
| `instruction` | 実際に何が起きたかの平易な説明 |

冪等性：一つの logical handoff は一つの key を使います。`--idempotency-key` を渡さない場合、CLI は canonical な `handoff_id` を再利用します。配送状態を探るために key を変えず、`automatic_delivery` と、dispatch がある場合は `dispatch-status` を確認してください。CLI は uncertain な配送を自動再試行しません。

`--prepare-only` は配送をスキップし、packet のみを返します（`automatic_delivery.attempted=false`、`reason="prepare_only"`）。

**準備済みは配送済みではありません。** ソース会話が未登録、または browser control が利用できない場合でも packet と `manual_delivery.copy_prompt` は返り、`automatic_delivery.attempted=false` と runtime の理由が付きます。これは手動継続に使える結果ですが、**完了した handoff ではありません**。実際に会話が作成されたのは `automatic_delivery.completed=true`（つまり `delivery_state=applied`）のときだけです。`uncertain` はそのまま報告され、自動再試行はしません。

`herdr-mcp webchat create --source-url URL` は、正確な source-window affinity のために source ベースの経路を利用し、呼び出し側が endpoint/provider/account ref を指定する必要はありません。ローカル CLI は trusted Unix IPC 経由で Runtime に同じ canonical source resolver を使わせ、最新の exact route から trusted local grant を作ります。create payload の route 入力は引き続き `source_url` だけで、Runtime は mutation 前にもう一度解決・検証します。その間に route が変われば exact grant が一致せず fail closed となり、古い endpoint へ誤配送しません。明示的な direct create では endpoint/provider/account と generation が引き続き必要です。

## 7. ローカル Agent の例

ローカル Agent への依頼例：

> 「binding 済みの ChatGPT Project に新しい会話を作り、永続 continuity `hc:...` を引き継ぎ、対象会話にまず resume させてから、そちらで現在のタスクを続けて。」

Agent は次の順で進めます。

1. **能力検出** —— `herdr-mcp webchat endpoints`、`herdr-mcp webchat resources --kind space`、`herdr-mcp webchat inspect SPACE_REF`。`consent.webchat_control` が真であることと `observation_generation` を確認します。
2. **identity の解決** —— 返されたリソースから正確な `endpoint_ref` / `account_ref` / `space_ref` を選びます。推測せず、別マシン・別 Project の ref を再利用しません。
3. **永続状態の解決** —— `herdr-mcp continuity resume hc:...`（先に境界付きの `herdr-mcp continuity search ... --project-path <checkout>` でも可。ただし `confirmation_required` に従うこと）。これがタスク永続状態の唯一の情報源です。
4. **canonical handoff を実行** —— `herdr-mcp webchat handoff --continuity-id hc:... --source-url '<正確な会話 URL>'`（チェーンに work chain があれば `--work-chain-id` を追加）。canonical な準備と自動配送を一度に行います。`--prepare-only` は packet だけを返します。
5. **配送の検証** —— `automatic_delivery.completed` / `delivery_state` を読みます。`completed=false` なら何も作成されていません。`manual_delivery.copy_prompt` を使い、dispatch がある場合は `herdr-mcp webchat dispatch-status <dispatch_id>` で確認します。新しい `--idempotency-key` で再試行しないでください。
6. **報告** —— 正確な `session_ref`、delivery state、対象に依頼した内容を返します。`continuity.resume` は対象側で実行されること、準備済みは完了した handoff ではないことを明示します。

実際のアカウント id、トークン、本番 secret を計画・メッセージ・報告に含めないでください。CLI が返す ref は不透明な識別子で、CLI に戻したりユーザーに報告してよいものですが、資格情報ではありません。

## 8. 配送と再試行のセマンティクス

| 状況 | 正しい対応 |
| --- | --- |
| Herdr の実行/結果証拠がない | ローカル実行を推測しない。canonical な手動経路を使うか、dispatch がある場合はその exact dispatch を再観測する |
| `applied` | 変更は永続化済み。続行し、後続は dispatch/evidence identity を使う |
| `not_applied` | 何も配送されていない。再試行は自動ループではなく意図的な判断 |
| `uncertain` | まず再観測（`webchat inspect`、`dispatch-status`）。変更を盲目的に再送しない |
| `browser_offline` / `resource_unavailable` | 対象に到達できない。待って再観測し、元の意図で再試行する。生存確認のために idempotency key を回さない |
| `rejected` | その試行は拒否された結果として扱い、報告して自動再試行を止める |
| `stopped` | 意図的に停止された結果であり、再試行すべき失敗ではない |

追加規則：

- **一つの意図 = 一つの idempotency key。** 同じ key の再実行は記録済み dispatch（`replayed: true`）を返し、二重実行しません。
- **identity を合成しない。** account、Project、session、generation、Continuity/work-chain の識別子はすべて返された結果から取得します。
- **delivery evidence は楽観より優先。** タイムアウトは配送ではなく、エラー欠如も配送ではなく、端末や会話のスクロールバックは settlement 証拠ではありません。
- **変更は account スコープで直列化**されます。競合する変更を並列化しないでください。
- **第二の状態権威を作らない。** ページ、Project、会話は Continuity/Work Memory の代替になりません。

## 9. Continuity / Work Memory / Browser session の違い

| 概念 | 役割 | 混同しやすいもの |
| --- | --- | --- |
| **Continuity** | 会話をまたぐ永続タスク状態。一つの `continuity_id` の権威 journal | live なブラウザタブ、Work Memory パーティション |
| **Work Memory** | より精密な履歴パーティション：`project_ref` + `repo_id` + `work_chain_id` | browser session、単なるリポジトリパス |
| **Browser / WebChat session** | 現在の Web 会話の実行キャリア（`session_ref`） | 永続タスク状態、repo/work chain |
| **Browser endpoint** | 登録・同意済みで制御対象になり得るブラウザ | ユーザーアカウントや Project |
| **Browser extension** | ブラウザ側の実行・binding・wake・観測 | ファイル転送、agent runtime、汎用 RPA 層 |
| **Herdr workspace** | ローカル開発環境（pane、agent、端末、worktree） | Web 会話 |

典型的な失敗は、これらを一つの「session id」に潰すことです。`continuity_id`、`work_chain_id`、`session_ref`、`space_ref`、`endpoint_ref`、Herdr pane/workspace id は別の名前空間で、寿命も異なります。

## 10. トラブルシューティング

| 症状 | 確認すること |
| --- | --- |
| endpoint が全く無い | `herdr-mcp native-host status`、`herdr-mcp extension standalone status`、`herdr-mcp doctor`（STANDALONE チャネルなら `standalone-extension-load` の警告） |
| endpoint はあるが `consent.webchat_control: false` | その endpoint で extension の制御スイッチ／consent が未許可。ブラウザ側の操作で、CLI フラグではありません |
| account / Project が曖昧 | 再列挙し、返された ref で選ぶ。推測や「最新」での選択はしない |
| `browser_resource_not_found` / session が古い | `herdr-mcp webchat resources` を再実行。会話が閉じられた、アーカイブされた、または新しい観測に置き換わった可能性 |
| 同じ会話が 2 つの browser endpoint から観測された | canonical URL は**最新**の観測に解決されるため、ブラウザ profile / extension identity の切り替えで新鮮な会話が使えなくなることはありません。2 つの異なる session が同じ最新時刻を共有する場合だけ `browser_canonical_url_ambiguous` で fail closed します |
| dispatch タイムアウト | まず `herdr-mcp webchat dispatch-status DISPATCH_ID` を読み、次に会話を観測してから判断 |
| 変更の delivery が不確実 | まず再観測。新しい idempotency key で再送しない |
| `code: "caller_grant_missing"` | 信頼されたローカル経路にいない（例：生の TCP MCP クライアント）。`herdr-mcp` CLI を使う。grant は TCP で自称できません |
| `code: "unsupported"` | 現在の対応マトリクス外の操作（「能力の検出」参照）。模擬せず報告する |
| continuity は見つかるが browser session が無い | 先に永続状態を解決（`continuity resume`）し、対象セッションを作成／オープンする。ページ消失を履歴喪失とみなさない |
| session はあるが `work_memory` が `null` | Continuity 層で止まる。使える `project_ref`/`repo_id`/`work_chain_id` は無く、合成もできない |
| Connector との混同 | Connector は Web AI → 開発マシンの方向。ローカル WebChat 操作は extension → ローカル IPC → runtime で、Connector は不要 |

## 11. 現在の境界

存在しない能力に対して文書や実装を積み上げないよう、ここを明示します。

- **非対応の browser 操作**（runtime は `code: "unsupported"` を返す）：`browser_space.create`、`browser_space.open`、`browser_message.append`、`browser_composer.set_reasoning`、`browser_composer.set_apps`、および `reasoning_effort` か `required_apps` を伴う `browser_dispatch.submit`。
- **対応しているが現在 CLI ラッパーが無いもの**：`browser_dispatch.stop`、`browser_endpoint.inspect`、`browser_space.inspect`。runtime MCP の私有メソッド境界から到達できます。`browser_session.open` は `herdr-mcp webchat open` で利用できます。
- **Handoff**：canonical な準備経路は `herdr_mcp.browser_handoff.prepare` で、ローカル Agent は `herdr-mcp webchat handoff` から使います（再利用し、続けて source ベースの配送を行う）。Web planner と extension HUD は引き続き私有メソッドを直接呼びます。通常の `webchat create` も正確な source-window affinity のため `--source-url` を受け取れますが、handoff packet の生成や書き換えは行いません。
- **`ego-browser`** は開発/UAT インフラで、ユーザー依存でも、この control plane の代替でもありません。
- **公開されていないもの**：ユーザーの非公開 ChatGPT 履歴本文の読み出し、dispatch 契約での添付送信、任意の DOM アクセス、registry が報告しない provider。

これは roadmap の約束ではなく実装の事実です。依存する前に、インストール済み runtime で必ず確認してください。

## 関連ドキュメント

- [ブラウザ連続性](browser-continuity.md) — ページ側の continuity、Auto/Queue、手動 handoff と Copy Prompt。
- [Browser Control Center](browser-control-center.md) — Chrome Side Panel の workspace/pane/binding 状態。
- [ブラウザ拡張](extension.md) — extension の identity、ローカルセキュリティ境界、JSON → MCP bridge。
- [CLI リファレンス](cli-reference.md) — `herdr-mcp` の全コマンド面。
- [トラブルシューティング](troubleshooting.md) — runtime、link、ブラウザの診断。
