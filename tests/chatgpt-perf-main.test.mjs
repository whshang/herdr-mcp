import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../extension/content/chatgpt-perf-main.js", import.meta.url), "utf8");

function makeContext({ external = false } = {}) {
  class ClassList {
    constructor(...names) { this.names = new Set(names); }
    contains(name) { return this.names.has(name); }
  }
  class FakeNode {
    constructor() {
      this.parentNode = null;
      this.ownerDocument = null;
      this.isConnected = false;
      this.children = [];
    }
    appendChild(child) {
      child.parentNode = this;
      child.isConnected = this.isConnected;
      this.children.push(child);
      return child;
    }
    get firstElementChild() { return this.children[0] || null; }
    get childElementCount() { return this.children.length; }
  }
  class FakeElement extends FakeNode {
    constructor(...classes) {
      super();
      this.classList = new ClassList(...classes);
    }
    get nextElementSibling() {
      if (!this.parentNode) return null;
      const index = this.parentNode.children.indexOf(this);
      return this.parentNode.children[index + 1] || null;
    }
  }
  const document = {};
  const context = {
    console,
    setTimeout,
    clearTimeout,
    performance,
    Node: FakeNode,
    Element: FakeElement,
    document,
  };
  context.window = context;
  if (external) context.__CHATGPT_CM_PERF_FIX__ = { version: "test" };
  vm.createContext(context);
  vm.runInContext(source, context);
  return { context, FakeNode, FakeElement, document };
}

function codeMirrorRoot(FakeElement, document) {
  const root = new FakeElement();
  root.ownerDocument = document;
  const announced = new FakeElement("cm-announced");
  announced.ownerDocument = document;
  const scroller = new FakeElement("cm-scroller");
  scroller.ownerDocument = document;
  root.appendChild(announced);
  root.appendChild(scroller);
  return root;
}

test("ChatGPT perf batches the exact uninitialized CodeMirror mount until the next task", async () => {
  const { context, FakeElement, document } = makeContext();
  const parent = new FakeElement();
  parent.ownerDocument = document;
  parent.isConnected = true;
  const editor = codeMirrorRoot(FakeElement, document);

  parent.appendChild(editor);
  assert.equal(editor.parentNode, null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.intercepted, 1);

  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(editor.parentNode, parent);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.mounted, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.batches, 1);
});

test("ChatGPT perf yields completely when the known userscript owns CodeMirror batching", () => {
  const { context, FakeNode, FakeElement, document } = makeContext({ external: true });
  const parent = new FakeElement();
  parent.ownerDocument = document;
  parent.isConnected = true;
  const editor = codeMirrorRoot(FakeElement, document);
  const original = FakeNode.prototype.appendChild;

  parent.appendChild(editor);
  assert.equal(editor.parentNode, parent);
  assert.equal(FakeNode.prototype.appendChild, original);
  assert.equal(context.__HERDR_CHATGPT_PERF__.external, true);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.intercepted, 0);
});
