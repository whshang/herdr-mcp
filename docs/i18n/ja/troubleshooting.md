# トラブルシューティング

*すべてを再起動する前に、失敗している層を特定する。*

herdr-mcp は Herdr、ローカル runtime、workstation link、Cloudflare Edge、OAuth/MCP、ブラウザ continuity にまたがります。最も速い診断は、壊れている層を最初に見つけることです。

次の順序で確認してください：

```text
Herdr
  ↓
local herdr-mcp runtime
  ↓
workstation link
  ↓
Cloudflare Edge
  ↓
OAuth / MCP
  ↓
ChatGPT tool snapshot
  ↓
browser continuity
```

ある層が壊れているなら、後ろの層を再設定することから始めないでください。

## 30 秒トリアージ

### ローカル HTTP runtime は listen しているか

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` または `401` は、プロセスが listen していることを意味します。接続失敗は、runtime が停止しているか、別のポートにいるか、その他到達不能であることを意味します。

macOS の LaunchAgent インストールの場合：

```bash
herdr-mcp status
herdr-mcp logs
herdr-mcp permissions status
```

file/git の各 tool が `macos_tcc_access_blocked` を返す場合は、`herdr-mcp permissions setup` を実行し、`herdr-mcp-broker` にアクセスを付与してから `herdr-mcp permissions verify` を実行してください。通常の `not_found` エラーは無関係です。

### Herdr 自体は利用可能か

```bash
herdr --version
herdr api schema >/dev/null
```

ローカル HTTP は動くのに `herdr_inspect` が実在の workspace を見られない場合は、Cloudflare に触れる前に Herdr の daemon/socket を調べてください。

### バイナリはインストール済みだが shell の PATH から外れていないか

```bash
ls -l ~/.local/bin/herdr-mcp
zsh -ic 'command -v herdr-mcp'
zsh -lc 'command -v herdr-mcp'
```

バイナリが存在するのに `command -v` の結果が空である状態は、インストール漏れではなく `installed_but_not_on_shell_path` です。現在のプロセスでは `export PATH="$HOME/.local/bin:$PATH"` を実行してください。zsh では `line='export PATH="$HOME/.local/bin:$PATH"'` を設定し、`grep -Fqx "$line" "$HOME/.zprofile" 2>/dev/null || printf '\n%s\n' "$line" >> "$HOME/.zprofile"` で同じ行を冪等に永続化します。再インストールしたり、二つ目の PATH owner を作ったりしないでください。

### Edge から workstation が見えているか

workstation がオフラインでも OAuth は成功し得ます。公開ログインの成功は、ローカル開発マシンが接続されている証明にはなりません。

Edge の health/status と workstation link を確認してください。

### Link はローカルネットワークで遮断されていないか

Link は環境にすでに存在するプロキシを再利用します。解決順序：

```text
HERDR_LINK_PROXY > HTTPS_PROXY/https_proxy > HTTP_PROXY/http_proxy > ALL_PROXY/all_proxy
  > macOS system proxy (scutil --proxy: HTTPS, then HTTP, then SOCKS)
```

`workers.dev` に到達できないときに重要な詳細：

- `socks5://` と `socks5h://` をサポートします。SOCKS5 の dial は remote-DNS セマンティクスを使います。ホスト名は未解決のままプロキシへ送られるため、ローカルで汚染された DNS リゾルバが `workers.dev` の接続性を壊すことはありません。
- プロキシ認証（HTTP Basic または SOCKS5 のユーザー名/パスワード）はサポートしません。資格情報を埋め込んだプロキシ URL は拒否され、資格情報が status やエラー出力に現れることもありません。
- macOS では PAC 設定を検出しますが、決して評価しません。Link は PAC スクリプトを取得も実行もしません。PAC だけが設定されている場合、Link は直接接続します。
- TLS を無効化したり、システムプロキシ設定を書き換えたり、Link を汎用フォワーダーに変えたりして link の接続性を「修正」しないでください。

### 新しい ChatGPT 会話は現在のカタログを取得するか

現在の first-party DEV/PROD の公開 ChatGPT contract は **epoch 7 / 19 actions** です。workstation での実行は **epoch 4 / 18 tools** で、`herdr_skill` を含みます。追加の公開 action は Edge ローカルの `herdr_devices` です。Runtime epoch 2/3 と公開 Edge の epoch-3 identity は、有界な rollback/compatibility ベースラインとしてのみ保持されます。

古い会話は古い `tools/list` スナップショットを保持している可能性があります。何かを再インストールする前に、サーバーを検証し、新しい会話を開いてください。

## 症状：Token は有効と検証されるのに Cloudflare API が 403 を返す

`/user/tokens/verify` は Token が有効であることだけを証明します。特定の呼び出しでの 403 は、不足している権限を示します：

- `GET .../accounts/<id>/workers/subdomain` が失敗 → **Account Settings Read** が不足しています。
- Worker script deploy の呼び出しが失敗 → **Workers Scripts Edit** が不足しています。
- R2 provisioning が失敗 → 任意の **Workers R2 Storage Edit** が付与されていません。コアのインストールには必要なく、ユーザーが artifact relay を明示的に有効化した場合にのみエラーとなります。

不足している権限だけを付与して再試行してください。より広い Token を盲目的に作り直したり、一般的なデプロイ失敗として報告したりしないでください。

## 症状：同じ Worker の別ホスト名は動くのに、一つのホスト名だけ失敗する

`*.workers.dev` がタイムアウトする、あるいは DNS/TLS で失敗する一方で、同じ Worker の Custom Domain が `/health` 200 を返す場合（またはその逆）、Worker のコードは健全で、失敗はホスト名/DNS/ネットワーク経路に固有のものです。Worker を再デプロイせず、別の Worker/R2/Connector も作成しないでください。ユーザーがドメインを所有している場合は、安定した本番 origin として Custom Domain を優先してください。そうでない場合は `workers.dev` を維持し、Link transport のフォールバック（direct → validated local proxy → shared relay）にネットワーク経路を任せてください。

## 症状：Connector を追加できない、または OAuth がループする

確認してください：

- MCP URL が `https://<stable-origin>/mcp` であること。
- 公開 base URL / OAuth issuer が同じ origin を使い、`/mcp` を含まないこと。
- protected-resource と authorization-server の metadata に到達できること。
- Worker のデプロイが意図したものであること。
- ChatGPT Workspace が Developer/custom MCP apps を許可していること。

OAuth Token の発行には成功したのに次の手順が失敗する場合、認証はすでに通過しており、MCP discovery/routing の段階にいると考えられます。

詳しくは [ChatGPT Connector](chatgpt-connector.md) を参照してください。

## 症状：Connector は接続済みと表示されるが、チャットの tools が 0

Connector のインストールと、会話が tool catalog を受け入れることは別の手順です。

次の順序で確認してください：

1. 新しい会話を開く。
2. 現在の公開 contract/version を確認する。
3. `tools/list` が成功することを検証する。
4. 互換性のない `inputSchema` が一つあって catalog 全体が拒否されていないか確認する。
5. 古い conversation のスナップショットとサーバー側の問題を区別する。

新しい会話で 18 tools が見え、古い会話で 17 が見える場合、通常サーバーは正常です。

## 症状：tools は見えるが `herdr_inspect` が workstation offline を報告する

これはもはや ChatGPT の schema 問題ではありません。ChatGPT が構造化された `workstation_offline` の結果を受け取ったなら、ChatGPT → MCP Edge の経路はその結果を返せる程度には生きており、Edge が選択された workstation への使用可能な Link WebSocket を持っていなかったということです。この判断をブラウザ拡張が行うことはありません。

現在の復旧は「全部を再起動する」のではなく層状です：

1. 最近まで接続していた workstation には、Edge 側で最大 **15 秒** の process-local reconnect grace が与えられます。検証済みの Link `hello` は待機中のリクエストを即座に起こします。この grace は Durable Object storage や alarm を書き込みません。
2. それでも workstation が利用できない場合、Edge は機械可読な復旧メタデータを返します：`retryable=true`、`delivery_state=not_delivered`、`retry_after_ms=5000`、および 5s / 10s / 20s の backoff を伴う読み取り専用 `herdr_inspect` probe ポリシーです。
3. ローカルの Link は通常の reconnect/backoff ループを続けます。Online への遷移が成功すると、prolonged-offline timer はクリアされます。
4. Link が **300 秒** 連続して Online になれない場合、診断証拠を伴って終了し、launchd の `KeepAlive` が新しい `dev.herdr-mcp.link-prod` プロセスを起動できるようにします。
5. server health watchdog は、実際に unhealthy なローカル server に対してのみ責任を持ちます。`workstation_offline` だけでは、健全な `dev.herdr-mcp.server` を再起動する理由になりません。

リプレイ規則は、retry hint よりも意図的に厳しくしています：

- `delivery_state=not_delivered`：リクエストは workstation に到達していません。復旧後に再送してもかまいません。その操作に idempotency key がある場合は再利用してください。
- `delivery_state=delivery_unknown`：read は明示的に retryable と示されている場合に再試行できますが、mutation はリプレイの前に state/evidence と突き合わせて reconciliation する必要があります。
- `delivery_state=delivered`、または非配送の証明がない場合：後から接続が切れたというだけの理由で mutation をリプレイしないでください。

workstation を次の順序で確認してください：

```bash
herdr-mcp status
herdr-mcp link status
launchctl print gui/$(id -u)/dev.herdr-mcp.link-prod
tail -n 100 ~/.config/herdr-mcp/link-prod.launchd.err.log
```

その後、workstation の identity、active runtime generation の健全性、Edge の最近の Link 状態を確認してください。workstation link の問題を直すために Connector を削除・再作成しないでください。また、ローカル server の健全性も悪いのでない限り、Link だけの失敗をグローバルな `herdr-mcp service restart` に格上げしないでください。

## 症状：inspect は動くがファイル操作が失敗する

よくあるゲート：

- パスが managed Git root の外にあり、かつ live な workspace/pane cwd によって正確に証明された non-Git operational root の外にある（別のプロジェクトから借用したディレクトリや、live topology が証明しない sibling は拒否されます）。
- ファイル名が secret 的なパス規則に一致する。
- 読み取り専用モードが有効になっている。
- 対象 root が書き込み allowlist に含まれていない。
- ファイルがすでに dirty で、明示的な確認が必要である。
- 同じプロジェクトで別の worker が活動していて、busy gate が並行書き込みを拒否している。

まず構造化エラーを読んでください。すべてのファイルゲートに対する既定の答えを shell によるバイパスにしないでください。

`herdr_exec` は `herdr_fs_*` よりも意図的に強い境界で、同じ secret-path フィルタリングは提供しません。

## 症状：TaskGroup / ExceptionGroup の control-plane エラーが一時的に起きる

agent やリポジトリが正常であっても、snapshot/pane 操作の失敗が表示されることがあります。

herdr-mcp は一部の read パスを、より狭い evidence ソースへ degrade することがあります：

- 大きな一つの snapshot の代わりに list API を使う。
- 決定的な Git 状態。
- managed-root への直接ファイル操作。

現在の事実を得るために `herdr_inspect` / `herdr_since` を再実行してください。control-plane の一度の一時的な乱れを、Git プロジェクトが使用不能である証拠として扱わないでください。

## 症状：prompt や exec がタイムアウトし、実行されたか分からない

規則は **mutation を盲目的に再試行しない** ことです。

### `herdr_prompt`

失敗が submit 後の状態待ちの間に起きた場合、agent はすでに prompt を受け取っている可能性があります。まず agent の状態/出力を inspect してください。同じ意図を繰り返す場合は `idempotency_key` を再利用してください。

### `herdr_exec`

コマンドがすでに可視の pane に配送されている場合、後からの control-plane タイムアウトは、そのコマンドを再送してよい許可ではありません。pane、Git 状態、ファイル、テストを inspect してください。

「クライアントに成功応答が届かなかった」ことは「何も起きなかった」ことを意味しません。

## 症状：ローカルの agent は終了したのに ChatGPT が続かない

これは多くの場合 MCP の失敗ではありません。

MCP が提供するのは：

```text
ChatGPT → workstation
```

ローカルタスクが後から完了しても、新しい ChatGPT turn が自動で作られることはありません。次の向きには：

```text
workstation → ChatGPT
```

browser continuity を使います：

- Native Messaging host がインストールされている。
- 現在の conversation が正しい workspace に binding されている。
- 該当する Auto scope が有効である、または HUD の手動アクションを使う。

詳しくは [Browser continuity](browser-continuity.md) を参照してください。

## 症状：HUD が誤った workspace 名を表示する

binding の identity は `workspace_id` で、label は表示データです。

ID が正しいのに label が古い場合、拡張は live な workspace catalog から label を更新すべきです。表示テキストを直すためだけに正しい binding を削除しないでください。

ID 自体が誤っている場合は、正しい workspace に binding してください。

## 症状：`standalone status` は最新に見えるのに、Chrome が古い/想定外の unpacked ビルドを実行している

これはパス ownership のドリフトです。`herdr-mcp extension standalone status` は Herdr がディスク上で何を管理しているかを証明するだけで、すでに設定済みの Chrome profile がどの unpacked ディレクトリを読み込んでいるかは証明しません。macOS では次を実行してください：

```bash
herdr-mcp doctor
```

デフォルトの `doctor` 要約が standalone 拡張の読み込みパスのずれを報告した場合は、`herdr-mcp doctor --verbose` で `expected` と `actual` のパスを確認してください。機械可読の証拠は `herdr-mcp doctor --json` の `standalone_extension` にあり、Chrome profile と `drift_count` も含まれます。`chrome://extensions` を開き、Herdr を見つけ、`expected_path`（通常は `~/.config/herdr-mcp/extensions/standalone/current`）からロード/再ロードしてください。その後 `doctor` を再実行します。別の identity 問題も証明されていない限り、Native Host ownership を変更したり拡張データを削除したりしないでください。doctor probe は読み取り専用で、Chrome の preferences にある Herdr の正確な extension ID エントリだけを検査します。

## 症状：Browser Control Center が開かない、workspace が表示されない、または Runtime unavailable のままになる

**Side Panel UI の問題**、**Native Messaging identity の問題**、**runtime の問題** を切り分けてください：

1. Herdr のツールバーアイコンをクリックし、Chrome が Control Center の Side Panel を直接開くことを確認してください。`control-center.html` に通常の Web ページとして遷移しないでください。
2. まず `herdr-mcp status` / `herdr-mcp doctor` で、ローカル runtime が健全であることを証明してください。
3. `herdr-mcp native-host status` が Native Messaging host の登録を報告するはずです。
4. Chrome が Store の拡張を更新した直後なら、影響を受ける Web ページを更新してください（必要なら Chrome を再起動します）。現在の content script が読み込まれるようにするためです。
5. `herdr-mcp native-host status` は、意図して選択した extension identity/channel と、active runtime generation と整合する Native Host runtime を報告するはずです。現在の runtime は STORE / STANDALONE / DEV をサポートし、古い runtime では利用できる channel が少ない場合があります。origin mismatch を、別のチャネルを推測して直そうとしないでください。まずインストール済み runtime がサポートするコマンドと、Chrome の実際の extension identity を inspect してください。
6. `Runtime healthy · event stream reconnecting` は、増分イベントが復旧している間もパネルが snapshot を持っていることを意味し、ローカル runtime 全体が停止していることを意味しません。権威ある reconciliation には Refresh を使ってください。

`Send instruction` は信頼されたローカル制御経路で実行されます。`Adjust current task` は正確な provider capability/outcome を返し、黙って Prompt になることはありません。ターミナルのみの pane では、fencing された `pane.send_input + Enter` 経路でコマンドを実行できます。任意の `Herdr API` は Preview-only のままです。いずれかの mutation が `uncertain` を報告した場合は、再試行の前に live state を inspect してください。Steer が `session_not_resolved` を報告する場合、選択された provider session に検証可能な control endpoint/thread/active-turn のマッピングがありません。これは capability の結果であり、transport の失敗ではありません。

詳しくは [Browser Control Center](browser-control-center.md) を参照してください。

## 症状：ChatGPT の応答が途中で止まる、切断される、または送信タイムアウトが表示される

元のタスクをすぐに再送しないでください。tool の mutation はすでに発生している可能性があります。

Continuity の復旧は evidence-first です：

1. 可能な場合は同じ origin の conversation state を inspect する。
2. server の状態が DOM より先に進んでいる場合は、ビューを更新/同期する。
3. evidence がリクエストを受け入れなかったと示す場合にのみ再試行する。
4. delivery が不確実な場合は fail closed する。
5. 通常の復旧を使い切った後にのみ handoff を検討する。

自動復旧で信頼できる evidence が得られない場合は、手動で更新し、**Herdr monitor** でローカル状態を読み直してから続けてください。

詳しくは [wake、復旧、handoff](browser-continuity.md) を参照してください。

## 症状：ChatGPT の Queue がすぐに送信されない、またはキュー内容が保留のままである

Queue は意図的に **即時送信ではありません**。assistant の turn が生きている間、内容は現在の conversation の durable なキューに留まり、turn が settled した後に、汎用の auto-continue より先に送信されるべきです。

確認してください：

- ページが ChatGPT であること。他のサイトは現在同じ Queue UI を公開していません。
- assistant がまだ生成中か、tool を使用中か、permission card を待っているか。live な turn は Queue によって中断されてはなりません。
- `turn-in-progress` や不確実な submit は ACK されず、破棄もされません。
- composer が空のときに Queue をクリックすると、まだ pending な batch を再試行できます。
- Queue を右クリックすると、現在の conversation キューを明示的にクリアします。
- handoff が確認された後、pending のエントリは新しい conversation へ移動し、source からリプレイされるべきではありません。

確認された delivery 無しに内容が消えることが、実際の信頼性上の失敗です。issue を起票する前に、conversation、現在の turn 状態、ブラウザ console の evidence を記録してください。

## 症状：手動 handoff が利用できない

ページ内 HUD の **Handoff** を使ってください。利用できない、または無効になっている場合は次を確認してください：

- 現在のサイト/conversation タイプが handoff をサポートしている。
- workspace が binding されている。
- workspace に active な working agent がいない。
- すでに active な transfer がない。

現在の scope は **Auto オン または Auto オフ** のいずれでもかまいません。handoff がサポートされる場合、対象の conversation は source の Auto 状態を継承し、transfer 中は source の自動 wake が一時停止します。

Handoff は packet を作成し、新しい conversation を作成し、seed を検証し、その後にのみ binding を移動しなければなりません。transfer が復旧可能/不確実な場合は、手動で unbind するのではなく、古い binding を安全アンカーとして保持してください。

## 症状：z.ai / DeepSeek が JSON tool call を出力した後に止まる

それは JSON→MCP bridge であり、ChatGPT Connector ではありません。

確認してください：

- Native Messaging host。
- ローカルの MCP tool catalog。
- 安定した conversation identity。
- 最後の実際の assistant メッセージが、まだ tool-call の JSON オブジェクトであるかどうか。
- `TOOL_RESULT` が返されたかどうか。
- ページの reload 後も、安全に再開できるだけの bridge context が残っているかどうか。

内部の tool JSON を最終的な自然言語の回答として扱わないでください。

詳しくは [JSON → MCP bridge](extension.md) を参照してください。

## 症状：Chromium が local-device / loopback 権限を要求する

一部の Chrome/Chromium profile は、ローカルの loopback アクセスに別の権限ゲートを適用します。

ブラウザの拡張設定で、その拡張のサイト/ローカルデバイスの権限を確認してください。Native Messaging が主要な信頼経路ですが、診断/互換の経路では依然として loopback 権限が表面化し得ます。

ブラウザの権限が pending であるというだけの理由で、Herdr の資格情報をローテーションしないでください。

## 症状：Cloudflare のデプロイが失敗する

次のケースを切り分けてください：

- 資格情報/identity の失敗。
- build/test の失敗。
- Worker はデプロイされたが health/routing が失敗した。
- workers.dev は動くが Custom Domain/DNS が失敗する。

最小権限の資格情報を使い、特定の層だけを修正してください。route の問題をアカウント管理者 Token に格上げしないでください。

詳しくは [Cloudflare Edge の資格情報](cloudflare-edge-token.md) と [Cloudflare Edge のデプロイ](cloudflare-edge-deployment.md) を参照してください。

## 症状：runtime のアップグレードでローカルの挙動が壊れた

同じ contract epoch 内の実装変更の場合：

```bash
bin/herdr-runtime-generation status
```

active/previous の generation を inspect し、適切な場合は Runtime A/B の rollback 経路を使ってください。

tool catalog/schema が変わった場合、それは contract migration であり、通常の runtime A/B 問題ではありません。`herdr-self-update` を使って epoch 境界をすり抜けないでください。

詳しくは [Runtime A/B](runtime-self-upgrade.md) を参照してください。

## issue に有用な evidence

有用で secret を含まない診断情報：

- `boot_id`。
- runtime version / contract epoch。
- workstation id。
- 失敗した tool。
- failure phase / delivery state。
- 失敗の前後で Git/pane/agent の状態が変化したか。
- Edge health/workstation status。
- ChatGPT の conversation が新しかったか古かったか。
- binding された workspace identity。

ログを共有する前に、bearer Token、OAuth JWT、Cloudflare secret、機密性の高いプロジェクト内容を除去してください。

## 再起動は最後に、最初ではなく

再起動はサービスを復旧させ得ますが、根本原因を説明する evidence を消し去ることもあります。

推奨：

1. 現在の状態を記録する。
2. 失敗している層を特定する。
3. 関連するコンポーネントだけを再起動する。
4. その後、その層と次の層を検証する。

これにより「また動くようになった」が、実際の診断になります。
