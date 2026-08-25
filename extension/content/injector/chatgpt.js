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
}

window.__H2W_ADAPTER__ = new ChatGPTAdapter();
