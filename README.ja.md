# herdr-mcp

[简体中文](README.md) · [English](README.en.md) · **日本語**

ChatGPT・Claude・Grok などの Web AI を自分の開発マシン上で継続的に動かすためのブリッジ：ファイル操作、Git、コマンド・テスト実行、ローカル Coding Agent の並列委譲、複数ワークステーション制御、ブラウザ連続性を一つのループに統合します。

**[ドキュメント](https://whshang.github.io/herdr-mcp/ja/)**

![Herdr-MCP 1.1 アーキテクチャ](assets/architecture-ja.svg)

## 一文でインストール

対象コンピュータ上の Coding Agent に次の一文を送ってください：

```text
https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/ja/agent-install.md に従って Herdr と herdr-mcp をインストール・設定してください。最新の Stable GitHub Release を使い、安全に自動化できる手順はすべて自動実行し、Cloudflare のサインイン/承認、macOS のフルディスクアクセス、ChatGPT OAuth/Connector 承認で私の操作が必要なときだけ一時停止してください。
```

Agent が環境確認、Herdr と herdr-mcp のインストール、Worker のデプロイ、ワークステーション接続、最終検証まで実行します。

通常の利用では git clone、npm、Cargo、Wrangler、手動 Runtime インストールは不要です。

## Cloudflare 設定

- Cloudflare Workers Free で十分動作し、支払い方法の登録は不要です。
- インストール Agent が `herdr-mcp worker bootstrap` を実行します。
- 独自ドメインがなくても `workers.dev` のまま利用できます。
- 適切なドメインがある場合は、ChatGPT 認可前に専用 Custom Domain を設定できます。
- ChatGPT に渡す MCP URL は必ず `/mcp` で終わる必要があります。

[Cloudflare 設定ガイド](docs/i18n/ja/cloudflare-edge-deployment.md)

## ChatGPT 設定

1. ChatGPT の **設定 → Apps & Connectors**（旧 UI：**Plugins → Browse plugins**）で **Developer mode** を有効にします。
2. カスタム MCP Connector を追加します（推奨名は **`herdr`**）。
3. 完全な MCP URL（例：`https://herdr.example.com/mcp`）を入力します。
4. OAuth を完了し、ローカル端末で 6 桁の検証コードを使って承認します。
5. ChatGPT Project 内で作業します。
6. **新しい会話の最初のターンでは、入力欄の `+` から `herdr` を選択または `@herdr` で参照します。**

ChatGPT Project のプロジェクト指示にローカルフォルダを書いておくと確実です：

```text
ローカルプロジェクト: /Users/you/Documents/my-project
```

ブラウザ拡張を導入すると、Herdr Control Center からデバイス・workspace・フォルダ対応を Project 指示へ同期できます。実行前の最終判断は常にライブの Herdr 状態に基づきます。

[ChatGPT 設定ガイド](docs/i18n/ja/chatgpt-connector.md)

## 権限と認可

macOS では必要な場合に、安定 broker にフルディスクアクセスを付与します：

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

`permissions status` が `needs_setup` を返したときだけ setup を実行し、**システム設定 → プライバシーとセキュリティ → フルディスクアクセス** で承認してください。固定された broker revision は既存のファイルアクセス許可を維持できます。Herdr TUI の画面収録は別の権限です。Herdr 更新後に収録できない場合は [macOS 権限のトラブルシューティング](docs/i18n/ja/troubleshooting.md#macos-のファイルアクセスと画面収録の権限)を参照してください。

初回 ChatGPT OAuth 接続時には、承認ページに正確なローカル承認コマンドと 6 桁コードが表示されます。自分のターミナルで承認し、Cloudflare Token やデバイス資格情報などの秘密をチャットに貼り付けないでください。

[インストールと認可](docs/i18n/ja/agent-install.md) · [トラブルシューティング](docs/i18n/ja/troubleshooting.md)

## 1.1 の主な特徴

- **作業現場が自分のマシンに残る。** Herdr TUI の workspace、PTY 端末、Git worktree、常駐サーバー、長時間タスクは Web 会話を閉じても消えません。
- **受信ポート非公開の複数マシン制御。** 1 つの Cloudflare Worker に複数の macOS / Linux / Windows マシンを outbound WSS Link で登録し、明示的な `device_id` ルーティングで誤爆を防ぎます。
- **複数アカウントと Connector の厳格な分離。** 認可済み Connector・WebChat アカウント・ブラウザ endpoint を独立して識別し、未認可・不一致セッションは fail-closed で拒否します。
- **決定的ツール + 任意のローカル Coding Agent。** 読み書き・Patch・Git・テストは 18 個の MCP ツールで直接実行し、大きな作業は Codex、Claude Code、Pi、Cline、OpenCode などの利用可能な Agent に非同期委譲できます。
- **マルチ WebChat 制御と引き継ぎ（1.1）。** ChatGPT（基準プランナー）、Claude、Grok セッションの作成・継続・停止・アーカイブ・次ターン予約・引き継ぎに加え、Doubao（豆包）画像生成フローに対応します。
- **ブラウザアダプター基盤とサイト Skill（1.1）。** `page_generation` フェンス付きの `browser-page` 操作（`observe` / `click` / `fill` / `navigate`）と、再利用可能な `.agents/skills/herdr-browser-<slug>` スキルを提供します。
- **明確な配信セマンティクスと署名検証付き自己更新（1.1）。** mutation は `applied` / `not_applied` / `uncertain` を明確に区別して盲目的な再実行を防止。`herdr-mcp update` は署名付き GitHub Release マニフェストと Sigstore 証明を検証して安全に更新します。

## よくある使い方

### 1 つのプロジェクト

ChatGPT Project 指示にフォルダを保存しておけば、そのまま依頼できます：

```text
このプロジェクトの Git 状態を確認し、現在のテスト失敗を修正してください。変更はこのプロジェクトだけに限定し、最後に該当テストを実行してください。
```

### 複数コンピュータの連携

```text
Herdr デバイスを一覧表示して。バックエンド変更は macbook-main、独立テストは linux-lab を使い、分離した worktree で両方検証して。
```

新しいマシンを追加するときは、登録済みデバイスで次を実行します：

```bash
herdr-mcp worker pair
```

### 1 台のマシンを複数アカウントで利用

1 台のワークステーションで複数の認可済み ChatGPT / WebChat アカウントを扱えます。Connector、アカウント、ブラウザセッションの identity は常に分離され、Registry で発見・認可されたセッションだけを操作します。

### ローカル Agent とマルチ WebChat の連携

ローカル Coding Agent から `herdr-mcp webchat` や `herdr-mcp browser-page` を通じて対応 WebChat セッション（ChatGPT・Claude・Grok・Doubao）や管理対象タブを安全に操作でき、独自の Playwright / DOM 自動化を再構築する必要はありません。

[ローカル Agent ↔ WebChat](docs/i18n/ja/local-agent-webchat-control.md)

## ブラウザ拡張（任意）

基本の ChatGPT → MCP → ワークステーション接続に拡張は不要です。

Side Panel の Control Center、Project 連携、次ターン予約、ID 不要の引き継ぎ、マルチ WebChat 制御、`browser-page` 操作を使う場合は、ストアから導入するか、現在の Runtime と一致するローカル管理ビルドを配置します：

```bash
herdr-mcp extension standalone install
```

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [拡張ガイド](docs/i18n/ja/extension.md) · [Control Center](docs/i18n/ja/browser-control-center.md) · [ブラウザー連続作業](docs/i18n/ja/browser-continuity.md)

## 状態確認とアップデート

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
herdr-mcp device list
herdr-mcp update
```

`herdr-mcp update` は署名付き GitHub Release マニフェストと Sigstore ビルド証明を検証してロールバック可能な更新を適用します。Fleet Worker のバージョン差分が通知された場合は、別途 `herdr-mcp worker update` を実行します。

macOS Apple Silicon、Linux x86_64、Linux ARM64 は Production。Windows x86_64 / ARM64 は現在 Candidate。WSL は未対応です。

[Platform support](docs/i18n/ja/platform-support-matrix.md) · [自己更新ガイド](docs/i18n/ja/runtime-self-upgrade.md) · [CLI reference](docs/i18n/ja/cli-reference.md) · [全ドキュメント](https://whshang.github.io/herdr-mcp/ja/)

## よくある質問

### なぜ Cloudflare を使うのですか？

ChatGPT は公開インターネット上で動作しますが、開発マシンは通常 NAT、firewall、変動するネットワーク、社内ネットワークの内側にあります。Herdr-MCP は workstation の受信ポートを公開せず、各 device から安定した Cloudflare entry へ outbound 接続します。

Cloudflare は公開 MCP/OAuth endpoint、device routing、再接続調整、複数 device に必要な少量の共有 state も担当します。

### Port forwarding、Tailscale、別の tunnel を使えますか？

代替 transport が完全な代替になるには、ChatGPT から到達できる公開 HTTPS MCP endpoint、信頼できる TLS、認証/OAuth、安全な device routing、確実な再接続、明確な mutation delivery semantics を同時に提供する必要があります。

private IP や Tailscale-only address は ChatGPT cloud から直接到達できません。現時点で正式にサポート・検証している経路は Cloudflare です。LAN / Tailscale 直接接続は 1.2 の候補であり、現行機能ではありません。設計には認証、暗号化、device identity と権限、ネットワーク公開範囲、配送セマンティクスが必要で、ChatGPT cloud 向けには到達可能な公開 endpoint も必要です。

### どの Herdr をサポートしていますか？

サポートするのは **Herdr TUI**（`herdr` CLI と `herdr server`）だけです。CI は Herdr TUI `0.9.5` に固定しています。新しい Herdr app とその herdr-core runtime（`hc`）は TUI と Agent を共有しない別の runtime であり、herdr-mcp v1.1 はサポートしません。Herdr TUI のドキュメントは <https://herdr.dev/tui/docs/> に移動しました。詳細は [プラットフォームサポートマトリクス](docs/i18n/ja/platform-support-matrix.md) を参照してください。

### 初回インストール後に Herdr window / Server が見つからない場合は？

**v1.0.1** 以降、`herdr-mcp install` は既存 Herdr を自動検出・検証し、stable update を試みます。Herdr が存在しない場合は公式 installer を実行し、Herdr Server/API が実際に到達可能になるまで installation 完了とはみなしません。

問題が残る場合：

```bash
herdr-mcp status
herdr-mcp doctor
```

shell PATH から `herdr` が見えないだけで二つ目の Herdr を再インストールせず、`doctor` の repair path に従ってください。

### `workstation_offline` が出たら？

Edge は応答できても、対象 computer に validated live connection がない状態です。短い切断は自動再接続を待ちます。

mutation では返された `delivery_state` / retry metadata に従い、delivery が uncertain の操作を盲目的に再実行しないでください。[トラブルシューティング](docs/i18n/ja/troubleshooting.md)も参照してください。

### Chrome 拡張は必須ですか？

いいえ。基本の ChatGPT → MCP → workstation 接続には不要です。Project binding、Control Center、browser continuity、次ターン queue、マルチ WebChat handoff、または `browser-page` 操作が必要な場合に追加します。

### 特定の Coding Agent が必須ですか？

いいえ。決定的な作業は直接実行でき、複雑な作業は対象 computer 上の利用可能な互換 Agent に委譲できます。

## 関連プロジェクトと謝辞

Herdr-MCP は複数のオープンプロジェクトから成熟した考え方を参考にしています。

- [Herdr](https://github.com/herdrdev/herdr) — 永続 workspace、terminal、Agent 環境。
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — 決定的な Coding MCP tools。
- [MCPX](https://github.com/opentokenz/mcpx) — 永続 remote MCP session と recovery。
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — Remote Worker / managed jobs architecture。
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — Web planner / Codex executor collaboration。
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — Codex harness + Web-model inference。
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — MCP-compatible service を ChatGPT に安全に公開する参考実装。

これらはアイデアの出典と ecosystem context を示すもので、Herdr-MCP への依存や各 project / author による endorsement を意味しません。詳しくは [Ecosystem comparison](docs/i18n/ja/herdr-vs-ecosystem.md) を参照してください。

## License

MIT
