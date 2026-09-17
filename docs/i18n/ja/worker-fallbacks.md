# Agent への委譲

*ローカルの Agent に作業を委譲すべきときと、直接実行にとどまるべきとき。*

herdr-mcp は Web AI を高レベルの planner として扱います。ローカルの coding agent は交換可能な実行 worker であり、2 つ目のオーケストレーション層ではありません。

本ページは 2 つの実用的な問いに答えます:

1. どのタスクがローカル worker に委譲する価値があるか？
2. 望ましい worker が利用不可、停滞、または timeout したとき、すでに発生した可能性のある mutation を繰り返さずにどう切り替えるか？

## まず、そのタスクに agent が本当に必要かを判断する

多くの開発アクションは決定的です:

```text
ファイルを読む        → herdr_fs_read
コードを検索する      → herdr_fs_grep
Git を調べる          → herdr_git
正確に編集する        → herdr_fs_edit / patch
コマンドを実行する    → herdr_exec
長時間のテストを実行  → herdr_exec_start/read
```

独立した推論が実際に価値を加える場合にのみ委譲してください。たとえば:

- なじみのないサブシステムを理解し、実装案を提示する;
- 狭く自己完結した機能を実装する;
- 別の仮説を並行して調査する;
- 完了した diff を独立にレビューする;
- 複数の技術的アプローチを比較する。

ルールは単純です:

> 決定的な作業は直接行う。独立した推論から利益を得る作業を委譲する。

外部 host の結果は worker 選択のシグナルではありません。応答に Herdr の通常の実行/結果 evidence（`op_id`、`session_id`、`backend`、`pane_id`、`exit_code`、`execution`、`failure_origin`、`device_id`）が 1 つも含まれない場合、ワークステーションや子プロセスの実行を Herdr に帰属させないでください。Agent を選ぶのは、そのタスクが独立した推論または並行ローカル作業から利益を得るからだけであり、以前の呼び出しが外部 host に拒否されたからではありません。

実行 evidence と worker 選択は別々の判断として扱ってください:

```text
直接 Herdr tool
  ↓
Herdr の実行/結果 evidence はあるか？ ── yes ──> その delivery/execution evidence に従う
  │ no
  └─> ローカルでの実行を推測せず、報告するか製品が文書化した reconciliation/手動経路を使う

独立したタスクで推論/並行作業が必要か？ ─ yes ──> herdr_prompt 経由で既存のローカル Agent
  └─> planner が結果としての目標 state を検証する
```

Agent には高レベルのタスク契約を送ってください: 望ましい成果、許可された範囲、関連するプロジェクト/コンテキスト、検証基準、およびそれ以上の委譲を行わないこと。既存の認可や確認要件はすべて保持してください。Agent dispatch は、適した作業に対する独立した実行の選択であり、外部から拒否された呼び出しに対するリトライ機構ではありません。

## 推奨される worker の順序

| 優先度 | Worker の種類 | 典型的な入口 | 適した用途 |
|---|---|---|---|
| 1 | Herdr-native coding worker | `herdr_prompt` | 狭い実装、調査、レビュー |
| 2 | 他に利用可能な Herdr 管理下の worker | `herdr_prompt` | 代替実装、並行検証 |
| 3 | 外部の headless coding-agent CLI | `herdr_exec_start` | native worker が利用できない場合の境界付きフォールバック作業 |
| 人間 | 対話的 TUI / shell | 手動 takeover | 承認、復旧、複雑な診断 |

ブランド名やモデル名は長期的なルールではありません。worker が良いデフォルトとなるのは、次のことができる場合です:

- headless で予測可能に実行できる;
- 狭いタスク境界を受け入れる;
- 観測可能な state を公開する;
- 検証可能な結果を生む;
- planner が timeout の後にコードが変更されたかどうかを判断できる。

## なぜ Herdr-native worker が先か

Herdr 管理下の worker はすでに可視の workspace/ペイン lifecycle の中にいるため、Web planner は次を観測できます:

- working / idle / done state;
- ペイン出力;
- cwd;
- workspace の所有権;
- prompt delivery evidence;
- `idempotency_key` の挙動。

典型的な流れ:

```text
herdr_prompt
  ↓
herdr_since / herdr_inspect
  ↓
Git / tests の検証
```

これは、完全に独立した CLI プロセスよりも長期間にわたってオーケストレーションしやすいです。

## 外部 CLI worker が適する場所

一部の coding agent は headless CLI を公開しており、フォールバック worker として機能できます。

このモデルを使用してください:

```text
Web planner
  ↓
herdr_exec_start
  ↓
external coding CLI
  ↓
Git / tests
```

外部 CLI を別の planner にして、さらにどう委譲するかを決めさせないでください。Web planner が先にジョブを狭めるべきです。

良いタスク契約は次を述べます:

- 正確なリポジトリ;
- ファイルまたは機能の境界;
- 無関係な編集をしない;
- 完了基準;
- 検証コマンド;
- それ以上の agent 委譲を行わない。

## なぜ外部 coding agent は長時間実行の exec セッションを使うべきか

coding agent は、最終的な自然言語の summary を出力するよりずっと早くコード変更を終えることがあります。

同期コマンドでは、失敗モードは次のようになります:

```text
CLI はすでにコードを変更した
      ↓
最終的なモデル summary を待つ
      ↓
client timeout
      ↓
失敗とみなす
      ↓
同じタスクを再送信する  ← 危険
```

推奨:

```text
herdr_exec_start
  ↓
herdr_exec_read
  ↓
Git / tests を調べる
  ↓
実際にキャンセルが必要なときだけ herdr_exec_kill
```

プロセスの timeout と coding タスクの失敗は同じものではありません。

## timeout の後は、リトライの前に事実を確認する

coding worker の timeout では、いずれの場合も次の順序で:

```text
1. worker/ペイン/プロセスの state を確認する
2. git status
3. git diff
4. 対象ファイルを確認する
5. 関連するテストを実行する
6. その後にのみ continue / fix / cancel / retry を選ぶ
```

関連する diff がすでに存在するなら、mutation は少なくとも部分的に発生しています。

正しい次のアクションは通常、検証するか、既存の worker に完了を依頼するか、Web planner に小さな残りを修正させることであり、元のタスク全体を再送信することではありません。

## 本当に停滞した worker をどう見分けるか

「数分間最終回答がない」ことを唯一のシグナルにしないでください。より良い evidence には次があります:

- プロセスまたはペインは active のままだが出力が変化しなくなる;
- 関連する Git diff が現れない;
- プロセスの活動がそれ以上進まない;
- worker が新しい evidence を生まずに同じ読み取りを繰り返す;
- タスクがその範囲に対して妥当な budget を超えている;
- 新しい情報がないまま critical path がブロックされている。

その場合は:

1. もう一度 state を読む;
2. mutation evidence がなければキャンセルする;
3. 決定的なツールまたは別の worker で続行する。

すでに時間を使ったからという理由だけで無期限に待ち続けないでください。

## 並行 worker が役立つとき

良い並列性:

```text
worker A → 実装
worker B → 独立したレビュー
```

または:

```text
worker A → ブラウザ層を調査
worker B → サーバー層を調査
```

悪い並列性:

```text
worker A、B、C がすべて同じファイルを編集する
```

ただし、それぞれが分離された worktree で作業し、Web planner が最終統合の所有権を明示的に持つ場合は除きます。

## worktree で本当の並行開発を分離する

複数の worker がコードを編集する必要がある場合:

```text
main worktree
    │
    ├─ worktree A → implementation
    └─ worktree B → alternative / review fix
```

これにより次を避けられます:

- dirty-file gate の競合;
- worker 同士が互いの変更を上書きする;
- どの diff をどの worker が生んだかが不確かになる;
- ある worker の reset/format 操作が別の worker に影響する。

Web planner は merge、cherry-pick、手動統合を選ぶ前に diff とテスト evidence を比較します。

## レビュー worker をデフォルトの主編集者にしない

独立したレビューは、独立しているからこそ価値があります。

推奨する順序:

1. 主経路で実装する;
2. 決定的なテストを実行する;
3. diff を独立したレビュー worker に渡す;
4. Web planner が指摘が妥当かを判断する;
5. 小さい問題は直接修正し、より大きな修正は有用な場合にのみ委譲する。

レビュー担当が最初から実装全体を所有していると、その独立性の多くが失われます。

## アップグレード後に外部 worker を再検証する

外部 agent CLI は急速に進化するため、長寿命のドキュメントがバージョン固有の前提を固定すべきではありません。

アップグレードの後に再確認する:

1. `--version`;
2. headless/非対話の help;
3. ツールを使わない回答の smoke テスト;
4. 一時的な Git リポジトリでの小さな編集;
5. Git evidence が timeout 後の mutation を明らかにするか;
6. worker がそれらに依存する場合の profile/plugin の読み込み。

その後にのみ、その worker を自動化された critical path に戻してください。

## 人手による takeover は第一級の能力

一部のタスクは人間が扱う方が適しています:

- OAuth またはログイン;
- セキュリティ承認;
- 対話的な TUI ワークフロー;
- 視覚的に複雑な state;
- 高リスクの外部 mutation;
- 挙動が予測不能になった worker。

Herdr の可視の workspace/ペイン モデルにより、手動での観測と takeover は緊急時の逃げ道ではなく、アーキテクチャの一部となっています。

## 推奨される委譲フロー

```text
Inspect
  ↓
決定的なツールで実行できるか？
  ├─ yes → fs/git/exec → Herdr の delivery/execution evidence を読む → 目標 state を検証する
  └─ no
       ↓
   1 つの狭い worker タスクを定義する
       ↓
   dispatch
       ↓
   since / process read
       ↓
   Git + tests
       ↓
   有用な場合は独立したレビュー
       ↓
   Web planner が次の一手を判断する
```

## 最終ルール

Worker は交換可能な実行リソースです。プロジェクトの state が真実の源です。

完了とは、単に次のことではありません:

> 「agent が終わったと言った。」

完了とは:

- diff が正しい;
- テストが通る;
- runtime state が期待と一致する;
- 重要な副作用がすべて説明されている。

これにより herdr-mcp は、オーケストレーションアーキテクチャを 1 つのモデルや CLI に縛ることなく、さまざまな agent と協働できます。
