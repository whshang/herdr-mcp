// injector/deepseek.js — DeepSeek 适配器 (选择器从 ctmc 抄录, 2026-08-03 实测)
class DeepSeekAdapter extends BaseAdapter {
  get name() { return "deepseek"; }

  // DeepSeek 输入框是 textarea[name=search] (实测 2026-08-03)
  getInputEl() {
    return document.querySelector("textarea[name=search]") || document.querySelector("textarea");
  }

  // 会话身份: DeepSeek 对话 URL 形如 https://chat.deepseek.com/a/chat/s/<id> → host+pathname
}

window.__H2W_ADAPTER__ = new DeepSeekAdapter();
