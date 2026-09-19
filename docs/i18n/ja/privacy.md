# 拡張のプライバシー

*ブラウザ拡張のデータ処理、権限、プライバシーポリシー。*

**施行日:** 2026-08-28

このポリシーは、Herdr ブラウザ拡張がユーザーデータをどのように扱うかを説明します。Herdr プロジェクトが配布する Chrome 拡張に適用され、拡張の [製品ドキュメント](extension.md) と併せて読む必要があります。

拡張の単一の目的（single purpose）は、対応する Web AI の会話をユーザーのローカル Herdr / herdr-mcp ワークステーションに接続することです。これによりブラウザは、live な workspace 状態を表示し、会話を workspace に binding し、長いタスクと長い会話の continuity を保ち、ユーザーの次のターンをキューし、Chrome Side Panel で有界な復旧と制御 UI を提供できます。

## 拡張が扱うデータ

そのユーザー向け機能を提供するため、拡張は対応する Web AI サイト上で次のデータを扱う場合があります:

- **ウェブサイトのコンテンツと個人的な通信:** continuity、キューされたメッセージ、handoff（引き継ぎ）/復旧、任意の LLM 分析、および後述するユーザーが起動する handoff フォールバックに必要な、会話テキストとページ状態。
- **ウェブ履歴:** 現在の対応サイトの URL、その URL から導出される conversation/project の識別子、およびアクティブなページを Herdr workspace に関連付けるために必要な限定的なナビゲーション状態。拡張は汎用的な閲覧履歴プロファイルを構築したり販売したりしません。
- **ユーザーアクティビティ:** ターン状態、submit/settle/復旧のタイムスタンプ、拡張のボタン/トグルの状態、および continuity と復旧のアクションがいつ安全かを判断するために必要なその他の有界な操作状態。
- **認証情報:** ユーザーが明示的に設定した TypeSafe/Jev または OpenAI 互換 LLM endpoint 用の任意の API key。
- **ローカル Herdr 状態:** ローカルにインストールされた Herdr / herdr-mcp runtime が返す workspace、pane、agent、ステータス、出力テイル、binding、固定ターゲットの情報。

拡張は、健康情報、金融/支払い情報、正確な位置情報、または広告プロファイルのためのデータを要求したり、意図的に収集したりしません。

## データの保存場所

拡張は `chrome.storage.local` を使用して、設定と continuity 状態をユーザーの Chrome プロファイルに保持します。保存されるものは次のとおりです:

- workspace/conversation の binding。
- キューされた次ターンのメッセージ。
- 自動化の設定と復旧の予算/状態。
- 固定されたローカルターゲットと言語設定。
- ローカルの herdr-mcp endpoint 設定。
- 任意でユーザーが設定する TypeSafe/Jev および OpenAI 互換 LLM endpoint、モデル、API key。

このローカル状態は、Manifest V3 service worker とブラウザページが、Chrome による suspend や再読み込みの後で安全に復旧できるように存在します。publisher は、このローカル状態を受け取る拡張の analytics または telemetry サービスを運用していません。

ユーザーは、拡張を削除するか Chrome でその拡張/サイトデータを消去することで、このローカルに保存された拡張データを削除できます。任意の LLM 設定も拡張の設定から削除できます。

## ネットワークの宛先

拡張は、そのユーザー向け機能に必要な範囲でのみ通信します:

1. **同じコンピュータ上のローカル Herdr / herdr-mcp。** Native Messaging を使用して、インストールされた native host と有界の要求および live な workspace 状態をやり取りします。この通信はユーザーのコンピュータ内に留まります。
2. **対応および実験的な Web AI サイト。** 拡張は、文書化されたブラウザ面で動作して現在の会話状態を観測し、ユーザー向けの continuity/復旧インタラクションを行います。ChatGPT が主要な対応面であり、Claude は文書化されたアダプタを使用します。z.ai と DeepSeek は実験的な統合で、既定では無効であり、ユーザーが Herdr 設定で対応するスイッチを明示的に有効にし、Chrome に対してその正確なサイトへのアクセスを許可した後にのみ、それぞれの content script が登録されます。
3. **ユーザー設定の意味判定 endpoint。** TypeSafe/Jev または OpenAI 互換 LLM endpoint を設定すると、保存またはテスト時に Chrome がその正確な origin へのアクセスを求めます。通常の Auto は、有界な最新 user/assistant ターンを TypeSafe/Jev に送って高速な意味判定を行い、必要な場合は関連する user/assistant テキストを設定済み LLM judge に送れます。Goal-aware Auto はさらに、有界な objective/open TODO/runtime の要約と最近の user/assistant テキストを TypeSafe/Jev に送り、製品ドキュメントで説明する 5 つの advisory semantic signal を取得できます。これらの signal 自体は完了 evidence ではありません。設定済み OpenAI 互換 endpoint は既存の有界な Goal Supervisor と handoff fallback 生成にも使われます。会話の handoff がユーザーによって起動された場合、またはユーザーが有効にした Auto ポリシーによってトリガーされた場合で、現在の Web AI 会話が、会話のハードリミットに達した、handoff プロンプトを送信できない、または主たる要約が有効な packet なしに settle した、という理由で必要な handoff サマリを生成できないとき、拡張は有界なソース transcript を設定済み LLM endpoint へ送信して handoff packet を生成できます。フォールバック transcript には、拡張が選択した user/assistant の会話テキストのみが含まれ、拡張の handoff 上限（現在 70,000 文字。切り詰めが必要な場合は初期のタスクフレーミングと直近の操作状態を保持）に制限されます。各 endpoint はユーザーが選択するものであり、既定では Herdr の publisher が選択または運用するものではありません。各 endpoint プロバイダー自身のプライバシーと保持の条件が適用されます。

拡張はユーザーデータを販売せず、広告ネットワークへ送信せず、無関係なプロファイリングや信用/融資の判断のためにユーザーデータを移転しません。

## 権限とリモートコード

拡張が要求する Chrome 権限は、説明した機能を提供するためだけのものです:

- `storage` — ローカル設定と continuity 状態を永続化します。
- `scripting` — MV3/ページの再読み込み後に対応 Web AI タブでパッケージ済みの content-script スタックを復旧/再注入し、有界なブラウザ側 continuity アクションを実行します。
- `alarms` — Chrome が MV3 service worker を suspend した後に、失われたローカル Herdr 状態ストリームとタイマーを復旧できるよう、定期的に起こします。
- `nativeMessaging` — ローカルにインストールされた herdr-mcp native host に接続します。
- `sidePanel` — Herdr Browser Control Center をホストします。
- host access — 常時有効なアクセスは、文書化された ChatGPT/Claude の面とローカルの herdr-mcp endpoint に限定されます。実験的な z.ai/DeepSeek へのアクセスとユーザー設定の TypeSafe/Jev または LLM endpoint は、ユーザーが明示的に有効化または設定した後にのみ要求される Chrome の任意の host permission を使用します。Herdr は `<all_urls>` を常時有効な host permission として要求しません。

**リモートの実行可能コードは使用していません。** 実行される JavaScript はすべて拡張にパッケージされています。ネットワーク応答はデータとして扱われ、JavaScript や Wasm として評価・import・実行されることはありません。

## Limited Use

Chrome API を通じて受け取った情報の使用は、Chrome Web Store の User Data Policy（その Limited Use 要件を含む）に準拠します。特に:

- ユーザーデータは、拡張の単一の目的とユーザー向け機能を提供または改善するためにのみ使用されます。
- ユーザーデータは、これらのユーザー向け機能のための許可された/必要な用途以外で第三者へ販売または移転されません。
- ユーザーデータは、パーソナライズ広告や興味関心に基づく広告に使用されません。
- ユーザーデータは、信用力の判断や融資目的に使用されません。
- publisher は、ユーザーが特定のデータを含むサポートを明示的に求めた場合、またはセキュリティや法令遵守のために必要な場合を除き、人間がユーザーの拡張データを読むことを許可しません。

Chrome Web Store ポリシーの参照: <https://developer.chrome.com/docs/webstore/user_data>

## セキュリティ

拡張が開始する公衆ネットワーク接続は、該当する場合に HTTPS/WSS を使用します。拡張と同一コンピュータ上の native program との間の Native Messaging 通信はローカルに留まります。任意の TypeSafe/Jev または LLM API key などのシークレットは、プロジェクトリポジトリや publisher の telemetry に意図的に書き込まれることはありません。

## このポリシーの変更

拡張の挙動が、データ処理を実質的に変える形で変更された場合、その挙動が公開される前にこのポリシーと Chrome Web Store の開示が更新されます。

## 連絡先とサポート

プロジェクトホームページ: <https://whshang.github.io/herdr-mcp/>

サポートと issue tracker: <https://github.com/whshang/herdr-mcp/issues>
