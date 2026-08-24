// json-bridge.js — z.ai / DeepSeek tool bridge without an MCP Connector.
// It intercepts a textarea submit, injects a compact Herdr tool protocol, parses
// assistant JSON tool calls, executes them through the background service worker,
// and feeds TOOL_RESULT messages back until the model returns a normal answer.
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  const SPEAKS = window.__H2W_SPEAKS_JSON__;
  const CORE = globalThis.H2W_JSON_BRIDGE_CORE;
  if (!ADAPTER || !SPEAKS?.enabled || !CORE || !["z.ai", "deepseek"].includes(ADAPTER.name)) {
    window.__H2W_JSON_BRIDGE__ = null;
    return;
  }

  const ROUND_YIELD_INTERVAL = 12;
  const MAX_PARALLEL = 4;
  const REPLY_TIMEOUT_MS = 120000;
  const STABLE_TICKS = 4;
  const TICK_MS = 500;

  let catalog = null;
  let catalogPromise = null;
  let catalogFailedAt = 0;
  let running = false;
  let selfSendingUntil = 0;
  let currentTaskSeq = 0;
  let hookTimer = null;
  let foldScheduled = false;
  let resumeScheduled = false;
  let lastResumeFingerprint = "";

  function runtimeAlive() {
    try { return !!chrome.runtime?.id; } catch (_) { return false; }
  }

  function sendBg(message) {
    return new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(message, (response) => {
          if (chrome.runtime.lastError || response === undefined) resolve(null);
          else resolve(response);
        });
      } catch (_) { resolve(null); }
    });
  }

  async function ensureCatalog(force = false) {
    if (catalog?.length && !force) return catalog;
    if (!force && catalogFailedAt && Date.now() - catalogFailedAt < 5000) return null;
    if (catalogPromise) return catalogPromise;
    catalogPromise = (async () => {
      const response = await sendBg({
        type: "h2w_json_bridge_catalog",
        site: ADAPTER.name,
        convKey: ADAPTER.getConversationKey(),
      });
      const tools = response?.ok && Array.isArray(response.tools) ? response.tools : null;
      if (!tools?.length) {
        catalogFailedAt = Date.now();
        console.log("[h2w-json] Herdr tool catalog unavailable", response?.error || "no-response");
        return null;
      }
      catalog = tools;
      catalogFailedAt = 0;
      console.log(`[h2w-json] ready on ${ADAPTER.name}: ${tools.length} Herdr tools`);
      return catalog;
    })().finally(() => { catalogPromise = null; });
    return catalogPromise;
  }

  function inputText() {
    const el = ADAPTER.getInputEl();
    if (!el) return "";
    return String(el.value != null ? el.value : (el.innerText || el.textContent || ""));
  }

  function isSelfSending() {
    return Date.now() < selfSendingUntil;
  }

  async function waitComposerCleared(timeoutMs = 7000) {
    const started = Date.now();
    while (Date.now() - started < timeoutMs) {
      const el = ADAPTER.getInputEl();
      if (!el || !ADAPTER.inputHasContent()) return true;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    return !ADAPTER.inputHasContent();
  }

  async function sendRaw(text) {
    if (!runtimeAlive()) return false;
    const el = ADAPTER.getInputEl();
    if (!el) return false;
    const oldOpacity = el.style.opacity;
    el.style.opacity = "0";
    try {
      ADAPTER.fillInput(text);
      await new Promise((resolve) => setTimeout(resolve, 420));
      selfSendingUntil = Date.now() + 3000;
      ADAPTER.send();
      let cleared = await waitComposerCleared(5000);
      if (!cleared) {
        const form = ADAPTER.getInputEl()?.closest("form");
        if (form && typeof form.requestSubmit === "function") {
          selfSendingUntil = Date.now() + 2000;
          try { form.requestSubmit(); } catch (_) {}
          cleared = await waitComposerCleared(2500);
        }
      }
      return cleared;
    } finally {
      setTimeout(() => {
        try { if (el.isConnected) el.style.opacity = oldOpacity || ""; } catch (_) {}
      }, 550);
    }
  }

  function replySnapshot() {
    return {
      text: SPEAKS.getLatestReply(),
      count: SPEAKS.getReplyBlockCount(),
      href: location.href,
    };
  }

  async function waitForReply(before, timeoutMs = REPLY_TIMEOUT_MS) {
    const started = Date.now();
    let sawReply = false;
    let last = "";
    let stable = 0;
    while (Date.now() - started < timeoutMs) {
      if (!runtimeAlive()) throw new Error("context-invalidated");
      const current = SPEAKS.getLatestReply();
      const count = SPEAKS.getReplyBlockCount();
      const changed = Boolean(current) && (
        current !== before.text || count > before.count || location.href !== before.href
      );
      if (changed) sawReply = true;
      if (sawReply) {
        if (current !== last) {
          last = current;
          stable = 0;
        } else if (current) {
          stable++;
        }
        if (stable >= STABLE_TICKS && SPEAKS.isReplyDone()) return current;
      }
      await new Promise((resolve) => setTimeout(resolve, TICK_MS));
    }
    const latest = SPEAKS.getLatestReply();
    if (latest && latest !== before.text) return latest;
    throw new Error("reply-timeout");
  }

  function knownToolNames() {
    return new Set((catalog || []).map((tool) => tool?.name).filter(Boolean));
  }

  async function callTool(call) {
    const known = knownToolNames();
    if (!known.has(call.tool)) {
      return { ok: false, error: "unknown-tool", detail: `Unknown Herdr tool: ${call.tool}` };
    }
    const response = await sendBg({
      type: "h2w_json_bridge_call",
      site: ADAPTER.name,
      convKey: ADAPTER.getConversationKey(),
      tool: call.tool,
      args: call.args || {},
    });
    return response || { ok: false, error: "background-unavailable" };
  }

  async function runToolBatch(calls) {
    const results = [];
    for (let offset = 0; offset < calls.length; offset += MAX_PARALLEL) {
      const chunk = calls.slice(offset, offset + MAX_PARALLEL);
      const chunkResults = await Promise.all(chunk.map(callTool));
      results.push(...chunkResults);
    }
    return results;
  }

  function zAiMessageRoot(el) {
    const messageRoot = el?.closest?.('[id^="message-"]:not([id$="-start"])');
    if (messageRoot) return messageRoot;
    const userRoot = el?.closest?.(".user-message");
    if (userRoot) return userRoot;
    let node = el;
    for (let i = 0; i < 10 && node; i++) {
      if (node.classList?.contains("user-message")) return node;
      if (node.id?.startsWith("message-") && !node.id.endsWith("-start")) return node;
      node = node.parentElement;
    }
    return el?.parentElement || el;
  }

  function foldRoot(root, label) {
    if (!root || root.dataset?.h2wJsonFolded || !root.parentElement) return;
    root.dataset.h2wJsonFolded = "1";
    const originalDisplay = root.style.display;
    const bar = document.createElement("div");
    bar.className = "h2w-json-fold";
    bar.style.cssText = "display:block;width:100%;box-sizing:border-box;font-size:12px;color:#9ca3af;cursor:pointer;padding:3px 0;text-align:left;";
    bar.__h2wFoldRoot = root;
    bar.__h2wOriginalDisplay = originalDisplay;
    const setExpanded = (expanded) => {
      root.style.display = expanded ? originalDisplay : "none";
      bar.textContent = `${expanded ? "▾" : "▸"} ${label}`;
      bar.dataset.h2wJsonExpanded = expanded ? "1" : "0";
    };
    bar.addEventListener("click", (event) => {
      event.stopPropagation();
      setExpanded(bar.dataset.h2wJsonExpanded !== "1");
    });
    root.parentElement.insertBefore(bar, root);
    setExpanded(false);
  }

  function cleanupFoldArtifacts() {
    for (const bar of document.querySelectorAll(".h2w-json-fold")) {
      const root = bar.__h2wFoldRoot;
      if (!root) continue;
      if (!root.isConnected) {
        bar.remove();
        continue;
      }
      if (bar.nextElementSibling !== root && root.parentElement) {
        root.parentElement.insertBefore(bar, root);
      }
    }
  }

  function foldInternalMessages() {
    try {
      cleanupFoldArtifacts();
      if (ADAPTER.name === "deepseek") {
        for (const msg of document.querySelectorAll(".ds-message")) {
          if (msg.dataset?.h2wJsonFolded) continue;
          const assistant = msg.querySelector(".ds-assistant-message-main-content");
          const text = String(msg.textContent || "");
          if (!assistant && (text.includes(CORE.MARKER) || /^\s*TOOL_RESULT:/m.test(text))) {
            foldRoot(msg, text.includes(CORE.MARKER) ? "Herdr JSON tools" : "Herdr tool result");
          } else if (assistant && CORE.extractToolCalls(String(assistant.textContent || "")).length) {
            foldRoot(msg, "Herdr tool call");
          }
        }
        return;
      }
      for (const el of document.querySelectorAll(".user-message, .chat-user")) {
        const text = String(el.textContent || "");
        if (text.includes(CORE.MARKER) || /^\s*TOOL_RESULT:/m.test(text)) {
          foldRoot(zAiMessageRoot(el), text.includes(CORE.MARKER) ? "Herdr JSON tools" : "Herdr tool result");
        }
      }
      for (const el of document.querySelectorAll(".markdown-prose, .chat-assistant")) {
        if (CORE.extractToolCalls(String(el.textContent || "")).length) foldRoot(zAiMessageRoot(el), "Herdr tool call");
      }
    } catch (_) {}
  }

  function scheduleFold() {
    if (foldScheduled) return;
    foldScheduled = true;
    queueMicrotask(() => {
      foldScheduled = false;
      foldInternalMessages();
    });
  }

  function bridgeConversationEntries() {
    if (ADAPTER.name === "deepseek") {
      return [...document.querySelectorAll(".ds-message")].map((root) => ({
        root,
        role: root.querySelector(".ds-assistant-message-main-content") ? "assistant" : "user",
        text: String(root.textContent || ""),
      }));
    }
    return [...document.querySelectorAll('[id^="message-"]:not([id$="-start"])')].map((root) => {
      const isUser = root.classList?.contains("user-message") || !!root.querySelector(".user-message");
      const isAssistant = !isUser && !!root.querySelector(".markdown-prose, .chat-assistant");
      return {
        root,
        role: isUser ? "user" : (isAssistant ? "assistant" : null),
        text: String(root.textContent || ""),
      };
    }).filter((entry) => entry.role);
  }

  function pendingBridgeReply() {
    const entries = bridgeConversationEntries();
    if (!CORE.hasPendingToolReply(entries)) return null;
    const last = entries[entries.length - 1];
    if (!last?.root || !SPEAKS.isReplyDone()) return null;
    return {
      text: last.text,
      fingerprint: `${location.href}\n${last.root.id || "assistant"}\n${last.text}`,
    };
  }

  async function continueToolLoop(initialReply, taskSeq) {
    let reply = initialReply;
    let round = 0;
    let protocolRepairs = 0;
    while (taskSeq === currentTaskSeq) {
      const replyState = CORE.toolReplyState(reply);
      const calls = CORE.extractToolCalls(reply);
      if (replyState === "none") return reply;
      round++;
      if (replyState !== "complete") {
        protocolRepairs++;
        if (protocolRepairs > 3) throw new Error("repeated-invalid-tool-json");
        console.log(`[h2w-json] repairing ${replyState} tool JSON`);
        const before = replySnapshot();
        const repairMessage = `TOOL_RESULT:\n${JSON.stringify([{
          index: 1,
          tool: "bridge_protocol",
          ok: false,
          error: `${replyState}-tool-json`,
          detail: "The previous assistant tool-call JSON was not executable. Re-emit the intended tool call(s) as complete valid JSON objects only, or answer the user normally if no tool is needed.",
        }])}`;
        await new Promise((resolve) => setTimeout(resolve, 650));
        const repairSent = await sendRaw(repairMessage);
        if (!repairSent) throw new Error("tool-json-repair-submit-failed");
        scheduleFold();
        reply = await waitForReply(before);
        scheduleFold();
        continue;
      }
      protocolRepairs = 0;
      console.log(`[h2w-json] round ${round}:`, calls.map((call) => call.tool).join(", "));
      const responses = await runToolBatch(calls);
      const resultMessage = CORE.formatToolResultBatch(calls, responses);
      const before = replySnapshot();
      await new Promise((resolve) => setTimeout(resolve, 650));
      const resultSent = await sendRaw(resultMessage);
      if (!resultSent) throw new Error("tool-result-submit-failed");
      scheduleFold();
      reply = await waitForReply(before);
      scheduleFold();
      if (round % ROUND_YIELD_INTERVAL === 0) {
        console.log(`[h2w-json] continuing after ${round} tool rounds; latest reply still contains tool JSON`);
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
    }
    return reply;
  }

  async function resumePendingBridge() {
    resumeScheduled = false;
    if (running || !runtimeAlive()) return false;
    const pending = pendingBridgeReply();
    if (!pending || pending.fingerprint === lastResumeFingerprint) return false;
    lastResumeFingerprint = pending.fingerprint;
    const tools = await ensureCatalog();
    if (!tools?.length) return false;
    running = true;
    const taskSeq = ++currentTaskSeq;
    console.log("[h2w-json] resuming pending tool JSON after page/script recovery");
    try {
      await continueToolLoop(pending.text, taskSeq);
      return true;
    } catch (e) {
      console.error("[h2w-json] pending bridge recovery failed:", e);
      return false;
    } finally {
      if (taskSeq === currentTaskSeq) running = false;
      scheduleFold();
    }
  }

  function schedulePendingResume() {
    if (resumeScheduled || running) return;
    const pending = pendingBridgeReply();
    if (!pending || pending.fingerprint === lastResumeFingerprint) return;
    resumeScheduled = true;
    setTimeout(() => { void resumePendingBridge(); }, 250);
  }

  async function runAgentLoop(userText, taskSeq, firstSubmitted) {
    let firstResolved = false;
    const resolveFirst = (value) => {
      if (firstResolved) return;
      firstResolved = true;
      firstSubmitted(value);
    };
    try {
      const tools = await ensureCatalog();
      if (!tools?.length) {
        const sent = await sendRaw(userText);
        resolveFirst({ ok: sent, bridged: false, error: sent ? "catalog-unavailable" : "submit-failed" });
        return;
      }

      const firstBefore = replySnapshot();
      const prompt = `${CORE.buildSystemPrompt(tools, ADAPTER.name)}\n\nUSER_TASK:\n${userText}`;
      const sent = await sendRaw(prompt);
      resolveFirst({ ok: sent, bridged: true, site: ADAPTER.name });
      if (!sent) throw new Error("submit-failed");
      scheduleFold();

      const reply = await waitForReply(firstBefore);
      scheduleFold();
      await continueToolLoop(reply, taskSeq);
    } catch (e) {
      resolveFirst({ ok: false, bridged: true, error: String(e?.message || e) });
      console.error("[h2w-json] agent loop failed:", e);
    } finally {
      if (taskSeq === currentTaskSeq) running = false;
      scheduleFold();
    }
  }

  async function submitTask(userText) {
    const text = String(userText || "").trim();
    if (!text) return { ok: false, error: "empty-task" };
    if (running) return { ok: false, blocked: "json-bridge-busy" };
    running = true;
    const taskSeq = ++currentTaskSeq;
    return new Promise((resolve) => {
      void runAgentLoop(text, taskSeq, resolve);
    });
  }

  function stopEvent(event) {
    try { event.preventDefault(); } catch (_) {}
    try { event.stopImmediatePropagation(); } catch (_) {}
    try { event.stopPropagation(); } catch (_) {}
  }

  function tryIntercept(event) {
    if (isSelfSending() || !runtimeAlive()) return false;
    const raw = inputText().trim();
    if (!raw) return false;
    stopEvent(event);
    if (running) {
      console.log("[h2w-json] task already running; submit ignored");
      return true;
    }
    void submitTask(raw);
    return true;
  }

  function hookInput() {
    const el = ADAPTER.getInputEl();
    if (!el || el.dataset.h2wJsonHooked) return;
    el.dataset.h2wJsonHooked = "1";
    el.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" || event.shiftKey || event.isComposing || event.keyCode === 229) return;
      tryIntercept(event);
    }, true);
    el.addEventListener("keypress", (event) => {
      if (event.key !== "Enter" || event.shiftKey || event.isComposing || event.keyCode === 229) return;
      tryIntercept(event);
    }, true);
    const form = el.closest("form");
    if (form && !form.dataset.h2wJsonHooked) {
      form.dataset.h2wJsonHooked = "1";
      form.addEventListener("submit", (event) => tryIntercept(event), true);
    }
    const sendButton = typeof ADAPTER.getSendButton === "function" ? ADAPTER.getSendButton() : null;
    if (sendButton && !sendButton.dataset.h2wJsonHooked) {
      sendButton.dataset.h2wJsonHooked = "1";
      sendButton.addEventListener("click", (event) => tryIntercept(event), true);
    }
    console.log(`[h2w-json] hooked ${ADAPTER.name} composer`);
  }

  const observer = new MutationObserver(() => {
    hookInput();
    scheduleFold();
    schedulePendingResume();
  });
  try { observer.observe(document.body, { childList: true, subtree: true }); } catch (_) {}
  hookInput();
  hookTimer = setInterval(() => {
    if (!runtimeAlive()) {
      clearInterval(hookTimer);
      try { observer.disconnect(); } catch (_) {}
      return;
    }
    hookInput();
    schedulePendingResume();
  }, 1500);
  void ensureCatalog();
  scheduleFold();
  schedulePendingResume();

  window.__H2W_JSON_BRIDGE__ = {
    enabled: true,
    site: ADAPTER.name,
    submitTask,
    sendRaw,
    refreshCatalog: () => ensureCatalog(true),
    isRunning: () => running,
  };
})();
