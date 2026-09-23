# herdr-mcp

[简体中文](README.md) · [English](README.en.md) · **日本語**

ChatGPT などの Web AI から、自分のコンピューター上のファイル、Git、コマンド、テスト、Coding Agent、複数マシンを継続して操作できます。

**[ドキュメント](https://whshang.github.io/herdr-mcp/ja/)**

## 一文でインストール

コンピューター上の Coding Agent に次の一文を渡します。

```text
https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/ja/agent-install.md に従って Herdr と herdr-mcp をインストール・設定してください。最新の Stable GitHub Release を使い、安全に自動化できる作業は続け、Cloudflare のログイン/認可、macOS のフルディスクアクセス、ChatGPT OAuth/Connector 承認で私の操作が必要なときだけ停止してください。
```

Agent が環境確認、Herdr / herdr-mcp のインストール、Worker 配備、workstation 接続、動作確認まで行います。

通常ユーザーは git clone、npm、Cargo、Wrangler、手動 Runtime インストールを必要としません。

## Cloudflare 設定

- Cloudflare Workers Free で十分です。支払い方法は不要です。
- インストール Agent が `herdr-mcp worker bootstrap` を実行します。
- ドメインがなくても `workers.dev` を使えます。
- 適切なドメインがある場合は、ChatGPT 認可前に専用 Custom Domain を設定できます。
- ChatGPT に登録する MCP URL は `/mcp` で終わる必要があります。

[Cloudflare 設定](docs/i18n/ja/cloudflare-edge-deployment.md)

## ChatGPT 設定

1. Plugins の **Developer mode** を有効にします。
2. **Plugins → Browse plugins** を開きます。
3. Herdr Connector を追加します。名前は **`herdr`** を推奨します。
4. `https://herdr.example.com/mcp` のような完全な MCP URL を入力します。
5. OAuth を完了します。
6. ChatGPT Project 内で作業します。
7. **新しい会話の最初のターンでは、手動で `herdr` を選択または @ 指定します。**

ChatGPT Project の instructions にローカルプロジェクトのフォルダーを書いておくことを推奨します。

```text
Local project: /Users/you/Documents/my-project
```

ブラウザー拡張を使う場合は、Herdr Control Center から device / workspace / local folder の mapping を Project instructions に同期できます。実行前は live Herdr state を優先します。

[ChatGPT 設定](docs/i18n/ja/chatgpt-connector.md)

## 認可設定

macOS では必要な場合だけ stable broker にフルディスクアクセスを与えます。

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

`permissions status` が `needs_setup` のときだけ setup を実行し、**システム設定 → プライバシーとセキュリティ → フルディスクアクセス** で許可します。

ChatGPT OAuth の初回接続では、承認ページに正確なローカル承認コマンドと 6 桁コードが表示されます。自分のターミナルで承認してください。Cloudflare Token や device credential などの secret をチャットへ貼らないでください。

[インストールと認可](docs/i18n/ja/agent-install.md) · [トラブルシューティング](docs/i18n/ja/troubleshooting.md)

## 主な特徴

- **状態は自分のコンピューターに残ります。** workspace、terminal、Git、worktree、Agent、長時間タスクを会話終了後も維持します。
- **1 つの ChatGPT から複数マシンを操作できます。** 1 Worker に複数 workstation を登録し、device を明示して routing します。
- **複数アカウントから 1 台を利用できます。** 認可済み Connector / WebChat account は別 identity として扱い、browser control は provider / account / session ごとに分離します。
- **既存の Coding Agent を利用できます。** 小さな作業は直接実行し、大きな作業は利用可能な Agent に委譲できます。
- **mutation の再試行を安全に扱います。** delivered / not-delivered / uncertain を区別します。
- **ブラウザー連続作業は任意です。** Chrome 拡張で Project binding、Control Center、次ターン queue、handoff を追加できます。

## よく使う方法

### 1 つの Project

ChatGPT Project instructions にローカルフォルダーを保存してから、そのまま作業を依頼します。

```text
このプロジェクトの Git 状態を確認し、現在のテスト失敗を修正してください。このプロジェクトだけを変更し、関連テストを実行してください。
```

### 複数マシン

```text
Herdr device を一覧表示してください。macbook-main で backend を変更し、linux-lab で独立テストを実行してください。worktree を分離し、両方を検証してください。
```

新しいコンピューターを追加する場合：

```bash
herdr-mcp worker pair
```

### 複数アカウントから 1 台を利用

1 台の workstation を複数の認可済み ChatGPT / WebChat account から利用できます。Connector、account、browser session の identity は分離されます。Browser control は Registry に登録済み・認可済みの session だけを操作します。

### Local Agent と ChatGPT

ローカル Coding Agent も herdr-mcp を通じて対応 WebChat session を作成・継続・handoff できます。別の Playwright / DOM automation は不要です。

[Local Agent ↔ WebChat](docs/i18n/ja/local-agent-webchat-control.md)

## ブラウザー拡張（任意）

基本の ChatGPT → MCP → workstation 接続には拡張は不要です。

Project binding、Control Center、browser continuity、次ターン queue、WebChat handoff が必要な場合に公式拡張を追加します。

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [拡張ガイド](docs/i18n/ja/extension.md) · [ブラウザー連続作業](docs/i18n/ja/browser-continuity.md)

## 状態確認

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
herdr-mcp device list
```

macOS Apple Silicon、Linux x86_64、Linux ARM64 は Production。Windows x86_64 / ARM64 は現在 Candidate。WSL は未対応です。

[Platform support](docs/i18n/ja/platform-support-matrix.md) · [CLI reference](docs/i18n/ja/cli-reference.md) · [全ドキュメント](https://whshang.github.io/herdr-mcp/ja/)

## License

MIT
