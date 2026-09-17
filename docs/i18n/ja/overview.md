# 概要

herdr-mcp は **Web AI の推論** を、**永続的で観測可能、かつ人間がいつでも引き継げるローカル開発ワークステーション** に接続します。

製品モデルの最短形は次の三層です。

```text
ChatGPT / Web AI
      │ MCP + OAuth
      ▼
Cloudflare Edge
      │ authenticated workstation link
      ▼
herdr-mcp + Herdr workstation
      ├─ files / Git / shell / images
      ├─ workspace / pane / agent state
      └─ optional local workers
```

ブラウザ拡張は任意の第四層です。ローカルの進捗を正しい Web 会話へ返し、Chrome Side Panel の Control Center を提供しますが、標準の MCP はそれに依存しません。

## 実際に得られるもの

- Web AI はコード片を生成するだけでなく、実際の Git プロジェクトを読み、変更できます。
- workspace、PTY、プロセス、Agent のライフタイムは、1 回のチャットのターンではなく Herdr に属します。
- Web planner は決定的な小さな作業を直接実行し、独立したローカル worker の恩恵を受けるタスクだけを委任します。
- ワークステーション側から公衆 Edge へ外向きに接続するため、開発マシンに公開の受信ポートは不要です。
- mutation セマンティクス、managed root、OAuth、Native Messaging、ブラウザ continuity には明示的な境界があります。

これは「もう一つの Coding Agent」ではありません。Herdr は永続的な作業現場（worksite）であり、herdr-mcp は Web planner がその作業現場を操作できるようにするリモート制御面です。

## Herdr と herdr-mcp の責務

**Herdr** は workspace / tab / pane / agent / session の概念、PTY、ネイティブ CLI、Socket API、ローカル Agent のライフサイクルを所有します。これらの挙動については [Herdr のドキュメント](https://herdr.dev/docs/) を権威として扱ってください。

**herdr-mcp** が所有するもの:

- ChatGPT / Web AI 向けの MCP 契約。
- Cloudflare Edge、OAuth、ワークステーション link。
- managed Git root 内のファイル、Git、shell、画像の機能に加え、live な workspace/pane cwd によって厳密に証明された non-Git operational root への読み取り/実行アクセス。
- planner 向けの状態サマリ、mutation セマンティクス、Agent への委任。
- 任意の Browser Continuity、Control Center、および実験的な JSON → MCP bridge。

この分担が tmux/cmux/ACP や他の coding MCP のアプローチより好ましい理由は [エコシステムとアーキテクチャの比較](herdr-vs-ecosystem.md) を読んでください。より深い設計原則は [設計哲学](design-philosophy.md) に、技術的な全体経路は [アーキテクチャ](architecture.md) にあります。

## どこから始めるか

- **Agent にインストールを直接実行させる:** [Agent インストール](agent-install.md)
- **手動インストールと運用を理解する:** [インストール](install.md)
- **インストール後に最初の実タスクを実行する:** [クイックスタート](quick-start.md)
- **ChatGPT を接続する:** [ChatGPT Connector](chatgpt-connector.md)
- **通常の作業スタイルを学ぶ:** [ベストプラクティス](best-practices.md)
- **長時間の Web continuity が必要なとき追加する:** [ブラウザ拡張](extension.md)
- **障害を診断する:** [トラブルシューティング](troubleshooting.md)
