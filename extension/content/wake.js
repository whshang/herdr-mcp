// wake.js — core wake-up content script
// Direction: herdr → web. On h2w_wake, fill and submit the current page's composer.
// - textarea sites (z.ai/deepseek): native React setter plus Enter
// - contenteditable sites (claude/chatgpt): MAIN-world execCommand("insertText"),
//   followed by a send-button click because isolated-world insertion does not commit the editor model
// - SpeaksJSON sites: wait for a changed reply area after submission and report delivery confirmation
// - permission dialogs: conservatively auto-click Allow on in-page permission cards
//   ChatGPT Connector cards are watched continuously; other sites are watched during wake-up.
// Status feedback uses the toolbar badge rather than an ambiguous in-page dot.
// Keep this version aligned with H2W_SCRIPT_VERSION in background.js.
const H2W_CONTENT_VERSION = "0.1.62";
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { console.warn("[h2w] no adapter; skipping"); return; }
  const SPEAKS = window.__H2W_SPEAKS_JSON__ || null;
  const CONTEXT_PRESSURE = globalThis.H2W_CONTEXT_PRESSURE || null;
  const CONVERSATION_HEALTH = globalThis.H2W_CONVERSATION_HEALTH || null;
  const RECOVERY_CONTROLLER = globalThis.H2W_RECOVERY_CONTROLLER || null;
  const BROWSER_PERFORMANCE = globalThis.H2W_BROWSER_PERFORMANCE || null;
  // Bounded, informational UI-pressure meter over the round watchers. Keeps
  // only per-window counters and never influences correctness decisions.
  let uiPressure = BROWSER_PERFORMANCE?.createUiPressureMeter
    ? BROWSER_PERFORMANCE.createUiPressureMeter()
    : null;
  const UI_TICK_DRIFT_FLOOR_MS = 150;
  let conversationHealth = null;
  let contextPressureRecord = null;
  let lastFreshnessProbeAt = 0;
  // Fail closed until background confirms the master automation state. The
  // read-only HUD/workspace observers do not depend on these flags.
  let automationEnabled = false;
  let automationAutoAllow = false;
  let hudLabels = {};
  const HEALTH_STORAGE_KEY = "h2wConversationHealthByConv";
  const CONTEXT_PRESSURE_STORAGE_KEY = "h2wContextPressureByConv";

  function runtimeAlive() {
    try { return !!chrome.runtime?.id; } catch { return false; }
  }
  function usesOperationalHud() {
    return ["chatgpt", "z.ai", "deepseek"].includes(ADAPTER.name);
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
  async function refreshAutomationState() {
    const state = await sendBg({ type: "h2w_automation_state", convKey: ADAPTER.getConversationKey() });
    if (!state?.ok) {
      automationEnabled = false;
      automationAutoAllow = false;
      return false;
    }
    automationEnabled = state.enabled === true;
    automationAutoAllow = state.autoAllow !== false;
    hudLabels = state.labels || hudLabels;
    return automationEnabled;
  }
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const normText = (s) => String(s || "").replace(/\s+/g, " ").trim();

  function markConversationState(record) {
    conversationHealth = record;
    if (record?.convKey) void persistConversationHealth(record);
    return record;
  }

  function markAssistantProgressIfActive(at = Date.now()) {
    if (!conversationHealth || !CONVERSATION_HEALTH) return;
    const currentTurnOpen = Number(conversationHealth.last_user_submit_at || 0) > 0
      && Number(conversationHealth.last_turn_end_at || 0) < Number(conversationHealth.last_user_submit_at || 0);
    if (currentTurnOpen || [
      CONVERSATION_HEALTH.CONVERSATION_STATES.REPLY_WAITING,
      CONVERSATION_HEALTH.CONVERSATION_STATES.REPLY_SUSPECT,
      CONVERSATION_HEALTH.CONVERSATION_STATES.RECOVERY_MESSAGE_SENT,
    ].includes(conversationHealth.state)) {
      markConversationState(CONVERSATION_HEALTH.markAssistantProgress(conversationHealth, at));
    }
  }

  function markObservedTurnEnded(at = Date.now()) {
    if (!conversationHealth || !CONVERSATION_HEALTH) return;
    markConversationState(CONVERSATION_HEALTH.markTurnEnded(conversationHealth, at));
  }

  async function loadConversationHealth(convKey) {
    if (!CONVERSATION_HEALTH || !convKey) return null;
    try {
      const stored = await chrome.storage.local.get([HEALTH_STORAGE_KEY]);
      const map = stored?.[HEALTH_STORAGE_KEY] || {};
      const existing = map?.[convKey];
      if (existing && existing.convKey === convKey) return existing;
    } catch (_) { /* fresh record below */ }
    return CONVERSATION_HEALTH.createConversationHealth(convKey);
  }

  async function persistConversationHealth(record) {
    if (!record?.convKey || !runtimeAlive()) return;
    try {
      const stored = await chrome.storage.local.get([HEALTH_STORAGE_KEY]);
      const map = { ...(stored?.[HEALTH_STORAGE_KEY] || {}), [record.convKey]: record };
      const entries = Object.entries(map);
      if (entries.length > 20) {
        entries.sort((a, b) => {
          const at = a[1]?.last_turn_end_at || a[1]?.last_user_submit_at || a[1]?.last_reload_at || 0;
          const bt = b[1]?.last_turn_end_at || b[1]?.last_user_submit_at || b[1]?.last_reload_at || 0;
          return bt - at;
        });
        for (const [key] of entries.slice(20)) delete map[key];
      }
      await chrome.storage.local.set({ [HEALTH_STORAGE_KEY]: map });
    } catch (_) { /* recovery state is best-effort persistence */ }
  }

  async function ensureConversationHealth(convKey = ADAPTER.getConversationKey()) {
    if (!CONVERSATION_HEALTH || !convKey) return null;
    if (conversationHealth?.convKey === convKey) return conversationHealth;
    return markConversationState(await loadConversationHealth(convKey));
  }

  async function loadContextPressure(convKey) {
    if (!CONTEXT_PRESSURE || !convKey) return null;
    try {
      const stored = await chrome.storage.local.get([CONTEXT_PRESSURE_STORAGE_KEY]);
      const existing = stored?.[CONTEXT_PRESSURE_STORAGE_KEY]?.[convKey];
      if (existing?.convKey === convKey) return existing;
    } catch (_) {}
    return CONTEXT_PRESSURE.emptyContextRecord(convKey);
  }

  async function persistContextPressure(record) {
    if (!record?.convKey || !runtimeAlive() || !CONTEXT_PRESSURE) return;
    try {
      const stored = await chrome.storage.local.get([CONTEXT_PRESSURE_STORAGE_KEY]);
      const map = { ...(stored?.[CONTEXT_PRESSURE_STORAGE_KEY] || {}), [record.convKey]: record };
      const entries = Object.entries(map);
      if (entries.length > 20) {
        entries.sort((a, b) => Number(b[1]?.updated_at || 0) - Number(a[1]?.updated_at || 0));
        for (const [key] of entries.slice(20)) delete map[key];
      }
      await chrome.storage.local.set({ [CONTEXT_PRESSURE_STORAGE_KEY]: map });
    } catch (_) {}
  }

  function observedConversationTurns() {
    if (!CONTEXT_PRESSURE) return [];
    const observations = [];
    const nodes = [...document.querySelectorAll('[data-message-author-role="user"], [data-message-author-role="assistant"]')];
    for (const el of nodes) {
      const role = el.getAttribute("data-message-author-role");
      const text = String(el.innerText || el.textContent || "").trim();
      if (!text || (role !== "user" && role !== "assistant")) continue;
      const turn = el.closest("[data-turn-id]");
      const message = el.closest("[data-message-id]");
      const stableId = turn?.getAttribute("data-turn-id")
        || el.getAttribute("data-turn-id")
        || message?.getAttribute("data-message-id")
        || el.getAttribute("data-message-id")
        || `observed:${CONTEXT_PRESSURE.textFingerprint(text)}`;
      observations.push({ id: `${stableId}:${role}`, role, text });
    }
    return observations;
  }

  function observedConversationMessageFloor() {
    if (ADAPTER.name !== "chatgpt") return 0;
    let floor = 0;
    for (const el of document.querySelectorAll('[data-testid^="conversation-turn-"]')) {
      const value = String(el.getAttribute("data-testid") || "");
      const match = /^conversation-turn-(\d+)$/.exec(value);
      if (!match) continue;
      floor = Math.max(floor, Number(match[1]) + 1);
    }
    return floor;
  }

  // ---- Settled-turn helpers: reuse the just-settled user/assistant turn for
  // context pressure instead of rescanning the full conversation DOM. ----
  function settledTurnIdForElement(el) {
    if (!el) return null;
    try {
      const turn = el.closest("[data-turn-id]");
      const message = el.closest("[data-message-id]");
      const virtualTurn = el.closest('[data-testid^="conversation-turn-"]');
      return turn?.getAttribute("data-turn-id")
        || el.getAttribute("data-turn-id")
        || message?.getAttribute("data-message-id")
        || el.getAttribute("data-message-id")
        || virtualTurn?.getAttribute("data-testid")
        || null;
    } catch (_) { return null; }
  }
  function settledTurnObservation(role, text, el) {
    const normalized = String(text || "").trim();
    if (role !== "user" && role !== "assistant") return null;
    if (!normalized) return null;
    const stableId = settledTurnIdForElement(el)
      || `settled:${CONTEXT_PRESSURE.textFingerprint(normalized)}`;
    return { id: `${stableId}:${role}`, role, text: normalized };
  }
  function settledTurnVirtualFloor(el) {
    if (!el || ADAPTER.name !== "chatgpt") return 0;
    try {
      const container = el.closest('[data-testid^="conversation-turn-"]') || el;
      const match = /^conversation-turn-(\d+)$/.exec(String(container.getAttribute("data-testid") || ""));
      return match ? Number(match[1]) + 1 : 0;
    } catch (_) { return 0; }
  }
  async function updateContextPressureFromSettledTurns(userText, assistantText, { userEl, assistantEl } = {}) {
    if (!CONTEXT_PRESSURE) return null;
    const convKey = ADAPTER.getConversationKey();
    if (!convKey) return null;
    if (contextPressureRecord?.convKey !== convKey) {
      contextPressureRecord = await loadContextPressure(convKey);
    }
    const observations = [
      settledTurnObservation("user", userText, userEl),
      settledTurnObservation("assistant", assistantText, assistantEl),
    ].filter(Boolean);
    const floor = settledTurnVirtualFloor(assistantEl) || settledTurnVirtualFloor(userEl);
    if (observations.length || floor > 0) {
      contextPressureRecord = CONTEXT_PRESSURE.mergeSettledTurns(
        contextPressureRecord,
        observations,
        floor,
      );
    }
    await persistContextPressure(contextPressureRecord);
    return CONTEXT_PRESSURE.summarizeContextRecord(contextPressureRecord);
  }

  async function updateContextPressure() {
    if (!CONTEXT_PRESSURE) return null;
    const convKey = ADAPTER.getConversationKey();
    if (!convKey) return null;
    if (contextPressureRecord?.convKey !== convKey) {
      contextPressureRecord = await loadContextPressure(convKey);
    }
    contextPressureRecord = CONTEXT_PRESSURE.mergeObservedTurns(contextPressureRecord, observedConversationTurns());
    const messageFloor = observedConversationMessageFloor();
    if (messageFloor > 0) {
      contextPressureRecord = CONTEXT_PRESSURE.mergeMessageCountFloor(
        contextPressureRecord,
        messageFloor,
        "chatgpt_virtual_turn_index",
      );
    }
    await persistContextPressure(contextPressureRecord);
    return CONTEXT_PRESSURE.summarizeContextRecord(contextPressureRecord);
  }

  // ---- MAIN-world insertion for contenteditable sites ----
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
    // ProseMirror splits newlines into paragraphs, so compare normalized text.
    return normText(el.innerText || el.textContent).includes(normText(text));
  }
  async function ensureCommitted(text, maxAttempts = 3) {
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      if (mainWorldCommitted(text)) return true;
      if (attempt > 0) console.warn(`[h2w] insertion attempt ${attempt + 1}: "${text.slice(0, 30)}..."`);
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

  function isHerdrWakeComposerText(text) {
    const t = normText(text);
    return t.length > 0 && /^herdr workspace\b/i.test(t);
  }
  function composerTextRaw() {
    const el = ADAPTER.getInputEl();
    if (!el) return "";
    if (el.value != null && el.tagName !== "DIV") return String(el.value);
    return String(el.innerText || el.textContent || "");
  }
  function composerNorm() { return normText(composerTextRaw()); }
  function composerHasSameWake(text) {
    const n = normText(text);
    const cur = composerNorm();
    if (!cur || !n) return false;
    if (ADAPTER.needsMainWorldInsert && mainWorldCommitted(text)) return true;
    return cur.includes(n.slice(0, 80)) || n.includes(cur.slice(0, 80));
  }
  function isExtensionStaleComposer(curNorm) {
    if (!curNorm) return false;
    if (isHerdrWakeComposerText(curNorm)) return true;
    if (lastFailedWakeNorm) {
      const fail = lastFailedWakeNorm.slice(0, 80);
      const cur = curNorm.slice(0, 80);
      if (cur.includes(fail) || fail.includes(cur)) return true;
    }
    return false;
  }
  async function clearComposer() {
    if (ADAPTER.needsMainWorldInsert) {
      const selector = ADAPTER.getWatchMainWorldSelector();
      if (selector) await insertMainWorld("", selector);
      return;
    }
    const el = ADAPTER.getInputEl();
    if (!el) return;
    if (el.tagName === "TEXTAREA" || el.tagName === "INPUT") {
      const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
      setter.call(el, "");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }

  function elementVisible(el) {
    if (typeof ADAPTER.elementVisible === "function") return ADAPTER.elementVisible(el);
    return !!(el && el.offsetParent);
  }

  function sendButtonReady(btn) {
    if (!btn || !elementVisible(btn)) return false;
    if (btn.disabled) return false;
    if (btn.getAttribute("aria-disabled") === "true") return false;
    if (btn.getAttribute("data-disabled") === "true") return false;
    return true;
  }

  function isSendButton(btn) {
    if (!sendButtonReady(btn)) return false;
    const blob = [
      btn.getAttribute("data-testid") || "",
      btn.getAttribute("aria-label") || "",
      btn.innerText || "",
    ].filter(Boolean).join(" ");
    if (/stop|停止|generating|生成中|streaming|cancel/i.test(blob)) return false;
    return /send|发送|submit|prompt/i.test(blob) || /send-button|composer-send/i.test(blob);
  }

  function findSendButton() {
    const list = typeof ADAPTER.getSendButtonCandidates === "function"
      ? ADAPTER.getSendButtonCandidates()
      : [];
    if (!list.length && typeof ADAPTER.getSendButton === "function") {
      const one = ADAPTER.getSendButton();
      if (one) list.push(one);
    }
    for (const btn of list) {
      if (isSendButton(btn)) return btn;
    }
    return null;
  }

  function dispatchEnterSubmit(el) {
    if (!el) return;
    el.focus();
    for (const type of ["keydown", "keypress", "keyup"]) {
      el.dispatchEvent(new KeyboardEvent(type, {
        key: "Enter", code: "Enter", keyCode: 13, which: 13, bubbles: true, cancelable: true,
      }));
    }
  }

  async function waitForComposerIdle(maxMs = 12000) {
    const t0 = Date.now();
    while (Date.now() - t0 < maxMs) {
      if (!isComposerGenerating()) return true;
      await wait(300);
    }
    return !isComposerGenerating();
  }

  // ---- Submission ----
  // For contenteditable sites, wait for an enabled send button because ProseMirror
  // often consumes synthetic keyboard events. Success requires the composer to clear.
  async function submitAfterPermissionClick() {
    if (!lastPermClickAt || Date.now() - lastPermClickAt > 6000) return false;
    await wait(1200);
    await waitForComposerIdle(4000);
    const btn = findSendButton();
    if (isSendButton(btn)) {
      btn.click();
      for (let j = 0; j < 20; j++) {
        await wait(200);
        if (!ADAPTER.inputHasContent()) return true;
      }
    }
    dispatchEnterSubmit(ADAPTER.getInputEl());
    for (let j = 0; j < 15; j++) {
      await wait(200);
      if (!ADAPTER.inputHasContent()) return true;
    }
    return false;
  }

  async function submitTextarea() {
    await waitForComposerIdle();
    await wait(420);
    for (let attempt = 0; attempt < 3; attempt++) {
      const btn = findSendButton();
      if (isSendButton(btn)) {
        btn.click();
        for (let j = 0; j < 20; j++) {
          await wait(200);
          if (!ADAPTER.inputHasContent()) return true;
        }
      }
      dispatchEnterSubmit(ADAPTER.getInputEl());
      for (let j = 0; j < 15; j++) {
        await wait(200);
        if (!ADAPTER.inputHasContent()) return true;
      }
      if (attempt < 2) {
        console.warn(`[h2w] textarea submit attempt ${attempt + 1} failed; retrying`);
        await wait(800);
        await waitForComposerIdle(4000);
      }
    }
    return submitAfterPermissionClick();
  }

  async function submit() {
    if (ADAPTER.needsMainWorldInsert) {
      await waitForComposerIdle();
      const postInsertMs = Math.min(1200, 350 + Math.floor((ADAPTER.getInputEl()?.innerText?.length || 0) / 40) * 50);
      await wait(postInsertMs);
      for (let attempt = 0; attempt < 3; attempt++) {
        for (let i = 0; i < 40; i++) {
          const btn = findSendButton();
          if (isSendButton(btn)) {
            btn.click();
            for (let j = 0; j < 20; j++) {
              await wait(200);
              if (!ADAPTER.inputHasContent()) return true;
            }
            console.warn("[h2w] composer still has content after Send click; retrying");
            break;
          }
          await wait(150);
        }
        const el = ADAPTER.getInputEl();
        dispatchEnterSubmit(el);
        for (let j = 0; j < 15; j++) {
          await wait(200);
          if (!ADAPTER.inputHasContent()) return true;
        }
        if (attempt < 2) {
          console.warn(`[h2w] submit attempt ${attempt + 1} failed; retrying`);
          await wait(800);
          await waitForComposerIdle(4000);
        }
      }
      const afterPerm = await submitAfterPermissionClick();
      if (afterPerm) return true;
      return false;
    }
    return submitTextarea();
  }

  // ---- Auto-allow in-page permission dialogs and tool cards ----
  // Reuse fail-closed logic from base.js: click only a visible, enabled, explicit
  // Allow action whose smallest card has permission text and an explicit deny action.
  // A WeakSet prevents duplicate clicks across repeated mutations.
  const PERM = window.__H2W_PERMISSION__;
  let permClicker = null;
  let permObs = null;
  let permScheduler = null;
  let permDeadline = 0;
  let lastPermClickAt = 0;
  function permissionTryClick() {
    if (!runtimeAlive() || Date.now() > permDeadline) { permissionStop(); return; }
    if (!automationEnabled || !automationAutoAllow) return;
    const r = permClicker.tryClick(document);
    if (r.handled) {
      lastPermClickAt = Date.now();
      console.log(`[h2w] auto-clicked permission action "${(r.button.innerText || r.button.textContent || "?").trim()}"`);
    }
  }
  function permissionStop() {
    if (permObs) { try { permObs.disconnect(); } catch (e) {} permObs = null; }
    if (permScheduler) { try { permScheduler.cancel(); } catch (_) {} permScheduler = null; }
  }
  function startPermissionWatch(durationMs = 90000) {
    const persistent = !Number.isFinite(durationMs);
    // A persistent observer already covers later finite watch requests.
    if (permObs && (persistent || permDeadline === Number.POSITIVE_INFINITY)) {
      if (persistent) permDeadline = Number.POSITIVE_INFINITY;
      permissionTryClick();
      return;
    }
    if (permObs) permissionStop();
    permDeadline = persistent ? Number.POSITIVE_INFINITY : (Date.now() + durationMs);
    // Mark only after a button is found and clicked so late-mounted buttons are not missed.
    permClicker = PERM.createPermissionClicker();
    permissionTryClick();
    permScheduler = BROWSER_PERFORMANCE?.createCoalescedScheduler
      ? BROWSER_PERFORMANCE.createCoalescedScheduler(permissionTryClick, {
          minIntervalMs: BROWSER_PERFORMANCE.DEFAULT_PERMISSION_COALESCE_MS,
          isSuspended: () => document.hidden,
        })
      : null;
    permObs = new MutationObserver(() => {
      if (document.hidden) return;
      uiPressure?.recordMutation();
      if (permScheduler) permScheduler.schedule();
      else permissionTryClick();
    });
    // childList catches late mounts; attributes catches buttons that later become enabled.
    try {
      permObs.observe(document.body, {
        childList: true, subtree: true,
        attributes: true, attributeFilter: ["disabled", "hidden", "aria-disabled", "aria-hidden"],
      });
    } catch (e) {}
    if (!persistent) setTimeout(permissionStop, durationMs + 5000);
  }

  function syncAutomationPermissionWatch() {
    if (ADAPTER.name !== "chatgpt" || !PERM) return;
    if (automationEnabled && automationAutoAllow) startPermissionWatch(Number.POSITIVE_INFINITY);
    else permissionStop();
  }

  // ---- Perform one wake-up ----
  let wakeInFlight = false;
  let lastWakeNorm = "";
  let lastWakeAt = 0;
  let lastFailedWakeNorm = "";
  function noteWakeResult(textNorm, sent) {
    if (sent) {
      lastWakeNorm = textNorm;
      lastWakeAt = Date.now();
      lastFailedWakeNorm = "";
    } else if (textNorm) {
      lastFailedWakeNorm = textNorm;
    }
  }
  async function performWake(data) {
    if (!runtimeAlive()) return { ok: false, error: "context-invalidated" };
    if (wakeInFlight) return { ok: false, blocked: "wake-in-flight" };
    const text = (data.template || "").trim();
    if (!text) return { ok: false, error: "empty-template" };
    const n = normText(text);
    // Short-window deduplication prevents repeated insertion from retries or duplicate timers.
    if (n && n === lastWakeNorm && Date.now() - lastWakeAt < 8000) {
      return { ok: false, blocked: "dedupe" };
    }
    let resumeOnly = false;
    let clearBeforeInsert = false;
    if (ADAPTER.inputHasContent() && !data.llmNudge) {
      if (composerHasSameWake(text)) {
        resumeOnly = true;
      } else if (isExtensionStaleComposer(composerNorm())) {
        clearBeforeInsert = true;
      } else {
        return { ok: false, blocked: "user-typing" };
      }
    }
    wakeInFlight = true;
    try {
      if (data.autoAllow !== false) startPermissionWatch();

      // z.ai/DeepSeek JSON bridge intentionally intercepts normal user submits
      // so it can add the Herdr tool protocol. Handoff prompts and seeds are
      // continuity-control messages, not agent tasks; send them through the
      // bridge's raw channel so they are not rewritten into a coding prompt.
      const jsonBridge = globalThis.__H2W_JSON_BRIDGE__ || null;
      if (data.handoff === true && typeof jsonBridge?.sendRaw === "function") {
        const sent = await jsonBridge.sendRaw(text);
        noteWakeResult(n, sent);
        return { ok: sent, committed: sent, site: ADAPTER.name, raw: true, error: sent ? undefined : "submit-failed" };
      }

      if (resumeOnly) {
        const sent = await submit();
        noteWakeResult(n, sent);
        return { ok: sent, committed: true, resumed: true, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
      }

      if (clearBeforeInsert) await clearComposer();

      if (ADAPTER.needsMainWorldInsert) {
        const idle = await waitForComposerIdle(15000);
        if (!idle) {
          return { ok: false, error: "composer-busy", blocked: "generating" };
        }
      }

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
        noteWakeResult(n, sent);
        return { ok: sent, committed: true, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
      }

      const el = ADAPTER.getInputEl();
      if (!el) return { ok: false, error: "no-input" };
      const oldOpacity = el.style.opacity;
      el.style.opacity = "0";
      ADAPTER.fillInput(text);
      await wait(420); // React-controlled inputs commit value asynchronously.
      const sent = await submit();
      setTimeout(() => { if (el) el.style.opacity = oldOpacity ?? ""; }, 600);
      noteWakeResult(n, sent);
      return { ok: sent, committed: true, site: ADAPTER.name, error: sent ? undefined : "submit-failed" };
    } finally {
      wakeInFlight = false;
    }
  }

  // ---- Delivery confirmation on SpeaksJSON sites ----
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

  // ---- Idle nudge helpers (shared with snapshot + turn watch) ----
  // ---- Latest-turn cache (ChatGPT turn watcher) ----
  // Structural mutations only mark the cache dirty; the next read performs
  // exactly one rediscovery for both roles. Non-structural sampling (streaming
  // text) reads the cached element's live innerText without rescanning the
  // full conversation history on every 800ms tick.
  const latestTurns = { user: null, assistant: null, userCount: 0, assistantCount: 0 };
  let latestTurnsDirty = true;
  let latestTurnCacheActive = false;
  function markLatestTurnsDirty() { latestTurnsDirty = true; }
  function rediscoverLatestTurns() {
    const nodes = document.querySelectorAll(
      '[data-message-author-role="user"], [data-message-author-role="assistant"]',
    );
    let user = null;
    let assistant = null;
    let userCount = 0;
    let assistantCount = 0;
    for (const node of nodes) {
      const role = node.getAttribute("data-message-author-role");
      if (role === "user") { user = node; userCount += 1; }
      else if (role === "assistant") { assistant = node; assistantCount += 1; }
    }
    latestTurns.user = user;
    latestTurns.assistant = assistant;
    latestTurns.userCount = userCount;
    latestTurns.assistantCount = assistantCount;
    latestTurnsDirty = false;
    return latestTurns;
  }
  function latestTurnForRole(role) {
    if (!latestTurnCacheActive) {
      const nodes = document.querySelectorAll(`[data-message-author-role="${role}"]`);
      return nodes[nodes.length - 1] || null;
    }
    if (latestTurnsDirty) rediscoverLatestTurns();
    return role === "user" ? latestTurns.user : latestTurns.assistant;
  }

  function lastMessageByRole(role) {
    try {
      if (typeof ADAPTER.getLastMessageText === "function") {
        const text = String(ADAPTER.getLastMessageText(role) || "").trim();
        if (text) return text;
      }
    } catch (_) {}
    const el = latestTurnForRole(role);
    return el ? String(el.innerText || "").trim() : "";
  }

  async function waitForStableAssistantReply(beforeText = "", beforeCount = 0, timeoutMs = 120000) {
    if (!SPEAKS?.enabled) return { ok: false, error: "reply-monitor-unavailable" };
    const deadline = Date.now() + timeoutMs;
    let latest = "";
    let stableTicks = 0;
    while (Date.now() < deadline) {
      if (!runtimeAlive()) return { ok: false, error: "context-invalidated" };
      const text = String(SPEAKS.getLatestReply() || "").trim();
      const count = Number(SPEAKS.getReplyBlockCount() || 0);
      const changed = Boolean(text) && (text !== beforeText || count > beforeCount);
      if (changed) {
        if (text !== latest) {
          latest = text;
          stableTicks = 0;
        } else {
          stableTicks += 1;
        }
        if (stableTicks >= 3 && SPEAKS.isReplyDone()) {
          return { ok: true, assistantText: text };
        }
      }
      await wait(500);
    }
    const text = String(SPEAKS.getLatestReply() || "").trim();
    return text && text !== beforeText
      ? { ok: true, assistantText: text, timedOut: true }
      : { ok: false, error: "reply-timeout" };
  }

  function hasHandoffTransferMarker(text, transferId) {
    const id = String(transferId || "").trim();
    if (!id) return false;
    const body = String(text || "");
    if (body.includes(`[HERDR_CONTINUITY_TRANSFER id=${id}]`)) return true;
    return body.includes(`<<<HERDR_HANDOFF_V1 id=${id}>>>`)
      && body.includes("<<<END_HERDR_HANDOFF_V1>>>");
  }

  async function waitForHandoffTarget(transferId, timeoutMs = 25000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (!runtimeAlive()) return { ok: false, error: "context-invalidated" };
      const targetConvKey = ADAPTER.getConversationKey();
      const lastUser = lastMessageByRole("user");
      if (targetConvKey && hasHandoffTransferMarker(lastUser, transferId)) {
        return {
          ok: true,
          targetConvKey,
          targetUrl: location.href,
          seedConfirmed: true,
        };
      }
      await wait(250);
    }
    return {
      ok: true,
      targetConvKey: ADAPTER.getConversationKey(),
      targetUrl: location.href,
      seedConfirmed: hasHandoffTransferMarker(lastMessageByRole("user"), transferId),
    };
  }
  function stopButtons() {
    return [...document.querySelectorAll("button, [role=button]")].filter((b) => {
      if (!b.offsetParent) return false;
      const blob = [
        b.innerText, b.textContent, b.getAttribute("aria-label"),
        b.getAttribute("data-testid"), b.getAttribute("title"),
      ].filter(Boolean).join(" ");
      return /stop|停止|stop generating|停止生成|stop streaming|停止流式/i.test(blob);
    });
  }
  function assistantStreaming() {
    const nodes = [...document.querySelectorAll('[data-message-author-role="assistant"]')];
    const last = nodes[nodes.length - 1];
    if (!last) return false;
    if (last.getAttribute("data-is-streaming") === "true") return true;
    if (last.querySelector('[data-is-streaming="true"]')) return true;
    return false;
  }
  function isComposerGenerating() {
    if (stopButtons().length > 0) return true;
    if (assistantStreaming()) return true;
    const send = findSendButton();
    if (send) {
      const blob = [
        send.getAttribute("aria-label"), send.getAttribute("data-testid"), send.innerText,
      ].filter(Boolean).join(" ");
      if (/stop|停止|generating|生成中|streaming/i.test(blob)) return true;
    }
    return false;
  }

  /** Mid-turn: streaming, stop button, or visible tool/MCP invocation still running. */
  function assistantToolsInProgress() {
    const nodes = [...document.querySelectorAll('[data-message-author-role="assistant"]')];
    const last = nodes[nodes.length - 1];
    if (!last) return false;
    if (last.querySelector('[aria-busy="true"]')) return true;
    for (const el of last.querySelectorAll('[class*="animate-spin"], [class*="animate-pulse"], svg.animate-spin')) {
      if (el.offsetParent) return true;
    }
    for (const el of last.querySelectorAll("[data-testid], [aria-label]")) {
      if (!el.offsetParent) continue;
      const blob = [
        el.getAttribute("data-testid") || "",
        el.getAttribute("aria-label") || "",
        String(el.className || "").slice(0, 80),
        String(el.textContent || "").slice(0, 100),
      ].join(" ");
      if (/tool|mcp|connector|plugin|herdr/i.test(blob)
        && /running|loading|pending|in.?progress|executing|calling|searching|fetching|working/i.test(blob)) {
        return true;
      }
    }
    return false;
  }

  function isTurnInProgress() {
    return isComposerGenerating() || assistantToolsInProgress();
  }

  // Keep aligned with binding-core.js looksLikeSubstantiveReply
  function looksLikeSubstantiveReply(text) {
    const t = String(text || "").replace(/\s+/g, " ").trim();
    if (t.length < 60) return false;
    if (/^(?:calling|called|running|searching|fetching|executing|using|invoking|waiting)\b/i.test(t) && t.length < 160) {
      return false;
    }
    if (/^herdr_[a-z_]+\b/i.test(t) && t.length < 140) return false;
    const stripped = t.replace(/\{"tool"[^}]*\}/gi, "").trim();
    if (stripped.length < 50) return false;
    return true;
  }

  // ---- Message listener ----
  try {
    chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") {
        sendResponse({ convKey: ADAPTER.getConversationKey(), url: location.href, site: ADAPTER.name });
        return;
      }
      if (msg?.type === "h2w_snapshot_turn") {
        const assistantText = lastMessageByRole("assistant");
        sendResponse({
          convKey: ADAPTER.getConversationKey(),
          userText: lastMessageByRole("user"),
          assistantText,
          generating: isComposerGenerating(),
          turnInProgress: isTurnInProgress(),
          substantive: looksLikeSubstantiveReply(assistantText),
          endedAt: Date.now(),
        });
        return;
      }
      if (msg?.type === "h2w_handoff_prompt") {
        (async () => {
          const beforeText = SPEAKS?.enabled ? SPEAKS.getLatestReply() : "";
          const beforeCount = SPEAKS?.enabled ? SPEAKS.getReplyBlockCount() : 0;
          const result = await performWake({
            template: msg.template || "",
            autoAllow: false,
            handoff: true,
          });
          if (!result?.ok || ADAPTER.name === "chatgpt") {
            sendResponse(result);
            return;
          }
          const reply = await waitForStableAssistantReply(beforeText, beforeCount);
          sendResponse({ ...result, ...reply });
        })();
        return true;
      }
      if (msg?.type === "h2w_handoff_seed") {
        (async () => {
          const result = await performWake({
            template: msg.template || "",
            autoAllow: false,
            handoff: true,
          });
          if (!result?.ok) { sendResponse(result); return; }
          const confirmed = await waitForHandoffTarget(msg.transferId);
          sendResponse({ ...result, ...confirmed });
        })();
        return true;
      }
      if (msg?.type === "h2w_handoff_probe") {
        sendResponse({
          ok: true,
          targetConvKey: ADAPTER.getConversationKey(),
          targetUrl: location.href,
          seedConfirmed: hasHandoffTransferMarker(lastMessageByRole("user"), msg.transferId),
        });
        return;
      }
      if (msg?.type === "h2w_handoff_committed" || msg?.type === "h2w_handoff_moved") {
        if (usesOperationalHud()) void refreshPageHud();
        sendResponse({ ok: true });
        return;
      }
      if (msg?.type === "h2w_bound" || msg?.type === "h2w_unbound") {
        console.log(`[h2w] ${msg.type === "h2w_bound" ? "bound " + msg.pane : "unbound"}`);
        if (usesOperationalHud()) void refreshPageHud();
        return;
      }
      if (msg?.type === "h2w_automation_changed") {
        void refreshAutomationState().then(() => {
          syncAutomationPermissionWatch();
          if (ADAPTER.name === "chatgpt") void refreshPageHud();
        });
        sendResponse({ ok: true });
        return;
      }
      if (msg?.type === "h2w_wake") {
        (async () => {
          if (!(await refreshAutomationState())) {
            sendResponse({ ok: false, blocked: "local-runtime-unavailable" });
            return;
          }
          await ensureConversationHealth();
          const result = await performWake(msg.data || {});
          if (result.ok && CONVERSATION_HEALTH) {
            markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth));
          }
          const confirm = result.ok ? await confirmReplyStarted() : { monitored: false };
          if (confirm?.replyStarted && CONVERSATION_HEALTH && conversationHealth) {
            markConversationState(CONVERSATION_HEALTH.markReplyStarted(conversationHealth));
          }
          sendBg({ type: "h2w_wake_ack", convKey: ADAPTER.getConversationKey(), result, confirm });
          sendResponse(result);
        })();
        return true;
      }
      sendResponse({});
    });
  } catch (e) { console.warn("[h2w] failed to register onMessage:", e.message); }

  // ---- Registration: report version and conversation identity ----
  // ChatGPT (and the other supported sites) use client-side navigation. Content
  // scripts survive those route changes, so startup registration alone can leave
  // background state attached to the previous conversation. Poll the canonical
  // conversation key instead of monkey-patching history.pushState: content scripts
  // run in an isolated world and cannot reliably intercept the page's History API.
  let registeredConvKey = null;

  async function registerCurrentConversation(reason = "startup") {
    if (!runtimeAlive()) return null;
    const convKey = ADAPTER.getConversationKey();
    if (!convKey) return null;
    const response = await sendBg({ type: "h2w_register", convKey, url: location.href, site: ADAPTER.name });
    if (response !== null) {
      const changed = registeredConvKey !== null && registeredConvKey !== convKey;
      registeredConvKey = convKey;
      const concreteChat = ADAPTER.name !== "chatgpt" || Boolean(chatGptConversationId());
      if (concreteChat) {
        await ensureConversationHealth(convKey);
        if (CONTEXT_PRESSURE) {
          contextPressureRecord = await loadContextPressure(convKey);
          void updateContextPressure().then((pressure) => {
            if (pressure && ADAPTER.name === "chatgpt") paintPageHud({ continuity: pressure });
          });
        }
      } else {
        conversationHealth = null;
        contextPressureRecord = null;
      }
      if (changed) {
        console.log(`[h2w] conversation route changed (${reason}): ${convKey}`);
        if (usesOperationalHud()) void refreshPageHud();
      }
    }
    return response;
  }

  function startConversationRouteWatch() {
    // One second is fast enough for UI binding while keeping route detection
    // negligible compared with the existing 5s HUD reconciliation interval.
    setInterval(() => {
      if (document.hidden) return;
      const convKey = ADAPTER.getConversationKey();
      if (convKey && convKey !== registeredConvKey) void registerCurrentConversation("poll");
    }, 1000);
    try {
      window.addEventListener("popstate", () => { void registerCurrentConversation("popstate"); });
      window.addEventListener("hashchange", () => { void registerCurrentConversation("hashchange"); });
      document.addEventListener("visibilitychange", () => {
        if (document.hidden) return;
        void registerCurrentConversation("visible");
        if (permObs) {
          if (permScheduler) permScheduler.flush();
          else permissionTryClick();
        }
      });
    } catch (_) { /* polling remains authoritative */ }
  }

  function assistantSignature(text) {
    const value = String(text || "");
    let hash = 2166136261;
    for (let i = 0; i < value.length; i += 1) {
      hash ^= value.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return `${value.length}:${hash >>> 0}`;
  }

  function chatGptConversationId() {
    const value = String(ADAPTER.getConversationKey() || location.href || "");
    const match = value.match(/\/c\/([^/?#]+)/);
    return match ? decodeURIComponent(match[1]) : null;
  }

  function chatGptCurrentConversationAnchor() {
    const conversationId = chatGptConversationId();
    if (!conversationId || ADAPTER.name !== "chatgpt") return null;
    const anchors = [...document.querySelectorAll('a[href*="/c/"]')];
    const matchesConversation = (anchor) => {
      try {
        const url = new URL(anchor.href, location.href);
        const match = url.pathname.match(/\/c\/([^/?#]+)/);
        return match && decodeURIComponent(match[1]) === conversationId;
      } catch (_) {
        return false;
      }
    };
    return anchors.find((anchor) => matchesConversation(anchor) && anchor.getAttribute("aria-label"))
      || anchors.find((anchor) => matchesConversation(anchor))
      || null;
  }

  function chatGptDomConversationTitle() {
    const anchor = chatGptCurrentConversationAnchor();
    if (!anchor) return "";
    const title = normText(anchor.innerText || anchor.textContent || "");
    if (!title || title.length > 160) return "";
    if (/^(跳至内容|skip to content|コンテンツへスキップ)$/i.test(title)) return "";
    return title;
  }

  function chatGptDomProjectTitle() {
    const anchor = chatGptCurrentConversationAnchor();
    const aria = normText(anchor?.getAttribute?.("aria-label") || "");
    const zh = aria.match(/[—–-]\s*项目\s+(.+?)\s+中的聊天\s*$/);
    if (zh?.[1]) return normText(zh[1]);
    return "";
  }

  function latestDomAssistantSnapshot() {
    const nodes = [...document.querySelectorAll('[data-message-author-role="assistant"]')];
    const el = nodes[nodes.length - 1] || null;
    if (!el) return { messageId: null, text: "", messageAt: null, changedAt: null };
    const container = el.closest("[data-message-id]") || el.closest("[data-turn-id]") || el;
    const timeNode = container?.querySelector?.("time[datetime]") || null;
    const parsedTime = timeNode?.getAttribute?.("datetime") ? Date.parse(timeNode.getAttribute("datetime")) : NaN;
    return {
      messageId: container?.getAttribute?.("data-message-id") || el.getAttribute("data-message-id") || null,
      text: String(el.innerText || el.textContent || "").replace(/\s+/g, " ").trim(),
      messageAt: Number.isFinite(parsedTime) ? parsedTime : null,
      changedAt: Number(conversationHealth?.last_assistant_progress_at || conversationHealth?.last_turn_end_at || 0) || null,
    };
  }

  function serverMessageText(message) {
    const parts = message?.content?.parts;
    if (!Array.isArray(parts)) return "";
    return parts.map((part) => typeof part === "string" ? part : (part?.text || "")).join("\n").replace(/\s+/g, " ").trim();
  }

  async function fetchChatGptConversationSnapshot() {
    const conversationId = chatGptConversationId();
    if (!conversationId || ADAPTER.name !== "chatgpt") return { ok: false, reason: "not-chatgpt-conversation" };
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5000);
    try {
      const response = await fetch(`/backend-api/conversation/${encodeURIComponent(conversationId)}`, {
        credentials: "include",
        cache: "no-store",
        headers: { accept: "application/json" },
        signal: controller.signal,
      });
      if (!response.ok) return { ok: false, reason: `http-${response.status}` };
      const body = await response.json();
      const mapping = body?.mapping || {};
      const currentNodeRole = String(mapping?.[body?.current_node]?.message?.author?.role || "");
      let nodeId = body?.current_node || null;
      let assistant = null;
      for (let i = 0; nodeId && i < 40; i += 1) {
        const node = mapping?.[nodeId];
        const message = node?.message;
        if (message?.author?.role === "assistant") { assistant = message; break; }
        nodeId = node?.parent || null;
      }
      if (!assistant?.id) return { ok: true, currentNodeRole, messageId: null, text: "", finished: null };
      const finishType = String(assistant?.metadata?.finish_details?.type || "");
      const status = String(assistant?.status || "");
      const completed = assistant?.end_turn === true
        || status === "finished_successfully"
        || ["stop", "max_tokens", "length"].includes(finishType);
      const explicitlyOpen = assistant?.end_turn === false || ["in_progress", "streaming"].includes(status);
      const finished = completed ? true : (explicitlyOpen ? false : null);
      const updateSeconds = Number(assistant?.update_time || assistant?.create_time || body?.update_time || 0);
      return {
        ok: true,
        currentNodeRole,
        messageId: String(assistant.id),
        text: serverMessageText(assistant),
        status,
        finished,
        createdAt: Number(assistant?.create_time || 0) > 0 ? Number(assistant.create_time) * 1000 : null,
        updatedAt: updateSeconds > 0 ? updateSeconds * 1000 : null,
      };
    } catch (error) {
      return { ok: false, reason: error?.name === "AbortError" ? "timeout" : "fetch-failed" };
    } finally {
      clearTimeout(timer);
    }
  }

  function explicitChatGptThreadError() {
    if (ADAPTER.name !== "chatgpt") return null;
    const retry = document.querySelector('button[data-testid="regenerate-thread-error-button"]');
    if (!retry) return null;
    const container = retry.closest('[class*="text-token-text-error"]') || retry.parentElement;
    return {
      retry,
      text: String(container?.innerText || container?.textContent || "").replace(/\s+/g, " ").trim(),
    };
  }

  function explicitChatGptDisconnectedReply() {
    if (ADAPTER.name !== "chatgpt") return null;
    const text = String(lastMessageByRole("assistant") || "").replace(/\s+/g, " ").trim();
    if (!text) return null;
    if (!/(?:连接已中断|正在等待完整回复|connection (?:was )?(?:interrupted|lost)|waiting for (?:the )?full response)/i.test(text)) {
      return null;
    }
    return { text };
  }

  async function maybeRecoverDisconnectedReply() {
    if (!automationEnabled || ADAPTER.name !== "chatgpt" || !conversationHealth || !RECOVERY_CONTROLLER || !CONVERSATION_HEALTH) return false;
    if (!explicitChatGptDisconnectedReply()) return false;
    const now = Date.now();
    const lastProgressAt = Number(
      conversationHealth.last_assistant_progress_at
      || conversationHealth.reply_started_at
      || conversationHealth.last_user_submit_at
      || 0,
    );
    if (!lastProgressAt || now - lastProgressAt < RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.assistantStallMs) {
      return true;
    }
    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.toolRunning || safety.permissionCardActive) return true;
    if (Number(conversationHealth.reload_attempt || 0) >= 1) return true;
    // ChatGPT already reports that this response stream is disconnected. One
    // reload reconciles the existing server-side turn; it never resubmits the
    // user's potentially side-effecting request, so the stale Stop button must
    // not block this bounded refresh.
    if (!RECOVERY_CONTROLLER.canReloadSafely({ ...safety, streaming: false })) return true;
    markConversationState(CONVERSATION_HEALTH.markReplySuspect(conversationHealth, "chatgpt_disconnected"));
    markConversationState(CONVERSATION_HEALTH.markReloadPending(conversationHealth));
    await wait(100);
    const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
    markConversationState({ ...reloading, reload_reason: "chatgpt_disconnected" });
    await wait(150);
    location.reload();
    return true;
  }

  async function maybeRecoverExplicitThreadError() {
    if (!automationEnabled || ADAPTER.name !== "chatgpt" || !conversationHealth || !RECOVERY_CONTROLLER || !CONVERSATION_HEALTH) return false;
    const threadError = explicitChatGptThreadError();
    if (!threadError) return false;
    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.streaming || safety.toolRunning || safety.permissionCardActive) return true;

    const now = Date.now();
    const retryAttempts = Number(conversationHealth.thread_error_retry_attempt || 0);
    const refreshAttempts = Number(conversationHealth.thread_error_reload_attempt || 0);
    const server = await fetchChatGptConversationSnapshot();
    const serverHasCurrentAssistant = Boolean(server?.ok && server?.currentNodeRole === "assistant");
    markConversationState({ ...conversationHealth, thread_error_last_seen_at: now });

    // If the server already has assistant work for this turn, retrying can
    // duplicate tool work. Reload the page to reconcile the local view.
    if (serverHasCurrentAssistant) {
      if (refreshAttempts >= 1) return true;
      if (!RECOVERY_CONTROLLER.canReloadSafely(safety)) return true;
      markConversationState(CONVERSATION_HEALTH.markReloadPending({
        ...conversationHealth,
        thread_error_reload_attempt: refreshAttempts + 1,
        reload_reason: "thread_error_server_ahead",
      }));
      await wait(100);
      const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
      markConversationState({
        ...reloading,
        thread_error_reload_attempt: refreshAttempts + 1,
        reload_reason: "thread_error_server_ahead",
      });
      await wait(150);
      location.reload();
      return true;
    }

    // A successful snapshot with no assistant newer than the user submit is
    // enough evidence to use ChatGPT's own one-shot Retry button.
    if (server?.ok && retryAttempts < 1 && !threadError.retry.disabled) {
      markConversationState({
        ...conversationHealth,
        thread_error_retry_attempt: retryAttempts + 1,
        thread_error_last_seen_at: now,
      });
      threadError.retry.click();
      const confirm = await confirmReplyStarted(RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.replyTimeoutMs);
      if (confirm?.replyStarted) {
        markConversationState(CONVERSATION_HEALTH.markReplyStarted(conversationHealth));
        return true;
      }
      const afterRetrySafety = recoverySafetySnapshot();
      if (refreshAttempts < 1 && RECOVERY_CONTROLLER.canReloadSafely(afterRetrySafety)) {
        markConversationState(CONVERSATION_HEALTH.markReloadPending({
          ...conversationHealth,
          thread_error_reload_attempt: refreshAttempts + 1,
          reload_reason: "thread_error_retry_timeout",
        }));
        await wait(100);
        const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
        markConversationState({
          ...reloading,
          thread_error_reload_attempt: refreshAttempts + 1,
          reload_reason: "thread_error_retry_timeout",
        });
        await wait(150);
        location.reload();
      }
      return true;
    }

    // If delivery cannot be verified, or Retry already failed once, a single
    // safety-gated reload is safer than blindly submitting a second user turn.
    if (refreshAttempts < 1 && RECOVERY_CONTROLLER.canReloadSafely(safety)) {
      const reloadReason = server?.ok ? "thread_error_retry_failed" : "thread_error_delivery_unknown";
      markConversationState(CONVERSATION_HEALTH.markReloadPending({
        ...conversationHealth,
        thread_error_reload_attempt: refreshAttempts + 1,
        reload_reason: reloadReason,
      }));
      await wait(100);
      const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
      markConversationState({
        ...reloading,
        thread_error_reload_attempt: refreshAttempts + 1,
        reload_reason: reloadReason,
      });
      await wait(150);
      location.reload();
      return true;
    }

    // Keep the explicit error authoritative. Do not fall through to a generic
    // recovery message that could create a second user turn.
    if (refreshAttempts >= 1
      && Date.now() - Number(conversationHealth.last_reload_at || 0) >= 10000) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({});
    }
    return true;
  }

  async function maybeRefreshStaleView() {
    if (!automationEnabled || ADAPTER.name !== "chatgpt" || !conversationHealth || !RECOVERY_CONTROLLER) return false;
    const now = Date.now();
    if (now - lastFreshnessProbeAt < RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.freshnessProbeIntervalMs) return false;
    const lastConversationActivity = Math.max(
      Number(conversationHealth.last_user_submit_at || 0),
      Number(conversationHealth.last_assistant_progress_at || 0),
      Number(conversationHealth.last_turn_end_at || 0),
    );
    if (!lastConversationActivity || now - lastConversationActivity < RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.assistantStallMs) return false;
    if (!conversationHealth.last_user_submit_at || now - Number(conversationHealth.last_user_submit_at) > 10 * 60 * 1000) return false;
    lastFreshnessProbeAt = now;

    const dom = latestDomAssistantSnapshot();
    const server = await fetchChatGptConversationSnapshot();
    const freshness = RECOVERY_CONTROLLER.classifyViewFreshness({ dom, server, now });
    if (CONVERSATION_HEALTH?.markFreshness) {
      markConversationState(CONVERSATION_HEALTH.markFreshness(conversationHealth, {
        ...freshness,
        serverMessageId: server?.messageId || null,
        pageMessageId: dom?.messageId || null,
      }, now));
    }
    if (!["server_ahead", "server_stalled"].includes(freshness.state)) return false;
    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.toolRunning || safety.permissionCardActive) return false;
    if (freshness.state === "server_stalled" && safety.streaming
      && now - lastConversationActivity < RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.serverStallMs + 30000) return false;
    if (Number(conversationHealth.stale_refresh_attempt || 0) >= 1) return false;
    if (!RECOVERY_CONTROLLER.canReloadSafely({ ...safety, streaming: false })) return false;

    const signature = assistantSignature(dom.text);
    const pending = CONVERSATION_HEALTH.markReloadPending({
      ...conversationHealth,
      stale_refresh_attempt: Number(conversationHealth.stale_refresh_attempt || 0) + 1,
      reload_reason: "stale_view",
      assistant_signature_before_reload: signature,
    });
    markConversationState(pending);
    const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
    markConversationState({ ...reloading, reload_reason: "stale_view", assistant_signature_before_reload: signature });
    await wait(150);
    location.reload();
    return true;
  }

  function recoverySafetySnapshot() {
    const composerText = composerNorm();
    let permissionCardActive = false;
    try { permissionCardActive = Boolean(PERM?.findAllowAction?.(document)); } catch (_) {}
    return {
      composerBusy: wakeInFlight || Boolean(composerText),
      composerHasHumanText: Boolean(composerText && !isExtensionStaleComposer(composerText)),
      streaming: isComposerGenerating(),
      toolRunning: assistantToolsInProgress(),
      permissionCardActive,
      deliveryUnknown: false,
      mutationDeliveryUncertain: false,
      reloadAttempts: Number(conversationHealth?.reload_attempt || 0),
      lastReloadAt: conversationHealth?.last_reload_at || null,
      now: Date.now(),
    };
  }

  async function maybeSendRecoveryProbe() {
    if (!conversationHealth || !RECOVERY_CONTROLLER) return false;
    if (!automationEnabled) return false;
    if (!RECOVERY_CONTROLLER.shouldSendRecovery(conversationHealth)) return false;
    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.streaming || safety.toolRunning || safety.permissionCardActive) return false;
    const result = await performWake({ template: hudLabels.recovery_probe_template, autoAllow: false, recovery: true });
    if (!result?.ok) return false;
    markConversationState(RECOVERY_CONTROLLER.markRecoverySent(conversationHealth));
    paintPageHud({});
    const confirm = await confirmReplyStarted(RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.replyTimeoutMs);
    if (confirm?.replyStarted && CONVERSATION_HEALTH) {
      markConversationState(CONVERSATION_HEALTH.markReplyStarted(conversationHealth));
      paintPageHud({});
    }
    return true;
  }

  async function maybeReloadForRecovery() {
    if (!conversationHealth || !RECOVERY_CONTROLLER || !CONVERSATION_HEALTH) return false;
    if (!automationEnabled) return false;
    if (conversationHealth.state !== CONVERSATION_HEALTH.CONVERSATION_STATES.REPLY_SUSPECT) return false;
    if ((conversationHealth.recovery_attempt || 0) < RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxRecoveryAttempts) return false;
    const safety = recoverySafetySnapshot();
    if (!RECOVERY_CONTROLLER.canReloadSafely(safety)) return false;
    const signature = assistantSignature(lastMessageByRole("assistant"));
    markConversationState(CONVERSATION_HEALTH.markReloadPending(conversationHealth));
    await wait(100);
    const reloading = RECOVERY_CONTROLLER.markReloaded(conversationHealth);
    markConversationState({ ...reloading, assistant_signature_before_reload: signature });
    await wait(150);
    location.reload();
    return true;
  }

  async function maybeEscalateContextRollover() {
    if (!CONTEXT_PRESSURE || !contextPressureRecord) return false;
    if (!automationEnabled) return false;
    const pressure = CONTEXT_PRESSURE.summarizeContextRecord(contextPressureRecord);
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    if (!hud?.ok) return false;
    const safety = recoverySafetySnapshot();
    const should = CONTEXT_PRESSURE.shouldAutoRollover({
      pressure,
      runtimeHealth: safety.streaming || safety.toolRunning ? "working" : "healthy",
      bound: hud.bound,
      canHandoff: hud.can_handoff,
      projectConversation: Boolean(hud.can_handoff),
      quiescent: !safety.composerBusy && !safety.streaming && !safety.toolRunning && !safety.permissionCardActive,
      deliveryUncertain: hud.handoff?.status === "seed_uncertain",
      mutationPending: hud.handoff?.status === "seed_submitting",
      handoffStatus: hud.handoff?.status,
      lastAutoAttemptAt: contextPressureRecord.last_auto_attempt_at,
    });
    if (!should) return false;
    contextPressureRecord = CONTEXT_PRESSURE.markAutoAttempt(contextPressureRecord, null, "context_pressure");
    await persistContextPressure(contextPressureRecord);
    const result = await sendBg({ type: "h2w_handoff_start", trigger: "context_pressure" });
    if (result?.ok) {
      contextPressureRecord = CONTEXT_PRESSURE.markRolloverCommitted(contextPressureRecord, result?.handoff?.id || null);
      await persistContextPressure(contextPressureRecord);
      paintPageHud({ hud, continuity: pressure });
      return true;
    }
    return false;
  }

  async function maybeEscalateRecoveryRollover() {
    if (!conversationHealth || !RECOVERY_CONTROLLER || !CONVERSATION_HEALTH) return false;
    if (!automationEnabled) return false;
    const exhaustedSuspect = conversationHealth.state === CONVERSATION_HEALTH.CONVERSATION_STATES.REPLY_SUSPECT
      && Number(conversationHealth.recovery_attempt || 0) >= RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxRecoveryAttempts
      && Number(conversationHealth.reload_attempt || 0) >= RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxReloadAttempts;
    if (conversationHealth.state !== CONVERSATION_HEALTH.CONVERSATION_STATES.RECOVERING && !exhaustedSuspect) return false;
    if (Date.now() - Number(conversationHealth.last_reload_at || 0) < 10000) return false;

    const assistantText = lastMessageByRole("assistant");
    const currentSignature = assistantSignature(assistantText);
    if (!isTurnInProgress() && assistantText && looksLikeSubstantiveReply(assistantText)
      && currentSignature !== conversationHealth.assistant_signature_before_reload) {
      markConversationState(CONVERSATION_HEALTH.markTurnEnded(conversationHealth));
      paintPageHud({});
      return true;
    }
    if (String(conversationHealth.reload_reason || "").startsWith("thread_error_")) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({});
      return true;
    }
    if (conversationHealth.reload_reason === "stale_view" && Number(conversationHealth.stale_activation_attempt || 0) < 1) {
      const safety = recoverySafetySnapshot();
      if (safety.composerBusy || safety.streaming || safety.toolRunning || safety.permissionCardActive) return false;
      const result = await performWake({ template: hudLabels.stale_view_activation_template, autoAllow: false, recovery: true });
      if (!result?.ok) return false;
      markConversationState(RECOVERY_CONTROLLER.markRecoverySent({
        ...conversationHealth,
        stale_activation_attempt: 1,
      }));
      paintPageHud({});
      const confirm = await confirmReplyStarted(RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.replyTimeoutMs);
      if (confirm?.replyStarted) {
        markConversationState(CONVERSATION_HEALTH.markReplyStarted(conversationHealth));
        paintPageHud({});
      }
      return true;
    }
    const safety = recoverySafetySnapshot();
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    const tabReachable = Array.isArray(hud?.bindings)
      ? hud.bindings.every((binding) => binding?.execution_state?.reachable !== false)
      : true;
    if (!RECOVERY_CONTROLLER.canRolloverSafely(conversationHealth, { ...safety, tabReachable })) return false;
    if (!hud?.ok || !hud?.can_handoff) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({ hud: hud?.ok ? hud : null });
      return false;
    }

    markConversationState(CONVERSATION_HEALTH.markRolloverRequired(conversationHealth));
    paintPageHud({ hud });
    const result = await sendBg({ type: "h2w_handoff_start", trigger: "recovery_exhausted" });
    if (!result?.ok) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({ hud });
      return false;
    }
    return true;
  }

  async function reconcileConversationHealthAfterLoad() {
    if (ADAPTER.name === "chatgpt" && !chatGptConversationId()) return;
    const record = await ensureConversationHealth();
    if (!record || !RECOVERY_CONTROLLER) return;
    const suspect = RECOVERY_CONTROLLER.classifyReplyTimeout(record);
    if (suspect !== record) markConversationState(suspect);
    await maybeEscalateRecoveryRollover();
  }

  function startConversationHealthWatch() {
    let healthCheckInFlight = false;
    setInterval(() => {
      if (document.hidden) return;
      if (ADAPTER.name === "chatgpt" && !chatGptConversationId()) return;
      if (healthCheckInFlight || !conversationHealth || !RECOVERY_CONTROLLER) return;
      healthCheckInFlight = true;
      void (async () => {
        // Refresh the effective automation state before every recovery cycle.
        // A saved Auto-on preference must fail closed as soon as the local
        // Herdr runtime becomes unreachable.
        await refreshAutomationState();
        if (await maybeRecoverExplicitThreadError()) return;
        if (await maybeRecoverDisconnectedReply()) return;
        const next = RECOVERY_CONTROLLER.classifyReplyTimeout(conversationHealth);
        if (next !== conversationHealth) markConversationState(next);
        if (await maybeRefreshStaleView()) return;
        if (await maybeSendRecoveryProbe()) return;
        if (await maybeReloadForRecovery()) return;
        if (await maybeEscalateContextRollover()) return;
        await maybeEscalateRecoveryRollover();
      })().finally(() => { healthCheckInFlight = false; });
    }, 5000);
  }

  (async () => {
    if (!runtimeAlive()) return;
    try { chrome.runtime.sendMessage({ type: "h2w_hello", version: H2W_CONTENT_VERSION }); } catch (e) {}
    await refreshAutomationState();
    await registerCurrentConversation("startup");
    await reconcileConversationHealthAfterLoad();
    startConversationRouteWatch();
    // ChatGPT Connector permission cards can appear outside wake-up, so watch continuously.
    syncAutomationPermissionWatch();
    // The operational HUD is shared by ChatGPT and the JSON-bridge sites.
    // ChatGPT-only idle/recovery watchers remain scoped to ChatGPT below.
    if (["chatgpt", "z.ai", "deepseek"].includes(ADAPTER.name)) {
      startPageHud();
    }
    // Talk-without-tools: watch turn boundaries and ask background to check MCP activity.
    if (ADAPTER.name === "chatgpt") {
      startIdleNudgeWatch();
      startConversationHealthWatch();
    }
  })();

  // ---- Idle nudge: zero openai-mcp tools/call on an action/claim turn ----
  function startIdleNudgeWatch() {
    let generating = false;
    let sawGrowth = false;
    let startedAt = 0;
    let userTextAtStart = "";
    const hydrationGraceUntil = Date.now() + 5000;
    // Module-level latest-turn cache is authoritative while this watcher runs.
    latestTurnCacheActive = true;
    const roleSnapshot = (role) => {
      const el = latestTurnForRole(role);
      return {
        count: role === "user" ? latestTurns.userCount : latestTurns.assistantCount,
        text: el ? String(el.innerText || el.textContent || "").trim() : "",
      };
    };
    const initialUser = roleSnapshot("user");
    const initialAssistant = roleSnapshot("assistant");
    let lastUserSignature = assistantSignature(initialUser.text);
    let lastUserCount = initialUser.count;
    let settleTimer = null;
    let lastReportedEnd = 0;
    let lastAsstLen = 0;
    let lastAsstSignature = assistantSignature(initialAssistant.text);
    let lastAsstCount = initialAssistant.count;
    let stableRounds = 0;

    const reportTurnEnded = (assistantText, endedAt) => {
      if (endedAt - lastReportedEnd < 3000) return;
      if (isTurnInProgress()) return;
      if (!looksLikeSubstantiveReply(assistantText)) return;
      if (!String(assistantText || "").trim()) return;
      lastReportedEnd = endedAt;
      markObservedTurnEnded(endedAt);
      const payload = {
        type: "h2w_turn_ended",
        convKey: ADAPTER.getConversationKey(),
        startedAt,
        endedAt,
        userText: userTextAtStart || lastMessageByRole("user"),
        assistantText,
      };
      console.log("[h2w] turn ended; updating continuity pressure and asking idle-nudge check");
      // Reuse the just-settled user/assistant turn instead of rescanning the
      // full conversation DOM; the full-history scan still runs on route change.
      void updateContextPressureFromSettledTurns(
        userTextAtStart || lastMessageByRole("user"),
        assistantText,
        { userEl: latestTurnForRole("user"), assistantEl: latestTurnForRole("assistant") },
      ).then((pressure) => {
        if (pressure) paintPageHud({ continuity: pressure });
      });
      paintPageHud({ pending: true });
      sendBg(payload).then((r) => {
        console.log("[h2w] idle-nudge result:", r);
        hudPending = false;
        void refreshPageHud();
      });
    };

    const onTick = () => {
      if (document.hidden) return;
      uiPressure?.recordTick();
      if (!chatGptConversationId()) {
        generating = false;
        sawGrowth = false;
        stableRounds = 0;
        // Rehydrate when an SPA route changes into a conversation.
        markLatestTurnsDirty();
        if (settleTimer) { clearInterval(settleTimer); settleTimer = null; }
        return;
      }
      const stopping = isComposerGenerating();
      const currentUser = roleSnapshot("user");
      const currentUserText = currentUser.text;
      const currentUserSignature = assistantSignature(currentUserText);
      const currentUserCount = currentUser.count;
      const userChanged = currentUserText && (currentUserSignature !== lastUserSignature || currentUserCount > lastUserCount);
      if (userChanged) {
        if (Date.now() >= hydrationGraceUntil && CONVERSATION_HEALTH && conversationHealth) {
          markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth));
        }
        lastUserSignature = currentUserSignature;
        lastUserCount = currentUserCount;
      }
      const currentAssistant = roleSnapshot("assistant");
      const assistantText = currentAssistant.text;
      const curLen = assistantText.length;
      const curSignature = assistantSignature(assistantText);
      const curCount = currentAssistant.count;
      const assistantChanged = curLen > 0 && (curSignature !== lastAsstSignature || curCount > lastAsstCount);

      if (stopping || assistantChanged || curLen > lastAsstLen) {
        if (curLen > 0 && (assistantChanged || curLen > lastAsstLen)) markAssistantProgressIfActive();
        if (!generating) {
          generating = true;
          sawGrowth = assistantChanged || curLen > lastAsstLen || stopping;
          startedAt = Date.now();
          userTextAtStart = currentUserText;
          if (settleTimer) { clearInterval(settleTimer); settleTimer = null; }
          stableRounds = 0;
          console.log("[h2w] turn start (streaming/stop/growth)");
        }
        if (curLen > lastAsstLen) sawGrowth = true;
        stableRounds = 0;
        lastAsstLen = curLen;
        lastAsstSignature = curSignature;
        lastAsstCount = curCount;
        return;
      }

      if (generating && sawGrowth && curLen > 0) {
        if (curLen === lastAsstLen) stableRounds += 1;
        else { stableRounds = 0; lastAsstLen = curLen; }
        if (stableRounds < 2) return; // ~1.6s stable after growth
        generating = false;
        sawGrowth = false;
        stableRounds = 0;
        const endedAt = Date.now();
        if (settleTimer) { clearInterval(settleTimer); settleTimer = null; }
        // Extra debounce for DOM settle
        settleTimer = setInterval(() => {
          if (isTurnInProgress()) {
            clearInterval(settleTimer);
            settleTimer = null;
            generating = true;
            sawGrowth = true;
            stableRounds = 0;
            return;
          }
          const cur = roleSnapshot("assistant").text;
          if (cur.length === lastAsstLen) stableRounds += 1;
          else { lastAsstLen = cur.length; stableRounds = 0; }
          if (stableRounds < 2) return;
          clearInterval(settleTimer);
          settleTimer = null;
          reportTurnEnded(cur, endedAt);
        }, 800);
      }
      lastAsstLen = curLen;
      lastAsstSignature = curSignature;
      lastAsstCount = curCount;
    };

    const tickScheduler = BROWSER_PERFORMANCE?.createCoalescedScheduler
      ? BROWSER_PERFORMANCE.createCoalescedScheduler(onTick, {
          minIntervalMs: BROWSER_PERFORMANCE.DEFAULT_MUTATION_COALESCE_MS,
          isSuspended: () => document.hidden,
        })
      : null;
    let lastIntervalAt = Date.now();
    setInterval(() => {
      const scheduledAt = Date.now();
      if (document.hidden) { lastIntervalAt = scheduledAt; return; }
      const driftMs = scheduledAt - lastIntervalAt - 800;
      lastIntervalAt = scheduledAt;
      // Timer drift is the sampler-fidelity fallback signal (throttled timers,
      // contended main thread); the floor ignores natural scheduling jitter.
      if (driftMs >= UI_TICK_DRIFT_FLOOR_MS) uiPressure?.recordTimerDrift(driftMs);
      if (tickScheduler) tickScheduler.schedule();
      else onTick();
    }, 800);
    try {
      const mo = new MutationObserver(() => {
        if (document.hidden) return;
        uiPressure?.recordMutation();
        markLatestTurnsDirty();
        if (tickScheduler) tickScheduler.schedule();
        else onTick();
      });
      // Structural changes are enough for low-latency turn detection and mark
      // the latest-turn cache dirty (one rediscovery on the next tick). Streaming
      // character updates are sampled by the bounded scheduler instead of
      // synchronously rescanning the full conversation on every token.
      mo.observe(document.documentElement, { childList: true, subtree: true });
      document.addEventListener("visibilitychange", () => {
        if (document.hidden) return;
        // The page can mutate while hidden; rehydrate the latest-turn cache on
        // the first visible tick instead of trusting stale element references.
        markLatestTurnsDirty();
        if (tickScheduler) tickScheduler.flush();
        else onTick();
      });
      // Optional Long Tasks observation feeds the meter's informational fields
      // only; nothing in the watcher depends on it.
      if (typeof PerformanceObserver === "function" && uiPressure) {
        const longTasks = new PerformanceObserver((list) => {
          if (document.hidden) return;
          for (const entry of list.getEntries()) uiPressure.recordLongTask(entry.duration);
        });
        longTasks.observe({ type: "longtask", buffered: false });
      }
    } catch (e) {}
  }

  // ---- Compact in-page HUD + operational drawer (ChatGPT) ----
  // The always-visible bar stays small. Clicking it opens a layered control
  // drawer for conversation binding and timing; the toolbar icon opens the
  // full Options page for advanced transport / model configuration.
  const HUD_ID = "h2w-page-hud";
  let hudPending = false;
  let hudCache = null;
  let hudEls = null;
  let hudExpanded = false;
  let hudActionBusy = false;
  let nativeConversationTitle = "";
  let renderedHerdrTitle = "";
  let titleSnapshot = null;
  let titleObserver = null;

  function cleanConversationTitle(value) {
    return normText(value)
      .replace(/\s+[-–—|]\s+(ChatGPT|Claude|DeepSeek|Z\.AI)$/i, "")
      .trim();
  }

  function captureNativeConversationTitle() {
    const current = normText(document.title);
    if (!current || current === renderedHerdrTitle) return;
    nativeConversationTitle = cleanConversationTitle(current) || current;
  }

  function titleStatusIcon(hud, state) {
    const health = String(conversationHealth?.state || "");
    const continuity = String(hud?.continuity?.state || "");
    const handoff = String(hud?.handoff?.status || "");
    const bindings = Array.isArray(hud?.bindings) ? hud.bindings : [];
    const workspaceWorking = state === "working"
      || bindings.some((binding) => binding?.status === "working" || Number(binding?.working_count) > 0);

    // The tab title answers a human question: "what needs my attention here?"
    // It deliberately does not expose the internal state-machine labels.
    if (isComposerGenerating() || health === "reply_waiting") return "⏳";
    if (state === "offline" || state === "failed" || health === "failed" || handoff === "failed") return "🔴";
    if (workspaceWorking) return "⚙️";
    if (["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(handoff)
      || ["recovery_message_sent", "reload_pending", "recovering"].includes(health)
      || continuity === "handoff_prepare") return "🔄";
    if (health === "rollover_required" || ["high_risk", "rollover_required"].includes(continuity)) return "🚨";
    if (continuity === "context_warning") return "🧠";
    if (state === "blocked" || ["reply_suspect", "rollover_recommended"].includes(health)
      || continuity === "rollover_recommended" || handoff === "seed_uncertain") return "⚠️";
    if (state === "done") return "👀";
    if (state === "idle") return "💤";
    return "⚪";
  }

  function syncDocumentTitle(hud, state) {
    captureNativeConversationTitle();
    const status = titleStatusIcon(hud, state);
    const project = chatGptDomProjectTitle()
      || hud?.active_workspace_label
      || hud?.workspace_label
      || hud?.workspace_id
      || hudLabels?.states?.unbound
      || "unbound";
    const conversation = chatGptDomConversationTitle() || nativeConversationTitle || ADAPTER.name || "conversation";
    const next = [status, project, conversation].map((value) => normText(value)).filter(Boolean).join("-");
    titleSnapshot = { hud, state };
    if (!next || document.title === next) {
      renderedHerdrTitle = next;
      return;
    }
    renderedHerdrTitle = next;
    document.title = next;
  }

  function startDocumentTitleSync() {
    captureNativeConversationTitle();
    if (titleObserver || typeof MutationObserver !== "function") return;
    const target = document.head || document.documentElement;
    if (!target) return;
    titleObserver = new MutationObserver(() => {
      const current = normText(document.title);
      if (!current || current === renderedHerdrTitle) return;
      nativeConversationTitle = cleanConversationTitle(current) || current;
      if (titleSnapshot) syncDocumentTitle(titleSnapshot.hud, titleSnapshot.state);
    });
    titleObserver.observe(target, { subtree: true, childList: true, characterData: true });
  }

  function parseHudSec(value, fallback) {
    if (value === "" || value === undefined || value === null) return fallback;
    const n = Number(value);
    if (!Number.isFinite(n)) return fallback;
    if (n <= 0) return 0;
    return Math.min(Math.floor(n), 86400);
  }

  function hudWorkspaceId(workspace) {
    return String(workspace?.id || workspace?.workspace_id || workspace?.workspace || "").trim();
  }

  function hudWorkspaceTitle(workspace) {
    const id = hudWorkspaceId(workspace);
    const label = String(workspace?.label || workspace?.workspace_label || "").trim();
    return label ? `${label} (${id})` : id;
  }

  function hudBoundRuntimeState(hud) {
    if (hud?.runtime_available === false) return "offline";
    const bindings = Array.isArray(hud?.bindings) ? hud.bindings : [];
    if (!hud?.bound || !bindings.length) return hud?.bound ? "bound" : "unbound";
    if (bindings.some((b) => b?.status === "working" || Number(b?.working_count) > 0)) return "working";
    if (bindings.some((b) => b?.status === "blocked")) return "blocked";
    const statuses = bindings.map((b) => String(b?.status || "").trim()).filter(Boolean);
    if (statuses.includes("done")) return "done";
    if (statuses.includes("idle")) return "idle";
    return statuses[0] || "bound";
  }

  function hudBindingForWorkspace(id) {
    return (Array.isArray(hudCache?.bindings) ? hudCache.bindings : [])
      .find((binding) => String(binding?.workspace_id || "") === id) || null;
  }

  function setHudExpanded(expanded) {
    const ui = ensurePageHud();
    hudExpanded = Boolean(expanded);
    ui.panel.hidden = !hudExpanded;
    ui.expand.textContent = hudExpanded ? "⌄" : "⌃";
    ui.expand.setAttribute("aria-expanded", String(hudExpanded));
    if (hudExpanded) renderHudWorkspaceBindings();
  }

  function setHudActionBusy(busy) {
    hudActionBusy = Boolean(busy);
    if (!hudEls) return;
    hudEls.quick.disabled = hudActionBusy;
    hudEls.tick.disabled = hudActionBusy;
    hudEls.fallback.disabled = hudActionBusy;
    syncHudManualButtons();
    syncHudHandoffButton();
  }

  function syncHudManualButtons() {
    if (!hudEls) return;
    const chatGptConversationActionsAvailable = ADAPTER.name !== "chatgpt" || Boolean(chatGptConversationId());
    const locked = hudActionBusy || hudCache?.enabled === true || !chatGptConversationActionsAvailable;
    for (const button of hudEls.manualButtons || []) {
      button.hidden = !chatGptConversationActionsAvailable;
      button.classList.toggle("locked", locked);
      button.disabled = locked;
      button.setAttribute("aria-disabled", String(locked));
    }
  }

  function syncHudHandoffButton() {
    if (!hudEls?.handoff) return;
    const hud = hudCache || null;
    const status = String(hud?.handoff?.status || "");
    const active = ["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(status)
      && hud?.handoff?.can_resume !== true;
    hudEls.handoff.hidden = hud?.manual_handoff_available !== true;
    hudEls.handoff.textContent = handoffButtonText(hud);
    hudEls.handoff.title = hudText("manual_handoff_hint");
    hudEls.handoff.disabled = hudActionBusy || active || hud?.bound !== true;
    hudEls.handoff.classList.toggle("locked", active);
    hudEls.handoff.setAttribute("aria-disabled", String(hudEls.handoff.disabled));
  }

  function showHudToast(text, kind = "") {
    const ui = ensurePageHud();
    ui.toast.textContent = String(text || "");
    ui.toast.className = `toast${kind ? ` ${kind}` : ""}`;
    ui.toast.hidden = !text;
    if (text) setTimeout(() => {
      if (ui.toast.textContent === String(text)) ui.toast.hidden = true;
    }, 3500);
  }

  function hudText(key, vars = null, fallback = "") {
    let text = String(hudLabels?.[key] || fallback || "");
    if (vars) {
      for (const [name, value] of Object.entries(vars)) {
        text = text.replaceAll(`{${name}}`, String(value));
      }
    }
    return text;
  }

  async function setHudProjectAutomation(enabled) {
    if (hudActionBusy) return;
    const projectMode = hudCache?.project_automation_available === true && Boolean(hudCache?.project_id);
    const conversationMode = hudCache?.conversation_automation_available === true;
    if (!projectMode && !conversationMode) return;
    setHudActionBusy(true);
    try {
      const on = Boolean(enabled);
      const result = await sendBg({
        type: "h2w_set_project_automation",
        project_id: projectMode ? hudCache.project_id : null,
        site: ADAPTER.name,
        convKey: ADAPTER.getConversationKey(),
        enabled: on,
      });
      if (result?.ok) {
        automationEnabled = on && hudCache?.runtime_available === true;
        hudCache = {
          ...(hudCache || {}),
          enabled: on,
          effective_enabled: automationEnabled,
          idleNudgeEnabled: automationEnabled,
          project_automation_enabled: projectMode ? on : hudCache?.project_automation_enabled,
          conversation_automation_enabled: conversationMode ? on : hudCache?.conversation_automation_enabled,
        };
        syncAutomationPermissionWatch();
        paintPageHud({ hud: hudCache });
        await refreshPageHud();
        showHudToast(on ? hudText("automation_enabled") : hudText("automation_disabled"), "ok");
      } else {
        showHudToast(hudText("automation_update_failed"), "err");
      }
    } catch (_) {
      showHudToast(hudText("automation_update_failed"), "err");
    } finally {
      setHudActionBusy(false);
    }
  }

  async function saveHudTiming() {
    if (hudActionBusy) return;
    const ui = ensurePageHud();
    const tick = parseHudSec(ui.tick.value, Number(hudCache?.progressTickSec) || 60);
    const fallback = parseHudSec(ui.fallback.value, Number(hudCache?.progressFallbackSec) || 1200);
    ui.tick.value = String(tick);
    ui.fallback.value = String(fallback);
    setHudActionBusy(true);
    const result = await sendBg({
      type: "h2w_set_config",
      config: { progressTickSec: tick, progressFallbackSec: fallback },
    });
    if (result?.ok) {
      hudCache = { ...(hudCache || {}), progressTickSec: tick, progressFallbackSec: fallback };
      showHudToast(hudText("timing_saved"), "ok");
    } else {
      showHudToast(hudText("timing_save_failed"), "err");
    }
    setHudActionBusy(false);
  }

  async function mutateHudBinding(workspace, shouldBind) {
    if (hudActionBusy) return;
    const id = hudWorkspaceId(workspace);
    if (!id) return;
    setHudActionBusy(true);
    let result;
    if (shouldBind) {
      result = await sendBg({
        type: "h2w_bind",
        convKey: ADAPTER.getConversationKey(),
        workspace_id: id,
        workspace_label: hudWorkspaceTitle(workspace),
        workspace_label_raw: workspace?.label || null,
        roots: workspace?.roots || [],
      });
    } else {
      result = await sendBg({
        type: "h2w_unbind",
        convKey: ADAPTER.getConversationKey(),
        workspace_id: id,
      });
    }
    if (result?.ok) {
      showHudToast(
        shouldBind
          ? hudText("bound_to", { name: hudWorkspaceTitle(workspace) })
          : hudText("unbound_from", { name: hudWorkspaceTitle(workspace) }),
        "ok",
      );
      await refreshPageHud();
    } else {
      showHudToast(hudText("binding_failed", { error: result?.error || "unknown" }), "err");
    }
    setHudActionBusy(false);
  }

  function renderHudWorkspaceBindings() {
    if (!hudEls || !hudExpanded) return;
    const list = hudEls.workspaces;
    list.textContent = "";
    const workspaces = Array.isArray(hudCache?.workspaces) ? [...hudCache.workspaces] : [];
    const boundIds = new Set(hudCache?.bound_workspace_ids || []);
    workspaces.sort((a, b) => {
      const ai = boundIds.has(hudWorkspaceId(a)) ? 0 : 1;
      const bi = boundIds.has(hudWorkspaceId(b)) ? 0 : 1;
      return ai - bi || hudWorkspaceTitle(a).localeCompare(hudWorkspaceTitle(b));
    });

    if (!workspaces.length) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = hudCache?.workspace_error
        ? hudText("workspaces_unavailable", { error: hudCache.workspace_error })
        : hudText("no_workspaces");
      list.appendChild(empty);
      return;
    }

    for (const workspace of workspaces) {
      const id = hudWorkspaceId(workspace);
      if (!id) continue;
      const bound = boundIds.has(id);
      const row = document.createElement("div");
      row.className = `ws-row${bound ? " bound" : ""}`;
      const copy = document.createElement("div");
      copy.className = "ws-copy";
      const title = document.createElement("div");
      title.className = "ws-title";
      title.textContent = hudWorkspaceTitle(workspace);
      const meta = document.createElement("div");
      meta.className = "ws-meta";
      const roots = Array.isArray(workspace?.roots) ? workspace.roots : [];
      const binding = bound ? hudBindingForWorkspace(id) : null;
      if (binding) {
        const state = String(binding.status || "bound");
        const active = Number(binding.working_count) || 0;
        const stateLabel = hudLabels?.states?.[state] || state;
        meta.textContent = active > 0 ? `${stateLabel} · ${hudText("active", { count: active })}` : stateLabel;
      } else {
        meta.textContent = roots.length ? String(roots[0]) : hudText("available");
      }
      copy.append(title, meta);
      const action = document.createElement("button");
      action.type = "button";
      action.className = `ws-action${bound ? " danger" : ""}`;
      action.textContent = bound ? hudText("unbind") : hudText("bind");
      action.disabled = hudActionBusy;
      action.addEventListener("pointerdown", (event) => event.stopPropagation());
      action.addEventListener("click", (event) => {
        event.stopPropagation();
        void mutateHudBinding(workspace, !bound);
      });
      row.append(copy, action);
      list.appendChild(row);
    }
  }

  async function manualContinueAction(action) {
    if (hudActionBusy || hudCache?.enabled === true) return { ok: false, error: "automation_enabled" };
    setHudActionBusy(true);
    try {
      const result = await sendBg({
        type: "h2w_manual_continue",
        action,
        convKey: ADAPTER.getConversationKey(),
        userText: lastMessageByRole("user"),
        assistantText: lastMessageByRole("assistant"),
      });
      if (result?.ok && result?.nudged === false && result?.continued === false) {
        showHudToast(hudText("judge_no_continue"), "ok");
      } else if (result?.ok) {
        showHudToast(hudText("continue_sent"), "ok");
      } else {
        showHudToast(hudText("continue_failed", { error: result?.error || "unknown" }), "err");
      }
      return result;
    } finally {
      setHudActionBusy(false);
    }
  }

  function handoffButtonText(hud) {
    const status = String(hud?.handoff?.status || "");
    if (hud?.handoff?.can_resume === true) return hudText("handoff_resume");
    if (status === "summary_requested") return hudText("handoff_compressing");
    if (["summary_ready", "target_opening", "seed_submitting"].includes(status)) return hudText("handoff_moving");
    return hudText("manual_handoff");
  }

  function handoffErrorText(error) {
    const code = String(error || "unknown");
    if (["project_conversation_required", "handoff_conversation_required"].includes(code)) return hudText("handoff_project_only");
    if (code === "binding_required") return hudText("handoff_binding_required");
    if (code === "workspace_busy") return hudText("handoff_workspace_busy");
    if (code === "automation_enabled") return hudText("handoff_automation_enabled");
    return hudText("handoff_failed", { error: code });
  }

  async function manualHandoffAction() {
    if (hudActionBusy) return { ok: false, error: "busy" };
    setHudActionBusy(true);
    try {
      const result = await sendBg({ type: "h2w_handoff_start", trigger: "manual" });
      if (result?.ok) {
        showHudToast(hudText("handoff_started"), "ok");
      } else {
        showHudToast(handoffErrorText(result?.error), "err");
      }
      await refreshPageHud();
      return result;
    } finally {
      setHudActionBusy(false);
    }
  }

  function ensurePageHud() {
    if (hudEls?.host?.isConnected) return hudEls;
    let host = document.getElementById(HUD_ID);
    if (!host) {
      host = document.createElement("div");
      host.id = HUD_ID;
      (document.documentElement || document.body).appendChild(host);
    }
    const shadow = host.shadowRoot || host.attachShadow({ mode: "open" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; }
        .bar {
          position: fixed; left: 0; right: 0; bottom: 0; z-index: 2147483646;
          height: 30px; padding: 3px 8px 3px 12px; box-sizing: border-box;
          display: flex; align-items: center; gap: 6px;
          font: 12px/1.4 ui-sans-serif, system-ui, -apple-system, sans-serif;
          color: #4d4d4d; background: rgba(255,255,255,.96);
          border-top: 1px solid #eaeaea;
          box-shadow: 0 -1px 8px rgba(0,0,0,.04);
          pointer-events: auto; user-select: none;
          backdrop-filter: blur(10px);
          /* Keep the automation cue deterministic. Frequent HUD refreshes can restart CSS transitions and leave the bar visually stuck at the neutral color. */
          transition: none;
        }
        .bar.automation-on {
          color: #244c35;
          background: rgba(236,253,245,.97);
          border-top-color: #86d9a5;
          box-shadow: 0 -2px 12px rgba(22,101,52,.10);
        }
        .bar.automation-on .workspace { color: #527361; }
        .bar.automation-on .quick,
        .bar.automation-on .manual,
        .bar.automation-on .handoff,
        .bar.automation-on .expand {
          background: rgba(255,255,255,.72);
          border-color: #a9dfbb;
        }
        .bar.automation-on .quick.on {
          color: #14532d;
          background: #dcfce7;
          border-color: #6fcf8e;
          font-weight: 650;
        }
        button, input { font: inherit; }
        button { color: inherit; }
        .summary {
          min-width: 0; flex: 1; height: 24px; padding: 0; border: 0; background: transparent;
          display: flex; align-items: center; gap: 7px; cursor: pointer; text-align: left;
        }
        .status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
        .workspace { color: #8a8a8a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; }
        .quick, .manual, .handoff, .expand {
          height: 22px; border: 1px solid #dedede; background: #fafafa; border-radius: 7px;
          cursor: pointer; padding: 0 7px; font-size: 11px; white-space: nowrap;
        }
        .quick.on { color: #166534; background: #f0fdf4; border-color: #bbf7d0; }
        .quick.off { color: #6b7280; background: #f5f5f5; }
        .manual.locked, .handoff.locked { opacity: .45; cursor: not-allowed; }
        .expand { width: 24px; padding: 0; font-size: 13px; }
        button:hover { filter: brightness(.97); }
        button:disabled, input:disabled { opacity: .55; cursor: wait; }
        .manual.locked:disabled, .handoff.locked:disabled { opacity: .45; cursor: not-allowed; }
        .bar.waiting .status { color: #8a4b00; }
        .bar.working .status { color: #a15c00; }
        .bar.done .status, .bar.idle .status { color: #18794e; }
        .bar.blocked .status { color: #b42318; }
        .bar.recovering .status { color: #9a3412; }
        .bar.failed .status { color: #b42318; }
        .panel {
          position: fixed; right: 8px; bottom: 36px; z-index: 2147483647;
          width: min(368px, calc(100vw - 16px)); max-height: min(62vh, 560px); overflow: auto;
          box-sizing: border-box; padding: 0;
          color: #252525; background: rgba(255,255,255,.985); border: 1px solid #dedede;
          border-radius: 12px; box-shadow: 0 14px 42px rgba(0,0,0,.18);
          font: 12px/1.4 ui-sans-serif, system-ui, -apple-system, sans-serif;
          pointer-events: auto; user-select: none; backdrop-filter: blur(14px);
        }
        .panel[hidden] { display: none !important; }
        .panel-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 10px 11px 8px; }
        .panel-title { font-size: 13px; font-weight: 700; }
        .conversation { margin-top: 1px; max-width: 260px; color: #909090; font-size: 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .options { border: 0; background: transparent; color: #6b7280; cursor: pointer; font-size: 11px; padding: 3px 5px; }
        .section { border-top: 1px solid #efefef; padding: 9px 11px; }
        .timing { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-top: 9px; }
        .timing label { display: grid; gap: 3px; color: #777; font-size: 10px; }
        .timing input { width: 100%; box-sizing: border-box; height: 27px; border: 1px solid #ddd; border-radius: 7px; padding: 2px 7px; color: #333; background: #fff; }
        .section-title { font-size: 10px; color: #858585; text-transform: uppercase; letter-spacing: .04em; margin-bottom: 5px; }
        .workspaces { display: grid; gap: 3px; min-width: 0; }
        .ws-row {
          display: flex; align-items: center; gap: 8px; width: 100%; min-width: 0;
          box-sizing: border-box; padding: 6px 7px; border-radius: 8px;
        }
        .ws-row:hover { background: #f7f7f7; }
        .ws-row.bound { background: #f4fbf6; }
        .ws-copy { flex: 1; min-width: 0; }
        .ws-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .ws-meta { margin-top: 1px; color: #9b9b9b; font-size: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .ws-action { flex: 0 0 auto; border: 1px solid #d5d5d5; background: #fff; border-radius: 7px; padding: 3px 7px; cursor: pointer; font-size: 10px; }
        .ws-action.danger { color: #b42318; }
        .empty { color: #999; padding: 7px 3px; }
        .toast { margin: 0 11px 9px; padding: 6px 8px; border-radius: 7px; background: #f5f5f5; color: #555; font-size: 10px; }
        .toast.ok { background: #f0fdf4; color: #166534; }
        .toast.err { background: #fef2f2; color: #b42318; }
        @media (prefers-color-scheme: dark) {
          .bar { color: #ddd; background: rgba(32,32,32,.96); border-color: #3a3a3a; }
          .bar.automation-on {
            color: #d9fbe5;
            background: rgba(25,57,38,.97);
            border-color: #397b52;
            box-shadow: 0 -2px 12px rgba(34,197,94,.12);
          }
          .bar.automation-on .workspace { color: #a6d5b7; }
          .bar.automation-on .quick,
          .bar.automation-on .manual,
          .bar.automation-on .handoff,
          .bar.automation-on .expand { background: #23452f; border-color: #477b59; }
          .bar.automation-on .quick.on { color: #bbf7d0; background: #1d5130; border-color: #5aa773; }
          .workspace { color: #8d8d8d; }
          .quick, .manual, .handoff, .expand { background: #292929; border-color: #454545; }
          .quick.on { color: #86efac; background: #143020; border-color: #245c36; }
          .panel { color: #e8e8e8; background: rgba(32,32,32,.985); border-color: #494949; }
          .section { border-color: #414141; }
          .timing input { color: #eee; background: #282828; border-color: #4a4a4a; }
          .ws-row:hover { background: #292929; }
          .ws-row.bound { background: #173020; }
          .ws-action { color: #ddd; background: #272727; border-color: #4a4a4a; }
        }
      </style>
      <div class="panel" part="panel" hidden>
        <div class="panel-head">
          <div><div class="panel-title"></div><div class="conversation"></div></div>
          <button type="button" class="options"></button>
        </div>
        <div class="section">
          <div class="section-title event-title"></div>
          <div class="timing">
            <label><span class="tick-label"></span><input class="tick" type="number" min="0" step="1"></label>
            <label><span class="fallback-label"></span><input class="fallback" type="number" min="0" step="1"></label>
          </div>
        </div>
        <div class="section"><div class="section-title bindings-title"></div><div class="workspaces"></div></div>
        <div class="toast" hidden></div>
      </div>
      <div class="bar" part="bar">
        <button type="button" class="summary"><span class="status"></span><span class="workspace"></span></button>
        <button type="button" class="manual manual-continue"></button>
        <button type="button" class="manual manual-status"></button>
        <button type="button" class="manual manual-judge"></button>
        <button type="button" class="handoff manual-handoff"></button>
        <button type="button" class="quick" aria-label=""></button>
        <button type="button" class="expand" aria-label="" aria-expanded="false">⌃</button>
      </div>
    `;
    hudEls = {
      host,
      bar: shadow.querySelector(".bar"),
      status: shadow.querySelector(".status"),
      workspace: shadow.querySelector(".workspace"),
      summary: shadow.querySelector(".summary"),
      quick: shadow.querySelector(".quick"),
      handoff: shadow.querySelector(".handoff"),
      expand: shadow.querySelector(".expand"),
      panel: shadow.querySelector(".panel"),
      conversation: shadow.querySelector(".conversation"),
      options: shadow.querySelector(".options"),
      panelTitle: shadow.querySelector(".panel-title"),
      eventTitle: shadow.querySelector(".event-title"),
      tickLabel: shadow.querySelector(".tick-label"),
      fallbackLabel: shadow.querySelector(".fallback-label"),
      bindingsTitle: shadow.querySelector(".bindings-title"),
      manualButtons: [...shadow.querySelectorAll(".manual")],
      tick: shadow.querySelector(".tick"),
      fallback: shadow.querySelector(".fallback"),
      workspaces: shadow.querySelector(".workspaces"),
      toast: shadow.querySelector(".toast"),
    };
    hudEls.summary.addEventListener("click", (event) => {
      event.stopPropagation();
      setHudExpanded(!hudExpanded);
    });
    hudEls.expand.addEventListener("click", (event) => {
      event.stopPropagation();
      setHudExpanded(!hudExpanded);
    });
    hudEls.quick.addEventListener("click", () => {
      void setHudProjectAutomation(!(hudCache?.enabled === true));
    });
    hudEls.handoff.addEventListener("click", () => { void manualHandoffAction(); });
    hudEls.manualButtons.forEach((button, index) => {
      const actions = ["direct", "status", "judge"];
      button.addEventListener("click", () => {
        if (hudActionBusy || hudCache?.enabled === true) return;
        void manualContinueAction(actions[index]);
      });
    });
    hudEls.tick.addEventListener("change", () => { void saveHudTiming(); });
    hudEls.fallback.addEventListener("change", () => { void saveHudTiming(); });
    hudEls.options.addEventListener("click", () => { void sendBg({ type: "h2w_open_options" }); });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && hudExpanded) setHudExpanded(false);
    });
    document.addEventListener("pointerdown", (event) => {
      if (!hudExpanded) return;
      const path = typeof event.composedPath === "function" ? event.composedPath() : [];
      if (path.includes(host) || event.target === host) return;
      setHudExpanded(false);
    }, true);
    return hudEls;
  }

  function liftComposer(px) {
    try {
      const ta = document.querySelector("#prompt-textarea");
      const form = ta?.closest("form");
      if (form) form.style.paddingBottom = `${px}px`;
    } catch (_) { /* ignore */ }
    try { document.documentElement.style.paddingBottom = `${px}px`; } catch (_) { /* ignore */ }
  }

  function recoveryLabel(hud) {
    const healthState = String(conversationHealth?.state || "");
    if (["reply_suspect", "recovery_message_sent", "reload_pending", "recovering", "rollover_recommended", "rollover_required"].includes(healthState)) {
      return healthState;
    }
    return String(hud?.handoff?.status || "none");
  }

  function hudVisualClass(state) {
    if (["working", "idle", "done", "blocked"].includes(state)) return state;
    if (state === "offline") return "failed";
    if (state === "reply_waiting") return "waiting";
    if (["reply_suspect", "recovery_message_sent", "reload_pending", "recovering", "rollover_recommended", "rollover_required"].includes(state)) {
      return "recovering";
    }
    if (state === "failed") return "failed";
    return "";
  }

  function paintPageHud(view = {}) {
    const ui = ensurePageHud();
    liftComposer(32);
    if (view.hud) hudCache = view.hud;
    if (view.continuity) hudCache = { ...(hudCache || {}), continuity: view.continuity };
    const hud = view.hud || hudCache || null;
    hudLabels = hud?.labels || hudLabels;
    if (view.pending === true) hudPending = true;
    if (view.pending === false) hudPending = false;

    // The bar reports the bound Herdr workspace runtime state independently
    // from whether automatic wake/nudge is enabled. Recovery remains visible
    // in the tooltip through the separate recovery field below.
    const runtimeState = hudBoundRuntimeState(hud);
    const continuity = hud?.continuity || null;
    const state = hud?.runtime_available === false
      ? "offline"
      : continuity?.state && continuity.state !== "healthy"
        ? continuity.state
        : runtimeState;
    const lastEvent = hud?.last?.reason
      ? `${hudText(`reason_${hud.last.reason}`, null, hud.last.reason)}${hud.last.at ? ` @ ${new Date(hud.last.at).toLocaleTimeString()}` : ""}`
      : null;
    const input = {
      workspace: hud?.workspace_label || hud?.workspace_id || null,
      agent: hud?.focus_agent || hud?.agent || null,
      conversation: ADAPTER.getConversationKey(),
      state,
      recovery: recoveryLabel(hud),
      lastEvent,
    };
    if (globalThis.H2W_HUD?.updateReadonlyHud) {
      globalThis.H2W_HUD.updateReadonlyHud(ui.status, { ...input, labels: hudLabels });
    } else {
      ui.status.textContent = `Herdr ● ${hudLabels?.states?.[state] || state}`;
    }
    const preferenceEnabled = hud?.enabled === true;
    const effectiveEnabled = hud?.effective_enabled === true;
    automationEnabled = effectiveEnabled;
    automationAutoAllow = hud?.autoAllow !== false;
    syncAutomationPermissionWatch();
    syncDocumentTitle(hud, state);
    ui.workspace.textContent = hud?.workspace_label || (hud?.bound
      ? hudText("bound_count", { count: hud.binding_count || 1 })
      : (hudLabels?.states?.unbound || ""));
    ui.panelTitle.textContent = hudText("controls");
    ui.options.textContent = hudText("advanced_options");
    ui.eventTitle.textContent = hudText("event_settings");
    ui.tickLabel.textContent = hudText("interval");
    ui.fallbackLabel.textContent = hudText("fallback");
    ui.bindingsTitle.textContent = hudText("bindings");
    const manualLabels = [
      [hudLabels.manual_continue, hudLabels.manual_continue_hint],
      [hudLabels.manual_status, hudLabels.manual_status_hint],
      [hudLabels.manual_judge, hudLabels.manual_judge_hint],
    ];
    for (let i = 0; i < (ui.manualButtons || []).length; i += 1) {
      const [text, title] = manualLabels[i] || [];
      ui.manualButtons[i].textContent = text || "";
      ui.manualButtons[i].title = title || "";
    }
    syncHudManualButtons();
    ui.quick.hidden = hud?.project_automation_available !== true
      && hud?.conversation_automation_available !== true;
    syncHudHandoffButton();
    ui.quick.textContent = preferenceEnabled ? hudText("automation_on", null, "Auto on") : hudText("automation_off", null, "Auto off");
    ui.quick.className = `quick ${preferenceEnabled ? "on" : "off"}`;
    ui.quick.setAttribute("aria-pressed", String(preferenceEnabled));
    ui.quick.setAttribute("aria-label", hudText("aria_toggle_automation"));
    const conversationAutomation = hud?.conversation_automation_available === true && !hud?.project_id;
    ui.quick.title = conversationAutomation
      ? (preferenceEnabled ? hudText("conversation_automation_on_hint") : hudText("conversation_automation_off_hint"))
      : (preferenceEnabled ? hudText("automation_on_hint") : hudText("automation_off_hint"));
    ui.expand.setAttribute("aria-label", hudText("aria_open_controls"));
    ui.expand.title = hudText("aria_open_controls");
    ui.conversation.textContent = ADAPTER.getConversationKey();
    if (document.activeElement !== ui.tick && shadowActiveElement(ui.host) !== ui.tick) {
      ui.tick.value = String(hud?.progressTickSec ?? 60);
    }
    if (document.activeElement !== ui.fallback && shadowActiveElement(ui.host) !== ui.fallback) {
      ui.fallback.value = String(hud?.progressFallbackSec ?? 1200);
    }
    renderHudWorkspaceBindings();
    const visual = hudVisualClass(state);
    ui.bar.className = `bar${effectiveEnabled ? " automation-on" : ""}${visual ? ` ${visual}` : ""}`;
  }

  function shadowActiveElement(host) {
    try { return host?.shadowRoot?.activeElement || null; } catch (_) { return null; }
  }

  async function refreshPageHud() {
    if (!runtimeAlive()) return;
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    paintPageHud({ hud: hud && hud.ok ? hud : null });
  }

  function startPageHud() {
    startDocumentTitleSync();
    paintPageHud({ pending: false });
    void refreshPageHud();
    setInterval(() => {
      if (!document.hidden) void refreshPageHud();
    }, 5000);
    try {
      document.addEventListener("visibilitychange", () => {
        if (!document.hidden) void refreshPageHud();
      });
    } catch (_) {}
  }

})();
