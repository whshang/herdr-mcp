// wake.js — 唤醒核心 (content script)
// 方向: herdr → 网页。收到 background 的 h2w_wake 时,向本页输入框写入消息并提交。
// - textarea 站点 (z.ai/deepseek): fillInput (React 原生 setter) + Enter
// - contenteditable 站点 (claude/chatgpt): 经 background `chrome.scripting` **MAIN world** `execCommand("insertText")`
//   (隔离世界不提交编辑模型, ctmc 实测教训), 再点发送按钮
// - 有 SpeaksJSON 的站点: 提交后等回复区出现 (与提交前快照对比), 回报投递确认
// - 权限弹窗: 页面内「允许/拒绝」卡片自动点允许 (保守 fail-closed)。
//   ChatGPT Connector 每次 tools/call 都会弹卡 — chatgpt 站点常驻观察;
//   其它站点仍在唤醒窗口期内观察 (站点常在唤醒后弹权限)。
// 状态反馈: 页内不再画点 (用户反馈困惑), 改用工具栏图标徽章 (background 驱动)。
// 版本: 与 background.js 的 H2W_SCRIPT_VERSION 同步 bump (改 content 代码必须)。
const H2W_CONTENT_VERSION = "0.1.7";
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
  const normText = (s) => String(s || "").replace(/\s+/g, " ").trim();

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
    // ProseMirror 会把 \n 拆成段落, innerText 空白与模板不完全一致 → 归一化再比
    return normText(el.innerText || el.textContent).includes(normText(text));
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

  function sendButtonReady(btn) {
    if (!btn || !btn.offsetParent) return false;
    if (btn.disabled) return false;
    if (btn.getAttribute("aria-disabled") === "true") return false;
    if (btn.getAttribute("data-disabled") === "true") return false;
    return true;
  }

  // ---- 发送 ----
  // contenteditable 站点: 等发送按钮可点再 click; 仅键盘事件常被 ProseMirror 吞掉。
  // 成功判定: 输入框被清空 (真正发出去了)。假阳性 return true 会导致「框里堆着字却以为发了」。
  async function submit() {
    if (ADAPTER.needsMainWorldInsert) {
      await wait(350); // 等编辑模型吃进 insertText, 按钮才从 disabled 变可点
      for (let i = 0; i < 20; i++) {
        const btn = ADAPTER.getSendButton();
        if (sendButtonReady(btn)) {
          btn.click();
          for (let j = 0; j < 15; j++) {
            await wait(200);
            if (!ADAPTER.inputHasContent()) return true;
          }
          // 点了但框还在 → 可能没发出, 继续重试 / 换策略
          console.warn("[h2w] 点了发送但输入框仍有内容, 重试");
        }
        await wait(150);
      }
      const el = ADAPTER.getInputEl();
      if (!el) return false;
      el.focus();
      el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true, cancelable: true }));
      el.dispatchEvent(new KeyboardEvent("keypress", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true, cancelable: true }));
      for (let j = 0; j < 10; j++) {
        await wait(200);
        if (!ADAPTER.inputHasContent()) return true;
      }
      return false;
    }
    return ADAPTER.send();
  }

  // ---- 权限弹窗自动允许 (页面内 DOM 弹窗/工具权限卡片; 浏览器原生权限条无法自动点) ----
  // 复用 base.js 的 __H2W_PERMISSION__ 纯逻辑 (fail-closed): 只点可见/可用/明确
  // 文本的"允许"按钮, 且按钮所在最小卡片须含权限类标题说明 + 明确拒绝按钮。
  // 重复 mutation 不重复点击 (WeakSet 去重, 见 base.js 的 createPermissionClicker)。
  const PERM = window.__H2W_PERMISSION__;
  let permClicker = null;
  let permObs = null;
  let permDeadline = 0;
  function permissionTryClick() {
    if (!runtimeAlive() || Date.now() > permDeadline) { permissionStop(); return; }
    const r = permClicker.tryClick(document);
    if (r.handled) {
      console.log(`[h2w] 权限卡片自动点「${(r.button.innerText || r.button.textContent || "?").trim()}」`);
    }
  }
  function permissionStop() {
    if (permObs) { try { permObs.disconnect(); } catch (e) {} permObs = null; }
  }
  function startPermissionWatch(durationMs = 90000) {
    const persistent = !Number.isFinite(durationMs);
    // 已有常驻观察时, 有限窗口的二次启动不必打断; 常驻可覆盖有限窗口。
    if (permObs && (persistent || permDeadline === Number.POSITIVE_INFINITY)) {
      if (persistent) permDeadline = Number.POSITIVE_INFINITY;
      permissionTryClick();
      return;
    }
    if (permObs) permissionStop();
    permDeadline = persistent ? Number.POSITIVE_INFINITY : (Date.now() + durationMs);
    // 卡片先出现/按钮后挂载: 只在 findAllowAction 实际找到并点击后才由 clicker 标记,
    // 不会因提前标记而漏掉后挂载的按钮。
    permClicker = PERM.createPermissionClicker();
    permissionTryClick();
    permObs = new MutationObserver(() => permissionTryClick());
    // childList 覆盖按钮后挂载; attributes 让初始 disabled/hidden 的按钮后来变可用
    try {
      permObs.observe(document.body, {
        childList: true, subtree: true,
        attributes: true, attributeFilter: ["disabled", "hidden", "aria-disabled", "aria-hidden", "style"],
      });
    } catch (e) {}
    if (!persistent) setTimeout(permissionStop, durationMs + 5000);
  }

  // ---- 执行一次唤醒 ----
  let wakeInFlight = false;
  let lastWakeNorm = "";
  let lastWakeAt = 0;
  async function performWake(data) {
    if (!runtimeAlive()) return { ok: false, error: "context-invalidated" };
    if (wakeInFlight) return { ok: false, blocked: "wake-in-flight" };
    const text = (data.template || "").trim();
    if (!text) return { ok: false, error: "empty-template" };
    const n = normText(text);
    // 短窗口去重: 重试/双扩展/双 timer 叠同一条时不反复往框里灌
    if (n && n === lastWakeNorm && Date.now() - lastWakeAt < 8000) {
      return { ok: false, blocked: "dedupe" };
    }
    if (ADAPTER.inputHasContent()) {
      // 框里已是同文案 → 只补发送, 不再插入
      if (mainWorldCommitted(text) || normText((ADAPTER.getInputEl()?.innerText || ADAPTER.getInputEl()?.textContent || "")).includes(n.slice(0, 80))) {
        wakeInFlight = true;
        try {
          if (data.autoAllow !== false) startPermissionWatch();
          const sent = await submit();
          if (sent) { lastWakeNorm = n; lastWakeAt = Date.now(); }
          return { ok: sent, committed: true, resumed: true, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
        } finally { wakeInFlight = false; }
      }
      return { ok: false, blocked: "user-typing" };
    }
    wakeInFlight = true;
    try {
      if (data.autoAllow !== false) startPermissionWatch();

      let committedOk = false;
      if (ADAPTER.needsMainWorldInsert) {
        committedOk = await ensureCommitted(text);
        if (!committedOk) {
          try {
            const el = ADAPTER.getInputEl();
            const strip = (node) => { for (const c of [...node.childNodes]) { if (c.nodeType === 3 && c.data.includes(text)) c.remove(); else strip(c); } };
            if (el) strip(el);
          } catch (e) {}
          return { ok: false, error: "insert-failed" };
        }
        const sent = await submit();
        if (sent) { lastWakeNorm = n; lastWakeAt = Date.now(); }
        return { ok: sent, committed: true, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
      }

      const el = ADAPTER.getInputEl();
      if (!el) return { ok: false, error: "no-input" };
      const oldOpacity = el.style.opacity;
      el.style.opacity = "0";
      ADAPTER.fillInput(text);
      await wait(420); // React 受控组件异步提交 value (ctmc 教训)
      const sent = await submit();
      setTimeout(() => { if (el) el.style.opacity = oldOpacity ?? ""; }, 600);
      if (sent) { lastWakeNorm = n; lastWakeAt = Date.now(); }
      return { ok: sent, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
    } finally {
      wakeInFlight = false;
    }
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
    // ChatGPT Connector 工具权限卡与「唤醒」无关 — 页面加载后即常驻自动允许。
    if (ADAPTER.name === "chatgpt" && PERM) startPermissionWatch(Number.POSITIVE_INFINITY);
  })();
})();
