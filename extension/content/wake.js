// wake.js — 唤醒核心 (content script)
// 方向: herdr → 网页。收到 background 的 h2w_wake 时,向本页输入框写入消息并提交。
// - textarea 站点 (z.ai/deepseek): fillInput (React 原生 setter) + Enter
// - contenteditable 站点 (claude/chatgpt): 经 background `chrome.scripting` **MAIN world** `execCommand("insertText")`
//   (隔离世界不提交编辑模型, ctmc 实测教训), 再点发送按钮
// - 有 SpeaksJSON 的站点: 提交后等回复区出现 (与提交前快照对比), 回报投递确认
// - 权限弹窗: 唤醒期间检测页面内权限对话框, 自动点"允许" (保守 fail-closed)
// 状态反馈: 页内不再画点 (用户反馈困惑), 改用工具栏图标徽章 (background 驱动)。
// 版本: 与 background.js 的 H2W_SCRIPT_VERSION 同步 bump (改 content 代码必须)。
const H2W_CONTENT_VERSION = "0.1.1";
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { console.warn("[h2w] 无适配器, 跳过"); return; }
  const SPEAKS = window.__H2W_SPEAKS_JSON__ || null;

  function runtimeAlive() {
    try { return !!chrome.runtime?.id; } catch { return false; }
  }
  function sendBg(msg) {
    return new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(msg, (resp) => {
          if (chrome.runtime.lastError || resp === undefined) resolve(null);
          else resolve(resp);
        });
      } catch (e) { resolve(null); }
    });
  }
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  // ---- MAIN world 插入 (contenteditable 站点) ----
  function insertMainWorld(text, selector) {
    return new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage({ type: "h2w_insert_main", text, selector }, (resp) => {
          if (chrome.runtime.lastError || !resp) resolve({ ok: false, error: "no-response" });
          else resolve(resp);
        });
      } catch (e) { resolve({ ok: false, error: String(e) }); }
    });
  }
  function mainWorldCommitted(text) {
    const el = ADAPTER.getInputEl();
    if (!el) return false;
    return (el.innerText || el.textContent || "").includes(text);
  }
  async function ensureCommitted(text, maxAttempts = 3) {
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      if (mainWorldCommitted(text)) return true;
      if (attempt > 0) console.warn(`[h2w] 第 ${attempt + 1} 次尝试插入「${text.slice(0, 30)}…」`);
      const selector = ADAPTER.getWatchMainWorldSelector();
      if (!selector) return false;
      const r = await insertMainWorld(text, selector);
      if (!r.ok) return false;
      for (let i = 0; i < 10; i++) {
        await wait(200);
        if (mainWorldCommitted(text)) return true;
      }
    }
    return mainWorldCommitted(text);
  }

  // ---- 发送 ----
  async function submit() {
    if (ADAPTER.needsMainWorldInsert) {
      const btn = ADAPTER.getSendButton();
      if (btn && !btn.disabled) { btn.click(); return true; }
      const el = ADAPTER.getInputEl();
      if (!el) return false;
      el.focus();
      el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
      el.dispatchEvent(new KeyboardEvent("keypress", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
      return true;
    }
    return ADAPTER.send();
  }

  // ---- 权限弹窗自动允许 (页面内 DOM 弹窗; 浏览器原生权限条无法自动点) ----
  // 仅在有权限类字样的对话框/弹层里点明确肯定按钮, 拒绝/取消类绝不点。
  const PERM_DLG_SELECTORS = '[role="dialog"], [role="alertdialog"], .modal, [class*="modal"], [class*="dialog"], [class*="Dialog"], [class*="popup"]';
  let permObs = null;
  let permDeadline = 0;
  let permHandled = new WeakSet();
  function findPermissionDialog() {
    const els = [...document.querySelectorAll(PERM_DLG_SELECTORS)];
    return els.find((el) => isPermissionDialogText(el.innerText || ""));
  }
  function findAllowButtonIn(dlg) {
    const btns = [...dlg.querySelectorAll("button, [role=button], [class*=btn]")];
    for (const b of btns) {
      const label = (b.innerText || b.textContent || b.getAttribute("aria-label") || "").trim();
      if (isAllowButtonText(label)) return b;
    }
    return null;
  }
  function permissionTryClick() {
    if (!runtimeAlive() || Date.now() > permDeadline) { permissionStop(); return; }
    const dlg = findPermissionDialog();
    if (dlg && !permHandled.has(dlg)) {
      permHandled.add(dlg);
      const btn = findAllowButtonIn(dlg);
      if (btn) {
        console.log(`[h2w] 权限弹窗自动点「${(btn.innerText || btn.textContent || "?").trim()}」`);
        btn.click();
      }
    }
  }
  function permissionStop() {
    if (permObs) { try { permObs.disconnect(); } catch (e) {} permObs = null; }
  }
  function startPermissionWatch(durationMs = 90000) {
    if (permObs) return; // 已在观察
    permDeadline = Date.now() + durationMs;
    permissionTryClick();
    permObs = new MutationObserver(() => permissionTryClick());
    try { permObs.observe(document.body, { childList: true, subtree: true }); } catch (e) {}
    setTimeout(permissionStop, durationMs + 5000);
  }

  // ---- 执行一次唤醒 ----
  async function performWake(data) {
    if (!runtimeAlive()) return { ok: false, error: "context-invalidated" };
    if (ADAPTER.inputHasContent()) {
      return { ok: false, blocked: "user-typing" };
    }
    const text = (data.template || "").trim();
    if (!text) return { ok: false, error: "empty-template" };
    // 权限弹窗观察: 提交前开启, 覆盖提交后的窗口期 (站点常在唤醒后弹权限)
    if (data.autoAllow !== false) startPermissionWatch();

    let committedOk = false;
    if (ADAPTER.needsMainWorldInsert) {
      committedOk = await ensureCommitted(text);
      if (!committedOk) {
        try {
          const el = ADAPTER.getInputEl();
          const strip = (n) => { for (const c of [...n.childNodes]) { if (c.nodeType === 3 && c.data.includes(text)) c.remove(); else strip(c); } };
          if (el) strip(el);
        } catch (e) {}
        return { ok: false, error: "insert-failed" };
      }
      const sent = await submit();
      return { ok: sent, committed: true, site: ADAPTER.name };
    }

    const el = ADAPTER.getInputEl();
    if (!el) return { ok: false, error: "no-input" };
    const oldOpacity = el.style.opacity;
    el.style.opacity = "0";
    ADAPTER.fillInput(text);
    await wait(420); // React 受控组件异步提交 value (ctmc 教训)
    const sent = await submit();
    setTimeout(() => { if (el) el.style.opacity = oldOpacity ?? ""; }, 600);
    return { ok: sent, site: ADAPTER.name };
  }

  // ---- 投递确认 (SpeaksJSON 站点): 提交后等回复区出现/内容变化 ----
  async function confirmReplyStarted(timeoutMs = 30000) {
    if (!SPEAKS || !SPEAKS.enabled) return { monitored: false };
    const beforeText = SPEAKS.getLatestReply();
    const beforeCount = SPEAKS.getReplyBlockCount();
    const started = Date.now();
    while (Date.now() - started < timeoutMs) {
      if (!runtimeAlive()) return { monitored: true, replyStarted: false, error: "context-invalidated" };
      const cur = SPEAKS.getLatestReply();
      const count = SPEAKS.getReplyBlockCount();
      if (cur && (cur !== beforeText || count > beforeCount)) return { monitored: true, replyStarted: true };
      await wait(1000);
    }
    return { monitored: true, replyStarted: false };
  }

  // ---- 消息监听 ----
  try {
    chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") {
        sendResponse({ convKey: ADAPTER.getConversationKey(), url: location.href, site: ADAPTER.name });
        return;
      }
      if (msg?.type === "h2w_bound" || msg?.type === "h2w_unbound") {
        console.log(`[h2w] ${msg.type === "h2w_bound" ? "已绑定 " + msg.pane : "已解绑"} (状态见工具栏图标)`);
        return;
      }
      if (msg?.type === "h2w_wake") {
        (async () => {
          const result = await performWake(msg.data || {});
          const confirm = result.ok ? await confirmReplyStarted() : { monitored: false };
          sendBg({ type: "h2w_wake_ack", convKey: ADAPTER.getConversationKey(), result, confirm });
        })();
        return;
      }
      sendResponse({});
    });
  } catch (e) { console.warn("[h2w] onMessage 注册失败:", e.message); }

  // ---- 注册: 上报版本 (旧脚本标签页自动刷新机制) + 会话身份 (绑定路由/恢复) ----
  (async () => {
    if (!runtimeAlive()) return;
    try { chrome.runtime.sendMessage({ type: "h2w_hello", version: H2W_CONTENT_VERSION }); } catch (e) {}
    await sendBg({ type: "h2w_register", convKey: ADAPTER.getConversationKey(), url: location.href, site: ADAPTER.name });
  })();
})();
