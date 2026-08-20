// webmcp/speaks-json.js — SpeaksJSON 层: 解析网页 AI 输出
// 能力: ① JSON tool call 识别 (平衡括号扫描, 从 ctmc base.js 原样移植)
//       ② 回复完成判定 (z.ai / deepseek 站点选择器从 ctmc 抄录)
// 方向: 本插件 v1 只做"唤醒", 本层供唤醒后的投递确认 (replyStarted) 与
//       未来"网页 AI → herdr"反向打通 (ctmc 方向) 复用。
(function () {
  const ADAPTER = window.__H2W_ADAPTER__;
  if (!ADAPTER) { window.__H2W_SPEAKS_JSON__ = null; return; }

  // 站点差异表: 回复区选择器 + 完成判定 (抄自 ctmc content/adapters/{zai,deepseek}.js)
  const SITES = {
    "z.ai": {
      replySelector: "[class*=markdown], [class*=answer], [class*=message-content], [class*=prose]",
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

    // 取最新一条助手回复 (textContent 不受 display:none 影响 — ctmc 教训)
    getLatestReply() {
      if (!this.enabled) return "";
      const blocks = document.querySelectorAll(this.site.replySelector);
      if (!blocks.length) return "";
      return (blocks[blocks.length - 1].textContent || "").trim();
    }

    // 助手回复块数量 (投递确认用: 新回复 = 文本变化或块数增加)
    getReplyBlockCount() {
      if (!this.enabled) return 0;
      return document.querySelectorAll(this.site.replySelector).length;
    }

    // 最新一条回复是否已完成 (流式)
    isReplyDone() {
      if (!this.enabled) return true;
      const blocks = document.querySelectorAll(this.site.replySelector);
      if (!blocks.length) return false;
      return this.site.isReplyDone(blocks[blocks.length - 1]);
    }

    // 从文本提取所有 tool call JSON (ctmc 原样移植: 平衡括号扫描, 支持 {} 内嵌)
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
        if (end === -1) break;  // 未闭合 (流式半截), 停止
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
