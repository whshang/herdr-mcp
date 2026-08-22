// base.js — adapter base class for locating, filling, submitting, and identifying chats
// Direction: herdr → web. The extension writes only on wake and does not run an agent loop.
class BaseAdapter {
  get name() { return "base"; }

  // ---- Site-specific declarations ----

  // Conversation key for restoring bindings after page refresh or browser restart.
  // Defaults to origin plus pathname without a trailing slash.
  getConversationKey() {
    try {
      return location.origin + location.pathname.replace(/\/+$/, "");
    } catch { return null; }
  }

  // Composer: textarea by default for z.ai and DeepSeek.
  getInputEl() {
    return document.querySelector("textarea");
  }

  // Contenteditable sites require MAIN-world execCommand insertion because the
  // isolated world changes the DOM without committing the editor model.
  get needsMainWorldInsert() { return false; }

  // MAIN-world insertion selector, overridden by contenteditable sites.
  getWatchMainWorldSelector() { return null; }

  // Submit by focusing and dispatching Enter. Delay for React-controlled value commits.
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

  // Send button for contenteditable sites; textarea sites use Enter.
  getSendButton() { return null; }

  // ---- Shared implementation ----

  // Fill text and dispatch input events using the native setter for controlled inputs.
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

  // Detect existing visible content to avoid overwriting user input.
  inputHasContent() {
    const el = this.getInputEl();
    if (!el) return false;
    const t = (el.value != null && el.tagName !== "DIV")
      ? el.value
      : (el.innerText || el.textContent || "");
    return String(t).replace(/\u200b/g, "").trim().length > 0;
  }
}

// ---- Fail-closed auto-allow for in-page permission dialogs ----
// Handle only explicit affirmative actions on in-page cards. Never click deny,
// cancel, dropdown, or unlabeled controls. Native browser permission bars are
// outside the page DOM and cannot be automated by the extension.
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
  // Accept affirmative prefixes after excluding denial and negation above.
  if (/^(允许|同意|授权|allow|approve|grant)/i.test(t)) return true;
  if (H2W_ALLOW_BUTTON_RE.test(t)) return true; // Whole words: ok/yes/continue.
  return false;
}
function isDenyButtonText(text) {
  const t = String(text || "").trim();
  if (!t) return false;
  return H2W_DENY_BUTTON_RE.test(t);
}

// ---- Locate the smallest permission card above an action button ----
// Supports classic dialogs and inline tool-action cards. Fail closed unless the
// card has permission text outside buttons and an explicit deny action in the
// same bounded action area. Click only a visible, enabled, explicitly labeled Allow.
const BUTTON_SELECTOR = "button, [role=button], [class*=btn]";

function qsa(root, sel) {
  if (!root || typeof root.querySelectorAll !== "function") return [];
  return [...root.querySelectorAll(sel)];
}
function buttonLabel(b) {
  return (b.innerText || b.textContent || (b.getAttribute && b.getAttribute("aria-label")) || "").trim();
}

// Extract element text while excluding button subtrees from card classification.
function isButtonLikeEl(el) {
  if (!el) return false;
  if (typeof el.matches === "function") { try { return el.matches(BUTTON_SELECTOR); } catch (e) {} }
  return false;
}
function textExcludingButtons(node) {
  if (!node) return "";
  if (node.nodeType === 3) return node.data || "";       // text
  if (node.nodeType !== 1 && node.nodeType !== 9) return ""; // Elements and documents only.
  if (isButtonLikeEl(node)) return "";                     // Skip button subtrees.
  let out = "";
  if (node.childNodes) for (const c of node.childNodes) out += textExcludingButtons(c);
  return out;
}
function nonButtonText(node) { return textExcludingButtons(node); }

// Require an explicit deny or cancel action.
function hasDenyButton(card) {
  return qsa(card, BUTTON_SELECTOR).some((b) => isDenyButtonText(buttonLabel(b)));
}

// Recognize classic dialog containers for fallback.
function isDialogContainer(el) {
  if (!el || el.nodeType !== 1) return false;
  const role = (typeof el.getAttribute === "function" && el.getAttribute("role")) || "";
  if (role === "dialog" || role === "alertdialog") return true;
  const cls = el.className || "";
  return /modal|dialog/i.test(cls);
}

// Find the smallest bounded action area containing this button and an explicit deny.
// Prefer ChatGPT's exact data-testid, then fall back to the nearest deny ancestor.
const TOOL_ACTION_BUTTONS_ID = "tool-action-buttons";
function actionAreaFor(btn) {
  // Pass 1: exact data-testid action area.
  const max = (btn.ownerDocument && (btn.ownerDocument.body || btn.ownerDocument.documentElement)) || null;
  for (let node = btn; node && node !== max; node = node.parentElement) {
    if (typeof node.getAttribute === "function" && node.getAttribute("data-testid") === TOOL_ACTION_BUTTONS_ID) {
      if (hasDenyButton(node)) return node;
      return null; // Do not expand an exact action area that lacks a deny action.
    }
  }
  // Pass 2: nearest ancestor containing a deny action.
  let node = btn.parentElement;
  while (node && node !== max) {
    if (hasDenyButton(node)) return node;
    node = node.parentElement;
  }
  return null;
}

// Exact path: bounded deny/allow action area with permission text above it.
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

// Generic fallback accepts only classic dialogs with permission text and a deny action.
function dialogCardForButton(btn) {
  let node = btn.parentElement;
  const max = (btn.ownerDocument && (btn.ownerDocument.body || btn.ownerDocument.documentElement)) || null;
  while (node && node !== max) {
    if (isDialogContainer(node) && isPermissionDialogText(nonButtonText(node)) && hasDenyButton(node)) return node;
    node = node.parentElement;
  }
  return null;
}

// Whether this button is safe to auto-click as Allow.
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
  // Exclude dropdown triggers.
  if (hasAttr && btn.hasAttribute("aria-haspopup")) return false;
  // Exclude aria labels that clearly indicate menus or more-actions controls.
  const aria = getAttr ? (btn.getAttribute("aria-label") || "").trim() : "";
  if (/menu|dropdown|option|more|选择|菜单|下拉|更多/i.test(aria)) return false;
  // Require explicit affirmative text.
  const label = buttonLabel(btn);
  if (!label) return false;
  return isAllowButtonText(label);
}

// Find a clickable Allow action and its permission card within root.
function findAllowAction(root) {
  const doc = root || document;
  const btns = qsa(doc, BUTTON_SELECTOR);
  // Pass 1: exact ChatGPT tool-action path.
  for (const b of btns) {
    if (!isClickableAllowButton(b)) continue;
    const card = preciseCardForButton(b);
    if (card) return { button: b, card };
  }
  // Pass 2: classic dialog fallback.
  for (const b of btns) {
    if (!isClickableAllowButton(b)) continue;
    const card = dialogCardForButton(b);
    if (card) return { button: b, card };
  }
  return null;
}

// Factory shared with wake.js; mark a button only after clicking it exactly once.
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

// Test hook for pure logic without Chrome globals.
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

window.__H2W_ADAPTER__ = null; // Subclasses attach their instance here.
