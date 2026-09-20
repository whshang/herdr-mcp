// injector/grok.js — grok.com Browser Registry adapter
// Keep Grok-specific URL/DOM/account details here. Browser Registry, consent,
// generation fencing, and typed dispatch remain provider-neutral.
class GrokAdapter extends BaseAdapter {
  get name() { return "grok"; }
  get needsMainWorldInsert() { return true; }

  getConversationIdentity() {
    try {
      const url = new URL(location.href);
      if (url.origin !== "https://grok.com") return null;
      const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
      const directMatch = url.pathname.match(/^\/c\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/?$/i);
      if (directMatch) {
        const sessionId = directMatch[1].toLowerCase();
        return {
          projectId: null,
          sessionId,
          convKey: `${url.origin}/c/${sessionId}`,
        };
      }

      const projectMatch = url.pathname.match(/^\/project\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/?$/i);
      const chatValues = url.searchParams.getAll("chat");
      if (!projectMatch || chatValues.length !== 1 || !uuid.test(chatValues[0])) return null;
      const projectId = projectMatch[1].toLowerCase();
      const sessionId = chatValues[0].toLowerCase();
      return {
        projectId,
        sessionId,
        convKey: `${url.origin}/project/${projectId}?chat=${sessionId}`,
      };
    } catch (_) {
      return null;
    }
  }

  getSessionIdentity() {
    return this.getConversationIdentity()?.sessionId || null;
  }

  getConversationKey() {
    return this.getConversationIdentity()?.convKey || null;
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
      const sessionId = this.getSessionIdentity();
      const ordinal = nodes.indexOf(element);
      if (sessionId && ordinal >= 0) {
        messageId = `grok-dom-v1:${sessionId}:${role}:${ordinal}`;
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

  getResultSettlementSnapshot(acceptedUserMessageRef) {
    if (typeof acceptedUserMessageRef !== "string" || !acceptedUserMessageRef) return null;
    const sessionId = this.getSessionIdentity();
    const syntheticPrefix = sessionId
      ? `grok-dom-v1:${sessionId}:user:`
      : null;
    if (syntheticPrefix && acceptedUserMessageRef.startsWith(syntheticPrefix)) {
      const ordinalText = acceptedUserMessageRef.slice(syntheticPrefix.length);
      if (!/^\d+$/.test(ordinalText)) return null;
      const userOrdinal = Number(ordinalText);
      const users = [...document.querySelectorAll('[data-testid="user-message"]')]
        .filter((element) => this.elementVisible(element));
      const acceptedUser = users[userOrdinal] || null;
      if (!acceptedUser) return null;

      const transcript = [
        ...document.querySelectorAll(
          '[data-testid="user-message"], [data-testid="assistant-message"]',
        ),
      ].filter((element) => this.elementVisible(element));
      const acceptedIndex = transcript.indexOf(acceptedUser);
      if (acceptedIndex < 0) return null;

      let assistantElement = null;
      let hasLaterUser = false;
      for (let index = acceptedIndex + 1; index < transcript.length; index += 1) {
        const element = transcript[index];
        const testId = element.getAttribute?.("data-testid");
        if (testId === "user-message") {
          hasLaterUser = true;
          break;
        }
        if (testId === "assistant-message") {
          const text = String(element.innerText || element.textContent || "")
            .replace(/\s+/g, " ")
            .trim();
          if (text) {
            assistantElement = element;
            break;
          }
        }
      }
      if (!assistantElement) return null;
      if (!hasLaterUser) {
        hasLaterUser = transcript
          .slice(acceptedIndex + 1)
          .some((element) => element.getAttribute?.("data-testid") === "user-message");
      }
      if (!hasLaterUser && this.isGenerationInProgress()) return null;

      const assistants = [...document.querySelectorAll('[data-testid="assistant-message"]')]
        .filter((element) => this.elementVisible(element));
      const assistantOrdinal = assistants.indexOf(assistantElement);
      if (assistantOrdinal < 0) return null;
      const assistantMessageId = assistantElement.getAttribute?.("data-message-id")
        || assistantElement.getAttribute?.("data-message-uuid")
        || `grok-dom-v1:${sessionId}:assistant:${assistantOrdinal}`;
      const assistantText = String(
        assistantElement.innerText || assistantElement.textContent || "",
      ).replace(/\s+/g, " ").trim();
      if (!assistantMessageId || !assistantText) return null;
      return {
        ok: true,
        currentNodeRole: "assistant",
        finished: true,
        messageId: assistantMessageId,
        userMessageId: acceptedUserMessageRef,
        text: assistantText,
      };
    }

    if (this.isGenerationInProgress()) return null;
    const user = this.getMessageSnapshot("user");
    const assistant = this.getMessageSnapshot("assistant");
    if (user.messageId !== acceptedUserMessageRef
        || !assistant.messageId
        || !assistant.text) {
      return null;
    }
    return {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: assistant.messageId,
      userMessageId: user.messageId,
      text: assistant.text,
    };
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
