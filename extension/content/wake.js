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
const H2W_CONTENT_VERSION = "0.1.72";
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
  let lastPageHealthProbeAt = 0;
  const pageHealthStartedAt = Date.now();
  // Fail closed until background confirms the master automation state. The
  // read-only HUD/workspace observers do not depend on these flags.
  let automationEnabled = false;
  let automationAutoAllow = false;
  let hudLabels = {};
  let queuedInsertCount = 0;
  let queuedInsertActionBusy = false;
  let queuedInsertButton = null;
  const QUEUED_INSERT_BUTTON_ID = "h2w-queued-insert-button";
  const QUEUED_INSERT_STYLE_ID = "h2w-queued-insert-style";
  const QUEUED_INSERT_LAST_BATCH_KEY = "h2wQueuedInsertLastBatchV1";
  const QUEUED_INSERT_OWNER_ATTR = "data-h2w-queue-owner";
  const queuedInsertOwnerId = `${H2W_CONTENT_VERSION}:${Date.now().toString(36)}:${Math.random().toString(36).slice(2)}`;
  const HEALTH_STORAGE_KEY = "h2wConversationHealthByConv";
  const CONTEXT_PRESSURE_STORAGE_KEY = "h2wContextPressureByConv";

  try { document.documentElement?.setAttribute(QUEUED_INSERT_OWNER_ATTR, queuedInsertOwnerId); } catch (_) {}

  function ownsQueuedInsertSurface() {
    try { return document.documentElement?.getAttribute(QUEUED_INSERT_OWNER_ATTR) === queuedInsertOwnerId; }
    catch (_) { return false; }
  }

  function conversationHasPendingReply() {
    const submitAt = Number(conversationHealth?.last_user_submit_at || 0);
    const endedAt = Number(conversationHealth?.last_turn_end_at || 0);
    return submitAt > 0 && submitAt > endedAt;
  }

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

  function classifyBgMessageFailure(message = "") {
    const text = String(message || "").toLowerCase();
    if (!runtimeAlive() || text.includes("extension context invalidated")) {
      return "extension-context-invalidated";
    }
    if (text.includes("receiving end does not exist")
      || text.includes("message port closed")
      || text.includes("could not establish connection")) {
      return "background-unavailable";
    }
    return "background-message-failed";
  }

  // Queue mutations need structured transport failures. Keep sendBg()'s older
  // null-on-failure contract for recovery/register callers that depend on it.
  function sendBgResult(msg) {
    return new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(msg, (resp) => {
          const lastError = chrome.runtime.lastError;
          if (lastError) {
            resolve({ ok: false, error: classifyBgMessageFailure(lastError.message) });
            return;
          }
          if (resp === undefined) {
            resolve({ ok: false, error: "background-no-response" });
            return;
          }
          resolve(resp);
        });
      } catch (error) {
        resolve({ ok: false, error: classifyBgMessageFailure(error?.message) });
      }
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
    updateQueuedInsertButton();
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

  let conversationHealthPersistChain = Promise.resolve();

  async function persistConversationHealth(record) {
    if (!record?.convKey || !runtimeAlive()) return false;
    const snapshot = { ...record };
    const write = async () => {
      try {
        const stored = await chrome.storage.local.get([HEALTH_STORAGE_KEY]);
        const map = { ...(stored?.[HEALTH_STORAGE_KEY] || {}), [snapshot.convKey]: snapshot };
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
        return true;
      } catch (_) {
        return false;
      }
    };
    const queued = conversationHealthPersistChain.then(write, write);
    conversationHealthPersistChain = queued.then(() => undefined, () => undefined);
    return queued;
  }

  async function reloadAfterPersistingConversationState() {
    // Recovery budgets must survive the reload that consumes them. The normal
    // state writer remains fire-and-forget for hot-path updates, but its writes
    // are serialized so this final barrier cannot be overwritten by an older
    // record after navigation starts.
    if (!conversationHealth?.convKey || !RECOVERY_CONTROLLER?.runAfterDurablePersistence) return false;
    return RECOVERY_CONTROLLER.runAfterDurablePersistence({
      persist: () => persistConversationHealth(conversationHealth),
      action: () => location.reload(),
      waitMs: 150,
      waitFn: wait,
    });
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

  function removeQueuedInsertButton() {
    if (queuedInsertButton?.isConnected) queuedInsertButton.remove();
    queuedInsertButton = null;
  }

  function removeStaleQueuedInsertButtons() {
    for (const button of document.querySelectorAll(`#${QUEUED_INSERT_BUTTON_ID}`)) {
      if (button !== queuedInsertButton) button.remove();
    }
  }

  function ensureQueuedInsertStyle() {
    if (document.getElementById(QUEUED_INSERT_STYLE_ID)) return;
    const style = document.createElement("style");
    style.id = QUEUED_INSERT_STYLE_ID;
    style.textContent = `
      #${QUEUED_INSERT_BUTTON_ID} {
        align-items: center;
        background: color-mix(in srgb, currentColor 8%, transparent);
        border: 0;
        border-radius: 999px;
        color: inherit;
        cursor: pointer;
        display: inline-flex;
        flex: 0 0 auto;
        font: inherit;
        font-size: 12px;
        font-weight: 600;
        height: 32px;
        justify-content: center;
        line-height: 1;
        margin-inline-end: 6px;
        min-width: 42px;
        opacity: .78;
        padding: 0 9px;
        white-space: nowrap;
      }
      #${QUEUED_INSERT_BUTTON_ID}:hover { opacity: 1; }
      #${QUEUED_INSERT_BUTTON_ID}[data-count]:not([data-count="0"]) {
        box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 22%, transparent);
        opacity: 1;
      }
      #${QUEUED_INSERT_BUTTON_ID}[aria-disabled="true"] { cursor: wait; opacity: .45; }
    `;
    (document.head || document.documentElement).appendChild(style);
  }

  function updateQueuedInsertButton() {
    const button = queuedInsertButton;
    if (!button) return;
    const base = hudText("queue_insert", null, "Queue");
    button.textContent = queuedInsertCount > 0
      ? hudText("queue_insert_count", { count: queuedInsertCount }, `${base} · ${queuedInsertCount}`)
      : base;
    button.dataset.count = String(queuedInsertCount);
    button.disabled = queuedInsertActionBusy;
    button.setAttribute("aria-disabled", String(queuedInsertActionBusy));
    button.setAttribute("aria-label", hudText("queue_insert_hint", null,
      "Queue this message without interrupting the current reply."));
    button.title = hudText("queue_insert_hint", null,
      "Queue this message without interrupting the current reply. Right-click to clear the queue.");
  }

  function queuedInsertFailureText(error) {
    const code = String(error || "background-no-response");
    if (code === "extension-context-invalidated") {
      return hudText("queue_extension_reloaded", null,
        "Herdr was updated. Reload this ChatGPT tab before queueing another message.");
    }
    if (["background-unavailable", "background-no-response", "background-message-failed"].includes(code)) {
      return hudText("queue_background_unavailable", null,
        "Herdr background is unavailable. Reload this ChatGPT tab and try again.");
    }
    if (code === "queue-storage-unavailable") {
      return hudText("queue_storage_unavailable", null,
        "Queue storage is temporarily unavailable. The message was not queued; try again.");
    }
    return hudText("queue_failed", { error: code }, `Queue failed: ${code}`);
  }

  async function refreshQueuedInsertStatus() {
    if (ADAPTER.name !== "chatgpt" || !chatGptConversationId()) {
      queuedInsertCount = 0;
      removeQueuedInsertButton();
      return null;
    }
    const convKey = ADAPTER.getConversationKey();
    const response = await sendBgResult({ type: "h2w_queue_status", convKey });
    if (convKey !== ADAPTER.getConversationKey()) return response;
    if (response?.ok) queuedInsertCount = Math.max(0, Number(response.status?.count) || 0);
    if (response?.error === "extension-context-invalidated") {
      removeQueuedInsertButton();
      return response;
    }
    ensureQueuedInsertButton();
    updateQueuedInsertButton();
    return response;
  }

  async function queueCurrentComposerMessage() {
    if (queuedInsertActionBusy || ADAPTER.name !== "chatgpt") return;
    const convKey = ADAPTER.getConversationKey();
    if (!chatGptConversationId() || !convKey) return;
    const text = String(composerTextRaw() || "").trim();
    if (!text) {
      if (queuedInsertCount <= 0) {
        showHudToast(hudText("queue_need_message", null, "Type a message before queueing it."));
        return;
      }
      queuedInsertActionBusy = true;
      updateQueuedInsertButton();
      try {
        const result = await sendBgResult({ type: "h2w_queue_flush", convKey, reason: "button-retry" });
        if (result?.delivered) {
          queuedInsertCount = Math.max(0, Number(result.status?.count) || 0);
          showHudToast(hudText("queue_sent", null, "Queued message sent."), "ok");
        } else if (result?.ok === false && result?.error) {
          showHudToast(queuedInsertFailureText(result.error), "err");
        } else {
          showHudToast(hudText("queue_waiting", { count: queuedInsertCount }, `Queued: ${queuedInsertCount}`));
        }
      } finally {
        queuedInsertActionBusy = false;
        updateQueuedInsertButton();
      }
      return;
    }

    queuedInsertActionBusy = true;
    updateQueuedInsertButton();
    try {
      const queued = await sendBgResult({ type: "h2w_queue_insert", convKey, text });
      if (!queued?.ok) {
        const key = queued?.error === "queue-full" ? "queue_full" : "queue_failed";
        if (key === "queue_full") showHudToast(hudText(key), "err");
        else showHudToast(queuedInsertFailureText(queued?.error), "err");
        if (queued?.error === "extension-context-invalidated") removeQueuedInsertButton();
        return;
      }
      queuedInsertCount = Math.max(0, Number(queued.status?.count) || queuedInsertCount + 1);
      updateQueuedInsertButton();

      // Persistence succeeds before clearing the composer. If the user changed
      // the draft while storage was in flight, keep the new draft untouched.
      if (composerNorm() !== normText(text)) {
        showHudToast(hudText("queue_added_draft_changed", { count: queuedInsertCount },
          `Queued (${queuedInsertCount}); the composer changed, so it was left untouched.`), "ok");
        return;
      }
      await clearComposer();
      for (let i = 0; i < 8 && composerNorm(); i += 1) await wait(75);
      if (composerNorm()) {
        showHudToast(hudText("queue_added_clear_failed", { count: queuedInsertCount },
          `Queued (${queuedInsertCount}), but the composer could not be cleared.`), "err");
        return;
      }

      if (isTurnInProgress()) {
        showHudToast(hudText("queue_added", { count: queuedInsertCount }, `Queued: ${queuedInsertCount}`), "ok");
        return;
      }
      const flushed = await sendBgResult({ type: "h2w_queue_flush", convKey, reason: "enqueue-idle" });
      if (flushed?.delivered) {
        queuedInsertCount = Math.max(0, Number(flushed.status?.count) || 0);
        showHudToast(hudText("queue_sent", null, "Queued message sent."), "ok");
      } else {
        showHudToast(hudText("queue_added", { count: queuedInsertCount }, `Queued: ${queuedInsertCount}`), "ok");
      }
    } finally {
      queuedInsertActionBusy = false;
      updateQueuedInsertButton();
    }
  }

  async function clearQueuedInsertMessages() {
    if (queuedInsertActionBusy || queuedInsertCount <= 0) return;
    const convKey = ADAPTER.getConversationKey();
    const question = hudText("queue_clear_confirm", { count: queuedInsertCount },
      `Clear ${queuedInsertCount} queued message(s)?`);
    if (!globalThis.confirm(question)) return;
    queuedInsertActionBusy = true;
    updateQueuedInsertButton();
    try {
      const response = await sendBgResult({ type: "h2w_queue_clear", convKey });
      if (response?.ok) {
        queuedInsertCount = 0;
        showHudToast(hudText("queue_cleared", null, "Queue cleared."), "ok");
      } else {
        showHudToast(queuedInsertFailureText(response?.error), "err");
        if (response?.error === "extension-context-invalidated") removeQueuedInsertButton();
      }
    } finally {
      queuedInsertActionBusy = false;
      updateQueuedInsertButton();
    }
  }

  function ensureQueuedInsertButton() {
    // A newly injected content script owns the Queue surface. Older contexts
    // left alive by an extension reload may still have timers, but they must
    // only remove their own stale node and must never create a second button.
    if (!runtimeAlive() || !ownsQueuedInsertSurface()) {
      removeQueuedInsertButton();
      return null;
    }
    if (ADAPTER.name !== "chatgpt" || !chatGptConversationId()) {
      removeQueuedInsertButton();
      return null;
    }
    ensureQueuedInsertStyle();
    const anchor = typeof ADAPTER.getComposerActionAnchor === "function"
      ? ADAPTER.getComposerActionAnchor()
      : findSendButton();
    const parent = anchor?.parentElement;
    if (!anchor || !parent) return queuedInsertButton;
    if (!queuedInsertButton || !queuedInsertButton.isConnected) {
      removeStaleQueuedInsertButtons();
      queuedInsertButton = document.createElement("button");
      queuedInsertButton.id = QUEUED_INSERT_BUTTON_ID;
      queuedInsertButton.dataset.h2wQueueOwner = queuedInsertOwnerId;
      queuedInsertButton.type = "button";
      queuedInsertButton.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        void queueCurrentComposerMessage();
      });
      queuedInsertButton.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        event.stopPropagation();
        void clearQueuedInsertMessages();
      });
    } else {
      removeStaleQueuedInsertButtons();
    }
    if (queuedInsertButton.parentElement !== parent || queuedInsertButton.nextSibling !== anchor) {
      parent.insertBefore(queuedInsertButton, anchor);
    }
    updateQueuedInsertButton();
    return queuedInsertButton;
  }

  function queuedInsertBatchWasDelivered(convKey, batchId) {
    try {
      const row = JSON.parse(sessionStorage.getItem(QUEUED_INSERT_LAST_BATCH_KEY) || "null");
      return row?.convKey === convKey && row?.batch_id === batchId;
    } catch (_) {
      return false;
    }
  }

  function rememberQueuedInsertBatch(convKey, batchId) {
    try {
      sessionStorage.setItem(QUEUED_INSERT_LAST_BATCH_KEY, JSON.stringify({ convKey, batch_id: batchId, at: Date.now() }));
    } catch (_) {}
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

  function captureSubmitAckBaseline(sendButton = null) {
    return {
      composer: composerNorm(),
      sendButton,
      userTurn: ADAPTER.name === "chatgpt" ? latestTurnForRole("user") : null,
    };
  }

  function submitWasAccepted(baseline) {
    if (!ADAPTER.inputHasContent()) return true;
    if (ADAPTER.name !== "chatgpt") return false;
    // An accepted ChatGPT send normally replaces or repurposes the exact Send
    // button before ProseMirror clears. Checking that captured node is O(1) and
    // avoids repeatedly scanning a long conversation while the page is hot.
    if (baseline?.sendButton
      && (!baseline.sendButton.isConnected || !isSendButton(baseline.sendButton))) {
      return true;
    }
    const latestUser = latestTurnForRole("user");
    if (latestUser && latestUser !== baseline?.userTurn) {
      const latestText = normText(latestUser.innerText || latestUser.textContent || "");
      const sentText = String(baseline?.composer || "");
      if (latestText && sentText
        && (latestText.includes(sentText.slice(0, 120)) || sentText.includes(latestText.slice(0, 120)))) {
        return true;
      }
    }
    return false;
  }

  async function waitForSubmitAck(baseline, timeoutMs) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeoutMs) {
      await wait(200);
      if (submitWasAccepted(baseline)) return true;
    }
    return submitWasAccepted(baseline);
  }

  // ---- Submission ----
  // For contenteditable sites, wait for an enabled send button because ProseMirror
  // often consumes synthetic keyboard events. ChatGPT can accept a send before
  // ProseMirror clears, so success also observes the Send-button transition or new user turn.
  async function submitAfterPermissionClick() {
    if (!lastPermClickAt || Date.now() - lastPermClickAt > 6000) return false;
    await wait(1200);
    await waitForComposerIdle(4000);
    const btn = findSendButton();
    if (isSendButton(btn)) {
      const baseline = captureSubmitAckBaseline(btn);
      btn.click();
      if (await waitForSubmitAck(baseline, ADAPTER.name === "chatgpt" ? 8000 : 4000)) return true;
    }
    const enterBaseline = captureSubmitAckBaseline(findSendButton());
    dispatchEnterSubmit(ADAPTER.getInputEl());
    if (await waitForSubmitAck(enterBaseline, 3000)) return true;
    return false;
  }

  async function submitTextarea() {
    await waitForComposerIdle();
    await wait(420);
    for (let attempt = 0; attempt < 3; attempt++) {
      const btn = findSendButton();
      if (isSendButton(btn)) {
        const baseline = captureSubmitAckBaseline(btn);
        btn.click();
        if (await waitForSubmitAck(baseline, ADAPTER.name === "chatgpt" ? 8000 : 4000)) return true;
      }
      const enterBaseline = captureSubmitAckBaseline(findSendButton());
      dispatchEnterSubmit(ADAPTER.getInputEl());
      if (await waitForSubmitAck(enterBaseline, 3000)) return true;
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
            const baseline = captureSubmitAckBaseline(btn);
            btn.click();
            if (await waitForSubmitAck(baseline, ADAPTER.name === "chatgpt" ? 8000 : 4000)) return true;
            console.warn("[h2w] composer still has content after Send click; retrying");
            break;
          }
          await wait(150);
        }
        const el = ADAPTER.getInputEl();
        const enterBaseline = captureSubmitAckBaseline(findSendButton());
        dispatchEnterSubmit(el);
        if (await waitForSubmitAck(enterBaseline, 3000)) return true;
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
    if (!data.queueInsert && n && n === lastWakeNorm && Date.now() - lastWakeAt < 8000) {
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
    // Only the NEW-conversation seed wrapper proves target delivery. The
    // source handoff request itself contains the raw HERDR_HANDOFF_V1 packet,
    // so accepting that marker here can misclassify the source conversation as
    // an already-seeded target during same-tab navigation.
    return body.includes(`[HERDR_CONTINUITY_TRANSFER id=${id}]`);
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
        void (async () => {
          const domAssistantText = lastMessageByRole("assistant");
          const domUserText = lastMessageByRole("user");
          const domGenerating = isComposerGenerating();
          const domTurnInProgress = isTurnInProgress();
          const server = ADAPTER.name === "chatgpt"
            ? await fetchChatGptConversationSnapshot()
            : { ok: false };
          const pressure = CONTEXT_PRESSURE ? await updateContextPressure() : null;
          const visibleLimit = visibleConversationLimitSignal();
          const serverAssistantCurrent = Boolean(server?.ok && server.currentNodeRole === "assistant");
          const serverUserCurrent = Boolean(server?.ok && server.currentNodeRole === "user");
          const serverSettled = serverAssistantCurrent && server.finished === true;
          const serverOpen = serverAssistantCurrent && server.finished === false;
          const assistantText = serverAssistantCurrent && server.text
            ? String(server.text)
            : (serverUserCurrent ? "" : domAssistantText);
          const userText = server?.ok && server.userText
            ? String(server.userText)
            : domUserText;
          sendResponse({
            convKey: ADAPTER.getConversationKey(),
            userText,
            assistantText,
            generating: serverSettled ? false : (serverOpen ? true : domGenerating),
            turnInProgress: serverSettled ? false : (serverOpen ? true : domTurnInProgress),
            substantive: looksLikeSubstantiveReply(assistantText),
            endedAt: Date.now(),
            serverOk: server?.ok === true,
            serverFinished: serverAssistantCurrent ? server.finished : null,
            serverMessageId: serverAssistantCurrent ? server.messageId || null : null,
            serverCurrentNodeRole: server?.ok ? server.currentNodeRole || "" : "",
            transcript: server?.ok && server.transcript ? server.transcript : domConversationTranscript(),
            pressureState: pressure?.state || null,
            handoffBlocked: Boolean(visibleLimit),
            handoffBlockReason: visibleLimit ? "conversation_limit_ui" : null,
          });
        })();
        return true;
      }
      if (msg?.type === "h2w_handoff_prompt") {
        (async () => {
          await ensureConversationHealth();
          const beforeText = SPEAKS?.enabled ? SPEAKS.getLatestReply() : "";
          const beforeCount = SPEAKS?.enabled ? SPEAKS.getReplyBlockCount() : 0;
          const result = await performWake({
            template: msg.template || "",
            autoAllow: false,
            handoff: true,
          });
          if (result?.ok && ADAPTER.name === "chatgpt" && CONVERSATION_HEALTH && conversationHealth) {
            markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth));
          }
          if (!result?.ok || ADAPTER.name === "chatgpt") {
            if (result?.ok && ADAPTER.name === "chatgpt") {
              // A hard conversation cap can surface only after Send is accepted.
              // Give the page a short window to expose that terminal UI signal
              // so background can switch immediately to the fallback summarizer.
              await wait(1200);
              const visibleLimit = visibleConversationLimitSignal();
              sendResponse({
                ...result,
                handoffBlocked: Boolean(visibleLimit),
                handoffBlockReason: visibleLimit ? "conversation_limit_ui" : null,
              });
              return;
            }
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
          await ensureConversationHealth();
          const result = await performWake({
            template: msg.template || "",
            autoAllow: false,
            handoff: true,
          });
          if (!result?.ok) { sendResponse(result); return; }
          if (ADAPTER.name === "chatgpt" && CONVERSATION_HEALTH && conversationHealth) {
            markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth));
          }
          const confirmed = await waitForHandoffTarget(msg.transferId);
          sendResponse({ ...result, ...confirmed });
        })();
        return true;
      }
      if (msg?.type === "h2w_handoff_probe") {
        const inputEl = ADAPTER.getInputEl();
        sendResponse({
          ok: true,
          targetConvKey: ADAPTER.getConversationKey(),
          targetUrl: location.href,
          seedConfirmed: hasHandoffTransferMarker(lastMessageByRole("user"), msg.transferId),
          // A ChatGPT Project-home navigation can report tab load complete before
          // the SPA mounts its composer. Background waits on this bounded signal
          // before it injects a handoff seed.
          composerReady: Boolean(inputEl && elementVisible(inputEl)),
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
      if (msg?.type === "h2w_queue_state") {
        if (ADAPTER.name === "chatgpt" && msg.convKey === ADAPTER.getConversationKey()) {
          queuedInsertCount = Math.max(0, Number(msg.status?.count) || 0);
          ensureQueuedInsertButton();
          updateQueuedInsertButton();
        }
        sendResponse({ ok: true });
        return;
      }
      if (msg?.type === "h2w_queue_deliver") {
        (async () => {
          if (ADAPTER.name !== "chatgpt") {
            sendResponse({ ok: false, error: "unsupported-site" });
            return;
          }
          const convKey = String(msg.convKey || "");
          const batchId = String(msg.batch_id || "");
          const text = String(msg.text || "").trim();
          if (!convKey || convKey !== ADAPTER.getConversationKey()) {
            sendResponse({ ok: false, blocked: "conversation-changed" });
            return;
          }
          if (!batchId || !text) {
            sendResponse({ ok: false, error: "queue-payload-invalid" });
            return;
          }
          if (queuedInsertBatchWasDelivered(convKey, batchId)) {
            sendResponse({ ok: true, deduped: true, queued_insert: true });
            return;
          }
          // The queued path never interrupts a live assistant turn and never
          // overwrites a draft the user started after queueing the message.
          if (isTurnInProgress()) {
            sendResponse({ ok: false, blocked: "turn-in-progress" });
            return;
          }
          if (ADAPTER.inputHasContent()) {
            sendResponse({ ok: false, blocked: "user-typing" });
            return;
          }
          await ensureConversationHealth();
          const result = await performWake({
            template: text,
            autoAllow: false,
            queueInsert: true,
          });
          if (result.ok) {
            rememberQueuedInsertBatch(convKey, batchId);
            if (CONVERSATION_HEALTH) {
              markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth));
            }
          }
          sendResponse({ ...result, queued_insert: true, batch_id: batchId });
        })();
        return true;
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
        if (ADAPTER.name === "chatgpt") void refreshQueuedInsertStatus();
        if (CONTEXT_PRESSURE) {
          contextPressureRecord = await loadContextPressure(convKey);
          void updateContextPressure().then((pressure) => {
            if (pressure && ADAPTER.name === "chatgpt") paintPageHud({ continuity: pressure });
          });
        }
      } else {
        conversationHealth = null;
        contextPressureRecord = null;
        if (ADAPTER.name === "chatgpt") {
          queuedInsertCount = 0;
          removeQueuedInsertButton();
        }
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
      if (ADAPTER.name === "chatgpt") ensureQueuedInsertButton();
    }, 1000);
    try {
      window.addEventListener("popstate", () => { void registerCurrentConversation("popstate"); });
      window.addEventListener("hashchange", () => { void registerCurrentConversation("hashchange"); });
      document.addEventListener("visibilitychange", () => {
        if (document.hidden) return;
        void registerCurrentConversation("visible");
        if (ADAPTER.name === "chatgpt") ensureQueuedInsertButton();
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

  function boundedHandoffTranscript(rows, maxChars = 70000) {
    const normalized = (rows || [])
      .map((row) => ({ role: String(row?.role || "").trim(), text: String(row?.text || "").trim() }))
      .filter((row) => ["user", "assistant"].includes(row.role) && row.text);
    const render = (items) => items.map((row) => `[${row.role}]\n${row.text}`).join("\n\n");
    const full = render(normalized);
    if (full.length <= maxChars) return full;

    const head = [];
    let headChars = 0;
    for (const row of normalized) {
      const next = `[${row.role}]\n${row.text}`;
      if (head.length >= 12 || headChars + next.length > 12000) break;
      head.push(row);
      headChars += next.length + 2;
    }
    const tail = [];
    let tailChars = 0;
    for (let i = normalized.length - 1; i >= head.length; i -= 1) {
      const row = normalized[i];
      const next = `[${row.role}]\n${row.text}`;
      if (tail.length >= 120 || tailChars + next.length > 54000) break;
      tail.unshift(row);
      tailChars += next.length + 2;
    }
    return `${render(head)}\n\n[... middle of conversation omitted by Herdr fallback ...]\n\n${render(tail)}`.slice(0, maxChars);
  }

  function chatGptConversationTranscript(body) {
    const mapping = body?.mapping || {};
    const rows = [];
    let nodeId = body?.current_node || null;
    for (let i = 0; nodeId && i < 240; i += 1) {
      const node = mapping?.[nodeId];
      const message = node?.message;
      const role = String(message?.author?.role || "");
      const text = serverMessageText(message);
      if ((role === "user" || role === "assistant") && text) rows.push({ role, text });
      nodeId = node?.parent || null;
    }
    rows.reverse();
    return boundedHandoffTranscript(rows);
  }

  function domConversationTranscript() {
    return boundedHandoffTranscript(observedConversationTurns());
  }

  function visibleConversationLimitSignal() {
    const candidates = [
      ...document.querySelectorAll('[role="alert"], [aria-live="assertive"], [data-testid*="error"], [data-testid*="limit"]'),
    ];
    const pattern = /maximum length for this conversation|conversation (?:has )?reached (?:its )?(?:maximum|limit)|conversation is too long|start (?:a )?new chat|continue in (?:a )?new chat|对话.{0,18}(?:达到|已达|超过).{0,18}(?:上限|最大)|(?:当前)?对话.{0,12}(?:过长|已满)|新建.{0,8}(?:聊天|对话)|会話.{0,12}(?:上限|長すぎ)/i;
    for (const el of candidates) {
      if (!el || el.offsetParent === null) continue;
      const text = normText(el.innerText || el.textContent || "");
      if (text && pattern.test(text)) return text.slice(0, 240);
    }
    return "";
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
      const transcript = chatGptConversationTranscript(body);
      const currentMessage = mapping?.[body?.current_node]?.message || null;
      const currentNodeRole = String(currentMessage?.author?.role || "");
      let nodeId = body?.current_node || null;
      let assistant = null;
      let assistantNodeId = null;
      for (let i = 0; nodeId && i < 40; i += 1) {
        const node = mapping?.[nodeId];
        const message = node?.message;
        if (message?.author?.role === "assistant") {
          assistant = message;
          assistantNodeId = nodeId;
          break;
        }
        nodeId = node?.parent || null;
      }
      let user = currentNodeRole === "user" ? currentMessage : null;
      if (!user) {
        nodeId = assistantNodeId ? mapping?.[assistantNodeId]?.parent || null : null;
        for (let i = 0; nodeId && i < 40; i += 1) {
          const node = mapping?.[nodeId];
          const message = node?.message;
          if (message?.author?.role === "user") { user = message; break; }
          nodeId = node?.parent || null;
        }
      }
      if (!assistant?.id) {
        return {
          ok: true,
          currentNodeRole,
          messageId: null,
          text: "",
          finished: null,
          userMessageId: user?.id ? String(user.id) : null,
          userText: serverMessageText(user),
          userCreatedAt: Number(user?.create_time || 0) > 0 ? Number(user.create_time) * 1000 : null,
          transcript,
        };
      }
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
        userMessageId: user?.id ? String(user.id) : null,
        userText: serverMessageText(user),
        userCreatedAt: Number(user?.create_time || 0) > 0 ? Number(user.create_time) * 1000 : null,
        transcript,
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

  function browserMemorySnapshot() {
    try {
      const memory = globalThis.performance?.memory;
      if (!memory) return {};
      return {
        usedJSHeapSize: Number(memory.usedJSHeapSize || 0) || 0,
        totalJSHeapSize: Number(memory.totalJSHeapSize || 0) || 0,
        jsHeapSizeLimit: Number(memory.jsHeapSizeLimit || 0) || 0,
      };
    } catch (_) {
      return {};
    }
  }

  function explicitChatGptRateLimitError() {
    if (ADAPTER.name !== "chatgpt") return false;
    const selectors = '[role="alert"], [class*="text-token-text-error"], [data-testid*="error"], [data-testid*="attachment"]';
    let nodes = [];
    try { nodes = [...document.querySelectorAll(selectors)].slice(-20); } catch (_) { return false; }
    return nodes.some((node) => {
      const text = String(node?.innerText || node?.textContent || "").replace(/\s+/g, " ").trim();
      return /(?:\b429\b|too many requests|rate[ -]?limit|请求过多|请求过于频繁|请求频率|速率限制)/i.test(text);
    });
  }

  function noteChatGptRateLimit(at = Date.now(), source = "resource") {
    if (!conversationHealth || !BROWSER_PERFORMANCE?.rateLimitBackoffMs) return false;
    const lastSeenAt = Number(conversationHealth.network_429_last_seen_at || 0);
    // A persistent error card can be sampled many times. Count one event per
    // ten seconds so a single 429 cannot instantly run the backoff to its cap.
    if (lastSeenAt && at - lastSeenAt < 10000) return false;
    const previousCount = lastSeenAt && at - lastSeenAt < 5 * 60 * 1000
      ? Number(conversationHealth.network_429_count || 0)
      : 0;
    const count = Math.min(8, previousCount + 1);
    const backoffMs = BROWSER_PERFORMANCE.rateLimitBackoffMs(count);
    markConversationState({
      ...conversationHealth,
      page_health_state: "rate_limited",
      page_health_checked_at: at,
      page_health_high_since: null,
      page_health_reason: "http_429_backoff",
      network_429_count: count,
      network_429_last_seen_at: at,
      network_429_source: source,
      network_backoff_until: at + backoffMs,
    });
    return true;
  }

  async function maybeRecoverPageHealth() {
    if (!automationEnabled || ADAPTER.name !== "chatgpt" || !conversationHealth
      || !BROWSER_PERFORMANCE?.classifyPageHealth || !RECOVERY_CONTROLLER) return false;
    const now = Date.now();
    const ui = uiPressure?.evaluate?.(now) || {};
    const resource429At = Number(ui.last_http_429_at || 0);
    if (resource429At > Number(conversationHealth.network_429_last_seen_at || 0)) {
      noteChatGptRateLimit(resource429At, "resource_timing");
    } else if (explicitChatGptRateLimitError()) {
      noteChatGptRateLimit(now, "visible_error");
    }
    if (Number(conversationHealth.network_backoff_until || 0) > now) {
      if (conversationHealth.page_health_state !== "rate_limited") {
        markConversationState({
          ...conversationHealth,
          page_health_state: "rate_limited",
          page_health_checked_at: now,
          page_health_high_since: null,
          page_health_reason: "http_429_backoff",
        });
      }
      // 429 never causes a retry/reload. Suppress the rest of this automatic
      // recovery cycle until the bounded backoff expires.
      return true;
    }

    const policy = BROWSER_PERFORMANCE.DEFAULT_PAGE_HEALTH_POLICY;
    if (!policy || now - lastPageHealthProbeAt < policy.minSampleWindowMs) return false;
    lastPageHealthProbeAt = now;

    const lastUserSubmitAt = Number(conversationHealth.last_user_submit_at || 0);
    const lastTurnEndAt = Number(conversationHealth.last_turn_end_at || 0);
    const lastAssistantProgressAt = Number(conversationHealth.last_assistant_progress_at || 0);
    const activeTurn = lastUserSubmitAt > 0 && lastTurnEndAt < lastUserSubmitAt;
    const lastActivityAt = Math.max(lastUserSubmitAt, lastTurnEndAt, lastAssistantProgressAt);
    const quiescentMs = Math.max(0, now - (lastActivityAt || pageHealthStartedAt));
    const stallBase = Math.max(lastAssistantProgressAt, lastUserSubmitAt);
    const stallMs = activeTurn && stallBase ? Math.max(0, now - stallBase) : 0;
    const uiReadyHigh = ui?.level === "high"
      && Number(ui.window_elapsed_ms || 0) >= policy.minSampleWindowMs;

    let serverSettled = false;
    if (activeTurn && stallMs >= policy.activeTurnStallMs && uiReadyHigh) {
      const server = await fetchChatGptConversationSnapshot();
      serverSettled = Boolean(server?.ok
        && server?.currentNodeRole === "assistant"
        && server?.finished === true);
    }

    const input = {
      ui,
      memory: browserMemorySnapshot(),
      activeTurn,
      serverSettled,
      stallMs,
      quiescentMs,
      highSince: Number(conversationHealth.page_health_high_since || 0),
      backoffUntil: Number(conversationHealth.network_backoff_until || 0),
      now,
    };
    let health = BROWSER_PERFORMANCE.classifyPageHealth(input, policy);
    let highSince = Number(conversationHealth.page_health_high_since || 0) || null;
    if (health.candidate && !highSince) {
      highSince = now;
      health = BROWSER_PERFORMANCE.classifyPageHealth({ ...input, highSince }, policy);
    } else if (!health.candidate) {
      highSince = null;
    }
    markConversationState({
      ...conversationHealth,
      page_health_state: health.state,
      page_health_checked_at: now,
      page_health_high_since: highSince,
      page_health_reason: health.reason || null,
    });
    if (health.action !== "reload") return false;

    const safety = recoverySafetySnapshot();
    // A server-settled assistant can leave a stale Stop button in the page.
    // Ignore that stale streaming bit only after the same-origin server probe
    // proves the assistant turn is already finished.
    const effectiveSafety = serverSettled ? { ...safety, streaming: false } : safety;
    const reloadReason = `page_health_${health.reason || "pressure"}`;
    if (RECOVERY_CONTROLLER.canReloadSafely(effectiveSafety)) {
      markConversationState({
        ...conversationHealth,
        reload_attempt: Number(conversationHealth.reload_attempt || 0) + 1,
        last_reload_at: now,
        reload_reason: reloadReason,
        page_health_state: "reload_pending",
        page_health_checked_at: now,
      });
      await reloadAfterPersistingConversationState();
      return true;
    }

    const lastReloadAt = Number(conversationHealth.last_reload_at || 0);
    const primaryReloadSpent = Number(conversationHealth.reload_attempt || 0)
      >= RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxReloadAttempts;
    // Level two is an escalation, never an alternative first action. If level
    // one was blocked for a live-safety reason, stay put rather than asking the
    // background worker to bypass that decision.
    if (!primaryReloadSpent || !lastReloadAt) return false;
    if (lastReloadAt && now - lastReloadAt < policy.backgroundEscalationDelayMs) return true;
    if (RECOVERY_CONTROLLER.canForceTabReloadSafely?.({
      ...effectiveSafety,
      backgroundReloadAttempts: Number(conversationHealth.page_health_background_reload_attempt || 0),
      lastBackgroundReloadAt: conversationHealth.page_health_last_background_reload_at || null,
      now,
    })) {
      markConversationState({
        ...conversationHealth,
        page_health_background_reload_attempt: Number(conversationHealth.page_health_background_reload_attempt || 0) + 1,
        page_health_last_background_reload_at: now,
        page_health_state: "background_reload_pending",
        page_health_checked_at: now,
        reload_reason: reloadReason,
      });
      // Consume the second-level reload budget durably before asking the MV3
      // service worker to reload this exact sender tab.
      if (!(await persistConversationHealth(conversationHealth))) return true;
      const forced = await sendBg({
        type: "h2w_force_tab_reload",
        convKey: ADAPTER.getConversationKey(),
        reason: reloadReason,
      });
      if (!forced?.ok) {
        markConversationState({
          ...conversationHealth,
          page_health_state: "background_reload_failed",
          page_health_checked_at: Date.now(),
        });
      }
      return true;
    }

    if (Number(conversationHealth.reload_attempt || 0) >= RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxReloadAttempts
      && Number(conversationHealth.page_health_background_reload_attempt || 0)
        >= RECOVERY_CONTROLLER.DEFAULT_RECOVERY_POLICY.maxBackgroundReloadAttempts) {
      let exhausted = {
        ...conversationHealth,
        page_health_state: "exhausted",
        page_health_checked_at: now,
      };
      if (CONVERSATION_HEALTH?.markRolloverRecommended) {
        exhausted = CONVERSATION_HEALTH.markRolloverRecommended(exhausted, now);
      }
      markConversationState(exhausted);
      paintPageHud({});
    }
    return false;
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
    await reloadAfterPersistingConversationState();
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
      await reloadAfterPersistingConversationState();
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
      // Persist the consumed Retry budget before invoking ChatGPT's native
      // action. A storage failure leaves the explicit error authoritative and
      // prevents a crash/reload from making a second Retry eligible.
      const retryStarted = await RECOVERY_CONTROLLER.runAfterDurablePersistence({
        persist: () => persistConversationHealth(conversationHealth),
        action: () => threadError.retry.click(),
        waitMs: 0,
      });
      if (!retryStarted) return true;
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
        await reloadAfterPersistingConversationState();
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
      await reloadAfterPersistingConversationState();
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
    await reloadAfterPersistingConversationState();
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
    await reloadAfterPersistingConversationState();
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
        if (await maybeRecoverPageHealth()) return;
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
    let lastReportedTurnKey = "";
    let lastAsstLen = initialAssistant.text.length;
    let lastAsstSignature = assistantSignature(initialAssistant.text);
    let lastAsstCount = initialAssistant.count;
    let stableRounds = 0;
    let observedSubmitAt = Number(conversationHealth?.last_user_submit_at || 0);
    let serverSettleInFlight = false;
    let lastServerSettleProbeAt = 0;
    let wasStopping = false;
    const serverSettleFallbackMs = Number(
      RECOVERY_CONTROLLER?.DEFAULT_RECOVERY_POLICY?.freshnessProbeIntervalMs || 15000,
    );

    const hasPendingReply = conversationHasPendingReply;

    const syncPendingSubmitAnchor = () => {
      const submitAt = Number(conversationHealth?.last_user_submit_at || 0);
      if (submitAt > observedSubmitAt) {
        observedSubmitAt = submitAt;
        startedAt = submitAt;
        userTextAtStart = "";
        lastReportedTurnKey = "";
      }
      return submitAt;
    };

    const pendingUserTextHint = () => {
      const submitAt = syncPendingSubmitAnchor();
      if (userTextAtStart) return normText(userTextAtStart);
      if (lastWakeNorm && submitAt > 0 && Math.abs(Number(lastWakeAt || 0) - submitAt) <= 10000) {
        return normText(lastWakeNorm);
      }
      return "";
    };

    const noteTrustedManualSubmit = () => {
      const text = composerNorm();
      if (!text || !CONVERSATION_HEALTH || !conversationHealth) return;
      const at = Date.now();
      markConversationState(CONVERSATION_HEALTH.markReplyWaiting(conversationHealth, at));
      observedSubmitAt = at;
      startedAt = at;
      userTextAtStart = text;
      lastReportedTurnKey = "";
    };

    document.addEventListener("click", (event) => {
      if (!event.isTrusted) return;
      const button = event.target?.closest?.("button, [role=button]");
      if (button && isSendButton(button)) noteTrustedManualSubmit();
    }, true);
    document.addEventListener("keydown", (event) => {
      if (!event.isTrusted || event.key !== "Enter" || event.shiftKey || event.isComposing) return;
      const input = ADAPTER.getInputEl();
      if (!input) return;
      const target = event.target;
      if (target !== input && !input.contains?.(target)) return;
      noteTrustedManualSubmit();
    }, true);

    const reportTurnEnded = (assistantText, endedAt, { userText = "", serverConfirmed = false } = {}) => {
      const normalizedAssistant = String(assistantText || "").trim();
      if (!hasPendingReply()) return false;
      const submitAt = syncPendingSubmitAnchor();
      if (!normalizedAssistant || !looksLikeSubstantiveReply(normalizedAssistant)) return false;
      const turnKey = `${submitAt || startedAt || 0}:${assistantSignature(normalizedAssistant)}`;
      if (turnKey === lastReportedTurnKey) return false;
      if (!serverConfirmed && endedAt - lastReportedEnd < 3000) return false;
      if (!serverConfirmed && isTurnInProgress()) return false;
      lastReportedEnd = endedAt;
      lastReportedTurnKey = turnKey;
      markObservedTurnEnded(endedAt);
      const effectiveUserText = normText(userText || pendingUserTextHint() || lastMessageByRole("user"));
      const payload = {
        type: "h2w_turn_ended",
        convKey: ADAPTER.getConversationKey(),
        startedAt: startedAt || submitAt,
        endedAt,
        userText: effectiveUserText,
        assistantText: normalizedAssistant,
      };
      console.log("[h2w] turn ended; updating continuity pressure and asking idle-nudge check");
      // Reuse the just-settled user/assistant turn instead of rescanning the
      // full conversation DOM; the full-history scan still runs on route change.
      void updateContextPressureFromSettledTurns(
        effectiveUserText,
        normalizedAssistant,
        serverConfirmed
          ? { userEl: null, assistantEl: null }
          : { userEl: latestTurnForRole("user"), assistantEl: latestTurnForRole("assistant") },
      ).then((pressure) => {
        if (pressure) paintPageHud({ continuity: pressure });
      });
      paintPageHud({ pending: true });
      sendBg(payload).then((r) => {
        console.log("[h2w] idle-nudge result:", r);
        hudPending = false;
        void refreshPageHud();
      });
      return true;
    };

    const serverSnapshotMatchesPendingTurn = (server) => {
      if (!hasPendingReply() || !server?.ok) return false;
      if (server.currentNodeRole !== "assistant" || server.finished !== true) return false;
      if (!String(server.text || "").trim()) return false;
      const submitAt = syncPendingSubmitAnchor();
      const expectedUser = pendingUserTextHint();
      const serverUser = normText(server.userText || "");
      if (expectedUser && serverUser) {
        const expectedPrefix = expectedUser.slice(0, 160);
        const serverPrefix = serverUser.slice(0, 160);
        if (!serverUser.includes(expectedPrefix) && !expectedUser.includes(serverPrefix)) return false;
      }
      const userCreatedAt = Number(server.userCreatedAt || 0);
      const assistantCreatedAt = Number(server.createdAt || server.updatedAt || 0);
      const clockSkewMs = 15000;
      if (userCreatedAt > 0 && userCreatedAt < submitAt - clockSkewMs) return false;
      if (!expectedUser && userCreatedAt <= 0 && assistantCreatedAt > 0
        && assistantCreatedAt < submitAt - clockSkewMs) return false;
      if (!expectedUser && !serverUser && userCreatedAt <= 0 && assistantCreatedAt <= 0) return false;
      return true;
    };

    const maybeReportServerSettledTurn = async (reason, { force = false } = {}) => {
      if (!automationEnabled || document.hidden || !hasPendingReply()) {
        return { checked: false, reported: false };
      }
      const now = Date.now();
      if (serverSettleInFlight) return { checked: false, reported: false };
      if (!force && now - lastServerSettleProbeAt < serverSettleFallbackMs) {
        return { checked: false, reported: false };
      }
      serverSettleInFlight = true;
      lastServerSettleProbeAt = now;
      try {
        const server = await fetchChatGptConversationSnapshot();
        if (!server?.ok) return { checked: false, reported: false };
        if (!serverSnapshotMatchesPendingTurn(server)) return { checked: true, reported: false };
        const reported = reportTurnEnded(server.text, Date.now(), {
          userText: server.userText || "",
          serverConfirmed: true,
        });
        if (reported) console.log(`[h2w] turn ended from ChatGPT server snapshot (${reason})`);
        return { checked: true, reported };
      } finally {
        serverSettleInFlight = false;
      }
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
      syncPendingSubmitAnchor();
      const stopping = isComposerGenerating();
      const stoppingEnded = wasStopping && !stopping;
      wasStopping = stopping;
      const currentUser = roleSnapshot("user");
      const currentUserText = currentUser.text;
      const currentUserSignature = assistantSignature(currentUserText);
      const currentUserCount = currentUser.count;
      const userChanged = currentUserText && (currentUserSignature !== lastUserSignature || currentUserCount > lastUserCount);
      if (userChanged) {
        // ChatGPT virtualizes old turns while scrolling. A mounted user node
        // changing is never sufficient evidence of a new submit, nor trusted
        // as the pending turn text. Real submits are captured at the composer
        // and extension-originated submits are already known through lastWake.
        lastUserSignature = currentUserSignature;
        lastUserCount = currentUserCount;
      }
      const currentAssistant = roleSnapshot("assistant");
      const assistantText = currentAssistant.text;
      const curLen = assistantText.length;
      const curSignature = assistantSignature(assistantText);
      const curCount = currentAssistant.count;
      const assistantChanged = curLen > 0 && (curSignature !== lastAsstSignature || curCount > lastAsstCount);
      const pendingReply = hasPendingReply();

      if (stoppingEnded && pendingReply) {
        void maybeReportServerSettledTurn("composer_stopped", { force: true });
      } else if (pendingReply && !stopping
        && Date.now() - lastServerSettleProbeAt >= serverSettleFallbackMs) {
        // This bounded fallback covers virtualized/off-viewport latest turns
        // even when ChatGPT's Stop button transition was missed.
        void maybeReportServerSettledTurn("pending_fallback");
      }

      if (stopping || (pendingReply && (assistantChanged || curLen > lastAsstLen))) {
        if (pendingReply && curLen > 0 && (assistantChanged || curLen > lastAsstLen)) markAssistantProgressIfActive();
        if (!generating) {
          generating = true;
          sawGrowth = assistantChanged || curLen > lastAsstLen || stopping;
          if (!startedAt) startedAt = Number(conversationHealth?.last_user_submit_at || Date.now());
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
          void maybeReportServerSettledTurn("dom_stable", { force: true });
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
        const resources = new PerformanceObserver((list) => {
          if (document.hidden) return;
          for (const entry of list.getEntries()) {
            if (Number(entry?.responseStatus || 0) === 429) {
              uiPressure.recordHttpStatus(429, Date.now());
            }
          }
        });
        resources.observe({ type: "resource", buffered: false });
      }
    } catch (e) {}
  }

  // ---- Compact in-page HUD ----
  // The HUD is intentionally a single high-frequency status/action bar.
  // Binding and workstation detail live only in the Side Panel; timing/model
  // configuration lives only in Options. Do not reintroduce a HUD drawer.
  const HUD_ID = "h2w-page-hud";
  let hudPending = false;
  let hudCache = null;
  let hudEls = null;
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

  function setHudActionBusy(busy) {
    hudActionBusy = Boolean(busy);
    if (!hudEls) return;
    hudEls.quick.disabled = hudActionBusy;
    syncHudManualButtons();
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

    const handoffStatus = String(hudCache?.handoff?.status || "");
    const transferBusy = ["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(handoffStatus)
      && hudCache?.handoff?.can_resume !== true;
    const handoffAvailable = hudCache?.manual_handoff_available === true;
    const handoffLocked = hudActionBusy
      || hudCache?.can_handoff !== true
      || Number(hudCache?.bound_working_count || 0) > 0
      || transferBusy;
    hudEls.handoff.hidden = !handoffAvailable;
    hudEls.handoff.classList.toggle("locked", handoffLocked);
    hudEls.handoff.disabled = handoffLocked;
    hudEls.handoff.setAttribute("aria-disabled", String(handoffLocked));
  }

  function showHudToast(text, kind = "") {
    // A toast must never be the path that creates an untranslated HUD shell.
    if (!hudLabelsReady(hudLabels)) return;
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

  function hudLabelsReady(labels) {
    const required = [
      "web_state", "scope_binding_count", "scope_binding_hint", "scope_unbound",
      "manual_continue", "manual_status", "manual_judge", "handoff", "handoff_hint",
      "handoff_resume", "handoff_working", "handoff_starting", "handoff_started", "handoff_fallback", "handoff_failed", "handoff_llm_required",
      "automation_on", "automation_off", "aria_toggle_automation",
    ];
    return Boolean(
      labels
      && required.every((key) => typeof labels[key] === "string" && labels[key].trim())
      && labels.states
      && typeof labels.states.idle === "string"
      && labels.states.idle.trim()
      && typeof labels.states.done === "string"
      && labels.states.done.trim()
    );
  }

  function clearUnreadyPageHud() {
    try { document.getElementById(HUD_ID)?.remove(); } catch (_) {}
    hudEls = null;
    try {
      const ta = document.querySelector("#prompt-textarea");
      const form = ta?.closest("form");
      form?.style?.removeProperty("padding-bottom");
    } catch (_) {}
    try { document.documentElement.style.removeProperty("padding-bottom"); } catch (_) {}
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

  async function manualHandoffAction() {
    if (hudActionBusy || hudCache?.manual_handoff_available !== true || hudCache?.can_handoff !== true) {
      return { ok: false, error: "handoff_unavailable" };
    }
    setHudActionBusy(true);
    showHudToast(hudText("handoff_starting"));
    try {
      const result = await sendBg({ type: "h2w_handoff_start", trigger: "manual" });
      if (result?.ok) {
        showHudToast(result?.fallback === true ? hudText("handoff_fallback") : hudText("handoff_started"), "ok");
      } else {
        const error = String(result?.error || "unknown");
        showHudToast(
          error === "handoff_fallback_llm_not_configured"
            ? hudText("handoff_llm_required")
            : hudText("handoff_failed", { error }),
          "err",
        );
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
          position: fixed; z-index: 2147483645; left: 50%; bottom: 7px; transform: translateX(-50%);
          max-width: calc(100vw - 20px); min-height: 28px; box-sizing: border-box;
          display: flex; align-items: center; gap: 6px; padding: 3px 5px 3px 9px;
          color: #252525; background: rgba(255,255,255,.96); border: 1px solid #dedede;
          border-radius: 10px; box-shadow: 0 4px 18px rgba(0,0,0,.11);
          font: 12px/1.2 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;
          backdrop-filter: blur(9px);
        }
        .bar.automation-on { color: #14532d; background: rgba(240,253,244,.97); border-color: #86d49f; }
        .summary { min-width: 0; display: flex; align-items: center; gap: 6px; white-space: nowrap; }
        .web-status, .status { font-weight: 650; }
        .scope-counts { color: #777; font-size: 10.5px; }
        .divider { color: #bbb; }
        button { font: inherit; color: inherit; }
        .quick, .manual {
          height: 22px; border: 1px solid #dedede; background: #fafafa; border-radius: 7px;
          cursor: pointer; padding: 0 7px; font-size: 11px; white-space: nowrap;
        }
        .quick.on { color: #166534; background: #f0fdf4; border-color: #bbf7d0; }
        .quick.off { color: #6b7280; background: #f5f5f5; }
        .manual.locked { opacity: .45; cursor: not-allowed; }
        .toast {
          position: fixed; z-index: 2147483646; left: 50%; bottom: 42px; transform: translateX(-50%);
          max-width: min(520px, calc(100vw - 24px)); padding: 6px 9px; border: 1px solid #d7d7d7;
          border-radius: 8px; background: rgba(255,255,255,.98); color: #333;
          box-shadow: 0 4px 16px rgba(0,0,0,.12); font: 11px/1.35 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;
        }
        .toast.ok { border-color: #86d49f; }
        .toast.err { border-color: #e49a9a; color: #991b1b; }
        @media (max-width: 820px) {
          .scope-counts { display: none; }
          .bar { max-width: calc(100vw - 10px); gap: 4px; }
          .quick, .manual { padding: 0 5px; font-size: 10px; }
        }
        @media (prefers-color-scheme: dark) {
          .bar { color: #eee; background: rgba(30,30,30,.96); border-color: #454545; box-shadow: 0 4px 18px rgba(0,0,0,.32); }
          .bar.automation-on { color: #b8efc8; background: rgba(29,64,43,.96); border-color: #477b59; }
          .scope-counts { color: #aaa; }
          .divider { color: #666; }
          .quick, .manual { background: #292929; border-color: #454545; }
          .quick.on { color: #bbf7d0; background: #1d5130; border-color: #5aa773; }
          .quick.off { color: #bbb; background: #292929; }
          .toast { color: #eee; background: rgba(30,30,30,.98); border-color: #555; }
          .toast.err { color: #fecaca; border-color: #7f1d1d; }
        }
      </style>
      <div class="toast" hidden></div>
      <div class="bar" part="bar">
        <div class="summary" aria-live="polite">
          <span class="web-status"></span><span class="divider">·</span>
          <span class="status"></span><span class="scope-counts"></span>
        </div>
        <button type="button" class="manual manual-continue"></button>
        <button type="button" class="manual manual-status"></button>
        <button type="button" class="manual manual-judge"></button>
        <button type="button" class="manual manual-handoff"></button>
        <button type="button" class="quick" aria-label=""></button>
      </div>
    `;
    hudEls = {
      host,
      bar: shadow.querySelector(".bar"),
      webStatus: shadow.querySelector(".web-status"),
      status: shadow.querySelector(".status"),
      scopeCounts: shadow.querySelector(".scope-counts"),
      quick: shadow.querySelector(".quick"),
      handoff: shadow.querySelector(".manual-handoff"),
      manualButtons: [
        shadow.querySelector(".manual-continue"),
        shadow.querySelector(".manual-status"),
        shadow.querySelector(".manual-judge"),
      ],
      toast: shadow.querySelector(".toast"),
    };
    hudEls.quick.addEventListener("click", () => {
      void setHudProjectAutomation(!(hudCache?.enabled === true));
    });
    hudEls.manualButtons.forEach((button, index) => {
      const actions = ["direct", "status", "judge"];
      button.addEventListener("click", () => {
        if (hudActionBusy || hudCache?.enabled === true) return;
        void manualContinueAction(actions[index]);
      });
    });
    hudEls.handoff.addEventListener("click", () => { void manualHandoffAction(); });
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

  function hudSiteLabel() {
    if (ADAPTER.name === "chatgpt") return "ChatGPT";
    if (ADAPTER.name === "claude") return "Claude";
    if (ADAPTER.name === "z.ai") return "z.ai";
    if (ADAPTER.name === "deepseek") return "DeepSeek";
    return ADAPTER.name || "Web";
  }

  function hudWebActivityLabel() {
    const healthState = String(conversationHealth?.state || "");
    let stateKey = conversationHasPendingReply() ? "reply_waiting" : "idle";
    if (["reply_suspect", "recovery_message_sent", "reload_pending", "recovering", "rollover_recommended", "rollover_required"].includes(healthState)) {
      stateKey = healthState;
    }
    const stateLabel = hudLabels?.states?.[stateKey] || stateKey;
    return hudText("web_state", { site: hudSiteLabel(), state: stateLabel }, `${hudSiteLabel()} ● ${stateLabel}`);
  }

  function paintPageHud(view = {}) {
    if (view.hud) hudCache = view.hud;
    if (view.continuity) hudCache = { ...(hudCache || {}), continuity: view.continuity };
    const hud = view.hud || hudCache || null;
    hudLabels = hud?.labels || hudLabels;
    // Queue is a separate conversation affordance and must not depend on HUD
    // localization becoming ready.
    updateQueuedInsertButton();
    // Never leave a half-initialized HUD in the page. In particular, MV3
    // worker cold start or an extension reload can briefly leave content code
    // without the localized label payload. Hide/remove the stale host until a
    // complete payload arrives instead of rendering empty action buttons.
    if (!hudLabelsReady(hudLabels)) {
      clearUnreadyPageHud();
      return;
    }
    const ui = ensurePageHud();
    liftComposer(32);
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
      workspace: null,
      agent: null,
      conversation: null,
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
    ui.webStatus.textContent = hudWebActivityLabel();
    const boundWorkspaceCount = Number(hud?.bound_workspace_count ?? hud?.binding_count ?? 0);
    ui.scopeCounts.textContent = hud?.bound
      ? hudText("scope_binding_count", { count: boundWorkspaceCount }, `🔗${boundWorkspaceCount}`)
      : hudText("scope_unbound");
    ui.scopeCounts.title = hud?.bound
      ? hudText("scope_binding_hint", { count: boundWorkspaceCount })
      : hudText("scope_unbound");
    ui.scopeCounts.setAttribute("aria-label", ui.scopeCounts.title || ui.scopeCounts.textContent || "");
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
    const handoffStatus = String(hud?.handoff?.status || "");
    const handoffBusy = ["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(handoffStatus)
      && hud?.handoff?.can_resume !== true;
    ui.handoff.textContent = handoffBusy
      ? hudText("handoff_working")
      : (hud?.handoff?.can_resume === true ? hudText("handoff_resume") : hudText("handoff"));
    ui.handoff.title = hudText("handoff_hint");
    syncHudManualButtons();
    ui.quick.hidden = hud?.project_automation_available !== true
      && hud?.conversation_automation_available !== true;
    ui.quick.textContent = preferenceEnabled ? hudText("automation_on", null, "Auto on") : hudText("automation_off", null, "Auto off");
    ui.quick.className = `quick ${preferenceEnabled ? "on" : "off"}`;
    ui.quick.setAttribute("aria-pressed", String(preferenceEnabled));
    ui.quick.setAttribute("aria-label", hudText("aria_toggle_automation"));
    const conversationAutomation = hud?.conversation_automation_available === true && !hud?.project_id;
    ui.quick.title = conversationAutomation
      ? (preferenceEnabled ? hudText("conversation_automation_on_hint") : hudText("conversation_automation_off_hint"))
      : (preferenceEnabled ? hudText("automation_on_hint") : hudText("automation_off_hint"));
    const visual = hudVisualClass(state);
    ui.bar.className = `bar${effectiveEnabled ? " automation-on" : ""}${visual ? ` ${visual}` : ""}`;
  }

  async function refreshPageHud() {
    if (!runtimeAlive()) return;
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    paintPageHud({ hud: hud && hud.ok ? hud : null });
  }

  function startPageHud() {
    startDocumentTitleSync();
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
