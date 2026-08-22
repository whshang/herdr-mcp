// injector/chatgpt.js — chatgpt.com wake-up adapter
// Selectors verified while signed in on 2026-08-20:
//   - composer: div#prompt-textarea[contenteditable="true"] (ProseMirror, role=textbox)
//   - send button: button[data-testid="send-button"]
//   - insertion: MAIN-world execCommand insertText commits to the ProseMirror model
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
      [
        'button[data-testid="send-button"]',
        'button[data-testid="composer-send-button"]',
        'button[aria-label="发送提示"]',
        'button[aria-label="Send prompt"]',
        'button[aria-label*="发送提示"]',
        'button[aria-label*="Send prompt" i]',
        'button[aria-label*="发送"]',
        'button[aria-label*="Send" i]',
      ].join(", "),
    );
  }

  // Conversation identity uses host plus pathname via the default implementation.
}

window.__H2W_ADAPTER__ = new ChatGPTAdapter();
