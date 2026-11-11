// injector/claude.js — claude.ai Browser Registry adapter
// Keep provider-specific URL/DOM/account details here. Browser Registry, consent,
// dispatch fencing, and idempotency remain provider-neutral in background/runtime.
const CLAUDE_ADAPTER_CAPABILITIES = Object.freeze({
  browserActuation: true,
  stopGeneration: true,
});
const CLAUDE_STOP_TESTID_RE = /^(?:stop|stop-button|chat-input-stop|chat-stop|chat-stop-button|composer-stop|composer-stop-button|stop-generation|stop-generation-button|stop-response|stop-response-button|stop-streaming|stop-streaming-button)$/i;
const CLAUDE_STOP_LABEL_RE = /^(?:stop|stop response|stop generating|stop streaming|stop output|stop reply|interrupt|interrupt response|停止|停止回应|停止响应|停止回答|停止生成|停止输出|停止回复|停止流式|停止流式传输|停止回應|停止響應|停止產生|停止輸出|停止回覆|中断|中断回应|中断响应|中断回答|中断生成|中斷|中斷回應|中斷產生|中止|中止回应|中止生成)$/i;

class ClaudeAdapter extends BaseAdapter {
  get name() { return "claude"; }
  get capabilities() { return CLAUDE_ADAPTER_CAPABILITIES; }
  get needsMainWorldInsert() { return true; }

  getSessionIdentity() {
    try {
      const url = new URL(location.href);
      if (url.origin !== "https://claude.ai") return null;
      const match = url.pathname.match(/^\/chat\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/?$/i);
      return match ? match[1].toLowerCase() : null;
    } catch (_) {
      return null;
    }
  }

  getConversationKey() {
    const sessionId = this.getSessionIdentity();
    return sessionId ? `https://claude.ai/chat/${sessionId}` : null;
  }

  getNativeSessionIdentity() {
    return this.getSessionIdentity();
  }

  getCanonicalConversationUrl() {
    return this.getConversationKey();
  }

  async resolveProjectIdentity(convKey) {
    let project = this.getProjectIdentity();
    if (project) return project;

    // Claude renders the stable /chat/<uuid> route before its chat header.
    // Wait boundedly for the breadcrumb so reload cannot alternate parent scope.
    const deadline = Date.now() + 2500;
    while (Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if (this.getConversationKey() !== convKey) return null;
      project = this.getProjectIdentity();
      if (project) return project;
    }
    return null;
  }

  getProjectIdentity() {
    if (!this.getSessionIdentity()) return null;
    const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    const matches = new Map();
    for (const link of document.querySelectorAll('[data-testid="chat-header"] a[href*="/project/"]')) {
      if (!this.elementVisible(link)) continue;
      let url = null;
      try {
        url = new URL(String(link.getAttribute?.("href") || link.href || ""), location.origin);
      } catch (_) {
        continue;
      }
      const match = url.origin === "https://claude.ai"
        ? url.pathname.match(/^\/project\/([0-9a-f-]{36})\/?$/i)
        : null;
      if (!match || !uuid.test(match[1])) continue;
      const projectId = match[1].toLowerCase();
      matches.set(projectId, {
        id: projectId,
        name: String(link.innerText || link.textContent || "").replace(/\s+/g, " ").trim() || null,
        key: `https://claude.ai/project/${projectId}`,
      });
    }
    return matches.size === 1 ? [...matches.values()][0] : null;
  }

  getInputEl() {
    const chains = [
      '[data-testid="chat-input"][contenteditable="true"]',
      'div[contenteditable="true"][role="textbox"]',
      '.ProseMirror[contenteditable="true"]',
      '.ql-editor[contenteditable="true"]',
    ];
    for (const selector of chains) {
      const element = document.querySelector(selector);
      if (element && this.elementVisible(element)) return element;
    }
    return null;
  }

  getWatchMainWorldSelector() {
    const element = this.getInputEl();
    if (!element) return null;
    if (element.id) return `#${CSS.escape(element.id)}[contenteditable="true"]`;
    const chains = [
      '[data-testid="chat-input"][contenteditable="true"]',
      'div[contenteditable="true"][role="textbox"]',
      '.ProseMirror[contenteditable="true"]',
      '.ql-editor[contenteditable="true"]',
    ];
    for (const selector of chains) {
      if (document.querySelector(selector) === element) return selector;
    }
    return null;
  }

  getSendButtonCandidates() {
    const chains = [
      'button[data-testid="chat-input-send"]',
      '[role="button"][data-testid="chat-input-send"]',
      'button[aria-label="Send message"]',
      'button[data-testid="send-button"]',
      'button[aria-label*="Send" i]',
      'button[aria-label*="发送"]',
      'button[aria-label*="傳送"]',
      'button[aria-label*="發送"]',
      'button[type="submit"]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of chains) {
      for (const element of document.querySelectorAll(selector)) {
        if (!this.elementVisible(element) || seen.has(element)) continue;
        seen.add(element);
        out.push(element);
      }
    }
    return out;
  }

  getSendButton() {
    return this.getSendButtonCandidates()[0] || null;
  }

  /** Composer-busy probe detail: booleans/counts only, never page text. */
  composerBusyProbeDetail() {
    const visible = (el) => { try { return Boolean(this.elementVisible(el)); } catch (_) { return false; } };
    const count = (selector, root = document) => {
      try { return Math.min([...root.querySelectorAll(selector)].filter(visible).length, 50); } catch (_) { return -1; }
    };
    const countAny = (selectors, root = document) => {
      try {
        const matched = new Set();
        for (const selector of selectors) {
          for (const el of root.querySelectorAll(selector)) {
            if (visible(el)) matched.add(el);
          }
        }
        return Math.min(matched.size, 50);
      } catch (_) {
        return -1;
      }
    };
    const rows = [...document.querySelectorAll('[data-testid="transcript-row"]')];
    let latest = null;
    let latestIndex = -1;
    for (const row of rows) {
      const raw = String(row.getAttribute?.("data-index") ?? "");
      const index = /^(0|[1-9]\d*)$/.test(raw) ? Number(raw) : -1;
      if (index > latestIndex) { latest = row; latestIndex = index; }
    }
    const role = String(latest?.getAttribute?.("data-perf-row") || "");
    return {
      claude_stop_aria_label_count: countAny([
        'button[aria-label*="Stop" i]',
        '[role="button"][aria-label*="Stop" i]',
        'button[aria-label*="Interrupt" i]',
        '[role="button"][aria-label*="Interrupt" i]',
        'button[aria-label*="停止"]',
        '[role="button"][aria-label*="停止"]',
        'button[aria-label*="中断"]',
        '[role="button"][aria-label*="中断"]',
        'button[aria-label*="中斷"]',
        '[role="button"][aria-label*="中斷"]',
        'button[aria-label*="中止"]',
        '[role="button"][aria-label*="中止"]',
      ]),
      claude_stop_testid_count: countAny([
        'button[data-testid="chat-input-stop"]',
        '[role="button"][data-testid="chat-input-stop"]',
        'button[data-testid="stop-button"]',
        '[role="button"][data-testid="stop-button"]',
        'button[data-testid*="stop" i]',
        '[role="button"][data-testid*="stop" i]',
      ]),
      claude_stop_candidate_count: Math.min(this.getStopButtonCandidates().length, 50),
      claude_transcript_row_count: Math.min(rows.length, 10000),
      claude_latest_row_index: latestIndex,
      claude_latest_row_role: role === "assistant" || role === "user" ? role : (role ? "other" : "none"),
      claude_latest_row_streaming_attr: latest?.getAttribute?.("data-perf-row-streaming") === "true",
      claude_latest_row_is_streaming_count: latest ? count('[data-is-streaming="true"]', latest) : 0,
      claude_latest_row_aria_busy_count: latest ? count('[aria-busy="true"]', latest) : 0,
      claude_document_is_streaming_count: count('[data-is-streaming="true"]'),
    };
  }

  isExplicitStopElement(element) {
    if (!element || !this.elementVisible(element)) return false;
    const testId = String(element.getAttribute?.("data-testid") || "").trim();
    if (testId && CLAUDE_STOP_TESTID_RE.test(testId)) return true;
    // `aria-label*="Stop"` also hits unrelated visible controls (2026-10-12
    // probe: 2 such buttons on an idle chat made every submit composer_busy).
    // Only an exact stop label or an exact stop test id counts.
    const labels = [
      element.getAttribute?.("aria-label"),
      element.getAttribute?.("title"),
      element.innerText,
      element.textContent,
    ];
    for (const raw of labels) {
      const normalized = String(raw || "").trim().replace(/\s+/g, " ");
      if (normalized && CLAUDE_STOP_LABEL_RE.test(normalized)) return true;
    }
    return false;
  }

  getStopButtonCandidates() {
    const chains = [
      'button[data-testid="chat-input-stop"]',
      '[role="button"][data-testid="chat-input-stop"]',
      'button[data-testid="stop-button"]',
      '[role="button"][data-testid="stop-button"]',
      'button[data-testid*="stop" i]',
      '[role="button"][data-testid*="stop" i]',
      'button[aria-label*="Stop" i]',
      '[role="button"][aria-label*="Stop" i]',
      'button[aria-label*="Interrupt" i]',
      '[role="button"][aria-label*="Interrupt" i]',
      'button[aria-label*="停止"]',
      '[role="button"][aria-label*="停止"]',
      'button[aria-label*="中断"]',
      '[role="button"][aria-label*="中断"]',
      'button[aria-label*="中斷"]',
      '[role="button"][aria-label*="中斷"]',
      'button[aria-label*="中止"]',
      '[role="button"][aria-label*="中止"]',
      'button[title*="Stop" i]',
      'button[title*="停止"]',
      'button[title*="中断"]',
      'button[title*="中斷"]',
      'button[title*="中止"]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of chains) {
      for (const element of document.querySelectorAll(selector)) {
        if (seen.has(element) || !this.isExplicitStopElement(element)) continue;
        seen.add(element);
        out.push(element);
      }
    }
    const input = this.getInputEl();
    const composerScope = input?.closest?.("fieldset, form") || input?.parentElement?.parentElement || null;
    if (composerScope && typeof composerScope.querySelectorAll === "function") {
      for (const element of composerScope.querySelectorAll('button, [role="button"]')) {
        if (seen.has(element) || !this.isExplicitStopElement(element)) continue;
        seen.add(element);
        out.push(element);
      }
    }
    return out;
  }

  getTranscriptMessageRef(element, role) {
    if (!element || (role !== "user" && role !== "assistant")) return null;
    const sessionId = this.getSessionIdentity();
    if (!sessionId) return null;
    const row = element.closest?.('[data-testid="transcript-row"]') || null;
    const article = element.closest?.('[role="article"]') || null;
    if (!row || !article) return null;

    const rowIndexRaw = String(row.getAttribute?.("data-index") || "");
    const rsIndexRaw = String(row.getAttribute?.("data-rs-index") || "");
    const perfRole = String(row.getAttribute?.("data-perf-row") || "");
    const articlePosRaw = String(article.getAttribute?.("aria-posinset") || "");
    const expectedPerfRole = role === "user" ? "human" : "assistant";
    if (!/^(0|[1-9]\d*)$/.test(rowIndexRaw)
        || rowIndexRaw !== rsIndexRaw
        || perfRole !== expectedPerfRole
        || !/^[1-9]\d*$/.test(articlePosRaw)) {
      return null;
    }
    const rowIndex = Number(rowIndexRaw);
    const articlePos = Number(articlePosRaw);
    if (!Number.isSafeInteger(rowIndex)
        || !Number.isSafeInteger(articlePos)
        || articlePos !== rowIndex + 1) {
      return null;
    }
    return `claude-dom-v1:${sessionId}:${role}:${rowIndex}`;
  }

  getResultSettlementSnapshot(acceptedUserMessageRef) {
    const sessionId = this.getSessionIdentity();
    if (!sessionId || typeof acceptedUserMessageRef !== "string") return null;
    const match = acceptedUserMessageRef.match(
      /^claude-dom-v1:([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}):user:(0|[1-9]\d*)$/i,
    );
    if (!match || match[1].toLowerCase() !== sessionId) return null;
    const userIndex = Number(match[2]);
    if (!Number.isSafeInteger(userIndex)) return null;
    const rows = [...document.querySelectorAll('[data-testid="transcript-row"]')];
    const userRow = rows.find((row) => String(row.getAttribute?.("data-index") || "") === String(userIndex));
    const assistantRow = rows.find((row) => (
      String(row.getAttribute?.("data-index") || "") === String(userIndex + 1)
    ));
    if (!assistantRow) return null;
    const assistantElement = assistantRow.querySelector?.(
      '[data-testid="assistant-message"], .font-claude-response, .font-claude-message',
    ) || null;
    if (!assistantElement || !this.elementVisible(assistantElement)) {
      return null;
    }
    const assistantMessageRef = this.getTranscriptMessageRef(assistantElement, "assistant");
    if (!assistantMessageRef) return null;
    let userMessageRef = null;
    if (userRow) {
      const userElement = userRow.querySelector?.(
        '[data-testid="user-message"], [data-testid="human-message"], .font-user-message',
      ) || null;
      if (!userElement || !this.elementVisible(userElement)) return null;
      userMessageRef = this.getTranscriptMessageRef(userElement, "user");
      if (userMessageRef !== acceptedUserMessageRef) return null;
    } else {
      const userScrolledOffTop = rows.length > 0 && rows.every((row) => {
        const rawIndex = String(row.getAttribute?.("data-index") || "").trim();
        return /^(?:0|[1-9]\d*)$/.test(rawIndex) && Number(rawIndex) > userIndex;
      });
      if (!userScrolledOffTop) return null;
      userMessageRef = acceptedUserMessageRef;
    }
    if (assistantRow.getAttribute?.("data-perf-row-streaming") === "true") return null;
    const assistantText = String(
      assistantElement.innerText || assistantElement.textContent || "",
    ).replace(/\s+/g, " ").trim();
    if (!assistantText) return null;
    return {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: assistantMessageRef,
      userMessageId: userMessageRef,
      text: assistantText,
    };
  }

  getMessageSnapshot(role) {
    const selectors = role === "user"
      ? ['[data-testid="user-message"]', '[data-testid="human-message"]', '.font-user-message']
      : role === "assistant"
        ? ['.font-claude-response', '[data-testid="assistant-message"]', '.font-claude-message']
        : [];
    const seen = new Set();
    const nodes = [];
    for (const selector of selectors) {
      for (const element of document.querySelectorAll(selector)) {
        if (!this.elementVisible(element) || seen.has(element)) continue;
        seen.add(element);
        nodes.push(element);
      }
      if (nodes.length > 0) break;
    }
    let element = nodes.at(-1) || null;
    let highestIndex = -1;
    for (const candidate of nodes) {
      const row = candidate.closest?.('[data-testid="transcript-row"]') || null;
      const rawIndex = String(row?.getAttribute?.("data-index") || "").trim();
      if (!/^(?:0|[1-9]\d*)$/.test(rawIndex)) continue;
      const idx = Number(rawIndex);
      if (Number.isSafeInteger(idx) && idx >= highestIndex) {
        highestIndex = idx;
        element = candidate;
      }
    }
    if (!element) return { messageId: null, text: "", count: 0 };
    const messageId = element.getAttribute?.("data-message-id")
      || element.getAttribute?.("data-message-uuid")
      || this.getTranscriptMessageRef(element, role)
      || null;
    return {
      messageId,
      text: String(element.innerText || element.textContent || "").replace(/\s+/g, " ").trim(),
      count: nodes.length,
    };
  }

  getLastMessageText(role) {
    return this.getMessageSnapshot(role).text;
  }

  isGenerationInProgress() {
    if (this.getStopButtonCandidates().length > 0) return true;
    // After a reload unrelated regions stay aria-busy while idle, so only the
    // newest assistant turn decides.
    // The transcript is virtualized; DOM order is not turn order after a
    // reload, so the newest turn is the row with the highest data-index.
    // aria-busy is not a streaming signal (idle rows keep it for lazy media).
    const rows = [...document.querySelectorAll('[data-testid="transcript-row"]')];
    let latest = null;
    let latestIndex = -1;
    for (const row of rows) {
      const raw = String(row.getAttribute?.("data-index") ?? "");
      const index = /^(0|[1-9]\d*)$/.test(raw) ? Number(raw) : -1;
      if (Number.isSafeInteger(index) && index > latestIndex) {
        latest = row;
        latestIndex = index;
      }
    }
    if (!latest) latest = rows[rows.length - 1] || null;
    if (latest) {
      if (latest.getAttribute?.("data-perf-row") !== "assistant") return false;
      if (latest.getAttribute?.("data-perf-row-streaming") === "true") return true;
      return [...(latest.querySelectorAll?.('[data-is-streaming="true"]') || [])]
        .some((element) => this.elementVisible(element));
    }
    return [...document.querySelectorAll('[data-is-streaming="true"]')]
      .some((element) => this.elementVisible(element));
  }

  async getAccountObservation() {
    const sha256Identity = async (value) => {
      if (!value || !globalThis.crypto?.subtle) return null;
      const digest = await globalThis.crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(value),
      );
      const hex = [...new Uint8Array(digest)]
        .map((byte) => byte.toString(16).padStart(2, "0"))
        .join("");
      return `claude-account-sha256:${hex}`;
    };

    try {
      const response = await fetch("/api/auth/current_account", {
        method: "GET",
        credentials: "include",
        cache: "no-store",
        headers: { accept: "application/json" },
      });
      if (response.ok) {
        const payload = await response.json();
        const rawEmail = payload?.account?.email_address || payload?.email_address || null;
        const email = typeof rawEmail === "string" ? rawEmail.trim().toLowerCase() : "";
        if (email && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
          const display = /^[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,24}$/.test(email) ? email : null;
          return { nativeIdentity: await sha256Identity(email), email: display };
        }
      }
    } catch (_) {}

    // Claude's current web app no longer exposes /api/auth/current_account.
    // It does publish the active account UUID through two independent local
    // cache hints. Require both validated hints to agree before using them so
    // stale/single-key page state cannot silently retarget Browser Registry.
    try {
      const hint = String(localStorage.getItem("__qk_hint_account_uuid") || "").trim().toLowerCase();
      const confirmed = String(localStorage.getItem("rq-cache-confirmed-account") || "").trim().toLowerCase();
      const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
      if (!uuid.test(hint) || !uuid.test(confirmed) || hint !== confirmed) {
        return { nativeIdentity: null, email: null };
      }
      return { nativeIdentity: await sha256Identity(confirmed), email: null };
    } catch (_) {
      return { nativeIdentity: null, email: null };
    }
  }

  async getAccountNativeIdentity() {
    return (await this.getAccountObservation()).nativeIdentity;
  }
}

registerH2WAdapter(new ClaudeAdapter());
