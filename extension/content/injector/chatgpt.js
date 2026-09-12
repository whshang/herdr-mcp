// injector/chatgpt.js — chatgpt.com wake-up adapter
// Selectors verified while signed in on 2026-08-20:
//   - composer: div#prompt-textarea[contenteditable="true"] (ProseMirror, role=textbox)
//   - send button: button[data-testid="send-button"]
//   - insertion: MAIN-world execCommand insertText commits to the ProseMirror model
class ChatGPTAdapter extends BaseAdapter {
  get name() { return "chatgpt"; }
  get needsMainWorldInsert() { return true; }

  getConversationKey() {
    try {
      const origin = location.origin;
      const pathname = location.pathname.replace(/\/+$/, "") || "/";
      if (pathname === "/") return origin;
      const normal = pathname.match(/^\/c\/([^/]+)$/);
      if (normal) return `${origin}/c/${normal[1]}`;

      const projectConversation = pathname.match(/^\/g\/(g-p-[^/]+)\/c\/([^/]+)$/i);
      const projectHome = pathname.match(/^\/g\/(g-p-[^/]+)(?:\/project)?$/i);
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
    return document.querySelector('#prompt-textarea[contenteditable="true"]');
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

  composerHasOnlyAppPills(requiredApps = []) {
    const input = this.getInputEl();
    if (!input) return false;
    const requested = [...new Set(requiredApps.map((app) => String(app || '').trim().toLowerCase()).filter(Boolean))];
    const selected = this.getSelectedComposerApps();
    if (!requested.length || requested.some((app) => !selected.includes(app))) return false;
    const clone = input.cloneNode(true);
    for (const node of clone.querySelectorAll('[data-inline-selection-pill], [data-inline-selection-pill-cursor-target]')) {
      node.remove();
    }
    return String(clone.textContent || '').replace(/\uFEFF/g, '').trim() === '';
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
    const seen = new Set();
    const matches = [];
    for (const leaf of document.querySelectorAll('span')) {
      if (!visible(leaf) || String(leaf.textContent || '').trim().toLowerCase() !== wanted) continue;
      const candidate = leaf.closest('[tabindex="0"]');
      if (!candidate || !visible(candidate) || input.contains(candidate) || seen.has(candidate)) continue;
      seen.add(candidate);
      matches.push(candidate);
    }
    return matches;
  }

  getWatchMainWorldSelector() {
    return '#prompt-textarea[contenteditable="true"]';
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

window.__H2W_ADAPTER__ = new ChatGPTAdapter();
