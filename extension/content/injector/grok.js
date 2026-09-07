// injector/grok.js — grok.com Browser Registry adapter
// Keep Grok-specific URL/DOM/account details here. Browser Registry, consent,
// generation fencing, and typed dispatch remain provider-neutral.
class GrokAdapter extends BaseAdapter {
  get name() { return "grok"; }
  get needsMainWorldInsert() { return true; }

  getSessionIdentity() {
    try {
      const url = new URL(location.href);
      if (url.origin !== "https://grok.com") return null;
      const match = url.pathname.match(/^\/c\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/?$/i);
      return match ? match[1].toLowerCase() : null;
    } catch (_) {
      return null;
    }
  }

  getConversationKey() {
    const sessionId = this.getSessionIdentity();
    return sessionId ? `https://grok.com/c/${sessionId}` : null;
  }

  getNativeSessionIdentity() {
    return this.getSessionIdentity();
  }

  getCanonicalConversationUrl() {
    return this.getConversationKey();
  }

  getInputEl() {
    const selectors = [
      '[data-testid="chat-input"] div.ProseMirror[role="textbox"]',
      'div.ProseMirror[role="textbox"][contenteditable="true"]',
      'div[contenteditable="true"][role="textbox"][aria-label*="Grok" i]',
    ];
    for (const selector of selectors) {
      const element = document.querySelector(selector);
      if (element && this.elementVisible(element)) return element;
    }
    return null;
  }

  getWatchMainWorldSelector() {
    const element = this.getInputEl();
    if (!element) return null;
    if (element.id) return `#${CSS.escape(element.id)}[contenteditable="true"]`;
    const selectors = [
      '[data-testid="chat-input"] div.ProseMirror[role="textbox"]',
      'div.ProseMirror[role="textbox"][contenteditable="true"]',
      'div[contenteditable="true"][role="textbox"][aria-label*="Grok" i]',
    ];
    for (const selector of selectors) {
      if (document.querySelector(selector) === element) return selector;
    }
    return null;
  }

  getSendButtonCandidates() {
    const selectors = [
      'button[data-testid="chat-submit"]',
      'button[aria-label="Submit"]',
      'button[aria-label="Send message"]',
      'button[aria-label="提交"]',
      'button[data-testid="send-button"]',
      'button[type="submit"]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of selectors) {
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
    const selectors = [
      'button[aria-label*="Stop" i]',
      'button[data-testid="stop-button"]',
      'button[data-testid*="stop" i]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of selectors) {
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
      ? ['[data-testid="user-message"]']
      : role === "assistant"
        ? ['[data-testid="assistant-message"]']
        : [];
    const nodes = [];
    const seen = new Set();
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
    let messageId = element.getAttribute?.("data-message-id")
      || element.getAttribute?.("data-message-uuid")
      || null;
    if (!messageId) {
      let ancestor = element.parentElement || null;
      for (let depth = 0; ancestor && depth < 8; depth += 1) {
        const ancestorId = String(ancestor.getAttribute?.("id") || "");
        const match = ancestorId.match(
          /^response-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i,
        );
        if (match) {
          messageId = `${match[1].toLowerCase()}-${role}`;
          break;
        }
        ancestor = ancestor.parentElement || null;
      }
    }
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
    return [...document.querySelectorAll('[aria-busy="true"]')]
      .some((element) => this.elementVisible(element));
  }

  async getAccountNativeIdentity() {
    try {
      const response = await fetch("/api/auth/session", {
        method: "GET",
        credentials: "include",
        cache: "no-store",
        headers: { accept: "application/json" },
      });
      if (!response.ok) return null;
      const payload = await response.json();
      const rawUserId = payload?.session?.userId || payload?.userId || null;
      const userId = typeof rawUserId === "string" ? rawUserId.trim().toLowerCase() : "";
      if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(userId)
          || !globalThis.crypto?.subtle) return null;
      const digest = await globalThis.crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(userId),
      );
      const hex = [...new Uint8Array(digest)]
        .map((value) => value.toString(16).padStart(2, "0"))
        .join("");
      return `grok-account-sha256:${hex}`;
    } catch (_) {
      return null;
    }
  }
}

window.__H2W_ADAPTER__ = new GrokAdapter();
