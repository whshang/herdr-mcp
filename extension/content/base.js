// base.js — 适配器基类 (从 ctmc 精简: 只保留"定位输入框/写入/发送/会话身份"能力)
// 本插件方向: herdr → 网页。插件只在收到 wake 时写入并提交,不做拦截/agent 循环。
class BaseAdapter {
  get name() { return "base"; }

  // ---- 站点差异声明 (子类必须覆盖) ----

  // 会话身份键: 用于绑定恢复 (页面刷新/浏览器重启后仍能定位同一会话)。
  // 默认: origin + pathname (去尾斜杠)。子类可覆盖为更精确的对话 ID。
  getConversationKey() {
    try {
      return location.origin + location.pathname.replace(/\/+$/, "");
    } catch { return null; }
  }

  // 输入框: 默认 textarea (z.ai / DeepSeek 均为 textarea)
  getInputEl() {
    return document.querySelector("textarea");
  }

  // 是否为 contenteditable 站点 (Claude/ChatGPT): 需要 MAIN world execCommand
  // 插入 (content script 隔离世界的 execCommand 只改 DOM,不会提交进编辑模型,
  // 实测提交时发出的是空文本 — ctmc 的 page_insert 教训)。
  get needsMainWorldInsert() { return false; }

  // MAIN world 插入目标选择器 (contenteditable 站点覆盖)
  getWatchMainWorldSelector() { return null; }

  // 发送: 默认聚焦 + 派发 Enter (实测 z.ai / DeepSeek 有效)
  // React 受控组件异步提交 value, fillInput 后立即 Enter 可能发空值 → 延迟 400ms
  send() {
    const ta = this.getInputEl();
    if (!ta) return false;
    ta.focus();
    setTimeout(() => {
      ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
      ta.dispatchEvent(new KeyboardEvent("keypress", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true }));
    }, 400);
    return true;
  }

  // 发送按钮 (contenteditable 站点覆盖; textarea 站点走 Enter 即可)
  getSendButton() { return null; }

  // ---- 公共实现 ----

  // 填入文本并触发输入事件 (React 受控组件需用原生 setter + 事件)
  fillInput(text) {
    const el = this.getInputEl();
    if (!el) return false;
    if (el.tagName === "TEXTAREA" || el.tagName === "INPUT") {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
      setter.call(el, text);
    } else {
      el.textContent = text;
    }
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  }

  // 当前输入框里已有内容? (提交前防覆盖用户正在输入的文字)
  inputHasContent() {
    const el = this.getInputEl();
    if (!el) return false;
    const t = el.value ?? el.textContent ?? "";
    return t.trim().length > 0;
  }
}

// ---- 权限弹窗自动允许的纯文本判定 (供 wake.js 使用; 保守 fail-closed) ----
// 只在"看起来像对话框/弹层"且文本含权限类字样时才考虑点击, 且只点明确的肯定
// 按钮 (允许/同意/授权/Allow/Approve/Grant), 绝不点拒绝/取消类。
// 注意: 只能处理页面内 (DOM) 权限弹窗; 浏览器原生权限条 (通知/麦克风/摄像头/
// 剪贴板) 不是页面 DOM, 扩展无法自动点击 — 平台硬限制。
const H2W_PERMISSION_DIALOG_RE = /(允许|授权|权限|同意|allow|permission|grant|approve)/i;
const H2W_ALLOW_BUTTON_RE = /^(ok|yes|continue)$/i;
const H2W_DENY_BUTTON_RE = /(拒绝|取消|不允许|deny|decline|block|no\b)/i;
function isPermissionDialogText(text) {
  return H2W_PERMISSION_DIALOG_RE.test(text || "");
}
function isAllowButtonText(text) {
  const t = String(text || "").trim();
  if (!t || t.length > 24) return false;
  if (H2W_DENY_BUTTON_RE.test(t)) return false;
  // 前缀肯定 (允许/同意并继续/Allow access…); 拒绝/取消/否定句已在上方排除
  if (/^(允许|同意|授权|allow|approve|grant)/i.test(t)) return true;
  if (H2W_ALLOW_BUTTON_RE.test(t)) return true; // 整词: ok/yes/continue
  return false;
}

window.__H2W_ADAPTER__ = null; // 子类实例挂这里 (同 ctmc 的 __WLLM_ADAPTER__)
