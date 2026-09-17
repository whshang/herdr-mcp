# Herdr 0.9 のマルチマシン制御と二経路の使い分け

*Herdr の saved SSH machine と Herdr-MCP Edge device を、両者の identity を混同せずに併用します。*

Herdr 0.9 と Herdr-MCP は、マルチマシン制御の異なる部分を解決します:

| 制御面 | identity | transport | 最適な用途 |
| --- | --- | --- | --- |
| Herdr saved machine | machine profile id + SSH target + Herdr session | SSH / Herdr remote client | 人間によるマルチマシン TUI、保守、bootstrap、UAT、復旧 |
| Herdr-MCP Edge device | immutable な `dev_*` `device_id` + device credential | 外向き Link → Edge → MCP | ChatGPT/Web-AI の routing、device affinity、delivery evidence |

同じ物理ワークステーションが両方の identity を持ち得ます。それらを統合しないでください。ラベルや hostname が一致することは、二つのレコードが同じワークステーションである証明にはなりません。

## 両方の経路が同じ session に到達するとき何が共有されるか

saved SSH machine と Edge device の両方が同じ Herdr サーバーと同じ Herdr session に到達する場合、それらは同じ live リソースを操作します:

- workspace/tab/pane のトポロジ。
- Agent と Agent の状態。
- 実際の PTY/フォアグラウンドプロセス。
- ターミナル出力。
- Git working tree とファイルシステム状態。

これはレプリケーションでも結果整合の同期でもありません。両方の制御経路が同じ Herdr サーバー/session と通信しています。一方の経路で行った変更は、もう一方の経路で読み直せば見えます。

SSH profile が別の Herdr session を指定している場合、同じ物理ホスト上であっても Herdr の workspace/pane 状態は独立しています。

Edge 専用の状態は saved machine profile とは共有されません。`device_id`、device credential、Link の online/offline 状態、runtime generation、reconnect evidence、mutation delivery state は Herdr-MCP の関心事のままです。saved-machine の profile id、SSH target、session、enabled 状態、TUI 選択は Herdr クライアントの関心事のままです。

## 裸の workspace id や pane id でマシンを指定しない

Herdr のサーバー id はサーバー/session スコープです。二つのマシンが同時に `w1`、`w1:t1`、`w1:p1` を持つことができます。

Edge の作業では、`device_id` または device に束縛された `herdr_ref_*` を workspace/pane 参照と一緒に保ってください。saved-machine の作業では、machine profile + Herdr session を workspace/pane id と一緒に保ってください。裸の `w1:p1` をグローバルに一意であるかのようにキャッシュしたり受け渡したりしないでください。

Herdr issue [#3732](https://github.com/herdrdev/herdr/issues/3732) は、0.9.0 に実在するクロスマシンの workspace-id 曖昧性を追跡しています。そのため Herdr-MCP は、saved machine と Edge device が同じワークステーションに到達する場合でも、device affinity を明示的に保ちます。

## Herdr 0.9 における現在のプログラム的制限

Connecting Machines TUI は saved machine を表示および切り替えできますが、Herdr 0.9 はまだ通常の CLI/socket 面で machine-scoped な pane/workspace コマンドを公開していません:

- TUI でリモートマシンを選択しても、別個のローカルな `herdr pane ...` や `herdr workspace ...` コマンドは retarget されません。
- `herdr --remote <target>` はリモート TUI に attach するものであり、現時点では pane/workspace サブコマンドと組み合わせられません。
- 同じ pane id がローカルサーバーとリモートサーバーに独立して存在する場合があります。

upstream がネイティブの machine-scoped addressing を公開するまで、明示的なプログラム的ブリッジは次のとおりです:

```bash
# saved profile を検出します。id、target、session を一緒に保ってください。
herdr machine list --json

# 選択したリモートサーバー/session 上で Herdr を明示的に実行します。
# <target> は通常 SSH config の alias です。認証は SSH が所有します。
ssh <target> '~/.local/bin/herdr --session <session> pane list'
```

リモートサーバーに入ったら、mutation の前にそのサーバーの live な workspace/pane id を読み直してください。ローカルサーバーで取得した id がそこでも有効だと仮定しないでください。

upstream のマルチマシン Ideas スレッドは [Discussion #515](https://github.com/herdrdev/herdr/discussions/515) です。Herdr-MCP は、将来 Herdr がネイティブの machine-scoped API を公開し、live な schema/capabilities がそれを確認できれば、ネイティブ経路を優先します。SSH ブリッジは明示的な互換経路のままであり、第二の identity システムではありません。

## ChatGPT はどの経路を使うべきか

ワークステーションが Edge device として enroll されている場合、ChatGPT/Web-AI の操作は通常 Edge 経路を使用します。これは immutable な device identity、generation fencing、reconnect 状態、mutation-delivery evidence を提供します。

saved-machine/SSH 経路は、保守、初回 bootstrap、Debian/Linux UAT、または要求された transport がそれである場合の復旧のために明示的に使用してください。mutation を行う Edge 呼び出しを SSH へ黙ってフェイルオーバーしないでください:

- `delivery_state=not_delivered`: 接続性と状態の検証後、明示的に選択した経路での再発行は安全な場合があります。
- `delivery_unknown`、delivered/uncertain な状態、または delivery evidence の欠落: 先に live な pane/Git/runtime/resource 状態を検査し、mutation を盲目的に再生しないでください。

Herdr TUI のマシン選択は、Edge 呼び出しのターゲットを決して変更しません。Edge 呼び出しは、その明示的/既定の Herdr-MCP device および返された `herdr_ref_*` affinity に束縛されたままです。

## 二つの経路が本当に一つの Herdr 基盤を共有していることの検証

管理されたテスト用ワークステーションでは、まず読み取り専用の証拠を使用してください:

1. saved-machine profile が意図した SSH target と Herdr session を指していることを確認します。
2. Edge fleet に意図した immutable な `device_id` が含まれ、それが online であることを確認します。
3. 両方の経路で workspace/pane を読み取ります。
4. 両方の経路の `pane.process_info` を比較します。pane id、shell PID/フォアグラウンドプロセスグループ、実行ファイル、cwd が一致することは、たまたま名前が同じ二つの pane ではなく、両方の経路が同じ PTY を指している強い証拠です。
5. 必要なら、一方の経路で idle なテスト shell に無害な marker を書き、もう一方の経路からそれを読み、その後で逆方向にも繰り返します。

プロダクションまたはビジーな Agent pane を marker テストに使用しないでください。また、既存の二つの制御経路が状態を共有していることを証明するためだけに pairing/revoke を使用しないでください。
