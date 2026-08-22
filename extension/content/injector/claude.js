// injector/claude.js — claude.ai wake-up adapter
// Selectors are defensive because signed-in behavior has not been verified locally.
// Claude uses a contenteditable rich-text editor, requiring MAIN-world insertion.
class ClaudeAdapter extends BaseAdapter {
  get name() { return "claude.ai"; }
  get needsMainWorldInsert() { return true; }

  // Defensive composer chain: ProseMirror, Quill, or generic contenteditable textbox.
  getInputEl() {
    const chains = [
      'div[contenteditable="true"][role="textbox"]',
      '.ProseMirror[contenteditable="true"]',
      '.ql-editor[contenteditable="true"]',
    ];
    for (const sel of chains) {
      const el = document.querySelector(sel);
      if (el && el.offsetParent !== null) return el;
    }
    // Fallback to the first visible contenteditable element.
    const all = [...document.querySelectorAll('[contenteditable="true"]')];
    return all.find((el) => el.offsetParent !== null) || all[0] || null;
  }

  getWatchMainWorldSelector() {
    const el = this.getInputEl();
    if (!el) return null;
    // Prefer an exact id; otherwise return the matched chain selector.
    if (el.id) return `#${CSS.escape(el.id)}[contenteditable="true"]`;
    const chains = [
      'div[contenteditable="true"][role="textbox"]',
      '.ProseMirror[contenteditable="true"]',
      '.ql-editor[contenteditable="true"]',
    ];
    for (const sel of chains) {
      if (document.querySelector(sel) === el) return sel;
    }
    return 'div[contenteditable="true"][role="textbox"]';
  }

  // Send button chain; wake.js falls back to Enter when none matches.
  getSendButtonCandidates() {
    const chains = [
      'button[data-testid="send-button"]',
      'button[aria-label*="Send" i]',
      'button[aria-label*="发送"]',
      'button[type="submit"]',
    ];
    const seen = new Set();
    const out = [];
    for (const sel of chains) {
      for (const el of document.querySelectorAll(sel)) {
        if (!seen.has(el)) { seen.add(el); out.push(el); }
      }
    }
    return out;
  }

  getSendButton() {
    return this.getSendButtonCandidates()[0] || null;
  }

  // Conversation identity uses host plus pathname for chat and project chat URLs.
}

window.__H2W_ADAPTER__ = new ClaudeAdapter();
