// injector/chatgpt.js — chatgpt.com wake-up adapter
// Composer layouts: legacy #prompt-textarea and the id-less Project ProseMirror textbox.
//   - send button: button[data-testid="send-button"]
//   - insertion: MAIN-world execCommand insertText commits to the ProseMirror model
const CHATGPT_ADAPTER_CAPABILITIES = Object.freeze({
  browserActuation: true,
  stopGeneration: true,
  sessionCreate: true,
  sessionOpen: true,
  titleProjection: true,
  chatModeGuard: true,
});
const CHATGPT_ADAPTER_POLICY = Object.freeze({
  experimentalStorageFlag: null,
  operationalHud: true,
  submitAckTimeoutMs: 8000,
  jsonBridge: false,
});

class ChatGPTAdapter extends BaseAdapter {
  get name() { return "chatgpt"; }
  get capabilities() { return CHATGPT_ADAPTER_CAPABILITIES; }
  get policy() { return CHATGPT_ADAPTER_POLICY; }
  get needsMainWorldInsert() { return true; }

  getVisibleModeRadio(pattern) {
    return [...document.querySelectorAll('button[role="radio"]')].find((button) => {
      if (!this.elementVisible(button)) return false;
      const rect = button.getBoundingClientRect();
      const label = String(button.innerText || button.textContent || "").replace(/\s+/g, " ").trim();
      return rect.width > 0 && rect.height > 0 && pattern.test(label);
    }) || null;
  }

  async prepareBrowserActuation() {
    const chat = this.getVisibleModeRadio(/^(?:聊天|Chat|チャット)$/i);
    const work = this.getVisibleModeRadio(/^(?:工作|Work|作業)$/i);
    if (!chat && !work) return { ok: true, switched: false };
    if (!chat || !work) return { ok: false, error: "chat_mode_ambiguous" };
    if (chat.getAttribute("aria-checked") === "true") {
      return { ok: true, switched: false };
    }
    if (work.getAttribute("aria-checked") !== "true") {
      return { ok: false, error: "chat_mode_ambiguous" };
    }
    try {
      chat.click();
    } catch (_) {
      return { ok: false, error: "chat_mode_switch_failed" };
    }
    const deadline = Date.now() + 3000;
    do {
      if (chat.getAttribute("aria-checked") === "true"
          && work.getAttribute("aria-checked") === "false") {
        return { ok: true, switched: true };
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < deadline);
    return { ok: false, error: "chat_mode_switch_timeout" };
  }

  async getAccountNativeIdentity(timeoutMs = 1500) {
    for (const url of ["/backend-api/me", "/api/auth/session"]) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), timeoutMs);
      try {
        const response = await fetch(url, {
          method: "GET",
          credentials: "include",
          cache: "no-store",
          headers: { accept: "application/json" },
          signal: controller.signal,
        });
        if (!response.ok) continue;
        const payload = await response.json();
        const candidate = payload?.id || payload?.user?.id || payload?.account?.id || null;
        if (typeof candidate === "string" && candidate.trim()) return candidate.trim();
      } catch (_) {
      } finally {
        clearTimeout(timer);
      }
    }
    return null;
  }

  getConversationKey() {
    try {
      const origin = location.origin;
      const pathname = location.pathname.replace(/\/+$/, "") || "/";
      const provisionalChat = (segment) => /^local-chatgpt(?::|%3a)/i.test(segment);
      if (pathname === "/") return origin;
      const normal = pathname.match(/^\/c\/([^/]+)$/);
      if (normal) return provisionalChat(normal[1]) ? null : `${origin}/c/${normal[1]}`;

      const projectConversation = pathname.match(/^\/g\/(g-p-[^/]+)\/c\/([^/]+)$/i);
      const projectHome = pathname.match(/^\/g\/(g-p-[^/]+)(?:\/project)?$/i);
      if (projectConversation && provisionalChat(projectConversation[2])) return null;
      const projectSegment = projectConversation?.[1] || projectHome?.[1] || null;
      if (!projectSegment) return null;
      // ChatGPT may decorate a Project resource id with a human-readable slug.
      // Bindings intentionally normalize that cosmetic suffix away.
      const m = projectSegment.match(/^(g-p-[0-9a-f]{32})(?:-[^/]*)?$/i);
      const projectId = m ? m[1] : projectSegment;
      const projectKey = `${origin}/g/${projectId}`;
      return projectConversation ? `${projectKey}/c/${projectConversation[2]}` : projectKey;
    } catch (_) {
      return null;
    }
  }

  getInputEl() {
    const legacy = document.querySelector('#prompt-textarea[contenteditable="true"]');
    if (legacy) return legacy;
    const candidates = document.querySelectorAll('div.ProseMirror[role="textbox"][contenteditable="true"]');
    return candidates.length === 1 && this.elementVisible(candidates[0])
      ? candidates[0]
      : null;
  }

  getSelectedComposerApps() {
    const input = this.getInputEl();
    if (!input) return [];
    const selected = new Set();
    for (const pill of input.querySelectorAll('[data-inline-selection-pill][data-symbol="ecosystemMention"][data-keyword]')) {
      const keyword = String(pill.getAttribute('data-keyword') || '').trim().toLowerCase();
      if (keyword) selected.add(keyword);
    }
    return [...selected];
  }

  getLatestUserAppKeywords(latestUser = null) {
    const latest = latestUser || (() => {
      const turns = [...document.querySelectorAll('[data-message-author-role="user"]')];
      return turns[turns.length - 1] || null;
    })();
    if (!latest) return [];
    const selected = new Set();
    for (const pill of latest.querySelectorAll('[data-inline-selection-pill][data-symbol="ecosystemMention"][data-keyword]')) {
      const keyword = String(pill.getAttribute('data-keyword') || '').trim().toLowerCase();
      if (keyword) selected.add(keyword);
    }
    return [...selected];
  }

  getComposerTextWithoutAppPills() {
    const input = this.getInputEl();
    if (!input) return '';
    const clone = input.cloneNode(true);
    for (const node of clone.querySelectorAll('[data-inline-selection-pill], [data-inline-selection-pill-cursor-target]')) {
      node.remove();
    }
    return String(clone.textContent || '').replace(/\uFEFF/g, '').trim();
  }

  composerHasOnlyAppPills(requiredApps = []) {
    const requested = [...new Set(requiredApps.map((app) => String(app || '').trim().toLowerCase()).filter(Boolean))];
    const selected = this.getSelectedComposerApps();
    if (!requested.length || requested.some((app) => !selected.includes(app))) return false;
    return this.getComposerTextWithoutAppPills() === '';
  }

  openComposerAppsMenu() {
    const input = this.getInputEl();
    const scope = input?.closest?.('form') || document;
    const button = scope.querySelector('#composer-plus-btn, button[data-testid="composer-plus-btn"]');
    if (!button || button.disabled === true || button.getAttribute('aria-disabled') === 'true') return false;
    button.click();
    return true;
  }

  getComposerAppCandidates(keyword) {
    const wanted = String(keyword || '').trim().toLowerCase();
    const input = this.getInputEl();
    if (!wanted || !input) return [];
    const visible = (element) => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects?.().length));
    const menuRoots = [...document.querySelectorAll('.popover, [role="menu"], [role="listbox"]')]
      .filter((node) => visible(node) && !input.contains(node));
    if (!menuRoots.length) return [];
    const keywordMatches = (element) => {
      const nodes = [
        element,
        ...Array.from(element.querySelectorAll?.('[data-keyword], [data-value]') || []),
      ];
      return nodes.some((candidate) => [
        candidate?.getAttribute?.('data-keyword'),
        candidate?.getAttribute?.('data-value'),
      ].some((value) => String(value || '').trim().toLowerCase() === wanted));
    };
    const seen = new Set();
    const matches = [];
    const selectors = [
      '[role="option"]',
      '[role="menuitem"]',
      '[data-testid*="app"]',
      '[tabindex="0"]',
    ];
    for (const root of menuRoots) {
      for (const node of root.querySelectorAll(selectors.join(','))) {
        if (!visible(node) || input.contains(node) || seen.has(node)) continue;
        const text = [
          node.textContent,
          node.getAttribute('aria-label'),
          node.getAttribute('data-value'),
        ].filter(Boolean).join(' ').trim().toLowerCase();
        if (!keywordMatches(node) && !text.includes(wanted)) continue;
        seen.add(node);
        matches.push(node);
      }
    }
    return matches.sort((a, b) => {
      const score = (node) => {
        const role = node.getAttribute('role') || '';
        const testid = node.getAttribute('data-testid') || '';
        let value = 0;
        if (keywordMatches(node)) value += 100;
        if (role === 'option') value += 50;
        if (role === 'menuitem') value += 40;
        if (/app|connector|mention/i.test(testid)) value += 20;
        return value;
      };
      return score(b) - score(a);
    });
  }

  getWatchMainWorldSelector() {
    const input = this.getInputEl();
    if (!input) return null;
    return input.id === 'prompt-textarea'
      ? '#prompt-textarea[contenteditable="true"]'
      : 'div.ProseMirror[role="textbox"][contenteditable="true"]';
  }

  getSendButtonCandidates() {
    const selectors = [
      'button[data-testid="send-button"]',
      'button[data-testid="composer-send-button"]',
      'button[aria-label="发送提示"]',
      'button[aria-label="Send prompt"]',
      'button[aria-label*="发送提示"]',
      'button[aria-label*="Send prompt" i]',
      'button[aria-label*="发送"]',
      'button[aria-label*="Send" i]',
    ];
    const seen = new Set();
    const out = [];
    for (const sel of selectors) {
      for (const el of document.querySelectorAll(sel)) {
        if (!seen.has(el)) { seen.add(el); out.push(el); }
      }
    }
    return out;
  }

  getSendButton() {
    return this.getSendButtonCandidates()[0] || null;
  }

  getStopButtonCandidates() {
    const input = this.getInputEl();
    const scope = input?.closest?.("form") || input?.parentElement?.parentElement || document;
    const selectors = [
      'button[data-testid="stop-button"]',
      '[role="button"][data-testid="stop-button"]',
      'button[aria-label="Stop generating" i]',
      'button[aria-label="Stop streaming" i]',
      'button[aria-label="停止生成"]',
      'button[aria-label="停止流式"]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of selectors) {
      for (const el of scope.querySelectorAll?.(selector) || []) {
        if (!seen.has(el)) { seen.add(el); out.push(el); }
      }
    }
    return out;
  }

  getComposerActionAnchor() {
    const input = this.getInputEl();
    const scope = input?.closest?.("form") || input?.parentElement?.parentElement || document;
    const selectors = [
      'button[data-testid="send-button"]',
      'button[data-testid="composer-send-button"]',
      'button[data-testid="stop-button"]',
      'button[aria-label*="Stop" i]',
      'button[aria-label*="停止"]',
      'button.composer-submit-button-color',
    ];
    for (const selector of selectors) {
      const button = scope.querySelector?.(selector);
      if (button) return button.closest?.("div.inline-flex") || button;
    }
    const button = this.getSendButton();
    return button?.closest?.("div.inline-flex") || button;
  }
}

registerH2WAdapter(new ChatGPTAdapter());
