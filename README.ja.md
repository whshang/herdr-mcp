# herdr-mcp

**ChatGPT / Web AI をローカル Herdr ワークステーションへ安全につなぎます。**

ChatGPT が計画と判断を持ち、Herdr が実際の作業現場を保持し、herdr-mcp がブラウザモデルをローカルの files / Git / shell / 長時間ジョブ / agents へ接続します。

**Docs:** https://whshang.github.io/herdr-mcp/ · **Source:** https://github.com/whshang/herdr-mcp

Languages: [English](README.md) · [简体中文](README.zh.md) · **日本語**

## Agent-first セットアップ

正式なインストール入口は、実行可能な Agent に直接向けて書かれたプロトコルです：

- [Quick agent install（英語）](docs/i18n/en/agent-install.md) — 短い end-to-end 実行プロトコル；
- [Agent install（英語）](docs/i18n/en/agent-install.md) — Cloudflare、Link、security、verification の詳細契約。

Agent はプロトコルを自分で読み、決定的な check / mutation を直接実行し、人の承認や選択が本当に必要な場合だけ停止します。通常 workstation の PROD runtime は published GitHub Releases から導入し、git checkout からは導入しません。network/login/third-party availability が blocker なら、Agent は停止して報告し、proxy や bypass infrastructure を勝手に作りません。

Local herdr-mcp runtime は native binary です。Node.js / npm は**不要**です。Node は Cloudflare / Wrangler bootstrap で Agent が一時的に使う場合だけあります。

Protocol は Herdr / herdr-mcp install、Cloudflare Edge、Link、ChatGPT Connector/OAuth、`herdr-mcp doctor` と real MCP smoke までを検証します。

手動/運用リファレンスは [Installation（英語）](docs/i18n/en/install.md) を参照してください。

## 最初のテスト

herdr Connector を有効にした新しい ChatGPT 会話で：

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

正常なら、ChatGPT は実際の workspace / pane / agent / Git / project files を MCP 経由で確認できます。

### v0.4.2 の画像・ビジュアル開発

v0.4.2 は同じ workstation security boundary を visual および file import にも拡張します。`herdr_fs_image` で managed project 内の PNG/JPEG/GIF/WebP を ChatGPT が直接確認できます。組み込みのプランナーポリシーは artifact を最短安全経路で処理します：managed ローカルファイルは直接 `herdr_fs_*` ツール、安全な署名付き HTTPS URL は直接 `herdr-mcp artifact import --signed-url`、直接消費できる MCP/Connector のファイル参照は直接消費し、残りのクロスバウンダリ転送だけが private・短寿命の Cloudflare R2 generic artifact relay を経由します。Rust runtime が HTTPS/SSRF、サイズ、MIME/file signature、digest、managed-root、dirty-file、busy-agent を検証してから repository に書き込みます。public MCP catalog は 18 tools のままです。

Browser extension は **generic file/artifact relay ではありません**。v0.4.2 では current ChatGPT Web conversation で完成した assistant turn の画像だけを対象にした narrow source-capture path を持ちます。ChatGPT cookie と短寿命 bearer は browser memory に残し、extension は `image_asset_pointer` / `file_id` を解決して allowlisted `chatgpt.com` HTTPS endpoint から画像を取得し、validated image bytes と non-secret metadata だけを local Native Host に渡します。cookie、bearer、Authorization header、download URL はこの boundary を越えません。その他の artifact routing は runtime + direct import + private R2 fallback が担当します。

## Browser extension は任意

Browser extension は conversation continuity、Chrome Side Panel の Control Center、workspace binding、次ターンの queue を追加します。最初の Connector 接続には不要です。

v0.4.2 source candidate では、同じ bound ChatGPT Project 内で新しい conversation を手動で開いたあと、内部 `continuity_id` を入力せず **“continue” / “resume”** と言うだけで再開できます。Herdr は stable な conversation / Project / workspace identity で Continuity Journal を検索し、その identity 自体が active chain を一意に特定できる場合だけ自動 resume します。曖昧な場合は bounded candidate evidence を提示して確認し、最新・文字類似だけで chain を選びません。詳細は [Browser continuity（英語）](docs/i18n/en/browser-continuity.md) を参照してください。

Browser extension の distribution identity は Runtime DEV/PROD とは別のモデルです：

| Extension channel | 用途 | identity / update source |
| --- | --- | --- |
| **STORE** | 通常ユーザーの既定 | fixed Chrome Web Store identity + Store update |
| **STANDALONE** | GitHub/manual install、Store 非依存 | fixed non-Store identity + deterministic package；v0.4.3+ |
| **DEV** | extension/source development | repo/worktree の unpacked path；path-derived identity |

現在の stable v0.4.2 Native Host は Store/DEV ownership です。v0.4.3 で fixed-identity **STANDALONE** を追加し、v0.4.2 の tag/assets は作り直しません。Agent は GitHub/manual standalone package を dev と呼ばず、path-derived DEV build を通常インストールの fallback にしません。

現在の stable Store path は、runtime + Connector を検証してから [Herdr Chrome Web Store item](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) を利用可能な場合にインストールし、`herdr-mcp native-host install` と `herdr-mcp native-host status` を実行します。v0.4.3+ で runtime が standalone support を明示し、Store が利用できないか user が independent distribution を選んだ場合は STANDALONE を選べます。

詳細：[Browser extension（英語）](docs/i18n/en/extension.md) · [Browser Control Center（英語）](docs/i18n/en/browser-control-center.md)

## 現在のサポート範囲

- herdr-mcp stable: `v0.4.2`
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

- [Installation（英語）](docs/i18n/en/install.md)
- [ChatGPT Connector（英語）](docs/i18n/en/chatgpt-connector.md)
- [Browser extension（英語）](docs/i18n/en/extension.md)
- [Browser extension privacy（英語）](docs/i18n/en/privacy.md)
- [Troubleshooting（英語）](docs/i18n/en/troubleshooting.md)
- [Architecture（英語）](docs/i18n/en/architecture.md)

Maintainer 向け release gate、UAT、CI、Runtime A/B、GA history は詳細ドキュメントに残しますが、初回インストール経路からは外しています。
