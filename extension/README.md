# herdr → Web 唤醒 (浏览器插件)

方向: **herdr agent 干完活 → 插件向绑定的网页会话输入框写入消息并提交 → 唤醒网页 AI**。
与 ctmc(网页 AI → 本地 MCP)方向相反;ctmc 的适配器选择器/写入套路/绑定模式在此复用。

## 架构

```
extension/
├── manifest.json              # MV3; 4 站点 content_scripts + background + popup + options
├── background.js              # 配置/绑定存储/每绑定一条 SSE 推送流/唤醒路由/MAIN world 插入
├── popup.html + popup.js      # 绑定 UI: 列 herdr agents → 点选绑定到当前会话
├── options.html + options.js  # herdr-mcp URL/token/唤醒模板/开关
├── icons/                     # 16/48/128 PNG
└── content/
    ├── base.js                # BaseAdapter: 输入框定位/React 原生 setter 写入/Enter 发送/会话身份键
    ├── injector/              # 第一层 Injectable: 站点输入框差异
    │   ├── zai.js             # textarea (ctmc 实测)
    │   ├── deepseek.js        # textarea[name=search] (ctmc 实测)
    │   ├── claude.js          # contenteditable 链 ⚠️ 未实测 (本机未登录)
    │   └── chatgpt.js         # #prompt-textarea (ego-browser 实测 2026-08-20)
    ├── webmcp/
    │   └── speaks-json.js     # 第二层 SpeaksJSON (仅 z.ai/deepseek): JSON tool call 平衡括号
    │                          #   解析 (ctmc 移植) + 回复完成判定 + 唤醒后投递确认
    └── wake.js                # 唤醒执行: 写输入框 → 提交 → (SpeaksJSON 站)确认回复开始
```

## 工作流

1. 用户在 z.ai/deepseek/claude.ai/chatgpt.com 打开一个对话。
2. 点扩展图标 → popup 列出 herdr 当前 agents(来自 `GET /push/state`)→ 点某个 agent 绑定。
   - 绑定键 = **会话身份键**(origin+pathname,ctmc 模式),存 `chrome.storage.local`(`herdrWakeBindings`),
     tabId 不作为权威(每次页面加载由 content script `h2w_register` 刷新)。
3. background 为该绑定开一条 SSE:`GET /push/events?pane=<pane>`(Bearer)。
4. agent `working→idle/done/blocked`(`agent_settled`)→ background 按模板生成唤醒消息
   (含输出片段, 服务端 2s 内补发 `agent_output`)→ 路由到绑定 tab 的 content script。
5. content script 写入并提交:
   - z.ai/deepseek: 原生 setter + input/change + 400ms 延迟 + Enter;
   - claude/chatgpt: 经 background `chrome.scripting` **MAIN world** `execCommand("insertText")`
     (隔离世界不提交编辑模型 — ctmc 实测教训) + 点发送按钮;
   - 若输入框已有内容(用户在打字)→ 不覆盖, 上报 blocked。
6. (z.ai/deepseek) 提交后 SpeaksJSON 确认回复区出现 (**与提交前快照对比**: 文本变化或
   块数增加才算新回复), ack 回报 background。

## 状态反馈: 工具栏图标徽章 (v0.1.1, 替代页内状态点)

页内右下角状态点在 v0.1.1 移除 (用户反馈含义不明)。改为工具栏图标徽章 (background 驱动):

| 时机 | 徽章 |
|---|---|
| 绑定 agent 开始工作 (`agent_working`) | 琥珀 `…` |
| 唤醒成功 (settle + wake ack ok) | 绿 `✓` (4s) |
| 唤醒失败 (wake ack error) | 红 `!` (8s) |
| 其余时间 / 无绑定 | 无徽章 (绑定状态看 popup) |

## 权限弹窗自动允许 (v0.1.1)

唤醒期间 (提交前 ~90s 窗口) 检测**页面内**权限弹窗并自动点肯定按钮:

- 判定 (保守 fail-closed, 见 `content/base.js` 纯函数 + 单测):
  - 弹窗需形如 `[role=dialog] / .modal / [class*=dialog]` 且文本含权限类字样
    (允许/授权/权限/同意/allow/permission/grant/approve);
  - 按钮需为肯定前缀 (允许/同意/授权/allow/approve/grant) 或整词 (ok/yes/continue);
  - **拒绝/取消/不允许/deny/decline/block/no 一律不点**;
- 开关: 选项页 "唤醒后自动点允许权限弹窗" (默认开)。
- **平台硬限制 (如实说明)**: 浏览器**原生**权限条 (通知/麦克风/摄像头/剪贴板) 不是
  页面 DOM, 扩展无法自动点击 — 这类提示仍需用户手动点。


## 唤醒语义 (阶段 3 修正)

**绑定 ≠ 立即唤醒** — 状态机在 `extension/binding-core.js` (纯函数, 可单测):

- 初始 `hello`: 只记录当前 seq/status 作为基线, **不唤醒** (绑定到已 idle/done 的
  agent 不应立刻打扰网页)。
- `agent_working`: armed (`status="working"`, 持久化)。
- `agent_settled`: 仅当 persisted status==="working" (工作确实发生过) 才唤醒;
  同一 seq 不重复唤醒 (lastSettle 去重)。
- 重连 `hello`: persisted `working` 且快照已 settled 且 seq 变化 → 补一次唤醒
  (离线期间错过的 settle 恢复)。
- **过期强制生效**: 绑定 24h 过期, `loadBindings()` 剪枝过期记录、中止对应推送流并
  持久化清理; 剪枝后该会话视为未绑定。

## 恢复 (阶段 3)

- **页面刷新**: content script 重载 → `h2w_register` 用会话身份键找回绑定, 刷新 tabId。
- **浏览器重启**: 绑定在 storage.local; background `onStartup` 等 `configReady`
  (配置从 storage 加载完) 后重建推送流; SSE 15s keepalive 同时充当 MV3 service
  worker 保活。配置变化 (URL/token/模板) → 全量中止旧流并重建。
- **错峰恢复**: agent 在扩展离线期间已 settled → 重连 `hello` 快照 (含
  `state_change_seq`) + `lastSettle.seq` 去重, 仅当 persisted status 为 working
  (工作确实发生过且未通知过) 才补一次唤醒。

## 安装 / 加载测试

1. 构建宿主: `cd ~/Documents/herdr-mcp && npx tsc`(需等并行 OAuth 改动落地后) + `herdr-mcp restart`。
2. Chrome → `chrome://extensions` → 开发者模式 → **加载已解压的扩展程序** → 选
   `~/Documents/herdr-mcp/extension` 目录。
3. 点扩展图标 → 选项页: 填 `http://127.0.0.1:8772` + `herdr-mcp token` → 测试连接。
4. 打开 z.ai/deepseek/claude.ai/chatgpt.com 对话 → 扩展图标 → 点一个 herdr agent 绑定。
5. 让该 agent 干一次活 (working→settled) → 观察网页输入框被写入并提交。

改 content 代码后: bump `H2W_SCRIPT_VERSION`(background.js 与 wake.js 顶部注释),
重载扩展后插件会自动刷新已打开的目标站标签页(ctmc 版本同步机制)。

## 验证状态

| 站点 | 输入框 | 发送 | 实测 |
|---|---|---|---|
| z.ai | `textarea`(base 默认) | Enter 派发 | ✅ ctmc 实测 (2026-08-03) |
| deepseek | `textarea[name=search]` | Enter 派发 | ✅ ctmc 实测 (2026-08-03) |
| chatgpt.com | `#prompt-textarea[contenteditable="true"]` | `button[data-testid="send-button"]` | ✅ ego-browser 实测 (2026-08-20): MAIN world execCommand 提交进 ProseMirror 模型 |
| claude.ai | contenteditable 防御性链 | send-button 链 | ⚠️ **未实测**: 本机 claude.ai 未登录(登录页), 选择器为推断 + 兜底链, 登录后需校准 |

### claude.ai 待办 (实测校准)

登录 claude.ai 后用 DevTools 或 ego-browser 确认:
- 输入框真实结构 (ProseMirror? `.ql-editor`? id/class)
- 发送按钮选择器 (data-testid / aria-label)
- MAIN world `execCommand("insertText")` 是否提交进编辑器模型
- 会话 URL 形态 (确认 host+pathname 会话键足够区分对话)

## 推送端点验证 (宿主侧)

```bash
# 需要 token (herdr-mcp token)
curl -N -s -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:8772/push/events?pane=wH:p1" | head -20
# 首帧应为 event: hello (含当前 agents 快照), 之后 15s 一条 keepalive 注释

curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8772/push/state
# {"server_time":..., "agents":[...]} — 全部 agent 当前状态

# 端到端 (真实 agent 转换):
node tests/push_sse.mjs --integration
```

## 已知限制 (如实记录)

- claude.ai 选择器未实测(见上)。
- 唤醒是"写消息 + 提交"的 v1,不做自动续跑/自动应答(ctmc v0.2.0 移除自动续跑的教训)。
- MV3 service worker 可能被回收: 依赖 SSE 15s keepalive 保活;极端情况下(浏览器
  挂起/网络断) settle 事件错过, 由重连后的 hello 快照兜底补唤醒。
- 若用户正在输入框打字, 唤醒会跳过(不覆盖用户内容)。
- 绑定 24h 过期(ctmc 模式), 过期后需重新绑定。
- 浏览器原生权限条 (通知/麦克风等) 无法自动点允许 — 只能自动点页面内 DOM 权限弹窗。
