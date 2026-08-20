// injector/chatgpt.js — chatgpt.com 适配器 (只做"唤醒")
// 选择器实测 (ego-browser, 2026-08-20, 已登录):
//   - 输入框: div#prompt-textarea[contenteditable="true"] (ProseMirror, role=textbox)
//   - 发送按钮: button[data-testid="send-button"] (aria-label="发送提示", 中文 locale)
//   - 写入: MAIN world execCommand insertText 实测能提交进 ProseMirror 模型
//     (content script 隔离世界不行 — ctmc 的 page_insert 教训)。
class ChatGPTAdapter extends BaseAdapter {
  get name() { return "chatgpt"; }
  get needsMainWorldInsert() { return true; }

  getInputEl() {
    return document.querySelector('#prompt-textarea[contenteditable="true"]');
  }

  getWatchMainWorldSelector() {
    return '#prompt-textarea[contenteditable="true"]';
  }

  getSendButton() {
    return document.querySelector(
      'button[data-testid="send-button"], button[aria-label*="发送"], button[aria-label*="Send" i]',
    );
  }

  // 会话身份: https://chatgpt.com/c/<conversation-id> → host+pathname (默认实现已覆盖)
}

window.__H2W_ADAPTER__ = new ChatGPTAdapter();
