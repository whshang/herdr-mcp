# herdr-mcp

**ChatGPT / Web AI をローカル Herdr ワークステーションへ安全につなぎます。**

ChatGPT が計画と判断を持ち、Herdr が実際の作業現場を保持し、herdr-mcp がブラウザモデルをローカルの files / Git / shell / 長時間ジョブ / agents へ接続します。

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: [English](README.md) · [简体中文](README.zh.md) · **日本語**

## 最短セットアップ：Coding Agent にこのプロンプトを渡す

Cursor、Codex、Claude Code、Pi、Cline など、URL を読んでコマンドを実行できるローカル Coding Agent に渡してください。

```text
Install and configure Herdr and herdr-mcp for me. First read and follow this guide end to end: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md .

Install the local herdr-mcp runtime from GitHub Releases, not from a git clone. Pause only when I personally need to sign in/create a Cloudflare API Token, or when I need to add the herdr Connector/app in ChatGPT. Automate and verify everything else.
```

Local herdr-mcp runtime は native binary です。Node.js / npm は**不要**です。Node は Cloudflare / Wrangler bootstrap で Agent が一時的に使う場合だけあります。

Agent は Herdr がなければ公式 stable installer で導入し、herdr-mcp を GitHub Releases から導入し、Cloudflare Edge / Link を構成します。最後に `herdr-mcp doctor` と実際の MCP smoke で検証します。人の操作が必要なのは Cloudflare のログイン/API Token と ChatGPT の herdr Connector/OAuth です。

手動手順は [Installation（英語）](docs/i18n/en/install.md) を参照してください。

## 最初のテスト

herdr Connector を有効にした新しい ChatGPT 会話で：

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

正常なら、ChatGPT は実際の workspace / pane / agent / Git / project files を MCP 経由で確認できます。

## Browser extension は任意

Browser extension は conversation continuity、Chrome Side Panel の Control Center、workspace binding、次ターンの queue を追加します。最初の Connector 接続には不要です。

一般ユーザーは **Chrome Web Store** からのみインストールします。

1. runtime + Connector の検証を先に完了します。
2. [Chrome Web Store](https://chromewebstore.google.com/) で `Herdr` を検索します。
3. 公式 Herdr extension を選び **Add to Chrome** を押します。
4. `herdr-mcp native-host install` と `herdr-mcp native-host status` を実行します。
5. 以後の extension update は Chrome Web Store の通常更新に任せます。通常ユーザーにローカル extension package は不要です。

> Extension は現在 Chrome Web Store の初回公開準備中です。listing 公開前はこの任意ステップをスキップし、ローカル開発版を代替インストールしないでください。

詳細：[Browser extension（英語）](docs/i18n/en/extension.md) · [Browser Control Center（英語）](docs/i18n/en/browser-control-center.md)

## 現在のサポート範囲

- herdr-mcp stable: `v0.4.1`
- public MCP contract: epoch 2 / 18 tools
- clean-machine evidence が最も揃っているのは macOS Apple Silicon
- Windows x64 binary は提供済みだが、Windows end-to-end UAT は継続中
- Linux runtime はまだ current stable の正式対応面として宣言していません

## Local runtime CLI

通常は Coding Agent に lifecycle を任せられます。stable の top-level user commands は次です：

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status
herdr-mcp rollback
herdr-mcp uninstall
```

`herdr-mcp service ...` は **Advanced / internal** の service control で、通常の install path ではありません。`0.4.1+` では `herdr-mcp scan --json` で、このクライアントから実際に起動可能な Herdr Agent kind の evidence-backed inventory を更新できます。詳細は [CLI reference（英語）](docs/i18n/en/cli-reference.md#capability-discovery-scan) を参照してください。

## 必要になったときだけ読む

- [Quick agent install（英語）](docs/i18n/en/quick-agent-install.md)
- [Installation（英語）](docs/i18n/en/install.md)
- [ChatGPT Connector（英語）](docs/i18n/en/chatgpt-connector.md)
- [Browser extension（英語）](docs/i18n/en/extension.md)
- [Browser extension privacy（英語）](docs/i18n/en/privacy.md)
- [Troubleshooting（英語）](docs/i18n/en/troubleshooting.md)
- [Architecture（英語）](docs/i18n/en/architecture.md)

Maintainer 向け release gate、UAT、CI、Runtime A/B、GA history は詳細ドキュメントに残しますが、初回インストール経路からは外しています。
