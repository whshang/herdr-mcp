// webmcp/speaks-json.js — parse web AI output
// Capabilities: balanced-brace JSON tool-call extraction and reply completion detection.
// Version 1 uses this layer for post-wake delivery confirmation; it can later
// support the reverse web-AI-to-herdr direction.
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { window.__H2W_SPEAKS_JSON__ = null; return; }

  // Site-specific reply selectors and completion checks.
  const SITES = {
    "z.ai": {
      replySelector: ".markdown-prose, [class*=markdown], [class*=answer], [class*=message-content], [class*=prose]",
      isReplyDone(el) {
        const t = (el.innerText || "").toLowerCase();
        return !/thinking\.\.\.|generating|loading\.\.\.|思考中|生成中|加载中/.test(t);
      },
    },
    "deepseek": {
      replySelector: ".ds-assistant-message-main-content",
      isReplyDone(el) {
        const msg = el.closest(".ds-message");
        if (msg && /Thought for \d+/.test(msg.innerText)) return true;
        if (msg) {
          const hasCursor = !!msg.querySelector("[class*=cursor], [class*=blink], [class*=loading]");
          if (hasCursor) return false;
        }
        return true;
      },
    },
  };

  class SpeaksJson {
    constructor(siteKey) {
      this.site = SITES[siteKey] || null;
    }
    get enabled() { return !!this.site; }

    // Read the latest assistant reply; textContent is stable across display changes.
    getLatestReply() {
      if (!this.enabled) return "";
      const blocks = document.querySelectorAll(this.site.replySelector);
      if (!blocks.length) return "";
      return (blocks[blocks.length - 1].textContent || "").trim();
    }

    // Count reply blocks for delivery confirmation.
    getReplyBlockCount() {
      if (!this.enabled) return 0;
      return document.querySelectorAll(this.site.replySelector).length;
    }

    // Whether the latest streaming reply is complete.
    isReplyDone() {
      if (!this.enabled) return true;
      const blocks = document.querySelectorAll(this.site.replySelector);
      if (!blocks.length) return false;
      return this.site.isReplyDone(blocks[blocks.length - 1]);
    }

    // Extract all tool-call JSON objects with balanced-brace scanning.
    extractToolCalls(text) {
      if (!text) return [];
      const calls = [];
      let from = 0;
      while (from < text.length) {
        const start = text.indexOf('{"tool"', from);
        if (start === -1) break;
        let depth = 0, inStr = false, escape = false, end = -1;
        for (let i = start; i < text.length; i++) {
          const c = text[i];
          if (escape) { escape = false; continue; }
          if (c === "\\") { escape = true; continue; }
          if (c === '"') { inStr = !inStr; continue; }
          if (inStr) continue;
          if (c === "{") depth++;
          else if (c === "}") { depth--; if (depth === 0) { end = i + 1; break; } }
        }
        if (end === -1) break;  // Stop at an incomplete streaming fragment.
        const jsonStr = text.slice(start, end);
        try {
          const obj = JSON.parse(jsonStr);
          if (obj && obj.tool) calls.push({ tool: obj.tool, args: obj.args || {} });
        } catch (e) {}
        from = end;
      }
      return calls;
    }

    extractToolCall(text) {
      const calls = this.extractToolCalls(text);
      return calls.length ? calls[0] : null;
    }
  }

  window.__H2W_SPEAKS_JSON__ = new SpeaksJson(ADAPTER.name);
})();
