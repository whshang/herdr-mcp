// injector/deepseek.js — DeepSeek adapter, selectors verified on 2026-08-03
class DeepSeekAdapter extends BaseAdapter {
  get name() { return "deepseek"; }

  get replySelector() {
    return ".ds-message .ds-assistant-message-main-content";
  }

  // DeepSeek uses textarea[name=search] as its composer.
  getInputEl() {
    return document.querySelector("textarea[name=search]") || document.querySelector("textarea");
  }

  // Conversation identity uses host plus pathname.
}

window.__H2W_ADAPTER__ = new DeepSeekAdapter();
