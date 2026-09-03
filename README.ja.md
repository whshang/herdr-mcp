# herdr-mcp

[English](README.md) · [简体中文](README.zh.md) · **日本語**

**考える場所は ChatGPT に。実際の作業はあなたのコンピュータに。**

Herdr-MCP は ChatGPT などの Web AI から、実際の開発マシン上のコード確認、Git、コマンド、テスト、Coding Agent の連携を可能にします。[Herdr](https://herdr.dev/) が workspace、ターミナル、サービス、リポジトリ、worktree、Agent の状態を会話をまたいで保持するため、チャットが終わっても長時間の作業環境は残ります。

```text
ChatGPT / Web AI
       │ MCP + OAuth
       ▼
Cloudflare Edge
       │ 認証済み outbound link
       ▼
   herdr-mcp
   ├─ files / Git / commands
   ├─ Coding Agents
   └─ Herdr workspaces / terminals / events
              ▲
              └─ optional Chrome extension: continuity / handoff / control center
```

モデルは計画を担当し、実際の状態はあなたのコンピュータに残ります。小さな作業は直接実行でき、大きな作業は複数の Agent や複数マシンへ分割しながら、観測・復旧・人間による引き継ぎが可能です。

**[Documentation](https://whshang.github.io/herdr-mcp/)**

## インストール

### 推奨：Agent に一文だけ渡す

```text
Herdr と herdr-mcp をインストールして設定してください。https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md を最後まで読み、現在の Stable GitHub Release を使って Cloudflare と ChatGPT まで設定してください。Cloudflare account に適切な active zone がある場合は専用 Custom Domain を優先し、ない場合は workers.dev を維持してください。workstation の network/fallback path を自動検証し、私自身のログインや認可が必要な場面だけ停止し、最後に接続全体を実際の MCP リクエストで検証してください。
```

Agent は `workers.dev` で Worker を bootstrap し、適切な Cloudflare zone があれば OAuth/Connector を固定する前に Custom Domain を推奨・設定します。その後 workstation Link と ChatGPT 認可を進め、実際の network path と MCP request を検証します。domain がなくてもインストールは停止せず、必要なら Link が direct `workers.dev` → 既存 local proxy → qualified shared Relay の順で自動 fallback します。

### 手動インストール

各手順を自分で進める場合は [Manual install](docs/i18n/en/install.md) を参照してください。

### ChatGPT の設定

必要に応じて Developer Mode を有効にし、**Settings → Apps** から `herdr` App/Connector を追加して OAuth を完了します。

[ChatGPT setup](docs/i18n/en/chatgpt-connector.md) · [OpenAI Developer Mode / MCP documentation](https://help.openai.com/en/articles/12584461)

### Cloudflare の設定

Cloudflare が安定した公開 MCP/OAuth 入口を提供し、各開発マシンは外向きに認証済み接続を張ります。各マシンへ公開 inbound port を開ける必要はありません。

[Cloudflare setup](docs/i18n/en/cloudflare-edge-deployment.md) · [Cloudflare Dashboard](https://dash.cloudflare.com/)

### Shared Relay は公開入口ではなくフォールバックです

Herdr-MCP は通常 workstation Link を直接接続します。自分の Cloudflare Custom Domain を設定している場合、共有 Relay Pool は使用しません。Custom Domain がない場合は、まず Worker の `workers.dev` へ直接接続し、検証済みのローカル proxy があれば次にそれを試し、それらが利用できない場合だけ Herdr の Relay Pool へ自動 fallback します。fresh install には v0.4.5 の中国本土 no-proxy UAT で検証した Deno/Supabase baseline が含まれ、新しい有効な signed pool cache があれば baseline を置き換えます。

Relay は MCP/OAuth URL やデバイス identity を置き換えず、汎用 proxy にもなりません。認証済みの `herdr-link` WebSocket を、自分の `workers.dev` Worker へ転送するだけです。本番 Pool は独立した Deno / Supabase failure domain を使い、デバイス単位の sticky・capacity-weighted selection と bounded failover を行います。長時間接続の大部分は Deno が担当し、Hosted Edge Function の lifetime と Free quota がより厳しい Supabase は少量の capacity share と完全な fallback を担当します。通常の利用者が両 provider のアカウントや Relay URL を設定する必要はありません。

## 複数コンピュータをまとめて操作する

1 つの Herdr Worker と 1 つの ChatGPT 接続で、登録済みの複数コンピュータを扱えます。ChatGPT は `herdr_devices` でデバイス一覧とオンライン状態を確認し、明示したデバイスへ作業をルーティングできます。

例：

```text
Herdr デバイス一覧を確認してください。backend は macbook-main、独立した test は macbook-lab を使い、両方の working tree を分離したまま実行し、最後に結果を相互検証してください。
```

複数マシンが mutation の候補になるのに対象を指定しなかった場合、Herdr は推測せず `device_ambiguous` を返します。後続操作や retry も選択済みデバイスを保持し、各コンピュータは独立した credential を持ちます。

Web AI は private workstation method を使い、登録済みコンピュータ間で少量の非機密 UTF-8 テキストをコピーすることもできます。public MCP tool は増えません。読み取りには integrity digest が付き、書き込みは HOME 配下の通常ファイル（symlink 不可）、256 KiB 上限、明示的な overwrite、既定の backup、機密らしい path/content の拒否に制限されます。binary、directory sync、credential transfer は対象外です。

### 新しいコンピュータを既存のデバイス群へ追加する

推奨は、すでに認可済みの ChatGPT 会話で次のように依頼することです。

```text
新しいコンピュータ用に、10 分間有効な Herdr pairing link を生成してください。
```

Herdr は Edge 上で直接 pairing を作成するため、古い workstation が online である必要はありません。ChatGPT は pairing address、単回使用の 6 桁 code、正確な expiry、コピー可能な `herdr-mcp worker connect "<pairing-address>"` をまとめて返します。認可済み Mac 上の `herdr-mcp worker pair` は CLI fallback として残ります。

新しいコンピュータ上の Coding Agent に次の一文を渡します。

```text
このコンピュータを既存の Herdr デバイス群へ接続してください。https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md に従い、pairing address は <pairing-address> を使い、CLI が要求した時だけ 6 桁 verification code を私に入力させ、完了後に同じ Worker 上でこのデバイスが online と表示されることを確認してください。
```

新しいコンピュータは既存の Worker と ChatGPT 接続へ参加します。別の Worker を作成したり、長期共有 secret をコピーしたりしません。

[Multi-device guide](docs/i18n/en/existing-worker-connect.md)

## 使い方の推奨

### Web AI に明確な作業ルールを渡す

開発タスクでは、次のようなデフォルト prompt が有効です。

```text
変更前に live Herdr workspace と Git 状態を確認してください。既存の dirty worktree は分離したままにしてください。決定的な read、Git check、patch、bounded command は直接実行し、独立または長時間の作業は有効なら利用可能な Coding Agent に委譲してください。完了報告の前に final diff を確認し、関連 test を実行してください。
```

リスクの高い変更では対象、安全制約、acceptance criteria を明記してください。調査だけなら read-only と指定します。

### Coding Agent を少なくとも 1 つ用意する

決定的な操作は Herdr-MCP が直接行えます。長い実装、大規模 refactor、test-fix loop、独立モジュールの並列作業では Coding Agent が有効です。Herdr は各コンピュータ上で利用可能な Agent を検出するため、特定ベンダーへ固定されません。

代表的な組み合わせ：

| 作業 | 推奨構成 |
| --- | --- |
| 調査、小さな patch、Git/test check | Web AI → Herdr-MCP direct tools |
| 中規模実装 | Web AI が計画 → 1 Agent が実装 → Web AI が検証 |
| 独立した複数モジュール | Web AI が分割 → isolated Agent/worktree → cross-check + tests |
| 複数コンピュータ | Web AI が device を選択 → 各マシンで独立実行 → 結果を統合検証 |
| 長時間の無人作業 | Chrome extension を追加して continuity / handoff |
| 人間が引き継ぐ | 同じ Herdr workspace/terminal を開き、実際の状態から継続 |

複数 Agent に同じ working tree を同時編集させないでください。並列 mutation は isolated worktree を使います。

長い test/build は `herdr_exec_start` で開始し、`herdr_exec_read(session_id, offset=next_offset)` で継続取得します。terminal scrollback を completion evidence として扱いません。完了済み session は bounded な最終出力と exit evidence を保持し、runtime replacement 後も回復できます。restart 時点で実行中だった process は、安全に takeover されたとは推測しません。

## Chrome extension

ChatGPT → MCP → 開発マシンの基本接続には必須ではありません。会話の継続、queued next-turn、Browser Control Center、対応する ChatGPT artifact capture が必要な場合に追加します。

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [Extension guide](docs/i18n/en/extension.md) · [Browser continuity](docs/i18n/en/browser-continuity.md)

## よくある質問

### なぜ Cloudflare を使うのですか？

ChatGPT は公開インターネット側で動作し、開発マシンは通常 NAT、firewall、可変ネットワーク、社内ネットワークの内側にあります。Herdr-MCP では各マシンが外向きに安定した Cloudflare 入口へ接続するため、開発マシンの inbound port を公開する必要がありません。

Cloudflare は公開 MCP/OAuth endpoint、device routing、reconnect coordination、複数デバイスに必要な小さな共有状態も担当します。

### Port forwarding、Tailscale、別の tunnel でも使えますか？

代替 transport には、ChatGPT から到達可能な public HTTPS MCP endpoint、trusted TLS、認証/OAuth、安全な device routing、reconnect、明確な mutation delivery semantics が必要です。

private IP や Tailscale-only address は ChatGPT の cloud service から直接到達できません。raw port forwarding は露出を増やします。一般的な tunnel でも endpoint は公開できますが、Herdr-MCP の routing、OAuth、multi-device、recovery は Cloudflare 経路で実装・検証されているため、これが正式なサポート経路です。

### `workstation_offline` が出たら？

Cloudflare は ChatGPT に応答できたものの、対象コンピュータへの有効な接続がその時点で無かったことを示します。短時間の切断では再接続を待ち、コンピュータ側も自動再接続を続けます。

```bash
herdr-mcp status
herdr-mcp doctor
```

mutation の場合は返された delivery/retry 情報に従い、delivery が不明な操作を無条件に繰り返さないでください。詳細は [Troubleshooting](docs/i18n/en/troubleshooting.md) を参照してください。

### アカウントの利用上限はどこで確認しますか？

ChatGPT のモデル利用上限は ChatGPT の plan/workspace 側で管理されます。アカウントに表示される usage / model limit を確認してください。plan によっては正確な残量ではなく reset window が表示されます。

Cloudflare の使用量は別です。**Workers & Pages → 対象 Worker → Analytics & Logs** と account の Billing/Usage で Worker、Durable Object などを確認できます。Herdr-MCP は idle device の coordination write を抑える設計です。

### Chrome extension は必須ですか？

必須ではありません。基本接続は単独で動作します。browser continuity、handoff、Browser Control Center、対応する browser-side artifact capture が必要な場合に追加してください。

### 特定の Coding Agent が必須ですか？

必須ではありません。決定的な作業は直接実行でき、複雑な作業は選択したコンピュータ上で利用可能な互換 Agent に委譲できます。

## 関連プロジェクトと謝辞

Herdr-MCP は複数の open-source project から有用なアイデアを学んでいます。

- [Herdr](https://github.com/herdrdev/herdr) — persistent workspace、terminal、Agent environment。
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — focused deterministic coding-MCP tools。
- [MCPX](https://github.com/opentokenz/mcpx) — durable remote MCP sessions と recovery ideas。
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — remote-worker architecture と managed jobs。
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — Web planner / Codex executor collaboration。
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — Codex harness + Web-model inference。
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — MCP-compatible service を ChatGPT に安全に公開する参考実装。

詳しい比較は [Ecosystem comparison](docs/i18n/en/herdr-vs-ecosystem.md) を参照してください。

## License

Herdr-MCP は **MIT License** で公開されています。第三者プロジェクトの名称、商標、コード、ドキュメントにはそれぞれのライセンスとポリシーが適用されます。
