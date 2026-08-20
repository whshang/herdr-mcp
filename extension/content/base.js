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

// ---- 权限弹窗自动允许 (供 wake.js 使用; 保守 fail-closed) ----
// 方向: 只处理页面内 DOM 权限卡片/弹窗, 且只点明确的肯定按钮
// (允许/同意/授权/Allow/Approve/Grant), 拒绝/取消/下拉触发/无文本一律不点。
// 浏览器原生权限条 (通知/麦克风/摄像头/剪贴板) 不是页面 DOM, 扩展无法自动点击 —
// 平台硬限制。
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
function isDenyButtonText(text) {
  const t = String(text || "").trim();
  if (!t) return false;
  return H2W_DENY_BUTTON_RE.test(t);
}

// ---- DOM 结构判定: 从 action buttons 向上定位最小"权限卡片" ----
// 触发方式分两类:
//   1) 经典弹窗: [role=dialog]/[role=alertdialog]/modal 等容器;
//   2) 内嵌 tool-action 卡片 (ChatGPT 工具权限): 卡片含权限标题 + 说明,
//      下方按钮区同时有拒绝/取消按钮与明确"允许"主按钮, 右侧还可能有
//      aria-haspopup=menu 的下拉箭头。
// 保守 fail-closed: 必须同时满足 (a) 卡片非按钮文本是权限类字样, (b) 同一
// 卡片内存在明确拒绝按钮, 才考虑点击; 且只点可见/可用/明确文本的允许按钮。
// 绝不因为父容器 (含整个页面) 某处有"允许"就误点。
const BUTTON_SELECTOR = "button, [role=button], [class*=btn]";

function qsa(root, sel) {
  if (!root || typeof root.querySelectorAll !== "function") return [];
  return [...root.querySelectorAll(sel)];
}
function buttonLabel(b) {
  return (b.innerText || b.textContent || (b.getAttribute && b.getAttribute("aria-label")) || "").trim();
}

// 提取元素的文本, 但跳过按钮子树 (按钮的 "允许" 不能反过来把卡片判成权限类)。
function isButtonLikeEl(el) {
  if (!el) return false;
  if (typeof el.matches === "function") { try { return el.matches(BUTTON_SELECTOR); } catch (e) {} }
  return false;
}
function textExcludingButtons(node) {
  if (!node) return "";
  if (node.nodeType === 3) return node.data || "";       // text
  if (node.nodeType !== 1 && node.nodeType !== 9) return ""; // 仅元素/文档
  if (isButtonLikeEl(node)) return "";                     // 按钮子树跳过
  let out = "";
  if (node.childNodes) for (const c of node.childNodes) out += textExcludingButtons(c);
  return out;
}
function nonButtonText(node) { return textExcludingButtons(node); }

// 卡片需包含一个明确拒绝/取消按钮 (fail-closed 必要条件之一)。
function hasDenyButton(card) {
  return qsa(card, BUTTON_SELECTOR).some((b) => isDenyButtonText(buttonLabel(b)));
}

// 经典弹窗容器判定 (role=dialog/alertdialog 或 class 含 modal)。用于 fallback。
function isDialogContainer(el) {
  if (!el || el.nodeType !== 1) return false;
  const role = (typeof el.getAttribute === "function" && el.getAttribute("role")) || "";
  if (role === "dialog" || role === "alertdialog") return true;
  const cls = el.className || "";
  return /modal|dialog/i.test(cls);
}

// 最小祖先: 同时包含该按钮与一个明确拒绝按钮的**有界 action 区** (非 body/html)。
// 确保 deny 与主 allow 在同一个动作区内, 不允许从卡片外取 allow。
// 优先精确 data-testid=tool-action-buttons (ChatGPT 真实卡片), 仅当找不到该
// testid 时才回退到“最小含 deny 祖先”语义, 避免真实卡扩大到外层 deny。
const TOOL_ACTION_BUTTONS_ID = "tool-action-buttons";
function actionAreaFor(btn) {
  // pass 1: 精确 testid 按钮区 (自身或祖先带 data-testid=tool-action-buttons)。
  //   从允许按钮自身向上, 该区已含主 allow; 再确认有 deny 才算 (fail-closed)。
  const max = (btn.ownerDocument && (btn.ownerDocument.body || btn.ownerDocument.documentElement)) || null;
  for (let node = btn; node && node !== max; node = node.parentElement) {
    if (typeof node.getAttribute === "function" && node.getAttribute("data-testid") === TOOL_ACTION_BUTTONS_ID) {
      if (hasDenyButton(node)) return node;
      return null; // 找到了 testid 按钮区但无 deny → 不扩大, 直接拒
    }
  }
  // pass 2: 语义 fallback — 最小含 deny 的祖先 (旧 dialog/无 testid 站)
  let node = btn.parentElement;
  while (node && node !== max) {
    if (hasDenyButton(node)) return node;
    node = node.parentElement;
  }
  return null;
}

// 精确路径: 允许按钮需位于一个同时含拒绝按钮的有界 action 区, 且该区之上有
// 权限类非按钮文本 (area 自身也算)。allow 不取外部按钮。返回卡片或 null。
function preciseCardForButton(btn) {
  const area = actionAreaFor(btn);
  if (!area) return null;
  if (isPermissionDialogText(nonButtonText(area))) return area;
  let node = area.parentElement;
  const max = (btn.ownerDocument && (btn.ownerDocument.body || btn.ownerDocument.documentElement)) || null;
  while (node && node !== max) {
    if (isPermissionDialogText(nonButtonText(node))) return node;
    node = node.parentElement;
  }
  return null;
}

// 通用 fallback: 保留旧 dialog 识别 — 仅当按钮位于经典弹窗容器内且该容器含权限
// 文本与拒绝按钮时才接受。避免 body 大容器含其它 allow 时误点。
function dialogCardForButton(btn) {
  let node = btn.parentElement;
  const max = (btn.ownerDocument && (btn.ownerDocument.body || btn.ownerDocument.documentElement)) || null;
  while (node && node !== max) {
    if (isDialogContainer(node) && isPermissionDialogText(nonButtonText(node)) && hasDenyButton(node)) return node;
    node = node.parentElement;
  }
  return null;
}

// 该按钮是否可安全自动点击的"允许"按钮。
function isClickableAllowButton(btn) {
  if (!btn) return false;
  if (btn.isConnected === false) return false;
  const hasAttr = typeof btn.hasAttribute === "function";
  const getAttr = typeof btn.getAttribute === "function";
  // enabled
  if (btn.disabled === true || (hasAttr && btn.hasAttribute("disabled"))) return false;
  if (getAttr && btn.getAttribute("aria-disabled") === "true") return false;
  // visible
  if (btn.hidden === true || (hasAttr && btn.hasAttribute("hidden"))) return false;
  if (getAttr && btn.getAttribute("aria-hidden") === "true") return false;
  // 排除下拉触发按钮 (aria-haspopup=menu 的下拉箭头)
  if (hasAttr && btn.hasAttribute("aria-haspopup")) return false;
  // 排除 aria-label 明确是下拉/更多类触发 (非文本按钮, 而非"允许")
  const aria = getAttr ? (btn.getAttribute("aria-label") || "").trim() : "";
  if (/menu|dropdown|option|more|选择|菜单|下拉|更多/i.test(aria)) return false;
  // 需有明确允许文本
  const label = buttonLabel(btn);
  if (!label) return false;
  return isAllowButtonText(label);
}

// 在 root (默认 document) 内找一个可点允许按钮及其权限卡片。
// 优先精确路径 (有界 action 区内 deny+allow 同区), 再回退通用 dialog 识别。
function findAllowAction(root) {
  const doc = root || document;
  const btns = qsa(doc, BUTTON_SELECTOR);
  // pass 1: 精确路径 (ChatGPT tool-action card)
  for (const b of btns) {
    if (!isClickableAllowButton(b)) continue;
    const card = preciseCardForButton(b);
    if (card) return { button: b, card };
  }
  // pass 2: 通用 fallback (经典 role=dialog/alertdialog/modal 容器)
  for (const b of btns) {
    if (!isClickableAllowButton(b)) continue;
    const card = dialogCardForButton(b);
    if (card) return { button: b, card };
  }
  return null;
}

// 工厂: 暴露给 wake.js 复用, 保证 "恰好点击一次" 语义与生产一致。
// 序列: 找到可用按钮 → 未处理才继续 → 调用 click() → 成功返回后才记入 WeakSet。
// 只有实际找到并点击才标记, 卡片先出现/按钮后挂载时不会因提前标记而漏点。
function createPermissionClicker() {
  const clicked = new WeakSet();
  return {
    tryClick(root) {
      const found = findAllowAction(root);
      if (!found) return { handled: false };
      const btn = found.button;
      if (clicked.has(btn)) return { handled: false, duplicate: true };
      btn.click();
      clicked.add(btn);
      return { handled: true, button: btn };
    },
    isClicked(btn) { return clicked.has(btn); },
  };
}

// 测试 hook: 纯逻辑可独立单测 (不依赖 Chrome 全局)。
window.__H2W_PERMISSION__ = {
  isPermissionDialogText,
  isAllowButtonText,
  isDenyButtonText,
  buttonLabel,
  nonButtonText,
  hasDenyButton,
  isDialogContainer,
  actionAreaFor,
  TOOL_ACTION_BUTTONS_ID,
  preciseCardForButton,
  dialogCardForButton,
  isClickableAllowButton,
  findAllowAction,
  createPermissionClicker,
  BUTTON_SELECTOR,
};

window.__H2W_ADAPTER__ = null; // 子类实例挂这里 (同 ctmc 的 __WLLM_ADAPTER__)
