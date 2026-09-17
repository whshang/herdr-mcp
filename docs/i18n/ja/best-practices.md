# ベストプラクティス

*Web が計画し、ワークステーションが事実を提供する。*

最も信頼できる herdr-mcp のワークフローは、次の一つのルールに従います。

> 目標、順序付け、意思決定は Web モデルが担います。決定論的なワークステーション作業は直接実行します。ローカル Agent は、独立した推論や並列作業が価値を加える場合にのみ使用します。

これは両方の極端を避けます。すべての action を agent タスクにしてしまうことと、長時間動く開発環境をステートレスな shell API のように扱うことです。

## 1. mutation の前に live state を調べる

handoff packet、以前の assistant メッセージ、古いターミナル出力は履歴的な文脈であり、現在の状態の証明ではありません。

まず次を確認します。

- 現在の Herdr workspace / ペイン / Agent;
- 対象の Git root;
- Git status / diff;
- タスクがそれに依存する場合は、関連する runtime / service の health。

典型的な入口:

```text
herdr_inspect
  ↓
herdr_git status
  ↓
herdr_fs_read / grep
```

長い空白、runtime の再起動、ブラウザ handoff の後に会話を再開した場合、このルールはさらに重要になります。

## 2. 決定論的な作業は直接行う

結果がすでに機械的に定まっている操作を agent に依頼しないでください。

優先する方法:

```text
ファイルの read/search   → herdr_fs_*
Git の事実               → herdr_git
正確な edit/patch        → herdr_fs_edit / patch
短いコマンド             → herdr_exec
長い test/build          → herdr_exec_start/read
```

これによりモデルの context を節約し、レイテンシを下げ、Web planner に直接のエビデンスを与えます。

## 3. 独立した worker に値する作業だけを委譲する

タスクが**独立した推論、実際の並列性、または独立したレビュー**から本当に利益を得る場合に委します。既知のファイルに対する決定論的な edit、Git クエリ、テスト実行は直接行うべきです。

境界の明確な委譲には、明確な問題、作業ディレクトリ、許可される mutation スコープ、受け入れエビデンス、停止条件があります。統合の責任は引き続き Web planner が持ち、worker の完了後に Git、テスト、runtime の事実を再確認します。

Agent の選択、代替 worker の順序、長時間実行、timeout / retry の安全策には単一の SSOT があります。[Agent delegation](worker-fallbacks.md) です。このページは第二の選択ポリシーを維持しません。

## 4. 実際の並列 edit には worktree を使う

2 つの worker が独立してコードを変更する必要がある場合は、それぞれに分離された worktree を与えます。

```text
main worktree
  ├─ worker A worktree
  └─ worker B worktree
```

これにより dirty-file / busy gate がノイズになるのを防ぎ、どの diff がどのタスクのものかを明確にします。

並列の読み取り / レビューは同じ root を共有できますが、並列で重複する mutation は一般に共有すべきではありません。

## 5. Git を信頼できる情報源として扱う

agent が「完了した」と言うことは、完了のエビデンスにはなりません。

検証します。

- `git status`;
- `git diff`;
- 対象ファイル;
- テスト / build;
- 関連する場合は runtime の動作。

agent が repo を mutation した可能性のある後に timeout した場合は、再試行するか決める前に Git を調べます。

## 6. 不確実な mutation を盲目的に再試行しない

危険なリモートの失敗は次の形です。

```text
mutation happened
  ↓
response was lost
```

例:

- `herdr_prompt` は配信されたが、status wait が timeout した;
- shell コマンドがペインに送られたが、その後 control plane が失敗した;
- Cloudflare の mutation が曖昧なネットワークエラーを返した;
- ブラウザ handoff の seed がすでに送信されているかもしれない。

正しい対応:

1. 実際の状態を調べる;
2. すでに起きたことを reconcile する;
3. mutation が発生していないことがエビデンスで示された場合にのみ再試行する。

同じ意図が再送される可能性がある場合は、agent プロンプトで `idempotency_key` を使用してください。

## 7. `herdr_since` は再開のために使い、すべてを読み直すためには使わない

Web クライアントは、ユーザーが次のメッセージを送ったときにのみ動作します。会話が idle の間に継続的に poll することはできません。

`herdr_since(cursor)` は、最後の観測以降に変わった内容のインクリメンタルなダイジェストを与えます。

用途:

- 長いローカルタスクの後の再開;
- 委譲した worker が完了したかの確認;
- ブラウザが会話を wake した後の継続;
- 毎ターンの完全な snapshot の回避。

サーバーが再起動し、cursor が無効になっている場合は、新しい inspect 状態を使ってそこから続けます。

## 8. ブラウザ continuity は推論ではなく時間のために使う

作業が現在の Web ターンより長く続く場合、拡張は進捗、回復、handoff を正しい会話に再接続します。拡張がもう一つの planner になることはなく、「resume」が新しい Herdr / Git / runtime のチェックを省略することを許可することもありません。

初回使用時は Auto をオフのままにしてください。handoff packet は履歴的な文脈として扱い、mutation の前に live state を再確認します。HUD の動作、自動化のスコープ、handoff の曖昧さ、429 / ページの回復、会話の rollover には単一の SSOT があります。[Browser Continuity](browser-continuity.md) です。Side Panel の操作は [Browser Control Center](browser-control-center.md) にあります。このページはブラウザの状態機械を意図的に重複させません。

## 9. ローカル runtime が進化する間、公開 Edge は安定させる

ローカル実装の更新を新しい公開 URL に結合しないでください。

推奨する分割:

```text
Cloudflare Edge / OAuth / public MCP
        stable

herdr-link
        stable connection

local runtime generation
        A/B upgradeable
```

同じ public contract epoch 内の実装変更には Runtime A/B を使用します。

tool catalog / schema の変更は別個の contract migration であり、意図的に稀であるべきです。

## 10. 権限は狭く正直に保つ

herdr-mcp には複数の境界があります。

- `herdr_fs_*` は managed root と secret-path gate によって制約される;
- 書き込み root は制限できる;
- 読み取り専用モードは mutation をブロックできる;
- busy / dirty の確認は偶発的な同時 edit を防ぐ;
- `herdr_exec` はより強い shell 境界であり、サンドボックスではない。

1 つのパスの問題を解決するためだけに、すべての権限を弱めないでください。まず、実際にどの gate が操作をブロックしているかを判断します。

## 11. デプロイプレーンを分離する

ドキュメントの変更で Cloudflare の資格情報をローテーションすべきではありません。ローカル runtime の bugfix で OAuth issuer を変更すべきではありません。Worker relay の更新で Git checkout を置き換えるべきではありません。

次のプレーンは分離しておいてください。

- documentation / Pages;
- public Edge;
- local Runtime A/B;
- browser extension;
- contract epoch;
- DNS / Custom Domain cutover。

独立したプレーンはテストしやすく、rollback しやすいです。

## 12. エビデンスに基づく停止条件を優先する

タスクが完了するのは、必要なエビデンスが揃ったときであり、会話が終わったように聞こえるときではありません。

例:

```text
code task
  → expected diff + tests

runtime upgrade
  → active generation + real tool call + rollback target

Edge deployment
  → health + workstation + OAuth/MCP

browser continuity fix
  → real target-site behavior + smoke tests
```

これにより、長時間の作業が「たぶん大丈夫そう」に流れていくのを防ぎます。

## 推奨するオーケストレーションループ

```text
Inspect
  ↓
Narrow read/search
  ↓
Check Git
  ↓
Do deterministic work
  ↓
Delegate only where useful
  ↓
Run long work with explicit handles
  ↓
Resume incrementally
  ↓
Verify Git/tests/runtime
  ↓
Review / integrate
  ↓
Commit / deploy
```

このループは意図的に退屈です。herdr-mcp の価値は、強力な Web planner が、ターンをまたいでも状態を失うことなく、このループを永続的なローカル開発環境に適用し続けられることです。

関連資料:

- [Architecture](architecture.md)
- [Browser continuity](browser-continuity.md)
- [Agent delegation](worker-fallbacks.md)
- [Troubleshooting](troubleshooting.md)
