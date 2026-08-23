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
const H2W_CONTENT_VERSION = "0.1.38";
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { console.warn("[h2w] no adapter; skipping"); return; }
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
      if (msg?.type === "h2w_bound" || msg?.type === "h2w_unbound") {
        console.log(`[h2w] ${msg.type === "h2w_bound" ? "bound " + msg.pane : "unbound"}`);
        if (ADAPTER.name === "chatgpt") void refreshPageHud();
        return;
      }
      if (msg?.type === "h2w_wake") {
        (async () => {
          const result = await performWake(msg.data || {});
          const confirm = result.ok ? await confirmReplyStarted() : { monitored: false };
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

  (async () => {
    if (!runtimeAlive()) return;
    try { chrome.runtime.sendMessage({ type: "h2w_hello", version: H2W_CONTENT_VERSION }); } catch (e) {}
    await registerCurrentConversation("startup");
    startConversationRouteWatch();
    // ChatGPT Connector permission cards can appear outside wake-up, so watch continuously.
    if (ADAPTER.name === "chatgpt" && PERM) startPermissionWatch(Number.POSITIVE_INFINITY);
    // Talk-without-tools: watch turn boundaries and ask background to check MCP activity.
    if (ADAPTER.name === "chatgpt") {
      startPageHud();
      startIdleNudgeWatch();
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

  // ---- In-page status bar (ChatGPT): config + last LLM judge, always on ----
  const HUD_ID = "h2w-page-hud";
  const HUD_LOCALES = ["en", "zh", "ja"];
  let hudPending = false;
  let hudCache = null;
  let hudEls = null;
  let hudCat = {};
  let hudLocale = "en";

  function hudT(key, vars) {
    let s = hudCat[key];
    if (s == null) s = key;
    if (vars && typeof s === "string") {
      for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
    }
    return s;
  }

  function hudReasonLabel(reason) {
    const raw = String(reason || "").trim();
    if (!raw) return "?";
    const key = `hud_reason_${raw.replace(/[^a-z0-9_]/gi, "_")}`;
    const mapped = hudCat[key];
    return mapped != null ? mapped : raw;
  }

  async function loadHudLocale() {
    let code = "en";
    try {
      const stored = await chrome.storage.local.get(["uiLocale", "uiLocaleInitialized"]);
      if (stored.uiLocale && HUD_LOCALES.includes(stored.uiLocale)) code = stored.uiLocale;
      else if (!stored.uiLocaleInitialized) {
        const raw = (chrome.i18n?.getUILanguage?.() || navigator.language || "en").toLowerCase();
        if (raw.startsWith("zh")) code = "zh";
        else if (raw.startsWith("ja")) code = "ja";
      }
    } catch (_) { /* keep en */ }
    try {
      const resp = await fetch(chrome.runtime.getURL(`locales/${code}.json`));
      hudCat = await resp.json();
      hudLocale = code;
    } catch (_) {
      try {
        const resp = await fetch(chrome.runtime.getURL("locales/en.json"));
        hudCat = await resp.json();
        hudLocale = "en";
      } catch (e2) { hudCat = {}; }
    }
  }

  function clipHud(s, n = 48) {
    const t = String(s || "").replace(/\s+/g, " ").trim();
    if (t.length <= n) return t;
    return `${t.slice(0, n)}…`;
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
          display: flex; align-items: center; gap: 12px;
          min-height: 32px; padding: 4px 12px;
          box-sizing: border-box;
          font: 12px/1.4 ui-sans-serif, system-ui, -apple-system, sans-serif;
          color: #171717; background: #ffffff;
          border-top: 1px solid #eaeaea;
        }
        .cfg { color: #4d4d4d; flex: 1 1 38%; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .ws-controls { display: flex; align-items: center; gap: 5px; flex: 0 0 auto; min-width: 0; }
        .ws { max-width: 240px; height: 24px; padding: 0 5px; border: 1px solid #d4d4d4; border-radius: 5px; background: #fff; color: #171717; font: inherit; }
        .ws-action { height: 24px; padding: 0 8px; border: 1px solid #d4d4d4; border-radius: 5px; background: #f7f7f7; color: #171717; font: inherit; cursor: pointer; }
        .ws-action:hover:not(:disabled) { background: #ededed; }
        .ws-action:disabled, .ws:disabled { opacity: .55; cursor: default; }
        .last { color: #171717; flex: 1 1 38%; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: right; }
        .bar.pending .last { color: #aa4d00; }
        .bar.ok .last { color: #107d32; }
        .bar.err .last { color: #d8001b; }
        .bar.off .cfg { color: #8f8f8f; }
      </style>
      <div class="bar" part="bar">
        <div class="cfg"></div>
        <div class="ws-controls">
          <select class="ws" aria-label="workspace"></select>
          <button class="ws-action" type="button"></button>
        </div>
        <div class="last"></div>
      </div>
    `;
    hudEls = {
      host,
      bar: shadow.querySelector(".bar"),
      cfg: shadow.querySelector(".cfg"),
      ws: shadow.querySelector(".ws"),
      wsAction: shadow.querySelector(".ws-action"),
      last: shadow.querySelector(".last"),
    };
    hudEls.ws.addEventListener("change", () => paintWorkspaceControls(hudCache));
    hudEls.wsAction.addEventListener("click", () => { void toggleHudWorkspaceBinding(); });
    return hudEls;
  }

  function hudWorkspaceTitle(w) {
    const id = String(w?.id || "").trim();
    const label = String(w?.label || "").trim();
    if (label && id) return `${label} (${id})`;
    if (label) return label;
    const roots = Array.isArray(w?.roots) ? w.roots : [];
    const root = roots[0] ? String(roots[0]).replace(/\/+$/, "").split("/").pop() : "";
    return root && id ? `${root} (${id})` : (id || root || "?");
  }

  function hudWorkspaceError(error) {
    const raw = String(error || "").trim();
    if (!raw) return "";
    if (raw.startsWith("loopback_permission_")) return hudT("loopback_permission_short");
    if (raw === "fetch_timeout") return hudT("loopback_timeout_short");
    return raw;
  }

  function paintWorkspaceControls(hud) {
    const ui = ensurePageHud();
    const select = ui.ws;
    const action = ui.wsAction;
    if (!select || !action) return;
    const workspaces = Array.isArray(hud?.workspaces) ? hud.workspaces.filter((w) => w?.id) : [];
    const bound = new Set(Array.isArray(hud?.bound_workspace_ids) ? hud.bound_workspace_ids : []);
    const prior = select.value;
    const preferred = prior && workspaces.some((w) => w.id === prior)
      ? prior
      : (workspaces.find((w) => bound.has(w.id))?.id || workspaces[0]?.id || "");

    select.textContent = "";
    if (!workspaces.length) {
      const opt = document.createElement("option");
      opt.value = "";
      opt.textContent = hud?.workspace_error
        ? `${hudT("no_workspaces")} · ${clipHud(hudWorkspaceError(hud.workspace_error), 40)}`
        : hudT("no_workspaces");
      select.appendChild(opt);
      select.disabled = true;
      action.disabled = true;
      action.textContent = hudT("bind_action");
      return;
    }

    for (const w of workspaces) {
      const opt = document.createElement("option");
      opt.value = w.id;
      opt.textContent = `${hudWorkspaceTitle(w)}${bound.has(w.id) ? ` · ${hudT("bound")}` : ""}`;
      select.appendChild(opt);
    }
    select.disabled = false;
    select.value = preferred;
    action.disabled = !select.value;
    action.textContent = bound.has(select.value) ? hudT("unbind") : hudT("bind_action");
  }

  async function toggleHudWorkspaceBinding() {
    const ui = ensurePageHud();
    const wsId = String(ui.ws?.value || "").trim();
    if (!wsId || !runtimeAlive()) return;
    const hud = hudCache || {};
    const workspaces = Array.isArray(hud.workspaces) ? hud.workspaces : [];
    const meta = workspaces.find((w) => w?.id === wsId) || { id: wsId };
    const bound = new Set(Array.isArray(hud.bound_workspace_ids) ? hud.bound_workspace_ids : []);
    ui.ws.disabled = true;
    ui.wsAction.disabled = true;
    let result = null;
    if (bound.has(wsId)) {
      result = await sendBg({ type: "h2w_unbind", convKey: ADAPTER.getConversationKey(), workspace_id: wsId });
    } else {
      result = await sendBg({
        type: "h2w_bind",
        workspace_id: wsId,
        workspace_label: hudWorkspaceTitle(meta),
        workspace_label_raw: meta?.label || null,
        roots: Array.isArray(meta?.roots) ? meta.roots : [],
      });
    }
    if (!result?.ok && result?.error !== "already-bound") {
      paintPageHud({
        hud: { ...hud, last: { at: Date.now(), reason: "workspace_action_failed", error: result?.error || "binding failed" } },
      });
    }
    await refreshPageHud();
  }

  function liftComposer(px) {
    try {
      const ta = document.querySelector("#prompt-textarea");
      const form = ta?.closest("form");
      if (form) form.style.paddingBottom = `${px}px`;
    } catch (_) { /* ignore */ }
    try { document.documentElement.style.paddingBottom = `${px}px`; } catch (_) { /* ignore */ }
  }

  function paintPageHud(view) {
    const ui = ensurePageHud();
    liftComposer(36);
    if (view.hud) hudCache = view.hud;
    const hud = view.hud || hudCache;
    if (view.pending === true) hudPending = true;
    if (view.pending === false) hudPending = false;
    const pending = hudPending;

    const parts = [`v${(hud && hud.version) || H2W_CONTENT_VERSION}`];
    if (hud) {
      parts.push(hud.enabled === false ? hudT("hud_wake_off") : hudT("hud_wake_on"));
      if (hud.bound) parts.push(hudT("hud_bound", { name: hud.workspace_label || hud.workspace_id || "?" }));
      else parts.push(hudT("hud_unbound"));
      if (hud.llmConfigured) {
        const model = hud.llmModel || "on";
        parts.push(hudT("hud_llm", { model: hud.llmHost ? `${model} @ ${hud.llmHost}` : model }));
      } else {
        parts.push(hudT("hud_llm_off"));
      }
      parts.push(hud.idleNudgeEnabled === false ? hudT("hud_nudge_off") : hudT("hud_nudge_on"));
      parts.push(hudT("hud_cooldown", { sec: String(hud.progressTickSec || 0) }));
    } else {
      parts.push(hudT("hud_loading"));
    }
    ui.cfg.textContent = parts.join(" · ");
    ui.cfg.setAttribute("lang", hudLocale);
    paintWorkspaceControls(hud);

    let lastText = hudT("hud_last_none");
    let kind = hud && hud.bound && hud.llmConfigured ? "" : "off";
    if (pending) {
      lastText = hudT("hud_last_pending");
      kind = "pending";
    } else if (hud?.last) {
      const ago = Math.max(0, Math.round((Date.now() - (hud.last.at || 0)) / 1000));
      const bits = [hudT("hud_last", { reason: hudReasonLabel(hud.last.reason), ago: String(ago) })];
      if (hud.last.raw) bits.push(hudT("hud_raw", { text: clipHud(hud.last.raw) }));
      if (hud.last.send) bits.push(hudT("hud_send", { text: clipHud(hud.last.send) }));
      if (hud.last.error) bits.push(clipHud(hud.last.error));
      lastText = bits.join(" · ");
      kind = hud.last.nudged ? "ok" : "";
      if (hud.last.reason === "llm_done") kind = "";
      else if (String(hud.last.reason || "").includes("timeout") || String(hud.last.reason || "").startsWith("llm_http") || hud.last.reason === "unbound") kind = "err";
    }
    ui.last.textContent = lastText;
    ui.last.setAttribute("lang", hudLocale);
    ui.bar.className = `bar${kind ? " " + kind : ""}`;
    ui.bar.setAttribute("lang", hudLocale);
  }

  async function refreshPageHud() {
    if (!runtimeAlive()) return;
    const hud = await sendBg({ type: "h2w_page_hud", convKey: ADAPTER.getConversationKey() });
    paintPageHud({ hud: hud && hud.ok ? hud : null });
  }

  function startPageHud() {
    void (async () => {
      await loadHudLocale();
      paintPageHud({ pending: false });
      void refreshPageHud();
    })();
    setInterval(() => { void refreshPageHud(); }, 5000);
    try {
      chrome.storage.onChanged.addListener((changes, area) => {
        if (area !== "local" || !changes.uiLocale) return;
        void (async () => {
          await loadHudLocale();
          paintPageHud({});
        })();
      });
    } catch (_) { /* ignore */ }
  }
})();
