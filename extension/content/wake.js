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
const H2W_CONTENT_VERSION = "0.1.41";
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { console.warn("[h2w] no adapter; skipping"); return; }
  const SPEAKS = window.__H2W_SPEAKS_JSON__ || null;
  const CONVERSATION_HEALTH = globalThis.H2W_CONVERSATION_HEALTH || null;
  const RECOVERY_CONTROLLER = globalThis.H2W_RECOVERY_CONTROLLER || null;
  let conversationHealth = null;
  const HEALTH_STORAGE_KEY = "h2wConversationHealthByConv";
  const RECOVERY_PROBE_TEMPLATE = [
    "Herdr recovery check: the previous assistant turn did not visibly start.",
    "Do not call tools, do not repeat or continue any external action, and do not mutate anything.",
    "Only report whether the previous request appears to have completed in this conversation. If you cannot verify that, reply exactly: recovery needed.",
  ].join("\n");

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

  function markConversationState(record) {
    conversationHealth = record;
    if (record?.convKey) void persistConversationHealth(record);
    return record;
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
  let permDeadline = 0;
  let lastPermClickAt = 0;
  function permissionTryClick() {
    if (!runtimeAlive() || Date.now() > permDeadline) { permissionStop(); return; }
    const r = permClicker.tryClick(document);
    if (r.handled) {
      lastPermClickAt = Date.now();
      console.log(`[h2w] auto-clicked permission action "${(r.button.innerText || r.button.textContent || "?").trim()}"`);
    }
  }
  function permissionStop() {
    if (permObs) { try { permObs.disconnect(); } catch (e) {} permObs = null; }
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
    permObs = new MutationObserver(() => permissionTryClick());
    // childList catches late mounts; attributes catches buttons that later become enabled.
    try {
      permObs.observe(document.body, {
        childList: true, subtree: true,
        attributes: true, attributeFilter: ["disabled", "hidden", "aria-disabled", "aria-hidden", "style"],
      });
    } catch (e) {}
    if (!persistent) setTimeout(permissionStop, durationMs + 5000);
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
  function lastMessageByRole(role) {
    const nodes = [...document.querySelectorAll(`[data-message-author-role="${role}"]`)];
    const el = nodes[nodes.length - 1];
    return el ? String(el.innerText || "").trim() : "";
  }

  function hasHandoffTransferMarker(text, transferId) {
    const id = String(transferId || "").trim();
    return Boolean(id && String(text || "").includes(`[HERDR_CONTINUITY_TRANSFER id=${id}]`));
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
          const result = await performWake({
            template: msg.template || "",
            autoAllow: false,
            handoff: true,
          });
          sendResponse(result);
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
        if (ADAPTER.name === "chatgpt") void refreshPageHud();
        sendResponse({ ok: true });
        return;
      }
      if (msg?.type === "h2w_bound" || msg?.type === "h2w_unbound") {
        console.log(`[h2w] ${msg.type === "h2w_bound" ? "bound " + msg.pane : "unbound"}`);
        if (ADAPTER.name === "chatgpt") void refreshPageHud();
        return;
      }
      if (msg?.type === "h2w_wake") {
        (async () => {
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
      await ensureConversationHealth(convKey);
      if (changed) {
        console.log(`[h2w] conversation route changed (${reason}): ${convKey}`);
        if (ADAPTER.name === "chatgpt") void refreshPageHud();
      }
    }
    return response;
  }

  function startConversationRouteWatch() {
    // One second is fast enough for UI binding while keeping route detection
    // negligible compared with the existing 5s HUD reconciliation interval.
    setInterval(() => {
      const convKey = ADAPTER.getConversationKey();
      if (convKey && convKey !== registeredConvKey) void registerCurrentConversation("poll");
    }, 1000);
    try {
      window.addEventListener("popstate", () => { void registerCurrentConversation("popstate"); });
      window.addEventListener("hashchange", () => { void registerCurrentConversation("hashchange"); });
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
    if (!RECOVERY_CONTROLLER.shouldSendRecovery(conversationHealth)) return false;
    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.streaming || safety.toolRunning || safety.permissionCardActive) return false;
    const result = await performWake({ template: RECOVERY_PROBE_TEMPLATE, autoAllow: false, recovery: true });
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

  async function maybeEscalateRecoveryRollover() {
    if (!conversationHealth || !RECOVERY_CONTROLLER || !CONVERSATION_HEALTH) return false;
    if (conversationHealth.state !== CONVERSATION_HEALTH.CONVERSATION_STATES.RECOVERING) return false;
    if (Date.now() - Number(conversationHealth.last_reload_at || 0) < 10000) return false;

    const assistantText = lastMessageByRole("assistant");
    const currentSignature = assistantSignature(assistantText);
    if (assistantText && looksLikeSubstantiveReply(assistantText)
      && currentSignature !== conversationHealth.assistant_signature_before_reload) {
      markConversationState(CONVERSATION_HEALTH.markTurnEnded(conversationHealth));
      paintPageHud({});
      return true;
    }
    if (!RECOVERY_CONTROLLER.recommendRollover(conversationHealth)) return false;

    const safety = recoverySafetySnapshot();
    if (safety.composerBusy || safety.streaming || safety.toolRunning || safety.permissionCardActive) return false;
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    if (!hud?.ok || !hud?.can_handoff) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({ hud: hud?.ok ? hud : null });
      return false;
    }

    markConversationState(CONVERSATION_HEALTH.markRolloverRequired(conversationHealth));
    paintPageHud({ hud });
    const result = await sendBg({ type: "h2w_handoff_start" });
    if (!result?.ok) {
      markConversationState(CONVERSATION_HEALTH.markRolloverRecommended(conversationHealth));
      paintPageHud({ hud });
      return false;
    }
    return true;
  }

  async function reconcileConversationHealthAfterLoad() {
    const record = await ensureConversationHealth();
    if (!record || !RECOVERY_CONTROLLER) return;
    const suspect = RECOVERY_CONTROLLER.classifyReplyTimeout(record);
    if (suspect !== record) markConversationState(suspect);
    await maybeEscalateRecoveryRollover();
  }

  function startConversationHealthWatch() {
    let healthCheckInFlight = false;
    setInterval(() => {
      if (healthCheckInFlight || !conversationHealth || !RECOVERY_CONTROLLER) return;
      healthCheckInFlight = true;
      void (async () => {
        const next = RECOVERY_CONTROLLER.classifyReplyTimeout(conversationHealth);
        if (next !== conversationHealth) markConversationState(next);
        if (await maybeSendRecoveryProbe()) return;
        if (await maybeReloadForRecovery()) return;
        await maybeEscalateRecoveryRollover();
      })().finally(() => { healthCheckInFlight = false; });
    }, 5000);
  }

  (async () => {
    if (!runtimeAlive()) return;
    try { chrome.runtime.sendMessage({ type: "h2w_hello", version: H2W_CONTENT_VERSION }); } catch (e) {}
    await registerCurrentConversation("startup");
    await reconcileConversationHealthAfterLoad();
    startConversationRouteWatch();
    // ChatGPT Connector permission cards can appear outside wake-up, so watch continuously.
    if (ADAPTER.name === "chatgpt" && PERM) startPermissionWatch(Number.POSITIVE_INFINITY);
    // Talk-without-tools: watch turn boundaries and ask background to check MCP activity.
    if (ADAPTER.name === "chatgpt") {
      startPageHud();
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
    let settleTimer = null;
    let lastReportedEnd = 0;
    let lastAsstLen = 0;
    let stableRounds = 0;

    const reportTurnEnded = (assistantText, endedAt) => {
      if (endedAt - lastReportedEnd < 3000) return;
      if (isTurnInProgress()) return;
      if (!looksLikeSubstantiveReply(assistantText)) return;
      if (!String(assistantText || "").trim()) return;
      lastReportedEnd = endedAt;
      const payload = {
        type: "h2w_turn_ended",
        convKey: ADAPTER.getConversationKey(),
        startedAt,
        endedAt,
        userText: userTextAtStart || lastMessageByRole("user"),
        assistantText,
      };
      console.log("[h2w] turn ended; asking idle-nudge check");
      paintPageHud({ pending: true });
      sendBg(payload).then((r) => {
        console.log("[h2w] idle-nudge result:", r);
        hudPending = false;
        void refreshPageHud();
      });
    };

    const onTick = () => {
      const stopping = isComposerGenerating();
      const assistantText = lastMessageByRole("assistant");
      const curLen = assistantText.length;

      if (stopping || curLen > lastAsstLen) {
        if (!generating) {
          generating = true;
          sawGrowth = curLen > lastAsstLen || stopping;
          startedAt = Date.now();
          userTextAtStart = lastMessageByRole("user");
          if (settleTimer) { clearInterval(settleTimer); settleTimer = null; }
          stableRounds = 0;
          console.log("[h2w] turn start (streaming/stop/growth)");
        }
        if (curLen > lastAsstLen) sawGrowth = true;
        stableRounds = 0;
        lastAsstLen = curLen;
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
          const cur = lastMessageByRole("assistant");
          if (cur.length === lastAsstLen) stableRounds += 1;
          else { lastAsstLen = cur.length; stableRounds = 0; }
          if (stableRounds < 2) return;
          clearInterval(settleTimer);
          settleTimer = null;
          reportTurnEnded(cur, endedAt);
        }, 800);
      }
      lastAsstLen = curLen;
    };

    setInterval(onTick, 800);
    try {
      const mo = new MutationObserver(() => onTick());
      mo.observe(document.documentElement, { childList: true, subtree: true, characterData: true });
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
    hudEls.master.disabled = hudActionBusy;
    hudEls.tick.disabled = hudActionBusy;
    hudEls.fallback.disabled = hudActionBusy;
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

  async function setHudMasterEnabled(enabled) {
    if (hudActionBusy) return;
    setHudActionBusy(true);
    const on = Boolean(enabled);
    const result = await sendBg({
      type: "h2w_set_config",
      config: { enabled: on, idleNudgeEnabled: on },
    });
    if (result?.ok) {
      hudCache = { ...(hudCache || {}), enabled: on, idleNudgeEnabled: on };
      paintPageHud({ hud: hudCache });
      showHudToast(on ? "Wake + nudge enabled" : "Wake + nudge paused", "ok");
    } else {
      showHudToast("Could not update wake state", "err");
    }
    setHudActionBusy(false);
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
      showHudToast("Timing saved", "ok");
    } else {
      showHudToast("Could not save timing", "err");
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
      showHudToast(shouldBind ? `Bound ${hudWorkspaceTitle(workspace)}` : `Unbound ${hudWorkspaceTitle(workspace)}`, "ok");
      await refreshPageHud();
    } else {
      showHudToast(`Binding failed: ${result?.error || "unknown"}`, "err");
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
        ? `Workspaces unavailable: ${hudCache.workspace_error}`
        : "No Herdr workspaces discovered";
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
        meta.textContent = active > 0 ? `${state} · ${active} active` : state;
      } else {
        meta.textContent = roots.length ? String(roots[0]) : "Available";
      }
      copy.append(title, meta);
      const action = document.createElement("button");
      action.type = "button";
      action.className = `ws-action${bound ? " danger" : ""}`;
      action.textContent = bound ? "Unbind" : "Bind";
      action.disabled = hudActionBusy;
      action.addEventListener("click", () => { void mutateHudBinding(workspace, !bound); });
      row.append(copy, action);
      list.appendChild(row);
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
        }
        button, input { font: inherit; }
        button { color: inherit; }
        .summary {
          min-width: 0; flex: 1; height: 24px; padding: 0; border: 0; background: transparent;
          display: flex; align-items: center; gap: 7px; cursor: pointer; text-align: left;
        }
        .status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
        .workspace { color: #8a8a8a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; }
        .quick, .expand {
          height: 22px; border: 1px solid #dedede; background: #fafafa; border-radius: 7px;
          cursor: pointer; padding: 0 7px; font-size: 11px; white-space: nowrap;
        }
        .quick.on { color: #166534; background: #f0fdf4; border-color: #bbf7d0; }
        .quick.off { color: #6b7280; background: #f5f5f5; }
        .expand { width: 24px; padding: 0; font-size: 13px; }
        button:hover { filter: brightness(.97); }
        button:disabled, input:disabled { opacity: .55; cursor: wait; }
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
        .control-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
        .control-title { font-weight: 650; }
        .control-hint { color: #999; font-size: 10px; margin-top: 1px; }
        .master {
          min-width: 52px; height: 24px; border: 1px solid #ddd; border-radius: 999px;
          background: #f5f5f5; cursor: pointer; padding: 0 8px; font-weight: 650; font-size: 10px;
        }
        .master.on { color: #166534; background: #ecfdf3; border-color: #bbf7d0; }
        .timing { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-top: 9px; }
        .timing label { display: grid; gap: 3px; color: #777; font-size: 10px; }
        .timing input { width: 100%; box-sizing: border-box; height: 27px; border: 1px solid #ddd; border-radius: 7px; padding: 2px 7px; color: #333; background: #fff; }
        .section-title { font-size: 10px; color: #858585; text-transform: uppercase; letter-spacing: .04em; margin-bottom: 5px; }
        .workspaces { display: grid; gap: 3px; }
        .ws-row { display: flex; align-items: center; gap: 8px; padding: 6px 7px; border-radius: 8px; }
        .ws-row:hover { background: #f7f7f7; }
        .ws-row.bound { background: #f4fbf6; }
        .ws-copy { flex: 1; min-width: 0; }
        .ws-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .ws-meta { margin-top: 1px; color: #9b9b9b; font-size: 9px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .ws-action { border: 1px solid #d5d5d5; background: #fff; border-radius: 7px; padding: 3px 7px; cursor: pointer; font-size: 10px; }
        .ws-action.danger { color: #b42318; }
        .empty { color: #999; padding: 7px 3px; }
        .toast { margin: 0 11px 9px; padding: 6px 8px; border-radius: 7px; background: #f5f5f5; color: #555; font-size: 10px; }
        .toast.ok { background: #f0fdf4; color: #166534; }
        .toast.err { background: #fef2f2; color: #b42318; }
        @media (prefers-color-scheme: dark) {
          .bar { color: #ddd; background: rgba(32,32,32,.96); border-color: #3a3a3a; }
          .workspace { color: #8d8d8d; }
          .quick, .expand { background: #292929; border-color: #454545; }
          .quick.on { color: #86efac; background: #143020; border-color: #245c36; }
          .panel { color: #e8e8e8; background: rgba(32,32,32,.985); border-color: #494949; }
          .section { border-color: #414141; }
          .timing input { color: #eee; background: #282828; border-color: #4a4a4a; }
          .master { background: #292929; border-color: #454545; }
          .master.on { color: #86efac; background: #143020; border-color: #245c36; }
          .ws-row:hover { background: #292929; }
          .ws-row.bound { background: #173020; }
          .ws-action { color: #ddd; background: #272727; border-color: #4a4a4a; }
        }
      </style>
      <div class="panel" part="panel" hidden>
        <div class="panel-head">
          <div><div class="panel-title">Herdr controls</div><div class="conversation"></div></div>
          <button type="button" class="options">Advanced options ↗</button>
        </div>
        <div class="section">
          <div class="control-row">
            <div><div class="control-title">Wake + small-model nudge</div><div class="control-hint">One switch controls both behaviors</div></div>
            <button type="button" class="master">On</button>
          </div>
          <div class="timing">
            <label>Interval (sec)<input class="tick" type="number" min="0" step="1"></label>
            <label>Fallback (sec)<input class="fallback" type="number" min="0" step="1"></label>
          </div>
        </div>
        <div class="section"><div class="section-title">Conversation bindings</div><div class="workspaces"></div></div>
        <div class="toast" hidden></div>
      </div>
      <div class="bar" part="bar">
        <button type="button" class="summary"><span class="status"></span><span class="workspace"></span></button>
        <button type="button" class="quick" aria-label="Toggle Herdr wake and nudge">Wake on</button>
        <button type="button" class="expand" aria-label="Open Herdr controls" aria-expanded="false">⌃</button>
      </div>
    `;
    hudEls = {
      host,
      bar: shadow.querySelector(".bar"),
      status: shadow.querySelector(".status"),
      workspace: shadow.querySelector(".workspace"),
      summary: shadow.querySelector(".summary"),
      quick: shadow.querySelector(".quick"),
      expand: shadow.querySelector(".expand"),
      panel: shadow.querySelector(".panel"),
      conversation: shadow.querySelector(".conversation"),
      options: shadow.querySelector(".options"),
      master: shadow.querySelector(".master"),
      tick: shadow.querySelector(".tick"),
      fallback: shadow.querySelector(".fallback"),
      workspaces: shadow.querySelector(".workspaces"),
      toast: shadow.querySelector(".toast"),
    };
    hudEls.summary.addEventListener("click", () => setHudExpanded(!hudExpanded));
    hudEls.expand.addEventListener("click", () => setHudExpanded(!hudExpanded));
    hudEls.quick.addEventListener("click", () => { void setHudMasterEnabled(!(hudCache?.enabled !== false)); });
    hudEls.master.addEventListener("click", () => { void setHudMasterEnabled(!(hudCache?.enabled !== false)); });
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
    const hud = view.hud || hudCache || null;
    if (view.pending === true) hudPending = true;
    if (view.pending === false) hudPending = false;

    // The bar reports the bound Herdr workspace runtime state independently
    // from whether automatic wake/nudge is enabled. Recovery remains visible
    // in the tooltip through the separate recovery field below.
    const state = hudBoundRuntimeState(hud);
    const lastEvent = hud?.last?.reason
      ? `${hud.last.reason}${hud.last.at ? ` @ ${new Date(hud.last.at).toLocaleTimeString()}` : ""}`
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
      globalThis.H2W_HUD.updateReadonlyHud(ui.status, input);
    } else {
      ui.status.textContent = `Herdr ● ${state}`;
    }
    const enabled = hud?.enabled !== false;
    ui.workspace.textContent = hud?.workspace_label || (hud?.bound ? `${hud.binding_count || 1} bound` : "not bound");
    ui.quick.textContent = enabled ? "Wake on" : "Wake off";
    ui.quick.className = `quick ${enabled ? "on" : "off"}`;
    ui.quick.setAttribute("aria-pressed", String(enabled));
    ui.master.textContent = enabled ? "On" : "Off";
    ui.master.className = `master ${enabled ? "on" : "off"}`;
    ui.master.setAttribute("aria-pressed", String(enabled));
    ui.conversation.textContent = ADAPTER.getConversationKey();
    if (document.activeElement !== ui.tick && shadowActiveElement(ui.host) !== ui.tick) {
      ui.tick.value = String(hud?.progressTickSec ?? 60);
    }
    if (document.activeElement !== ui.fallback && shadowActiveElement(ui.host) !== ui.fallback) {
      ui.fallback.value = String(hud?.progressFallbackSec ?? 1200);
    }
    renderHudWorkspaceBindings();
    const visual = hudVisualClass(state);
    ui.bar.className = `bar${visual ? ` ${visual}` : ""}`;
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
    paintPageHud({ pending: false });
    void refreshPageHud();
    setInterval(() => { void refreshPageHud(); }, 5000);
  }

})();
