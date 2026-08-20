// injector/zai.js — z.ai 适配器 (选择器与写入套路从 ctmc 抄录, 2026-08-03 实测)
class ZaiAdapter extends BaseAdapter {
  get name() { return "z.ai"; }

  // 输入框: 默认 textarea (ctmc base 实测有效)
  // 会话身份: z.ai 对话 URL 形如 https://chat.z.ai/chat/s/<id> → host+pathname 即可
}

window.__H2W_ADAPTER__ = new ZaiAdapter();
