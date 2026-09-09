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
    }
    matches(selector) {
      if (selector === "#code-block-viewer.cm-editor") {
        return this.id === "code-block-viewer" && this.classList.contains("cm-editor");
      }
      if (selector === '#code-block-viewer.cm-editor[data-herdr-code-block-contained="1"]') {
        return this.matches("#code-block-viewer.cm-editor")
          && this.getAttribute("data-herdr-code-block-contained") === "1";
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
  const context = {
    console,
    performance,
    queueMicrotask,
    Node: FakeNode,
    Element: FakeElement,
    MutationObserver: FakeMutationObserver,
    document,
  };
  context.window = context;
  vm.createContext(context);
  const originalAppendChild = FakeNode.prototype.appendChild;
  vm.runInContext(source, context);
  return { context, document, FakeElement, mutationObservers, originalAppendChild };
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

test("ChatGPT perf v3 leaves Node.prototype.appendChild untouched", () => {
  const { context, originalAppendChild } = makeContext();
  assert.equal(context.Node.prototype.appendChild, originalAppendChild);
  assert.equal(context.__HERDR_CHATGPT_PERF__.version, "3");
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

test("ChatGPT perf MutationObserver updates intrinsic height while React streams code", () => {
  const { context, document, FakeElement, mutationObservers } = makeContext();
  const { viewer, code } = codeViewer(FakeElement, document, "a\nb");
  document.body.appendChild(viewer);

  const observer = mutationObservers[0];
  observer.trigger([{ target: document.body, addedNodes: [viewer] }]);
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "52px");

  code.textContent = Array.from({ length: 11 }, (_, i) => `line-${i}`).join("\n");
  observer.trigger([{ target: code, addedNodes: [] }]);
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "232px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_prepared, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_updated, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.observer_batches, 2);
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

test("ChatGPT perf disable removes active containment and enable restores it", () => {
  const { context, document, FakeElement } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, "a\nb");
  document.body.appendChild(viewer);
  context.__HERDR_CHATGPT_PERF__.scan();

  context.__HERDR_CHATGPT_PERF__.disable();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "");
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, false);

  context.__HERDR_CHATGPT_PERF__.enable();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), "1");
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, true);
});
