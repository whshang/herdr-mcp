# ctmc 逆向调研 (阶段 0)

> 调研对象: `~/Documents/ctmc`(已完成的浏览器插件,方向: 网页 AI → 本地 MCP)。
> 本项目的方向相反: herdr agent 干完活 → 触发浏览器插件向绑定的网页会话写入消息并提交。
> 本文件逐项记录 ctmc 的关键实现细节,供阶段 1-3 直接复用/避开。

调研时点: 2026-08-20, ctmc HEAD = `4f36a23` (v0.3.0), 扩展版本 WLLM_SCRIPT_VERSION = 2.13.0。

---

## 1. manifest: 版本 / 权限 / host_permissions / content_scripts 匹配

`extension/manifest.json` (MV3):

```json
{
  "manifest_version": 3,
  "name": "Web LLM → Local MCP (ctmc)",
  "version": "0.4.1",
  "permissions": ["storage", "scripting"],
  "host_permissions": [
    "http://127.0.0.1:*/*",
    "http://localhost:*/*",
    "<all_urls>"
  ],
  "background": { "service_worker": "background.js" },
  "action": {},
  "options_page": "options.html",
  "content_scripts": [
    {
      "matches": ["https://chat.z.ai/*"],
      "js": ["content/adapters/base.js", "content/adapters/zai.js", "content/core.js"],
      "run_at": "document_idle"
    },
    {
      "matches": ["https://chat.deepseek.com/*"],
      "js": ["content/adapters/base.js", "content/adapters/deepseek.js", "content/core.js"],
      "run_at": "document_idle"
    },
    {
      "matches": ["https://www.perplexity.ai/*"],
      "js": ["content/adapters/base.js", "content/adapters/perplexity.js", "content/core.js"],
      "run_at": "document_idle"
    }
  ]
}
```

要点:
- **permissions 只有 `storage` + `scripting`**(没有 tabs/activeTab)。`scripting` 用于后台在页面 **MAIN world** 执行文本插入(`chrome.scripting.executeScript` world:"MAIN"),这是 content script 隔离世界做不到的关键动作。
- `host_permissions` 含 `<all_urls>`(给 content_scripts 匹配 + 后台 fetch 本地 MCP),另加 `http://127.0.0.1:*/*` / `localhost` 是历史遗留。
- 无 `web_accessible_resources`(不需要)。
- 无 popup;点图标 `chrome.action.onClicked` → 直接打开 options 页。
- 内容脚本是**按站点加载适配器文件**(base.js + 站点 adapter + core.js),而不是通用脚本运行时探测——匹配规则即站点名单。
- 站点名单: z.ai (`https://chat.z.ai/*`)、deepseek (`https://chat.deepseek.com/*`)、perplexity (`https://www.perplexity.ai/*`)。**没有 claude.ai / chatgpt.com**(ctmc 的 ChatGPT 适配器在 v0.2.0 被移除,见 §7)。

---

## 2. 站点适配器: 选择器原文 + 写入/发送/完成判定

架构: `BaseAdapter` (base.js) 提供公共实现,子类只声明差异。**核心 4 方法**: `getInputEl` / `send` / `getLatestReply` / `isReplyDone`,外加折叠用的 `getUserMsgEls` / `getAssistantMsgEls` / `classifyMsg` / `findMsgRoot`,以及 `getSendButton` / `getFileInput` / `getDropTarget`。

### 2.1 base.js 公共实现(绝大多数站无需覆盖)

```js
// 输入元素: 默认 textarea (z.ai / DeepSeek 均为 textarea)
getInputEl() {
  return document.querySelector("textarea");
}

// 填入文本并触发输入事件 (React 受控组件需用原生 setter + 事件)
fillInput(text) {
  const el = this.getInputEl();
  if (!el) return false;
  if (el.tagName === "TEXTAREA" || el.tagName === "INPUT") {
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
    setter.call(el, text);
  } else {
    el.textContent = text;
  }
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  return true;
}

// 发送: 聚焦 + 派发 Enter 键 (实测 z.ai / DeepSeek / qwen 均有效)
// 注意: React 受控组件异步提交 value, fillInput 后立即 Enter 可能发空值
// (实测 qwen 的 sendToolResult 会卡在输入框), 延迟 400ms 等状态提交后再发
send() {
  const ta = this.getInputEl();
  if (!ta) return false;
  ta.focus();
  setTimeout(() => {
    ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
    ta.dispatchEvent(new KeyboardEvent("keypress", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
  }, 400);
  return true;
}

// 取最新一条助手回复 (用 textContent 而非 innerText: innerText 对 display:none 返回 "")
getLatestReply() {
  const blocks = document.querySelectorAll(this.replySelector);
  if (!blocks.length) return "";
  return (blocks[blocks.length - 1].textContent || "").trim();
}
```

**React 受控组件写入套路(必须照抄)**:
1. `Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(el, text)` — 原生 setter 直写,绕过 React 的 value tracker;
2. 随后派发 `input` + `change` 事件(bubbles)让 React 状态同步;
3. **发送前延迟 400ms**(React 异步提交 value,立即 Enter 会发空值)。

### 2.2 z.ai (`content/adapters/zai.js`) — 几乎全走 base

```js
// z.ai 回复区: markdown 渲染块 / 助手气泡
get replySelector() {
  return "[class*=markdown], [class*=answer], [class*=message-content], [class*=prose]";
}

// 流式是否完成: 检查最后一条助手消息区域内的进行中标记 (不查全局 body, 避免静态文案误判)
isReplyDone() {
  const blocks = document.querySelectorAll(this.replySelector);
  if (!blocks.length) return false;
  const last = blocks[blocks.length - 1];
  const t = (last.innerText || "").toLowerCase();
  return !/thinking\.\.\.|generating|loading\.\.\.|思考中|生成中|加载中/.test(t);
}
```

输入框 = base 默认 `textarea`;发送 = base Enter 派发;附件 = `input[type=file]` DataTransfer 注入。

### 2.3 DeepSeek (`content/adapters/deepseek.js`)

```js
// DeepSeek 输入框是 textarea[name=search] (实测 2026-08-03)
getInputEl() {
  return document.querySelector("textarea[name=search]") || document.querySelector("textarea");
}

// 助手主内容区 (排除 .ds-think-content 思维链, 否则长思考污染 JSON 提取)
get replySelector() {
  return ".ds-assistant-message-main-content";
}

// 完成标志: 最后一条助手消息思考标题变 "Thought for N seconds"
// (生成中标题为 "Thinking...", 或消息仍在流式输出)
isReplyDone() {
  const blocks = document.querySelectorAll(this.replySelector);
  if (!blocks.length) return false;
  const msg = blocks[blocks.length - 1].closest(".ds-message");
  if (msg && /Thought for \d+/.test(msg.innerText)) return true;
  // Instant 模式 (无思考标题) 兜底: 检查最后一条消息是否有生成中光标/动画
  const lastMsg = blocks[blocks.length - 1].closest(".ds-message");
  if (lastMsg) {
    const hasCursor = !!lastMsg.querySelector("[class*=cursor], [class*=blink], [class*=loading]");
    if (hasCursor) return false;
  }
  return true;
}
```

- 发送: base 的 Enter 派发(无稳定发送按钮选择器,`getSendButton()` 返回 null)。
- 折叠定位: `.ds-message` 是用户/助手共用根;含 `.ds-assistant-message-main-content` = 助手,否则用户。

### 2.4 完成判定 & 流式等待的通用逻辑 (core.js `waitForReply`)

- 轮询 `getLatestReply()`,内容连续稳定 4×500ms (2s) = 完成;总超时 90s (MAX_WAIT=180 tick)。
- 判定变化 = 文本变 **或** 助手消息数增加。
- SPA 跳转(url 变)会重置等待状态;检测到 reCAPTCHA 提前返回。
- 超时输出完整诊断(回复区是否出现过 / 空 tick / url 变化等)。

### 2.5 Perplexity 是 contenteditable(Lexical)— 纯监控模式

```js
// 输入框: contenteditable div (Lexical), 内含 ctmc connector chip (装饰节点)
// 不能 textContent 覆盖 (会冲掉 chip), 必须用 execCommand 插入
getInputEl() {
  return document.querySelector("#ask-input");
}
send() {
  const btn = document.querySelector('button[aria-label="提交"]');
  if (!btn) return false;
  btn.click();
  return true;
}
```

**关键坑 (core.js watchSendText + background page_insert)**: content script 隔离世界的 `execCommand` 只改 DOM、不会提交进 Lexical 模型(点提交发出的是空文本)。必须经 background 用 `chrome.scripting.executeScript` 在 **MAIN world** 执行同样的插入,并验证文本已以 `<span data-lexical-text>` 进模型才点提交。

---

## 3. JSON tool call 解析约定 (base.js `extractToolCalls`)

- **无 fenced code block 约定** —— 不是 ```json 围栏解析,而是**在全文里找 `{"tool"` 起点**,然后用**平衡括号扫描**提取完整 JSON 对象(正则 `[^{}]*` 不支持 apply_patch/exec_command 里的 `{}`)。
- 栈式扫描: `depth`/`inStr`/`escape` 三状态机,遇到 `{` depth++、`}` depth--,depth 归 0 即一个完整对象边界。
- 容错: 单条 `JSON.parse` 失败跳过;未闭合(流式半截)直接停止;支持**一条回复多个 tool call**(并行执行,MAX_PARALLEL=8);从匹配末尾继续找下一个,避免死循环。
- 折叠用的识别正则: `ASST_TOOL_RE = /\{\s*"tool"\s*:\s*"[^"]+"\s*,\s*"args"\s*:\s*\{/`。
- DeepSeek 陷阱提示 (promptHint): KaTeX 会把成对 `$...$` 当数学公式吃掉,提示模型把 `$` 写成 `\u0024`,`JSON.parse` 还原。

---

## 4. 插件 ↔ 本地 MCP 传输层

**不是 WebSocket,是 Streamable HTTP**(MCP 2025-11-25 协议):

- 地址: `http://127.0.0.1:8770/mcp`(每项目一个 port,由 ctmc router 分配;router 本身在 `http://127.0.0.1:8769/projects` 列出 `[{name, url, port, token}]`)。
- **鉴权**: `Authorization: Bearer <token>`(每个项目独立 token)。
- **握手** (background.js `mcpInit`):
  1. POST `/mcp` body `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{protocolVersion:"2025-11-25",...}}`,header `Accept: application/json, text/event-stream`;
  2. 从响应头 `mcp-session-id` 取 sessionId,按 `url#token指纹(SHA-256 前8字节)` 缓存;
  3. 再发 `notifications/initialized`(带 `Mcp-Session-Id` + `MCP-Protocol-Version` header),不等待 body。
- **调用**: POST `tools/call`,`Mcp-Session-Id` header 带上;解析响应: 取 `data:` 行里最后一行 `JSON.parse`;404 或 `error.code === -32001` 表示 session 失效 → 删缓存重试一次。
- **超时**: fetch AbortController 25s deadline (MCP_DEADLINE_MS);content script 侧另包 60s Promise.race。
- **CORS 绕法**: content script 直接 fetch 本地 127.0.0.1 会被站点 CSP/origin 白名单挡,所以**所有 MCP 调用走 `chrome.runtime.sendMessage` → background fetch**。
- 请求去重: `requestId` + url#token 指纹作 Map key,并发同 key 复用同一 Promise。
- 调用日志: 按天存 `chrome.storage.local` (`wllm_calls_<date>`),只保留当天。

---

## 5. "绑定会话"怎么做 (ctmc 已移除的 ChatGPT 绑定, v0.2.0 之前)

绑定逻辑在 core.js(bindChatGPTTask),虽然后来移除,但持久化模式值得抄:

- **conversationKey 派生自 URL**: `chatgpt:${pathname.match(/^\/c\/([^/?#]+)/)[1]}`。
- **绑定记录存 chrome.storage.local**, key = `ctmc_binding_${conversationKey}`:

```js
{
  taskId, projectRoot,
  confirmed: true,
  confirmedAt,                       // 时间戳
  expiresAt: confirmedAt + 86400000, // 24h 过期
  revision: `sha256:${digest}`,      // 内容寻址: sha256(conversationKey\0taskId\0projectRoot\0confirmedAt)
}
```

- **恢复**: 页面加载时按当前 conversationKey 查 storage,`binding?.confirmed && binding.taskId && binding.expiresAt > Date.now()` 则复用,否则视为未绑定。刷新/重载浏览器都天然恢复(chrome.storage.local 持久化)。
- **tabId 本身不持久化**(tabId 重启即失效);持久化的是**会话身份**(URL 派生 key),tabId 只用于消息路由。
- 防并发: `epoch` 计数器 + 每次 await 后重查 `getConversationKey()`,对话切换即取消绑定流程。
- 绑定前置: 读 router 8769 项目列表,选项目 → 选任务 → 确认写 storage → 恢复监控。
- 服务端侧还有 task_checkpoint / permits 等事务层(v0.2.0 移除),本项目不需要那么重。

---

## 6. 版本同步机制 (核心坑之一)

- content script 不会因扩展重载而自动重注入;**改 content 代码必须 bump 版本号**并让 background 强制刷新已打开的目标站标签页:
  - core.js 顶部 `WLLM_VERSION = "2.13.0"`(与 background 的 `WLLM_SCRIPT_VERSION` 一致,必须同步 bump);
  - 内容脚本启动 `sendMessage({type:"wllm_hello", version})` → background `tabVersions.set(tabId, version)`;
  - background 只在自身版本号变化时(`wllmBgVersion` storage 标记)等 6s 扫描目标站标签页,版本不匹配 → `chrome.tabs.reload`(同一生命周期 `reloadedTabs` Set 防循环)。
- 运行时失效守卫 (core.js): `chrome.runtime?.id` 检测到扩展重载 → 停全部定时器 + `sessionStorage` 计数(≤2 次/标签页)自动 `location.reload()` 加载新脚本。

---

## 7. git log 里暴露的坑 (避免重踩)

| commit | 坑 |
|---|---|
| `e9b794c` | ChatGPT watcher 的"向用户提问"检测必须看**请求形态**(问号收尾/祈使句/索取凭据句式),不能看正文提及的名词,否则误杀率高 |
| `e9b7920` | bind 失败要把错误码显示在 badge 上,别静默 |
| `13cff3a` | 恢复监控时**不要重新绑定**;binding key 迁移要留路径 |
| `8992817` | 对话切换时按**当前绑定**刷新状态 |
| `b557123` | `sendMessage` 调用本身要包 try/catch(根治 "Extension context invalidated" throw) |
| `74e2dab` | 附件方案不可行时回退贴全文(50K 截断)——上传/注入有站点耦合,兜底要简单 |
| `856ef04` | TOOL_RESULT 截断 3000→50K(大文件读不完根因) |
| `2e90cfe` | **v0.2.0 移除了浏览器自动续跑**(ChatGPT 绑定 + permit + task_checkpoint 全删),恢复手动。原因: 安全边界太复杂,自动续跑不可靠。**本项目只做"唤醒"(写入+提交),不做自动续跑/自动应答,正好避开这条红线** |
| core.js 注释 | 折叠用 textContent 不用 innerText(display:none 元素 innerText 返回 "") |
| core.js 注释 | DeepSeek KaTeX `$` 陷阱、qwen 输入框 React 异步提交需 400ms 延迟 |
| background.js 注释 | 旧脚本标签页自动刷新机制(版本不匹配强制 reload,防"改代码不生效") |
| manifest | 无 tabs 权限;MAIN world 执行靠 `chrome.scripting` + `<all_urls>` |

---

## 8. 对本项目的直接启示 (herdr → 网页)

1. **输入框写入**: z.ai / deepseek 是 `textarea`(原生 setter + input/change 事件 + 400ms 延迟 Enter);claude.ai / chatgpt.com 是 contenteditable(大概率需要 MAIN world execCommand 插入 + 提交按钮 click)。ctmc 的 `page_insert` 机制(background `chrome.scripting.executeScript` world:"MAIN")可直接复用。
2. **发送**: z.ai/deepseek 用 Enter 派发即可;claude/chatgpt 找发送按钮 click(chatgpt: `button[data-testid="send-button"]`,ctmc 移除前就是这么写的)。
3. **JSON 解析**: SpeaksJSON 层直接移植 base.js 的 `extractToolCalls`(平衡括号扫描)。
4. **绑定**: 复用 `chrome.storage.local` + URL 派生 key + expiresAt/revision 模式;不要持久化 tabId 本身。
5. **传输**: 插件经 background 访问本地 HTTP;本项目的推送通道在 herdr-mcp 侧(SSE),插件 content/background 消费。
6. **避坑**: 不做自动应答;版本号同步机制照抄;sendMessage 包 try/catch。
