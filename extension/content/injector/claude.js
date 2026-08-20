// injector/claude.js — claude.ai 适配器 (只做"唤醒": 定位输入框 → 写入 → 提交)
// ⚠️ 选择器未实测: 本机 claude.ai 未登录 (ego-browser 实测被重定向到 /login)。
//    使用防御性选择器链, 登录后需实测校准 (见 extension/README.md 待办)。
// 依据: claude.ai 输入区为 contenteditable 富文本编辑器 (ProseMirror/Quill 系),
//    需要 MAIN world execCommand 插入 (同 ChatGPT, content script 隔离世界不提交模型)。
class ClaudeAdapter extends BaseAdapter {
  get name() { return "claude.ai"; }
  get needsMainWorldInsert() { return true; }

  // 输入框: 防御性链 — ProseMirror / Quill / 通用 contenteditable textbox
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
    // 兜底: 第一个可见 contenteditable
    const all = [...document.querySelectorAll('[contenteditable="true"]')];
    return all.find((el) => el.offsetParent !== null) || all[0] || null;
  }

  getWatchMainWorldSelector() {
    const el = this.getInputEl();
    if (!el) return null;
    // 有 id → 精确选择器; 否则返回命中的链选择器 (background 的 MAIN 插入会取
    // 最后一个可见匹配, 输入框通常是页面上最后一个 contenteditable)
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

  // 发送按钮: data-testid / aria-label (中英) 链; 都找不到时 wake.js 回退 Enter
  getSendButton() {
    const chains = [
      'button[data-testid="send-button"]',
      'button[aria-label*="Send" i]',
      'button[aria-label*="发送"]',
    ];
    for (const sel of chains) {
      const el = document.querySelector(sel);
      if (el && !el.disabled) return el;
    }
    return null;
  }

  // 会话身份: claude.ai 对话 URL 形如 /chat/<id> 或 /project/<pid>/chat/<id> → host+pathname
}

window.__H2W_ADAPTER__ = new ClaudeAdapter();
