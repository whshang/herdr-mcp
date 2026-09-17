# 能力ベンチマーク

*設計上の選択: 何を取り込み、何をコピーしないか。*

このページはメンテナーとコントリビューター向けです。機能数の比較ではありません。Herdr、他の MCP 実装、coding-agent フレームワーク、ブラウザ統合から来た能力を herdr-mcp に取り込むべきかを判断するための、長期的な ADR です。

問いは次のとおりです:

> この能力は、Herdr、agent runtime、あるいはすでにその問題を所有している別のシステムを複製することなく、Web AI がローカル開発環境をより確実に制御できるようにするか？

## まず境界を定義する

herdr-mcp は次のものではありません:

- 第二の Herdr;
- もう 1 つの coding agent;
- 汎用リモート shell 製品;
- workflow/recipe DSL;
- 汎用ブラウザ自動化フレームワーク。

herdr-mcp は Web AI とローカル Herdr/workstation の間の control plane です。したがって最も価値の高い第一級の能力は次のとおりです:

1. Web モデルが他の方法では到達できないワークステーション能力;
2. 長いタスクと会話をまたいで持続する状態;
3. パブリック Web からプライベートなワークステーションへの、安定した安全な経路;
4. 観測可能な mutation、delivery、recovery のセマンティクス。

## 判断フィルター

新しい能力を評価するときは、次のように問います:

```text
Web planner は本当にそれを欠いているか？
  ↓ yes
Herdr はすでにネイティブに公開しているか？
  ↓ yes → 発見/passthrough し、複製しない
  ↓ no
既存の fs/git/exec プリミティブで表現できるか？
  ↓ yes → それらを再利用する
  ↓ no
専用の能力は信頼性を実質的に高めるか？
  ↓ yes → 安定した public surface を検討する
```

これは「別のプロジェクトにそのための tool がある」ということより重要です。

## 選択 1: 固定された public MCP surface と、動的な Herdr のロングテール

Herdr のネイティブ Socket API は広く、進化し続けています。すべての `workspace.*`、`pane.*`、`agent.*` メソッドを public MCP tool として登録すると:

- 各会話に持ち込まれる schema が膨張する;
- Herdr のアップグレードが ChatGPT の public ABI と結合する。

そのため herdr-mcp は 2 つの層を使います:

```text
高頻度のリモート作業
  → 専用 MCP tools

ロングテールのネイティブ Herdr 操作
  → herdr_methods + herdr_call
```

ワークステーションの Runtime Execution Contract は **epoch 4 / 18 tools** です。現在の first-party DEV/PROD パブリック Edge contract は **epoch 7 / 19 actions** で、その action 集合には Edge ローカルの `herdr_devices` が含まれます。Runtime epoch 2/3 とパブリック Edge epoch-3 identity は、境界付きの rollback/compatibility ベースラインとしてのみ保持されています。将来の catalog 変更は、付随的な runtime 変更ではなく、明示的な contract epoch を必要とします。

## 選択 2: ファイル、Git、shell は第一級

これらは Herdr の責務ではありませんが、まさにリモートの Web モデルが自力ではアクセスできないものです。

第一級の能力には次が含まれます:

- ファイルの read/list/search/image;
- 正確な edit/write/patch;
- Git status/diff/log;
- 短い shell コマンド;
- 長時間実行されるコマンドセッション。

決定論的なリポジトリ作業に、別のモデルを起動する必要はありません。

## 選択 3: 長いコマンドは独自のライフサイクルを持つ

build、テスト、開発サーバーは 1 回の MCP リクエストより長く生き延びることがあります。コマンドの生存期間を同期 HTTP リクエストに束縛すると、timeout と重複実行のリスクが生じます。

そのため:

```text
短いコマンド
  → herdr_exec

長いコマンド
  → herdr_exec_start
        ↓
     read / kill
```

ハンドルベースのコマンドライフサイクルは、別の agent abstraction を発明することなく、実際のリモート実行の問題を解決します。

## 選択 4: Git の事実は決定論的なまま

`git status`、`git diff`、`git log` は、ループに余分なモデルを入れることによる利益はありません。

`herdr_git` が存在するのは、直接的な事実が次の点で優れているからです:

- 安価である;
- 高速である;
- 検証しやすい;
- mutation 完了のエビデンスとして有用である。

低頻度の Git コマンドは依然として shell を経由できます。専用の public tool は、安定した schema と頻繁な使用が追加される surface に見合う場合にのみ正当化されます。

## 選択 5: mutation のセマンティクスは自動リトライより重要

危険なリモート障害は次のものです:

```text
mutation が発生
  ↓
応答が失われた
```

そのため herdr-mcp は次を優先します:

- delivery evidence;
- idempotency key;
- transport failure と post-submit の待機を分離する;
- 不確実な mutation を再試行する前に状態を確認する。

これは agent prompt、shell コマンド、runtime activation、Cloudflare/DNS の変更、browser handoff に当てはまります。

「エラー時にリトライ」は、開発 control plane にとって安全な汎用ポリシーではありません。

## 選択 6: shell は sandbox として提示されない

一部のシステムはコマンドを safe/trusted/dangerous に分類し、強い分離があるかのような印象を与えることがあります。

herdr-mcp は実際の境界を明示したままにします:

- `herdr_fs_*` は managed roots、write gates、secret っぽいパスのフィルタリングによって制約されます;
- `herdr_exec` はワークステーションのユーザーとして実行され、より強い能力です。

コンテナ級の分離が必要なら、フラグで示唆するのではなく、実際のセキュリティアーキテクチャとして設計すべきです。

## 選択 7: プロジェクト指示システムを複製しない

Coding agent はすでに独自のプロジェクト指示、skill、`AGENTS.md` 形式の仕組みを持っています。

herdr-mcp は別の自動プロジェクト指示スキャナを構築しません。それは次を生むからです:

- コンテキストの重複;
- 優先順位の衝突;
- Web planner とローカル worker の間のルールの不一致。

リモート planner の運用ポリシーは `herdr_skill` に属し、プロジェクト固有のルールは、それを実際に実行するプロジェクトと agent runtime が所有し続けます。

## 選択 8: ブラウザ拡張は欠けている方向だけを埋める

リクエスト駆動の MCP が提供するのは:

```text
Web AI → workstation
```

ただし、リクエスト駆動の MCP は、完了したローカルタスクが後から新しい browser turn を開始するようにはしません。

したがって拡張は、欠けている時間/戻りの方向を提供します:

```text
workstation → browser conversation
```

価値のある拡張能力には次が含まれます:

- workspace binding;
- progress / settled;
- evidence-first recovery;
- fail-closed handoff;
- ネイティブ Connector を持たないサイト向けの、境界付き JSON→MCP bridge。

プロジェクトは意図的にこれを汎用ブラウザ自動化へ拡張しません。

## 選択 9: パブリック Edge とローカル runtime は分離されている

Connector URL は安定しているべきで、ローカル runtime はアップグレード可能であるべきです。

そこから次が導かれます:

- OAuth/public MCP/workstation routing のための Cloudflare Edge;
- 持続的な outbound WSS のための `herdr-link`;
- ローカル generation の変更のための Runtime A/B。

これは 1 つの Node プロセスへ直接トンネリングするより構造化されていますが、パブリック identity を変えずに runtime のアップグレード/rollback を可能にします。

## 選択 10: ローカル agent は worker であり、もう 1 つの planner ではない

Pi、Cline、OpenCode、DSH、あるいは将来の coding-agent CLI は、すべて有用な worker になり得ます。

長期的な選択基準は次のとおりです:

- ヘッドレス/自動化可能な運用;
- 観測可能な状態;
- 境界付きのタスク所有権;
- 検証可能な結果;
- timeout の後に mutation が起きたかどうかを判断できること。

Herdr ネイティブの worker は `herdr_prompt` を使います。外部 CLI は、適切な場合に長い exec セッションを使えます。

[Agent delegation](worker-fallbacks.md) を参照してください。

## 現在の判断マトリクス

| 能力 | 判断 | 理由 |
|---|---|---|
| Herdr workspace/pane/agent API | 動的 passthrough | ネイティブ API surface のコピーを避ける |
| file read/search/patch | 第一級 MCP | そうでなければ Web planner から到達できない |
| Git status/diff/log | 第一級 MCP | 頻繁で決定論的なエビデンス |
| 長いコマンドセッション | 第一級 MCP | ライフサイクルが tool call をまたぐ |
| image read | 第一級 MCP | Web モデルにとって実際のピクセルコンテキスト |
| agent prompt | 薄いラッパー | delivery/idempotency のセマンティクスが重要 |
| recipe/workflow DSL | 作らない | Web AI がすでに planner |
| 第二の agent registry | 作らない | Herdr がすでに所有している |
| 自動プロジェクト指示スキャン | 作らない | agent-runtime ポリシーの複製を避ける |
| shell pseudo-sandbox | 主張しない | セキュリティ境界は実在しなければならない |
| browser progress/recovery/handoff | 作る | MCP の時間的/戻りのギャップを埋める |
| JSON→MCP bridge | 境界付きの互換性 | ネイティブ custom MCP を持たないサイト専用 |
| Runtime A/B | 作る | Connector identity をローカルのアップグレードから分離する |
| Custom Domain | 任意 | 安定した命名であり、製品の前提条件ではない |

## 新しい能力の受け入れ規則

public tool や自動化モジュールを追加する前に、次に答えてください:

1. `herdr_call` ですでに表現できるか？
2. 既存の fs/git/exec プリミティブで表現できるか？
3. なぜ安定した public schema が必要なのか？
4. 1 ターンあたりのコンテキストコストはいくらか？
5. mutation がすでに起きたかどうかをどうやって知るのか？
6. 失敗はどう回復するのか？
7. ラッパーの unit test だけでなく、実際の挙動に対してテストできるか？
8. Herdr、agent runtime、Cloudflare を複製していないか？

これらの答えが不明確なら、まだ surface を広げないでください。

## このページの維持方法

これは実験ログではありません。

上流の tool やプロジェクトが新しい能力を導入したときは、ここで**判断**を更新します。正確なバージョン、smoke test の日付、1 回限りの UAT 結果、バグのエビデンスは、CHANGELOG、issue、実験記録に置きます。

そうすることでこのページは、長期的に重要な問いに役立ち続けます:

> herdr-mcp はなぜこの形なのか、そして次の能力は本当にその境界の内側に取り込む価値があるのか？
