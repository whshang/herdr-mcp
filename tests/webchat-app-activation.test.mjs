// WebChat App activation: plain composer text must never count as a selected
// native App.
//
// The real extension implementation is exercised here:
//  - extension/content/base.js + extension/content/injector/chatgpt.js run in a
//    VM against a small DOM fake, so App identity comes from the shipped adapter.
//  - the activation gate `ensureRequiredComposerApps` is read from the shipped
//    extension/content/wake.js source and executed, so the refusal path under
//    test is the product code, not a re-implementation.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const baseSource = readFileSync(new URL("../extension/content/base.js", import.meta.url), "utf8");
const chatgptSource = readFileSync(new URL("../extension/content/injector/chatgpt.js", import.meta.url), "utf8");
const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

const COMPOSER_ID = "prompt-textarea";

// ---- Minimal DOM fake -------------------------------------------------------
// Supports the CSS subset the ChatGPT adapter and the activation gate use:
// tag, #id, .class, [attr], [attr="v"], [attr*="v"], and descendant combinators.

function parseCompound(part) {
  const compound = { tag: null, id: null, classes: [], attrs: [] };
  const pattern = /(^[a-zA-Z*]+)|(#[A-Za-z0-9_-]+)|(\.[A-Za-z0-9_:/\\-]+)|(\[[^\]]+\])/g;
  let consumed = 0;
  for (const match of part.matchAll(pattern)) {
    if (match.index !== consumed) return null;
    consumed = match.index + match[0].length;
    const token = match[0];
    if (token.startsWith("#")) compound.id = token.slice(1);
    else if (token.startsWith(".")) compound.classes.push(token.slice(1));
    else if (token.startsWith("[")) {
      const attrMatch = token.match(/^\[([A-Za-z0-9_:-]+)(?:([*^$]?=)"([^"]*)")?\]$/);
      if (!attrMatch) return null;
      compound.attrs.push({ name: attrMatch[1], op: attrMatch[2] || null, value: attrMatch[3] ?? null });
    } else compound.tag = token.toLowerCase();
  }
  return consumed === part.length ? compound : null;
}

function matchCompound(element, compound) {
  if (!compound) return false;
  if (compound.tag && compound.tag !== "*" && element.tagName.toLowerCase() !== compound.tag) return false;
  if (compound.id && element.id !== compound.id) return false;
  for (const className of compound.classes) {
    if (!String(element.getAttribute("class") || "").split(/\s+/).includes(className)) return false;
  }
  for (const { name, op, value } of compound.attrs) {
    const actual = element.getAttribute(name);
    if (actual === null) return false;
    if (op === "=" && actual !== value) return false;
    if (op === "*=" && !actual.includes(value)) return false;
    if (op === "^=" && !actual.startsWith(value)) return false;
  }
  return true;
}

function matchSelector(element, selector) {
  const trimmed = String(selector || "").trim();
  if (!trimmed) return false;
  for (const group of trimmed.split(",")) {
    const chain = group.trim().split(/\s+/).filter(Boolean);
    if (!chain.length) continue;
    if (!matchCompound(element, parseCompound(chain[chain.length - 1]))) continue;
    let node = element.parentNode;
    let index = chain.length - 2;
    while (index >= 0 && node) {
      if (matchCompound(node, parseCompound(chain[index]))) index -= 1;
      node = node.parentNode;
    }
    if (index < 0) return true;
  }
  return false;
}

class MiniText {
  constructor(text) {
    this.parentNode = null;
    this.isConnected = true;
    this.textContent = String(text);
  }
  cloneNode() { return new MiniText(this.textContent); }
  get parentElement() { return null; }
}

class MiniElement {
  constructor(document, tagName = "div") {
    this.tagName = String(tagName).toUpperCase();
    this.ownerDocument = document;
    this.parentNode = null;
    this.children = [];
    this.value = null;
    this.id = "";
    this.isConnected = true;
    this.visible = true;
    this.offsetWidth = 10;
    this.offsetHeight = 10;
    this.disabled = false;
    this._attrs = new Map();
    this._text = "";
    this.dispatched = [];
    this.clicked = 0;
    this.focused = 0;
    this.onClick = null;
    document.register(this);
  }
  get offsetParent() { return this.visible ? {} : null; }
  get parentElement() { return this.parentNode instanceof MiniElement ? this.parentNode : null; }
  get attributes() {
    return [...this._attrs.entries()].map(([name, value]) => ({ name, value }));
  }
  set textContent(value) {
    this.children = [];
    this._text = String(value);
  }
  get textContent() {
    if (this.children.length) return this.children.map((child) => child.textContent).join("");
    return this._text;
  }
  getAttribute(name) { return this._attrs.has(name) ? this._attrs.get(name) : null; }
  setAttribute(name, value) { this._attrs.set(name, String(value)); return this; }
  hasAttribute(name) { return this._attrs.has(name); }
  appendChild(child) {
    child.parentNode = this;
    this.children.push(child);
    return child;
  }
  appendText(text) { return this.appendChild(new MiniText(text)); }
  remove() {
    if (!this.parentNode) return;
    const index = this.parentNode.children.indexOf(this);
    if (index >= 0) this.parentNode.children.splice(index, 1);
    this.parentNode = null;
    this.isConnected = false;
  }
  contains(node) {
    let current = node;
    while (current) {
      if (current === this) return true;
      current = current.parentNode;
    }
    return false;
  }
  closest(selector) {
    let node = this;
    while (node) {
      if (node instanceof MiniElement && matchSelector(node, selector)) return node;
      node = node.parentNode;
    }
    return null;
  }
  cloneNode(deep = false) {
    const clone = new MiniElement(this.ownerDocument, this.tagName);
    for (const [name, value] of this._attrs) clone.setAttribute(name, value);
    clone.id = this.id;
    clone.visible = this.visible;
    clone._text = this._text;
    if (deep) {
      for (const child of this.children) clone.appendChild(child.cloneNode(true));
    }
    return clone;
  }
  descendants() {
    const out = [];
    for (const child of this.children) {
      if (child instanceof MiniElement) {
        out.push(child, ...child.descendants());
      }
    }
    return out;
  }
  querySelectorAll(selector) { return this.descendants().filter((node) => matchSelector(node, selector)); }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  getBoundingClientRect() {
    return this.visible ? { width: 120, height: 28, top: 0, left: 0 } : { width: 0, height: 0, top: 0, left: 0 };
  }
  getClientRects() { return this.visible ? [this.getBoundingClientRect()] : []; }
  scrollIntoView() {}
  focus() { this.focused += 1; this.ownerDocument.activeElement = this; }
  dispatchEvent(event) { this.dispatched.push(event.type); return true; }
  click() {
    this.clicked += 1;
    if (typeof this.onClick === "function") this.onClick();
  }
}

function createFakeDocument() {
  const registry = [];
  const document = {
    activeElement: null,
    visibilityState: "visible",
    documentElement: null,
    title: "ChatGPT",
    register(element) {
      registry.push(element);
      if (element.tagName === "BODY" && !document.body) {
        document.body = element;
        document.documentElement = element;
      }
    },
    createElement(tag) { return new MiniElement(document, tag); },
    hasFocus() { return true; },
    getElementById(id) {
      return registry.find((element) => element.id === id) || null;
    },
    querySelectorAll(selector) {
      return registry.filter((element) => element.tagName !== "BODY" && matchSelector(element, selector));
    },
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
  };
  document.body = new MiniElement(document, "body");
  document.documentElement = document.body;
  return document;
}

// ---- Shipped ChatGPT adapter under a DOM fake ------------------------------

function loadChatGptAdapter(document) {
  const context = vm.createContext({
    console,
    document,
    location: { origin: "https://chatgpt.com", pathname: "/" },
    window: null,
    getComputedStyle() {
      return { display: "block", visibility: "visible", opacity: "1", position: "static" };
    },
    setTimeout,
    KeyboardEvent: class KeyboardEvent {
      constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
    },
    Event: class Event {
      constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
    },
    chrome: { runtime: { lastError: undefined, sendMessage() {} } },
  });
  context.window = context;
  vm.runInContext(baseSource, context, { filename: "base.js" });
  vm.runInContext(chatgptSource, context, { filename: "injector/chatgpt.js" });
  const adapter = context.window.__H2W_ADAPTER__;
  assert.equal(adapter?.name, "chatgpt", "the shipped ChatGPT adapter must register");
  return adapter;
}

function createComposer(document) {
  const form = document.createElement("form");
  document.body.appendChild(form);
  const composer = document.createElement("div");
  composer.id = COMPOSER_ID;
  composer.setAttribute("contenteditable", "true");
  form.appendChild(composer);
  return { form, composer };
}

function verifiedAppPill(document, keyword) {
  const pill = document.createElement("span");
  pill.setAttribute("data-inline-selection-pill", "");
  pill.setAttribute("data-symbol", "ecosystemMention");
  pill.setAttribute("data-keyword", keyword);
  pill.appendText(keyword);
  return pill;
}

function inlineMention(document, keyword, { withPath = true } = {}) {
  const span = document.createElement("span");
  span.setAttribute("data-appearance", "inline-mention");
  span.setAttribute("app-mention-name", keyword);
  if (withPath) span.setAttribute("app-mention-path", `/app/${keyword}`);
  span.appendText(keyword);
  return span;
}

// ---- Shipped activation gate -----------------------------------------------

const GATE_START = "  async function ensureRequiredComposerApps(requiredApps) {";
const GATE_END = "\n  function captureSubmitAckBaseline(";

function loadActivationGate({ adapter, document, onTrustedMention, insertMainWorld }) {
  const start = wakeSource.indexOf(GATE_START);
  const end = wakeSource.indexOf(GATE_END, start);
  assert.ok(start >= 0 && end > start, "the App activation gate must be extractable from wake.js");
  const gate = wakeSource.slice(start, end);
  for (const token of ["required-app-not-found", "required-app-selection-not-observed", "required-app-ambiguous"]) {
    assert.ok(gate.includes(token), `the extracted gate must still report ${token}`);
  }

  let fakeNow = 1_000_000;
  const chrome = {
    runtime: {
      lastError: undefined,
      sendMessage(message, callback) {
        if (message?.type === "h2w_focus_own_tab") {
          callback?.({ ok: true });
          return;
        }
        if (message?.type === "h2w_type_trusted_mention") {
          if (message.mode !== "clear" && typeof onTrustedMention === "function") {
            onTrustedMention(String(message.keyword || ""));
          }
          callback?.({ ok: true });
          return;
        }
        callback?.({ ok: false, error: "unexpected-message" });
      },
    },
  };

  const context = vm.createContext({
    console,
    document,
    chrome,
    ADAPTER: adapter,
    Date: { now: () => fakeNow },
    Set,
    Array,
    String,
    Number,
    Math,
    JSON,
    window: null,
    PointerEvent: class PointerEvent {
      constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
    },
    MouseEvent: class MouseEvent {
      constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
    },
    async wait(ms) { fakeNow += Number(ms) || 0; },
    async clearComposer() { insertMainWorld("", adapter.getWatchMainWorldSelector(), false); },
    async clearAutomationSearch(app) {
      const text = adapter.getComposerTextWithoutAppPills().toLowerCase();
      if (text === `@${app}` || text === app) {
        insertMainWorld("", adapter.getWatchMainWorldSelector(), false);
      }
    },
    insertMainWorld,
  });
  context.window = context;
  const factory = vm.runInContext(
    `(function () {\n${gate}\nreturn ensureRequiredComposerApps;\n})()`,
    context,
    { filename: "wake-ensureRequiredComposerApps.js" },
  );
  assert.equal(typeof factory, "function");
  return factory;
}

// Real insertion stand-in: the background main-world insert replaces the editor
// text (replace) or appends to it, exactly as the editor model would commit.
function createMainWorldInserter(composer) {
  return (text, selector, append = false) => {
    if (!selector) return { ok: false, error: "no-selector" };
    if (append) composer.appendText(text);
    else composer.textContent = text;
    return { ok: true };
  };
}

test("user types an @ mention as plain text | Given no native App chip was selected | When the composer App identity is read | Then nothing is selected and the text is not an App activation", () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);
  composer.textContent = "@herdr send the report";

  assert.deepEqual([...adapter.getSelectedComposerApps()], []);
  assert.equal(adapter.getComposerTextWithoutAppPills(), "@herdr send the report");
  assert.equal(adapter.composerHasOnlyAppPills(["herdr"]), false);

  // A native pill is the only thing that selects an App.
  composer.textContent = "";
  composer.appendChild(verifiedAppPill(document, "herdr"));
  composer.appendText(" send the report");
  assert.deepEqual([...adapter.getSelectedComposerApps()], ["herdr"]);
  assert.equal(adapter.getComposerTextWithoutAppPills(), "send the report");
  assert.equal(adapter.composerHasOnlyAppPills(["herdr"]), false, "remaining draft text is not an App-only composer");
});

test("user sees mention-shaped provider DOM | Given an inline mention or pill missing its verified identity | When the composer App identity is read | Then the incomplete node is not counted as a selected App", () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);

  composer.appendChild(inlineMention(document, "herdr", { withPath: false }));
  assert.deepEqual([...adapter.getSelectedComposerApps()], [], "an inline mention without app-mention-path is unverified");

  composer.textContent = "";
  const keywordPillWithoutKeyword = verifiedAppPill(document, "herdr");
  keywordPillWithoutKeyword.setAttribute("data-keyword", "");
  composer.appendChild(keywordPillWithoutKeyword);
  assert.deepEqual([...adapter.getSelectedComposerApps()], [], "a pill without a data-keyword identity is unverified");

  composer.textContent = "";
  composer.appendChild(inlineMention(document, "herdr"));
  assert.deepEqual([...adapter.getSelectedComposerApps()], ["herdr"], "a provider-owned inline mention with a path is a real App");
});

test("user dispatches with a required App | Given the composer already holds plain @herdr text | When the activation gate runs | Then it refuses without counting the text as an App and leaves the draft untouched", async () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);
  composer.textContent = "@herdr";

  const insertMainWorld = createMainWorldInserter(composer);
  let trustedMentionTyped = false;
  const gate = loadActivationGate({
    adapter,
    document,
    insertMainWorld,
    onTrustedMention() { trustedMentionTyped = true; },
  });

  const result = await gate(["herdr"]);
  assert.equal(result.ok, false);
  assert.equal(result.error, "required-app-not-found");
  assert.deepEqual([...adapter.getSelectedComposerApps()], [], "plain text must never become a selected App");
  assert.equal(composer.textContent, "@herdr", "the user's draft must not be replaced or submitted");
  assert.equal(trustedMentionTyped, false, "no native mention typing may start over a real draft");
});

test("user dispatches with a required App | Given the native picker opens for the typed mention | When the single verified suggestion is activated | Then the gate reports the verified App readback", async () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);

  const insertMainWorld = createMainWorldInserter(composer);
  const gate = loadActivationGate({
    adapter,
    document,
    insertMainWorld,
    onTrustedMention(keyword) {
      composer.textContent = `@${keyword}`;
      // The provider renders one suggestion row outside the composer; clicking
      // it replaces the search text with the provider-owned App pill.
      const popover = document.createElement("div");
      popover.setAttribute("class", "popover");
      document.body.appendChild(popover);
      const option = document.createElement("div");
      option.setAttribute("role", "option");
      option.setAttribute("data-keyword", keyword);
      option.appendText(keyword);
      option.onClick = () => {
        composer.textContent = "";
        composer.appendChild(verifiedAppPill(document, keyword));
        popover.remove();
      };
      popover.appendChild(option);
    },
  });

  const result = await gate(["herdr"]);
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.deepEqual([...(result.apps || [])], ["herdr"]);
  assert.deepEqual([...adapter.getSelectedComposerApps()], ["herdr"]);
  assert.equal(adapter.getComposerTextWithoutAppPills(), "");
});

test("user dispatches with a required App | Given the typed mention never opens a native suggestion | When the activation gate runs | Then it refuses with bounded probe evidence and selects nothing", async () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);

  const insertMainWorld = createMainWorldInserter(composer);
  const gate = loadActivationGate({
    adapter,
    document,
    insertMainWorld,
    onTrustedMention(keyword) { composer.textContent = `@${keyword}`; },
  });

  const result = await gate(["herdr"]);
  assert.equal(result.ok, false);
  assert.equal(result.error, "required-app-not-found");
  assert.deepEqual([...adapter.getSelectedComposerApps()], []);
  assert.equal(result.app_selection_probe?.candidate_count, 0);
  assert.equal(result.app_selection_probe?.search_text_committed, true);
  assert.equal(composer.textContent, "", "the automation's own search text must be cleaned up");
});

test("user dispatches with a required App | Given two native suggestions match the keyword | When the activation gate tries to activate one | Then it refuses as ambiguous and selects nothing", async () => {
  const document = createFakeDocument();
  const adapter = loadChatGptAdapter(document);
  const { composer } = createComposer(document);

  const insertMainWorld = createMainWorldInserter(composer);
  const gate = loadActivationGate({
    adapter,
    document,
    insertMainWorld,
    onTrustedMention(keyword) {
      composer.textContent = `@${keyword}`;
      const popover = document.createElement("div");
      popover.setAttribute("class", "popover");
      document.body.appendChild(popover);
      for (let index = 0; index < 2; index += 1) {
        const option = document.createElement("div");
        option.setAttribute("role", "option");
        option.setAttribute("data-keyword", keyword);
        option.appendText(keyword);
        popover.appendChild(option);
      }
    },
  });

  const result = await gate(["herdr"]);
  assert.equal(result.ok, false);
  assert.equal(result.error, "required-app-ambiguous");
  assert.deepEqual([...adapter.getSelectedComposerApps()], []);
});
