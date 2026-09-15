// injector/claude.js — claude.ai Browser Registry adapter
// Keep provider-specific URL/DOM/account details here. Browser Registry, consent,
// dispatch fencing, and idempotency remain provider-neutral in background/runtime.
class ClaudeAdapter extends BaseAdapter {
  get name() { return "claude"; }
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
      'button[aria-label="Send message"]',
      'button[data-testid="send-button"]',
      'button[aria-label*="Send" i]',
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

  getStopButtonCandidates() {
    const chains = [
      'button[aria-label*="Stop" i]',
      'button[data-testid="stop-button"]',
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
    const element = nodes.at(-1) || null;
    if (!element) return { messageId: null, text: "", count: 0 };
    return {
      messageId: element.getAttribute?.("data-message-id")
        || element.getAttribute?.("data-message-uuid")
        || null,
      text: String(element.innerText || element.textContent || "").replace(/\s+/g, " ").trim(),
      count: nodes.length,
    };
  }

  getLastMessageText(role) {
    return this.getMessageSnapshot(role).text;
  }

  isGenerationInProgress() {
    if (this.getStopButtonCandidates().length > 0) return true;
    const selectors = ['[data-is-streaming="true"]', '[aria-busy="true"]'];
    return selectors.some((selector) => [...document.querySelectorAll(selector)]
      .some((element) => this.elementVisible(element)));
  }

  async getAccountNativeIdentity() {
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
          return await sha256Identity(email);
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
      if (!uuid.test(hint) || !uuid.test(confirmed) || hint !== confirmed) return null;
      return await sha256Identity(confirmed);
    } catch (_) {
      return null;
    }
  }
}

window.__H2W_ADAPTER__ = new ClaudeAdapter();
