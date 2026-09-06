// injector/gemini.js — gemini.google.com browser-control adapter
// Alpha 5 intentionally proves only the minimum plain-submit vertical.
// Provider-native selectors and ids stay inside this adapter and never become
// Herdr core mutation inputs or portable resource identity.
class GeminiAdapter extends BaseAdapter {
  get name() { return "gemini"; }
  get needsMainWorldInsert() { return true; }

  getConversationKey() {
    try {
      if (location.origin !== "https://gemini.google.com") return null;
      const match = location.pathname.match(/^\/app\/([^/?#]+)\/?$/);
      if (!match) return null;
      const nativeId = decodeURIComponent(match[1]);
      if (!nativeId || nativeId.length > 512 || /[\u0000-\u001f\u007f]/.test(nativeId)) return null;
      return `${location.origin}/app/${encodeURIComponent(nativeId)}`;
    } catch (_) {
      return null;
    }
  }

  getNativeSessionIdentity() {
    const key = this.getConversationKey();
    if (!key) return null;
    try {
      const match = new URL(key).pathname.match(/^\/app\/([^/?#]+)$/);
      return match ? decodeURIComponent(match[1]) : null;
    } catch (_) {
      return null;
    }
  }

  getCanonicalConversationUrl() {
    return this.getConversationKey();
  }

  getInputEl() {
    const selectors = [
      'div.ql-editor.textarea[contenteditable="true"]',
      'div.ql-editor[contenteditable="true"]',
      'rich-textarea [contenteditable="true"]',
      '[aria-label="Enter a prompt here"][contenteditable="true"]',
      '[contenteditable="true"][role="textbox"]',
    ];
    for (const selector of selectors) {
      const element = document.querySelector(selector);
      if (element && this.elementVisible(element)) return element;
    }
    return null;
  }

  getWatchMainWorldSelector() {
    const input = this.getInputEl();
    if (!input) return null;
    const selectors = [
      'div.ql-editor.textarea[contenteditable="true"]',
      'div.ql-editor[contenteditable="true"]',
      'rich-textarea [contenteditable="true"]',
      '[aria-label="Enter a prompt here"][contenteditable="true"]',
      '[contenteditable="true"][role="textbox"]',
    ];
    return selectors.find((selector) => document.querySelector(selector) === input) || null;
  }

  getSendButtonCandidates() {
    const selectors = [
      'button[aria-label="Send message"]',
      'button[aria-label*="Send" i]',
      'button.send-button',
      '.send-button button',
    ];
    const seen = new Set();
    const output = [];
    for (const selector of selectors) {
      for (const element of document.querySelectorAll(selector)) {
        if (!seen.has(element) && this.elementVisible(element)) {
          seen.add(element);
          output.push(element);
        }
      }
    }
    return output;
  }

  getSendButton() {
    return this.getSendButtonCandidates()[0] || null;
  }

  getStopButtonCandidates() {
    const selectors = [
      'button[aria-label*="Stop" i]',
      '[role="button"][aria-label*="Stop" i]',
    ];
    const seen = new Set();
    const output = [];
    for (const selector of selectors) {
      for (const element of document.querySelectorAll(selector)) {
        if (!seen.has(element) && this.elementVisible(element)) {
          seen.add(element);
          output.push(element);
        }
      }
    }
    return output;
  }

  messageNodes(role) {
    const selectors = role === "user"
      ? ['user-query', '.user-query', '.query-text', '[data-message-author="user"]']
      : ['model-response', '.model-response', '.model-response-text', '.response-content', '[data-message-author="assistant"]'];
    for (const selector of selectors) {
      const nodes = [...document.querySelectorAll(selector)].filter((element) => this.elementVisible(element));
      if (nodes.length) return nodes;
    }
    return [];
  }

  getMessageSnapshot(role) {
    if (role !== "user" && role !== "assistant") {
      return { messageId: null, text: "", count: 0 };
    }
    const nodes = this.messageNodes(role);
    const element = nodes[nodes.length - 1] || null;
    if (!element) return { messageId: null, text: "", count: 0 };
    const messageId = element.getAttribute?.("data-message-id")
      || element.getAttribute?.("data-turn-id")
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
    return [...document.querySelectorAll('[aria-busy="true"]')]
      .some((element) => this.elementVisible(element));
  }

  async getAccountNativeIdentity() {
    const selectors = [
      'a[aria-label*="Google Account"]',
      'button[aria-label*="Google Account"]',
      '[role="button"][aria-label*="Google Account"]',
    ];
    let email = null;
    for (const selector of selectors) {
      for (const element of document.querySelectorAll(selector)) {
        const label = String(element.getAttribute?.("aria-label") || "");
        const match = label.match(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/i);
        if (match) {
          email = match[0].toLowerCase();
          break;
        }
      }
      if (email) break;
    }
    if (!email || !globalThis.crypto?.subtle) return null;
    const digest = await globalThis.crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(email),
    );
    const hex = [...new Uint8Array(digest)]
      .map((value) => value.toString(16).padStart(2, "0"))
      .join("");
    return `google-account-sha256:${hex}`;
  }
}

window.__H2W_ADAPTER__ = new GeminiAdapter();
