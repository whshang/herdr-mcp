// injector/zai.js — z.ai adapter, selectors and insertion verified on 2026-08-03
class ZaiAdapter extends BaseAdapter {
  get name() { return "z.ai"; }

  get replySelector() {
    return ".markdown-prose, [class*=chat-assistant] [class*=markdown], [class*=chat-assistant] [class*=message-content], [class*=assistant] [class*=prose], [class*=message-content], [class*=prose]";
  }

  getConversationKey() {
    try {
      const path = location.pathname.replace(/\/+$/, "") || "";
      return `${location.origin}${path}`;
    } catch { return null; }
  }

  getLastMessageText(role) {
    const selector = role === "user"
      ? ".user-message, .chat-user"
      : this.replySelector;
    const nodes = [...document.querySelectorAll(selector)].filter((el) => this.elementVisible(el));
    const last = nodes[nodes.length - 1];
    return last ? String(last.innerText || last.textContent || "").trim() : "";
  }

  getSendButton() {
    return document.querySelector("#send-message-button:not(:disabled), form button[type=submit]:not(:disabled)");
  }

  send() {
    const button = this.getSendButton();
    if (button) {
      button.click();
      return true;
    }
    const form = this.getInputEl()?.closest("form");
    if (form && typeof form.requestSubmit === "function") {
      try { form.requestSubmit(); return true; } catch (_) {}
    }
    return super.send();
  }

  isReplyDone() {
    const blocks = document.querySelectorAll(this.replySelector);
    if (!blocks.length) return false;
    const last = blocks[blocks.length - 1];
    const text = String(last.textContent || "").toLowerCase();
    return !/thinking\.\.\.|generating|loading\.\.\.|思考中|生成中|加载中/.test(text);
  }

  // Current z.ai uses /c/<chat_id> for persisted chats and / as the new-chat
  // launcher. Keep the origin+path identity so background can migrate the
  // temporary root binding to the newly created chat id after first submit.
}

window.__H2W_ADAPTER__ = new ZaiAdapter();
