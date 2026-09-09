import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../extension/content/chatgpt-perf-main.js", import.meta.url), "utf8");

function makeContext() {
  class ClassList {
    constructor(...names) { this.names = new Set(names); }
    add(name) { this.names.add(name); }
    contains(name) { return this.names.has(name); }
    [Symbol.iterator]() { return this.names[Symbol.iterator](); }
  }

  class StyleDecl {
    constructor() { this.values = new Map(); }
    setProperty(name, value) { this.values.set(name, String(value)); }
    getPropertyValue(name) { return this.values.get(name) || ""; }
    removeProperty(name) { this.values.delete(name); }
  }

  class FakeNode {
    constructor() {
      this.parentNode = null;
      this.ownerDocument = null;
      this.isConnected = false;
      this.children = [];
      this._text = "";
    }
    appendChild(child) {
      child.parentNode = this;
      child.ownerDocument ||= this.ownerDocument;
      child._setConnected(this.isConnected);
      this.children.push(child);
      return child;
    }
    _setConnected(value) {
      this.isConnected = value;
      for (const child of this.children) child._setConnected(value);
    }
    get parentElement() { return this.parentNode instanceof FakeElement ? this.parentNode : null; }
    get firstElementChild() { return this.children[0] || null; }
    get childElementCount() { return this.children.length; }
    set textContent(value) {
      this._text = String(value);
      this.children = [];
    }
    get textContent() {
      if (this.children.length) return this.children.map((child) => child.textContent).join("");
      return this._text;
    }
  }

  class FakeElement extends FakeNode {
    constructor(tagName = "div", ...classes) {
      super();
      this.tagName = tagName.toUpperCase();
      this.id = "";
      this.classList = new ClassList(...classes);
      this.attributes = new Map();
      this.style = new StyleDecl();
      this.dataset = {};
      this._height = 0;
    }
    setRectHeight(value) { this._height = Number(value); return this; }
    getBoundingClientRect() {
      return { x: 0, y: 0, width: 0, height: this._height, top: 0, right: 0, bottom: this._height, left: 0 };
    }
    matches(selector) {
      if (selector === "*") return true;
      if (/^[a-z]+$/i.test(selector)) return this.tagName === selector.toUpperCase();
      if (selector === "#code-block-viewer.cm-editor") {
        return this.id === "code-block-viewer" && this.classList.contains("cm-editor");
      }
      if (selector === '#code-block-viewer.cm-editor[data-herdr-code-block-contained="1"]') {
        return this.matches("#code-block-viewer.cm-editor")
          && this.getAttribute("data-herdr-code-block-contained") === "1";
      }
      if (selector === "[data-message-author-role]") {
        return this.getAttribute("data-message-author-role") !== null;
      }
      if (selector === '[data-message-author-role="assistant"]') {
        return this.getAttribute("data-message-author-role") === "assistant";
      }
      if (selector === '[data-message-author-role="user"]') {
        return this.getAttribute("data-message-author-role") === "user";
      }
      if (selector === '[data-message-author-role][data-herdr-message-contained="1"]') {
        return this.matches("[data-message-author-role]")
          && this.getAttribute("data-herdr-message-contained") === "1";
      }
      if (selector === '.ProseMirror[contenteditable="true"]') {
        return this.classList.contains("ProseMirror") && this.getAttribute("contenteditable") === "true";
      }
      if (selector === '[data-herdr-editable-block-contained="1"]') {
        return this.getAttribute("data-herdr-editable-block-contained") === "1";
      }
      return false;
    }
    closest(selector) {
      let node = this;
      while (node) {
        if (node instanceof FakeElement && node.matches(selector)) return node;
        node = node.parentNode;
      }
      return null;
    }
    contains(node) {
      if (node === this) return true;
      for (const child of this.children) {
        if (child === node) return true;
        if (child instanceof FakeElement && child.contains(node)) return true;
      }
      return false;
    }
    querySelectorAll(selector) {
      const out = [];
      const visit = (node) => {
        for (const child of node.children || []) {
          if (child instanceof FakeElement && child.matches(selector)) out.push(child);
          visit(child);
        }
      };
      visit(this);
      return out;
    }
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
    setAttribute(name, value) { this.attributes.set(name, String(value)); }
    getAttribute(name) { return this.attributes.get(name) ?? null; }
    removeAttribute(name) { this.attributes.delete(name); }
    remove() {
      if (!this.parentNode) return;
      this.parentNode.children = this.parentNode.children.filter((child) => child !== this);
      this.parentNode = null;
      this._setConnected(false);
    }
  }

  class FakeDocument extends FakeNode {
    constructor() {
      super();
      this.ownerDocument = this;
      this.isConnected = true;
      this.activeElement = null;
      this.listeners = new Map();
      this._querySelectorHook = null;
      this.documentElement = new FakeElement("html");
      this.documentElement.ownerDocument = this;
      this.documentElement._setConnected(true);
      this.head = new FakeElement("head");
      this.head.ownerDocument = this;
      this.documentElement.appendChild(this.head);
      this.body = new FakeElement("body");
      this.body.ownerDocument = this;
      this.documentElement.appendChild(this.body);
    }
    createElement(tagName) {
      const element = new FakeElement(tagName);
      element.ownerDocument = this;
      return element;
    }
    querySelectorAll(selector) {
      const out = [];
      if (this.documentElement.matches(selector)) out.push(this.documentElement);
      out.push(...this.documentElement.querySelectorAll(selector));
      return out;
    }
    querySelector(selector) {
      const hooked = this._querySelectorHook?.(selector);
      if (hooked) return hooked;
      return this.querySelectorAll(selector)[0] || null;
    }
    addEventListener(type, handler) {
      const list = this.listeners.get(type) || [];
      list.push(handler);
      this.listeners.set(type, list);
    }
    removeEventListener(type, handler) {
      this.listeners.set(type, (this.listeners.get(type) || []).filter((entry) => entry !== handler));
    }
    emit(type, target) {
      for (const handler of this.listeners.get(type) || []) handler({ target });
    }
  }

  const mutationObservers = [];
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.connected = false;
      mutationObservers.push(this);
    }
    observe() { this.connected = true; }
    disconnect() { this.connected = false; }
    trigger(records) {
      if (this.connected) this.callback(records, this);
    }
  }

  const document = new FakeDocument();
  let currentSelection = null;
  const context = {
    console,
    performance,
    queueMicrotask,
    Node: FakeNode,
    Element: FakeElement,
    MutationObserver: FakeMutationObserver,
    document,
    getSelection: () => currentSelection,
    requestAnimationFrame: (callback) => { callback(performance.now()); return 1; },
    setTimeout,
  };
  context.window = context;
  vm.createContext(context);
  const originalAppendChild = FakeNode.prototype.appendChild;
  vm.runInContext(source, context);
  return {
    context,
    document,
    FakeNode,
    FakeElement,
    mutationObservers,
    originalAppendChild,
    setSelection(value) { currentSelection = value; },
  };
}

function codeViewer(FakeElement, document, text, { wrapped = false } = {}) {
  const viewer = new FakeElement("div", "cm-editor", "q9tKkq_viewer", ...(wrapped ? ["q9tKkq_wrapLines"] : []));
  viewer.id = "code-block-viewer";
  viewer.ownerDocument = document;

  const scroller = new FakeElement("div", "cm-scroller");
  scroller.ownerDocument = document;
  const pre = new FakeElement("pre", "cm-content", "q9tKkq_readonly");
  pre.ownerDocument = document;
  const code = new FakeElement("code");
  code.ownerDocument = document;
  code.textContent = text;

  pre.appendChild(code);
  scroller.appendChild(pre);
  viewer.appendChild(scroller);
  return { viewer, code };
}

function message(FakeElement, document, role, height) {
  const root = new FakeElement("div").setRectHeight(height);
  root.ownerDocument = document;
  root.setAttribute("data-message-author-role", role);
  return root;
}

function editableWritingBlock(FakeElement, document, messageRoot, { height = 13440, blockHeight = 442, descendants = 9 } = {}) {
  const pm = new FakeElement("div", "ProseMirror").setRectHeight(height);
  pm.ownerDocument = document;
  pm.setAttribute("contenteditable", "true");
  const block = new FakeElement("ul").setRectHeight(blockHeight);
  block.ownerDocument = document;
  for (let i = 0; i < descendants; i += 1) {
    const li = new FakeElement("li");
    li.ownerDocument = document;
    block.appendChild(li);
  }
  pm.appendChild(block);
  messageRoot.appendChild(pm);
  return { pm, block };
}

test("ChatGPT perf v4 leaves Node.prototype.appendChild untouched", () => {
  const { context, originalAppendChild } = makeContext();
  assert.equal(context.Node.prototype.appendChild, originalAppendChild);
  assert.equal(context.__HERDR_CHATGPT_PERF__.version, "4");
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, true);
});

test("ChatGPT perf prepares the current React code viewer with measured intrinsic height", () => {
  const { context, document, FakeElement } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, Array.from({ length: 231 }, (_, i) => `line-${i}`).join("\n"));
  document.body.appendChild(viewer);

  assert.equal(context.__HERDR_CHATGPT_PERF__.scan(), 1);
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), "1");
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "4632px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_prepared, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.max_intrinsic_height_px, 4632);
});

test("ChatGPT perf MutationObserver resolves characterData targets back to the code viewer", () => {
  const { context, document, FakeNode, FakeElement, mutationObservers } = makeContext();
  const { viewer, code } = codeViewer(FakeElement, document, "a\nb");
  document.body.appendChild(viewer);

  const observer = mutationObservers[0];
  observer.trigger([{ target: document.body, addedNodes: [viewer] }]);
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "52px");

  const textNode = new FakeNode();
  textNode.ownerDocument = document;
  textNode.textContent = Array.from({ length: 11 }, (_, i) => `line-${i}`).join("\n");
  code.appendChild(textNode);
  observer.trigger([{ target: textNode, addedNodes: [] }]);

  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "232px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_updated, 1);
});

test("ChatGPT perf contains a large committed message using its exact measured height", () => {
  const { context, document, FakeElement } = makeContext();
  const root = message(FakeElement, document, "assistant", 13626.375);
  document.body.appendChild(root);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
  assert.equal(root.style.getPropertyValue("--herdr-message-intrinsic-size"), "13626.375px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.messages_prepared, 1);
});

test("ChatGPT perf does not add message containment to small turns", () => {
  const { context, document, FakeElement } = makeContext();
  const root = message(FakeElement, document, "assistant", 252);
  document.body.appendChild(root);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(root.getAttribute("data-herdr-message-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.messages_prepared, 0);
});

test("ChatGPT perf keeps the current streaming assistant message revealed until generation settles", () => {
  const { context, document, FakeElement } = makeContext();
  const root = message(FakeElement, document, "assistant", 1400);
  const stop = new FakeElement("button");
  document.body.appendChild(root);
  document._querySelectorHook = (selector) => selector.includes("stop-button") ? stop : null;

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(root.getAttribute("data-herdr-message-contained"), null);

  document._querySelectorHook = null;
  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
});

test("ChatGPT perf remeasures a contained message after its DOM changes", () => {
  const { context, document, FakeElement, mutationObservers } = makeContext();
  const root = message(FakeElement, document, "assistant", 1000);
  const child = new FakeElement("div");
  child.ownerDocument = document;
  root.appendChild(child);
  document.body.appendChild(root);
  context.__HERDR_CHATGPT_PERF__.scan();

  root.setRectHeight(1400.5);
  mutationObservers[0].trigger([{ target: child, addedNodes: [] }]);

  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
  assert.equal(root.style.getPropertyValue("--herdr-message-intrinsic-size"), "1400.5px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.messages_updated, 1);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.messages_cleared >= 1);
});

test("ChatGPT perf skips message containment for dynamic media", () => {
  const { context, document, FakeElement } = makeContext();
  const root = message(FakeElement, document, "assistant", 1800);
  const image = new FakeElement("img");
  image.ownerDocument = document;
  root.appendChild(image);
  document.body.appendChild(root);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(root.getAttribute("data-herdr-message-contained"), null);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.messages_skipped >= 1);
});

test("ChatGPT perf contains heavy writing-block children only while the editor is unfocused", () => {
  const { context, document, FakeElement } = makeContext();
  const root = message(FakeElement, document, "assistant", 13626.375);
  const { pm, block } = editableWritingBlock(FakeElement, document, root);
  document.body.appendChild(root);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  assert.equal(block.style.getPropertyValue("--herdr-editable-block-intrinsic-size"), "442px");
  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");

  document.activeElement = pm;
  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(root.getAttribute("data-herdr-message-contained"), null);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.editable_blocks_cleared >= 1);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.messages_cleared >= 1);
});

test("ChatGPT perf reveals a contained writing block while text inside it is selected", () => {
  const { context, document, FakeElement, setSelection } = makeContext();
  const root = message(FakeElement, document, "assistant", 13626.375);
  const { block } = editableWritingBlock(FakeElement, document, root);
  const leaf = block.firstElementChild;
  document.body.appendChild(root);
  context.__HERDR_CHATGPT_PERF__.scan();

  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");

  setSelection({ rangeCount: 1, isCollapsed: false, anchorNode: leaf, focusNode: leaf });
  document.emit("selectionchange", leaf);

  assert.equal(root.getAttribute("data-herdr-message-contained"), null);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);

  setSelection(null);
  document.emit("selectionchange", document.body);
  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
});

test("ChatGPT perf ignores unrelated cm-editor DOM", () => {
  const { context, document, FakeElement, mutationObservers } = makeContext();
  const editor = new FakeElement("div", "cm-editor");
  editor.ownerDocument = document;
  document.body.appendChild(editor);

  mutationObservers[0].trigger([{ target: document.body, addedNodes: [editor] }]);
  assert.equal(editor.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_prepared, 0);
});

test("ChatGPT perf skips wrapped viewers instead of guessing scroll geometry", () => {
  const { context, document, FakeElement } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, "a very long wrapped line", { wrapped: true });
  document.body.appendChild(viewer);

  assert.equal(context.__HERDR_CHATGPT_PERF__.scan(), 0);
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_skipped, 1);
});

test("ChatGPT perf disable removes every containment layer and enable restores safe ones", () => {
  const { context, document, FakeElement } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, "a\nb");
  const root = message(FakeElement, document, "assistant", 13626.375);
  const { block } = editableWritingBlock(FakeElement, document, root);
  document.body.appendChild(viewer);
  document.body.appendChild(root);
  context.__HERDR_CHATGPT_PERF__.scan();

  context.__HERDR_CHATGPT_PERF__.disable();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(root.getAttribute("data-herdr-message-contained"), null);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, false);

  context.__HERDR_CHATGPT_PERF__.enable();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), "1");
  assert.equal(root.getAttribute("data-herdr-message-contained"), "1");
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, true);
});
