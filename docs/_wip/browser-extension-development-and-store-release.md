# 浏览器扩展开发、开发版安装与 Chrome Web Store 上架计划

> 状态：WIP / 开发期文档
>
> 当前决定：**开发期仍可使用 unpacked build，但最终用户分发正式切换到 Chrome Web Store。已启动 Developer 注册 / Store item 首次发布流程。**
>
> 本文记录开发期安装、Native Messaging 身份、测试门槛、未来商店上架步骤和待办，避免后续把“代码已合并”“开发版已加载”“商店版已发布”混为一谈。

## 1. 当前产品形态

Herdr 浏览器扩展不是独立 runtime。它是本机 Rust `herdr-mcp` 与支持站点之间的连续性和控制层。

当前链路：

```text
ChatGPT / Claude / z.ai / DeepSeek
        ↓ content scripts
Chrome MV3 extension
        ↓ service worker
Chrome Native Messaging
        ↓ local host
herdr-mcp Rust runtime
        ↓ Unix socket / local Herdr API
Herdr workspace / pane / agent
```

当前扩展名称：`herdr → Web wake`。

当前开发版本：`0.1.71`。`0.1.69` 首次上传 Chrome Web Store 时因 manifest `description` 超过 132 字符被拒；`0.1.70` 完成 Store 合规修正，`0.1.71` 继续收紧 HUD / Side Panel 的职责边界与信息密度。

核心能力包括：

- workspace / conversation / Project binding；
- progress、settled、页面恢复与长对话 handoff；
- ChatGPT Project continuity；
- z.ai / DeepSeek JSON → MCP bridge；
- 工具栏图标直接打开 Chrome Side Panel Browser Control Center；
- Side Panel 跟随当前激活 tab，识别 Project / conversation；workspace 实时状态与当前页面 binding / unbinding 合并在同一行，手动接力留在 Current page；
- HUD 仅保留网页状态、Herdr 状态、紧凑 binding 数量、Auto 和三个当前网页会话动作；不再有 drawer 或 binding UI；
- workspace / pane / agent 实时状态；
- explicit pinned target；
- Phase A read-only / mutation dry-run control model；
- ChatGPT Queued Insert：生成中只入队，不用原生 Send 打断当前回复，当前 turn settled 后优先于泛化“继续”发送；
- 页面自愈、429/backoff、send timeout 等恢复策略。

扩展与 Rust runtime 是两个独立发布面。扩展版本升级不等于 runtime 已升级，runtime 升级也不等于浏览器已经 reload 新扩展。

## 2. 现在开发版在哪里安装

### 2.1 安装目录

开发版直接加载仓库中的：

```text
<repo>/extension/
```

这里的 `manifest.json` 必须位于所选目录根部。

### 2.2 Chrome / Chromium 安装步骤

1. 先确保本机 `herdr-mcp` runtime 正常。
2. 在对应 repo checkout 中安装 Native Messaging host：

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

3. Chrome 打开：

```text
chrome://extensions
```

4. 打开右上角 **Developer mode / 开发者模式**。
5. 点击 **Load unpacked / 加载已解压的扩展程序**。
6. 选择：

```text
<repo>/extension/
```

7. 打开 ChatGPT / Claude / z.ai / DeepSeek，检查页面 HUD 是否加载。
8. 点击 Herdr 工具栏图标，确认 Side Panel Control Center 直接打开。
9. 首次开发环境至少确认 Native Messaging 不报：

```text
Access to the specified native messaging host is forbidden.
```

如果出现该错误，优先检查 extension ID 与 Native Messaging manifest 的 `allowed_origins` 是否一致，而不是复制 bearer/token 到扩展设置。

### 2.3 开发版更新

当同一个 checkout 的 `extension/` 内容更新后：

1. 打开 `chrome://extensions`；
2. 找到 `herdr → Web wake`；
3. 点击 **Reload / 重新加载**；
4. 刷新已打开的支持站点页面；
5. 检查 HUD / Control Center / Options 的版本和功能。

当前 content/background 具有版本自愈逻辑，但开发验证仍应显式 Reload，不能把自动 reinjection 当作发布验证替代品。

## 3. 开发期最重要的身份规则：不要随意换加载路径

当前 unpacked build 没有在 manifest 中固定 `key`。

Chromium 对 unpacked extension 的身份与本地加载路径相关；而 Herdr Native Messaging installer 会把允许的扩展 origin 写入 host manifest：

```json
{
  "allowed_origins": [
    "chrome-extension://<extension-id>/"
  ]
}
```

Chrome Native Messaging 的 `allowed_origins` **不能使用 wildcard**。

因此开发期规则是：

- 尽量长期从同一个绝对路径加载 `<repo>/extension/`；
- 不要为了测试随意把 `extension/` copy 到不同目录后继续沿用旧 Native Host；
- 如果必须换 checkout/path，重新运行：

```bash
bin/herdr-extension-host install
bin/herdr-extension-host status
```

- 不要把 `HERDR_MCP_TOKEN` 写入 extension storage 来绕过 origin 错误；
- 不要把 Native Messaging `allowed_origins` 放宽成 wildcard；Chrome 也不允许这么做。

未来 Chrome Web Store 版必须从 path-derived dev identity 迁移到固定 Store Extension ID，见第 9 节。

## 4. 当前开发目录结构

主要目录：

```text
extension/
├── manifest.json
├── background.js                 # MV3 service worker orchestration
├── options.html / options.js
├── control-center.html
├── control-center.css
├── control-center.js             # Chrome Side Panel UI
├── control-center-model.js
├── browser-state.js              # pure state normalization / incremental reducer
├── browser-state-store.js
├── target-pin.js
├── control-actions.js            # action/risk classification; Phase A mutation dry-run
├── queued-insert-core.js         # queued user message state machine
├── conversation-health.js
├── recovery-controller.js
├── context-pressure.js
├── performance-core.js
├── content/
│   ├── base.js
│   ├── injector/
│   │   ├── chatgpt.js
│   │   ├── claude.js
│   │   ├── zai.js
│   │   └── deepseek.js
│   ├── hud/
│   ├── webmcp/
│   └── wake.js
└── locales/
```

### 4.1 Service Worker 原则

`background.js` 只承担 orchestration：

- storage / bindings；
- Native Messaging；
- shared push stream；
- content-script routing；
- handoff / recovery coordination；
- Queued Insert persistence/dispatch；
- Control Center port fan-out。

纯状态处理优先放进独立模块，不继续把所有逻辑堆入 `background.js`。

### 4.2 Control Center 数据流

当前设计：

```text
Rust /push/state
    ↓ one initial snapshot
background.js
    ↓
BrowserStateView
    ↓
Side Panel

Rust /push/events
    ↓ incremental lifecycle/status events
background.js
    ↓
BrowserStateView incremental reducer
    ↓
Side Panel partial/coalesced render
```

要求：

- 不做固定频率 state polling；
- initial snapshot + incremental events；
- reconnect / event gap / explicit Refresh 才做 full reconcile；
- terminal output必须 bounded；
- hidden Side Panel 不做无意义高频 DOM rebuild；
- explicit target pin 不随 focus 自动 retarget；
- Phase A mutation 仍然 dry-run / fail-closed。

### 4.3 Queued Insert 语义

ChatGPT composer 旁的 Queued Insert 不是第二个“立即发送”按钮。

语义：

```text
user text
  ↓ Queue
persistent per-conversation queue
  ↓
assistant turn still generating? ── yes ──> keep queued
  ↓ no / turn settled
merge queued entries in order
  ↓
send one next-turn user message
  ↓ success ACK
remove only delivered batch
```

约束：

- 不点击原生 Send 打断 live assistant turn；
- queued content 比自动泛化“继续”优先；
- `turn-in-progress` 失败不得 ACK/drop；
- handoff commit 时 queue 必须迁移到 target conversation；
- queue 有数量/大小/过期上限；
- 不把 queue 变成无限持久消息数据库。

## 5. 开发版测试矩阵

先遵守根目录 `AGENTS.md` 的 Validation ownership matrix。

**不要使用仓库级 `npm test` 作为 Rust runtime gate。**

### 5.1 浏览器扩展专项

典型快速 gate：

```bash
node --test tests/browser-control-plane.test.mjs tests/queued-insert.test.mjs
node tests/manual/extension_smoke.mjs
node tests/manual/background_bind_test.mjs
```

按改动范围补充：

```bash
node --test tests/extension-local-auth.test.mjs
node --test tests/extension-native-host.test.mjs
node --test tests/extension-recovery.test.mjs
node --test tests/options-i18n.test.mjs
```

如果测试依赖 `dist/*.js`，必须先：

```bash
npm run build
```

全仓 Node/compatibility gate 只有在确实需要时才按 CI 顺序运行；不要因为 `package.json` 存在就假定 Node 是 herdr-mcp 主 runtime。

### 5.2 Rust runtime gate

如果改到 Rust runtime、Native Host、`/push/state`、`/push/events` 等：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

浏览器 extension test 与 Rust runtime test 是两条独立 validation lane。

### 5.3 真实浏览器 smoke

涉及 UI / content script / Native Messaging / Side Panel 时，静态测试不够。

至少验证：

- unpacked extension 真实加载；
- MV3 service worker target 正常；
- 工具栏图标可直接打开 Control Center；
- Side Panel / `control-center.html` 可真实渲染；
- 页面无 console/runtime exception；
- Native Messaging exact origin 行为正确；
- supported site content script 实际注入；
- Queue button 位于 composer action 区合理位置；
- generating 时 Queue 不触发原生 Send；
- reload/reconnect 后 state 恢复。

测试使用独立 Chrome/Chromium profile，不能污染用户日常浏览器 profile。

## 6. 版本管理

当前扩展版本来源：

```text
extension/manifest.json
```

还存在 background/content version 常量和 smoke 断言。升级时必须保持一致。

开发规则：

- 功能变更需要浏览器重新加载时，提升 extension version；
- 不允许 manifest 已升级但 background/content version 未同步；
- 不把 Rust runtime tag 当 extension version；
- 不把 extension version 当 Chrome Web Store publication state。

建议后续增加单一版本源和自动同步脚本，避免手工三处更新。

## 7. 当前安全边界

### 7.1 Native Messaging

正常路径：

```text
extension service worker
  ↓ runtime.connectNative / sendNativeMessage
managed Native Messaging host
  ↓ mode-0600 Unix socket
herdr-mcp Rust runtime
```

原则：

- extension 不保存长期 Herdr bearer；
- Native Messaging manifest 不保存长期 secret；
- exact extension origin allowlist；
- local host 是本机 privilege boundary；
- Cloudflare/public control path 与 Browser Control Plane 本地 IPC 分离。

### 7.2 Web Store 前权限审计

当前 manifest 包含：

```json
{
  "permissions": [
    "storage",
    "scripting",
    "alarms",
    "nativeMessaging",
    "sidePanel"
  ],
  "host_permissions": [
    "http://127.0.0.1:8772/*",
    "<all_urls>"
  ]
}
```

上架前必须逐项证明必要性，尤其是：

- `<all_urls>`；
- `scripting`；
- `nativeMessaging`；
- 网站内容/对话数据访问。

Chrome Web Store 要求申请完成 single purpose 所需的最小权限。不能为了未来功能提前请求宽权限。

## 8. Chrome Web Store 首次发布已经启动

2026-08-28 已在本机 Chrome 打开 Chrome Web Store Developer Dashboard 注册入口。当前官方要求重新核对后确认：

- 新 publisher 需要注册 Chrome Web Store Developer、接受协议并支付一次性注册费（当前官方说明为 US$5）；
- 发布/更新扩展的 Google Account 必须启用 2-Step Verification；
- 首次发布前必须完成 Store listing 与 Privacy practices；
- Privacy practices 必须说明 single purpose、逐项解释 permissions，并准确声明 remote code；
- Manifest V3 不允许执行 remotely hosted code；
- 官方 minimum-permission policy 要求只申请当前功能真正需要的最小权限。

因此当前 `host_permissions` 中的 `<all_urls>` 在首次提交前必须独立做一次最小权限审计；不要仅因为现有代码能运行就假定商店审核会接受宽权限。

当前最终用户文档已经改为：**Chrome Web Store 是唯一正式浏览器扩展安装路径**。Store listing 正式上线前，普通用户直接跳过扩展，不回退到 `Load unpacked`。

官方参考：

- https://developer.chrome.com/docs/webstore/register
- https://developer.chrome.com/docs/webstore/prepare
- https://developer.chrome.com/docs/webstore/cws-dashboard-privacy
- https://developer.chrome.com/docs/webstore/using-api

当前阶段继续：

```text
Git/main
  ↓
unpacked development extension
  ↓
real browser smoke
```

本阶段不做：

- Developer Dashboard 正式提交；
- Store Listing 上线；
- production Store ID 写入 installer；
-公开发布；
- Chrome Web Store API 自动发布；
-用户级一键 installer 产品化。

原因：Store ID 与 Native Messaging origin 是一个需要一次性设计正确的身份边界；不能为了“先上架看看”临时打补丁。

## 9. Chrome Web Store 上架执行设计

### 9.1 第一阶段：注册 publisher + 创建 Store item + 冻结 ID

当前执行顺序：

1. 注册 Chrome Web Store Developer；
2. 从 `extension/` 生成 ZIP；
3. ZIP 根目录直接包含 `manifest.json`；
4. Developer Dashboard → Add new item → upload；
5. 先完成 Store listing / Privacy / permission justification；
6. 记录 Chrome Web Store item / Extension ID；
7. 从 Dashboard 获取开发期需要的 public key / identity 信息；
8. 冻结 production extension identity；
9. 优先用 Trusted Testers 做第一轮真实商店安装 / 自动更新 UAT；
10. UAT 通过后再提交 public review。

官方参考：

- https://developer.chrome.com/docs/webstore/prepare
- https://developer.chrome.com/docs/extensions/reference/manifest/key

### 9.2 固定 Store ID 与开发 ID

上架前必须明确两种身份策略之一：

#### 策略 A：开发版与商店版使用同一 ID（优先评估）

使用 Store item 对应的 public key 在开发期固定 unpacked extension ID。

优点：

- Native Host `allowed_origins` 简单；
- local storage / permissions / identity 更接近生产；
-真实 pre-release smoke 与商店身份一致。

注意：

- 只处理公开 identity/public key；
- 绝不能提交任何 Chrome Web Store 私钥、OAuth refresh token 或发布凭据。

#### 策略 B：dev/store 两个明确 ID

Native Host installer 显式允许受控 dev origin + production Store origin。

例如：

```json
{
  "allowed_origins": [
    "chrome-extension://<DEV_ID>/",
    "chrome-extension://<STORE_ID>/"
  ]
}
```

禁止 wildcard。

官方 Native Messaging 规则：

- https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging

### 9.3 Native Host installer 改造

商店发布前，installer 不能继续只依赖当前 repo path 推导 production identity。

必须支持：

- fixed production Store ID；
- 可选、明确受控的 development ID；
- existing registered origin migration；
- install/status/uninstall/rollback；
- origin drift 检测；
- 不因升级 runtime 覆盖成错误 origin；
- Chrome / Chromium / Edge / Brave / ego lite 的目标清单仍保持各浏览器自己的注册路径。

发布版最终用户不应该执行：

```text
git clone
Load unpacked
bin/herdr-extension-host install
```

最终目标：

```text
Herdr installer
  ├─ install/update Rust runtime
  ├─ install Native Messaging host
  └─ register production extension origin

Chrome Web Store
  └─ install browser extension
```

## 10. Chrome Web Store 打包

未来新增一个确定性脚本，例如：

```text
scripts/package-chrome-extension.mjs
```

目标输出：

```text
dist/chrome/herdr-web-wake-<version>.zip
```

ZIP 必须满足：

```text
manifest.json
background.js
control-center.html
...
```

而不是：

```text
extension/
  manifest.json
```

打包脚本应：

- 从 manifest 读取版本；
- 只包含运行时必须文件；
- 排除测试、临时日志、开发 profile、私钥、凭据；
- 输出 SHA-256；
- 可复现构建；
- 在 CI 中解压后重新跑 manifest/static smoke。

## 11. Store Listing / Privacy / Policy

上架前必须完成单独 policy audit。

### 11.1 Single purpose

建议方向：

> Connect supported AI web conversations to the user's local Herdr workspace runtime for user-controlled continuity, monitoring and browser-side control.

最终文案应进一步收紧，保证所有主要权限和功能都能解释为这个 single purpose 的组成部分。

### 11.2 Privacy

扩展会处理网页内容、用户输入、对话状态和本机 Herdr workspace 信息，因此必须准备准确 Privacy Policy 和 Store Privacy disclosures。

必须写清：

- 访问什么数据；
- 哪些只在本机处理；
- 哪些会发往用户主动使用的网站或 Herdr 服务；
- 是否保存；
- 保存多久；
- 如何删除；
- 是否有人类可访问；
- 是否与第三方共享；
- 安全传输方式；
- 明确遵守 Chrome Web Store Limited Use 要求。

官方政策：

- https://developer.chrome.com/docs/webstore/program-policies/policies
- https://developer.chrome.com/docs/webstore/user_data

### 11.3 Remote-hosted code

Manifest V3 送审时不允许把可执行 JS/WASM 从远端下载后运行，也不能通过 `eval()` 等方式执行远端逻辑。

Herdr 可以获取数据、API 响应和模型结果，但**执行逻辑必须包含在提交审核的 extension package 中**。

送审前做一次：

```text
remote-hosted-code audit
```

### 11.4 Listing assets

至少准备：

- extension icon；
- Store description；
- screenshots；
- promotional images（若所选 listing 需要）；
- Privacy Policy URL；
- support/homepage URL；
- Test instructions；
- reviewer 可复现的 Native Host 安装/连接方式。

特别是 Herdr 依赖本机 Native Host，审核人员必须获得足够的测试说明，否则“扩展装好但 Runtime unavailable”容易被误判为不可用。

## 12. 上架渠道顺序

未来不要直接 Public。

建议：

```text
Local unpacked
    ↓
Private trusted testers
    ↓
Unlisted beta
    ↓
Public
```

Chrome Web Store 支持 Public / Unlisted / Private；三种可见性都需要符合相同政策和审核要求。

官方参考：

- https://developer.chrome.com/docs/webstore/cws-dashboard-distribution
- https://developer.chrome.com/docs/webstore/publish

如果未来维持单独 beta item，必须遵守 Chrome 对 testing/beta listing 的命名与重复内容规则。

## 13. Chrome Web Store API：以后再自动化

公开发布稳定后再接 CI 自动上传/提交，不在首次上架前做。

未来流程可为：

```text
merge release branch
  ↓
extension package gate
  ↓
artifact + sha256
  ↓
Chrome Web Store API upload
  ↓
submit for review
  ↓
manual approval / deferred publish
```

Chrome Web Store API 当前支持创建/更新/发布 item；发布账号要求满足 Google Developer account / 2-step verification 等条件。

官方参考：

- https://developer.chrome.com/docs/webstore/using-api

发布 OAuth client、refresh token、access token 必须只存 GitHub Actions secret /受管 secret store，禁止进入 repo。

## 14. 首次商店发布前的硬 Gate

以下全部满足才允许从 WIP 转正式 release plan：

- [ ] extension single purpose 已冻结；
- [ ] Store item 已创建但尚未公开；
- [ ] production Store ID 已冻结；
- [ ] Native Host installer 支持 production Store origin；
- [ ] dev/store identity 策略明确；
- [ ] manifest permissions 完成最小权限审计；
- [ ] `<all_urls>` 已证明必要，或缩小；
- [ ] Privacy Policy 已上线；
- [ ] User Data / Limited Use disclosures 完整；
- [ ] remote-hosted-code audit PASS；
- [ ] extension package deterministic；
- [ ] unpacked真实浏览器 smoke PASS；
- [ ] Store-ID build + Native Host real smoke PASS；
- [ ] toolbar action → Side Panel、active-tab Current page、single-path binding/handoff、compact HUD、Queued Insert smoke PASS；
- [ ] ChatGPT / Claude / z.ai / DeepSeek 支持矩阵重新验证；
- [ ] handoff/recovery 回归 PASS；
- [ ] Rust Native Messaging install/status/uninstall/rollback PASS；
- [ ] reviewer test instructions 可复现；
- [ ] screenshots/listing metadata 与真实产品一致；
- [ ] Private trusted-tester UAT PASS；
- [ ] rollback / disable strategy 已确认。

## 15. 开发期发布前检查清单

普通开发版更新不走商店，但每次准备给真实用户/测试人员 reload 前至少检查：

```text
[ ] branch/worktree ownership clean
[ ] manifest version correct
[ ] background/content version aligned
[ ] extension unit tests green
[ ] extension_smoke green
[ ] background_bind_test green
[ ] Native Messaging tests green if touched
[ ] Rust gate green if runtime/push/native-host touched
[ ] real Chromium smoke green for UI/content changes
[ ] native host origin matches the directory actually loaded by browser
[ ] no secret/token/private key in extension tree
[ ] git diff --check
```

## 16. 常见故障

### `Access to the specified native messaging host is forbidden.`

优先检查：

1. Chrome 当前 extension ID；
2. Native Messaging manifest `allowed_origins`；
3. 是否换过 unpacked `extension/` 路径；
4. 是否需要重新执行 `bin/herdr-extension-host install`；
5. 是否加载了另一个临时 worktree 的 extension。

不要用 bearer/token 绕过。

### UI 已更新但页面还是旧内容

1. `chrome://extensions` Reload；
2. 页面 hard reload；
3. 检查 service worker 是否是新版本；
4. 检查 content script version；
5. 必要时重新打开该站点 tab。

### `Runtime unavailable`

区分：

- extension 没加载；
- Native Host origin 不匹配；
- Native Host 没注册；
- Rust runtime 没运行；
- runtime 已运行但 extension IPC socket 不健康。

不要把所有情况归因于 localhost/token。

## 17. 相关项目文档

当前稳定用户/架构文档：

- `docs/i18n/zh-CN/extension.md`
- `docs/i18n/zh-CN/browser-control-center.md`
- `docs/i18n/zh-CN/browser-continuity.md`
- `docs/i18n/zh-CN/extension-bridge.md`
- `docs/i18n/zh-CN/extension-wake.md`
- `docs/i18n/zh-CN/agent-install.md`
- `docs/i18n/zh-CN/troubleshooting.md`
- `AGENTS.md`
- `docs/ga-release-gate.md`

本文只负责开发期与未来 Store release planning。正式发布后，应把稳定安装方式同步到用户文档，而不是让用户长期依赖本 WIP 文档。

## 18. 当前决定记录

截至 2026-08-28：

- 浏览器扩展继续以 unpacked development build 方式开发；
- 浏览器扩展开发版本已进入 `0.1.71`；Side Panel 是 binding / unbinding、手动接力与本机详细状态的主入口，底部固定本地 Herdr target；HUD 保持当前网页会话的紧凑状态与快捷动作面；
- Browser Control Center Phase A 与 pane lifecycle 已进入 main，0.1.65 进一步补齐 en / zh / ja、Pinned Target / Preview-only 产品文案与 Settings 入口；
- ChatGPT Queued Insert 已进入 main，正式用户文档使用“排队 / Queue”描述其 next-turn 语义；
- Chrome Web Store 首次发布流程**已经启动**；Developer Dashboard publisher 已可用，Store item 已创建；
- 普通用户正式安装路径已经切为 Chrome Web Store，Store listing 上线前直接跳过扩展；
- Store Extension ID 已冻结为 `kpcengcaammanfnbclapecdgahdmhanp`；Store listing、128x128 icon、1280x800 screenshot 与 Privacy practices 已写入 draft；
- **当前 Store 发布 blocker：Native Messaging origin 仍绑定开发版 path-derived ID。** stable `0.4.0` 当前 `native-host status` 的 extension ID 为 `dklcamincneeijhcelpkdbcekfemldii`，与 Store ID 不同；在正式提交审核前必须提供受支持的 Store-origin 安装/迁移路径，并完成商店安装后的真实 Native Messaging smoke；
- Store Privacy Policy 已补入正式 docs，待 docs PR 合并并由 GitHub Pages 发布后，将 `https://whshang.github.io/herdr-mcp/docs/en/privacy.html` 写入 Store Privacy tab；
- Publisher contact email 仍需配置并完成 Google verification；该邮箱会公开展示，不得由 Agent 猜测；
- 首次 Store submit 前必须审计 `<all_urls>` 等 permissions 是否能缩窄；
- 后续每次 Store 发布仍需重新核对实时 Chrome Web Store policy，不得把本 WIP 的时间点描述当成永久不变的商店规则。
