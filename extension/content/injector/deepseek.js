// injector/deepseek.js — DeepSeek adapter, selectors verified on 2026-08-03
const DEEPSEEK_ADAPTER_POLICY = Object.freeze({
  experimentalStorageFlag: "experimentalDeepSeekEnabled",
  operationalHud: true,
  submitAckTimeoutMs: 4000,
  jsonBridge: true,
});

class DeepSeekAdapter extends BaseAdapter {
  get name() { return "deepseek"; }
  get policy() { return DEEPSEEK_ADAPTER_POLICY; }

  get replySelector() {
    return ".ds-message .ds-assistant-message-main-content";
  }

  // DeepSeek uses textarea[name=search] as its composer.
  getInputEl() {
    return document.querySelector("textarea[name=search]") || document.querySelector("textarea");
  }

  // Conversation identity uses host plus pathname.
}

registerH2WAdapter(new DeepSeekAdapter());
