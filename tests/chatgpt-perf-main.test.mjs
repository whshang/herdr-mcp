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
    insertBefore(child, reference) {
      const index = this.children.indexOf(reference);
      if (index < 0) return this.appendChild(child);
      child.parentNode = this;
      child.ownerDocument ||= this.ownerDocument;
      child._setConnected(this.isConnected);
      this.children.splice(index, 0, child);
      return child;
    }
    _setConnected(value) {
      this.isConnected = value;
      for (const child of this.children) child._setConnected(value);
    }
    get parentElement() { return this.parentNode instanceof FakeElement ? this.parentNode : null; }
    get firstElementChild() { return this.children[0] || null; }
    get childElementCount() { return this.children.length; }
    set textContent(value) { this._text = String(value); this.children = []; }
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
      if (selector.includes(",")) return selector.split(",").some((part) => this.matches(part.trim()));
      if (/^[a-z]+$/i.test(selector)) return this.tagName === selector.toUpperCase();
      if (selector === '[class~="group/tool-message"]') {
        return this.classList.contains("group/tool-message");
      }
      if (selector === '[data-herdr-tool-cluster-observed="1"]') {
        return this.getAttribute("data-herdr-tool-cluster-observed") === "1";
      }
      if (selector === '[data-herdr-tool-cluster-hidden="1"]') {
        return this.getAttribute("data-herdr-tool-cluster-hidden") === "1";
      }
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
      if (selector === '.ProseMirror[contenteditable="true"]') {
        return this.classList.contains("ProseMirror") && this.getAttribute("contenteditable") === "true";
      }
      if (selector === '[data-herdr-editable-block-contained="1"]') {
        return this.getAttribute("data-herdr-editable-block-contained") === "1";
      }
      if (selector.includes('data-testid="stop-button"')) {
        return this.getAttribute("data-testid") === "stop-button";
      }
      if (selector.startsWith('button[aria-label="Stop generating"')) {
        return this.tagName === "BUTTON" && this.getAttribute("aria-label") === "Stop generating";
      }
      if (selector.startsWith('button[aria-label="Stop streaming"')) {
        return this.tagName === "BUTTON" && this.getAttribute("aria-label") === "Stop streaming";
      }
      if (selector === 'button[aria-label="停止生成"]') {
        return this.tagName === "BUTTON" && this.getAttribute("aria-label") === "停止生成";
      }
      if (selector === 'button[aria-label="停止流式"]') {
        return this.tagName === "BUTTON" && this.getAttribute("aria-label") === "停止流式";
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
      return this.children.some((child) => child === node || (child instanceof FakeElement && child.contains(node)));
    }
    querySelectorAll(selector) {
      this.ownerDocument?._noteElementQuery(selector);
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
    querySelector(selector) {
      if (selector === "img,video,iframe,canvas") {
        const tags = new Set(["IMG", "VIDEO", "IFRAME", "CANVAS"]);
        let found = null;
        const visit = (node) => {
          for (const child of node.children || []) {
            if (found) return;
            if (child instanceof FakeElement && tags.has(child.tagName)) { found = child; return; }
            visit(child);
          }
        };
        visit(this);
        return found;
      }
      return this.querySelectorAll(selector)[0] || null;
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
      this.activeElement = null;
      this.listeners = new Map();
      this._querySelectorHook = null;
      this.documentQueryCount = 0;
      this.elementQueryCount = 0;
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
    _noteElementQuery() { this.elementQueryCount += 1; }
    createElement(tagName) {
      const element = new FakeElement(tagName);
      element.ownerDocument = this;
      return element;
    }
    querySelectorAll(selector) {
      this.documentQueryCount += 1;
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
    emit(type, eventOrTarget) {
      const event = eventOrTarget && typeof eventOrTarget === "object" && "target" in eventOrTarget
        ? eventOrTarget
        : { target: eventOrTarget };
      for (const handler of this.listeners.get(type) || []) handler(event);
    }
  }

  const mutationObservers = [];
  class FakeMutationObserver {
    constructor(callback) { this.callback = callback; this.connected = false; mutationObservers.push(this); }
    observe() { this.connected = true; }
    disconnect() { this.connected = false; }
    trigger(records) { if (this.connected) this.callback(records, this); }
  }

  const intersectionObservers = [];
  class FakeIntersectionObserver {
    constructor(callback, options) {
      this.callback = callback;
      this.options = options;
      this.observed = new Set();
      this.connected = true;
      intersectionObservers.push(this);
    }
    observe(element) { if (this.connected) this.observed.add(element); }
    unobserve(element) { this.observed.delete(element); }
    disconnect() { this.connected = false; this.observed.clear(); }
    trigger(entries) { if (this.connected) this.callback(entries, this); }
  }

  const timers = new Map();
  const idleCallbacks = new Map();
  let nextTimer = 1;
  let nextIdle = 1000;
  function fakeSetTimeout(callback, delay = 0) {
    const id = nextTimer++;
    timers.set(id, { callback, delay });
    return id;
  }
  function fakeClearTimeout(id) { timers.delete(id); }
  function fakeRequestIdleCallback(callback) {
    const id = nextIdle++;
    idleCallbacks.set(id, callback);
    return id;
  }
  function fakeCancelIdleCallback(id) { idleCallbacks.delete(id); }

  const document = new FakeDocument();
  let currentSelection = null;
  const context = {
    console,
    performance,
    Node: FakeNode,
    Element: FakeElement,
    MutationObserver: FakeMutationObserver,
    IntersectionObserver: FakeIntersectionObserver,
    document,
    getSelection: () => currentSelection,
    setTimeout: fakeSetTimeout,
    clearTimeout: fakeClearTimeout,
    requestIdleCallback: fakeRequestIdleCallback,
    cancelIdleCallback: fakeCancelIdleCallback,
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
    intersectionObservers,
    originalAppendChild,
    setSelection(value) { currentSelection = value; },
    runAllTimers() {
      for (const [id, item] of Array.from(timers)) {
        timers.delete(id);
        item.callback();
      }
    },
    runAllIdle() {
      for (const [id, callback] of Array.from(idleCallbacks)) {
        idleCallbacks.delete(id);
        callback({ didTimeout: false, timeRemaining: () => 50 });
      }
    },
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

function assistantWritingRoot(FakeElement, document) {
  const message = new FakeElement("div");
  message.ownerDocument = document;
  message.setAttribute("data-message-author-role", "assistant");
  const root = new FakeElement("div", "ProseMirror");
  root.ownerDocument = document;
  root.setAttribute("contenteditable", "true");
  const block = new FakeElement("div");
  block.ownerDocument = document;
  const leaf = new FakeElement("span");
  leaf.ownerDocument = document;
  block.appendChild(leaf);
  root.appendChild(block);
  message.appendChild(root);
  return { message, root, block, leaf };
}

function toolClusterTree(FakeElement, document, { tools = 8, outerTools = 0 } = {}) {
  const outer = new FakeElement("div");
  outer.ownerDocument = document;
  const cluster = new FakeElement("div", "flex", "max-w-full", "flex-col", "gap-4", "grow");
  cluster.ownerDocument = document;
  const toolNodes = [];
  for (let i = 0; i < tools; i += 1) {
    const wrapper = new FakeElement("span", "group/tool-message");
    wrapper.ownerDocument = document;
    const button = new FakeElement("button");
    button.ownerDocument = document;
    wrapper.appendChild(button);
    cluster.appendChild(wrapper);
    toolNodes.push(wrapper);
  }
  outer.appendChild(cluster);
  for (let i = 0; i < outerTools; i += 1) {
    const wrapper = new FakeElement("span", "group/tool-message");
    wrapper.ownerDocument = document;
    outer.appendChild(wrapper);
  }
  return { outer, cluster, toolNodes };
}

function farEntry(target, height = 442) {
  return { target, isIntersecting: false, boundingClientRect: { height } };
}

function nearEntry(target, height = 442) {
  return { target, isIntersecting: true, boundingClientRect: { height } };
}

test("v8 keeps React mutation hot path free of synchronous layout reads", () => {
  const { context, originalAppendChild } = makeContext();
  assert.equal(context.__HERDR_CHATGPT_PERF__.version, "8");
  assert.equal(context.Node.prototype.appendChild, originalAppendChild);
  assert.equal(source.includes("getBoundingClientRect"), false);
  assert.equal(source.includes('querySelectorAll?.("*")'), false);
});

test("v8 does not install the rejected streaming style throttle", () => {
  assert.equal(source.includes("data-herdr-streaming-throttle"), false);
  assert.equal(source.includes("transition-duration: 0.001ms"), false);
  assert.equal(source.includes("streaming_throttle_activations"), false);
});

test("v8 preserves the proven code-viewer intrinsic estimate", () => {
  const { context, document, FakeElement } = makeContext();
  const text = Array.from({ length: 231 }, (_, i) => `line-${i}`).join("\n");
  const { viewer } = codeViewer(FakeElement, document, text);
  document.body.appendChild(viewer);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), "1");
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "4632px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_prepared, 1);
});

test("streaming code mutation reveals immediately and only remeasures on settled scan", () => {
  const { context, document, FakeNode, FakeElement, mutationObservers } = makeContext();
  const { viewer, code } = codeViewer(FakeElement, document, "a\nb");
  document.body.appendChild(viewer);
  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "52px");

  const text = new FakeNode();
  text.ownerDocument = document;
  text.textContent = Array.from({ length: 11 }, (_, i) => `line-${i}`).join("\n");
  code.appendChild(text);
  mutationObservers[0].trigger([{ target: text, addedNodes: [], removedNodes: [] }]);

  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_updated, 0);

  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(viewer.style.getPropertyValue("--herdr-code-block-intrinsic-size"), "232px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_updated, 1);
});

test("MutationObserver registers a direct writing root without a document-wide scan", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  const before = document.documentQueryCount;

  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);

  assert.equal(document.documentQueryCount, before);
  assert.equal(intersectionObservers[0].observed.has(block), true);
});

test("far heavy writing block is contained from asynchronous observer geometry", () => {
  const { context, document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);

  intersectionObservers[0].trigger([farEntry(block, 442.375)]);

  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  assert.equal(block.style.getPropertyValue("--herdr-editable-block-intrinsic-size"), "442.375px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.editable_blocks_contained, 1);
});

test("near writing block is always revealed", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);
  intersectionObservers[0].trigger([farEntry(block, 442)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");

  intersectionObservers[0].trigger([nearEntry(block, 442)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
});

test("focused writing root is never contained", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  document.activeElement = root;
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);
  intersectionObservers[0].trigger([farEntry(block, 900)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
});

test("selection change reveals the touched writing root without global rescan", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers, setSelection } = makeContext();
  const { message, root, block, leaf } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);
  intersectionObservers[0].trigger([farEntry(block, 442)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  const before = document.documentQueryCount;

  setSelection({ rangeCount: 1, isCollapsed: false, anchorNode: leaf, focusNode: leaf });
  document.emit("selectionchange", leaf);

  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(document.documentQueryCount, before);
});

test("current streaming assistant root remains revealed", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  const stop = new FakeElement("button");
  document.body.appendChild(message);
  document._querySelectorHook = (selector) => selector.includes("stop-button") ? stop : null;
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);

  intersectionObservers[0].trigger([farEntry(block, 900)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
});

test("dynamic media blocks are skipped", () => {
  const { context, document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  const image = new FakeElement("img");
  image.ownerDocument = document;
  block.appendChild(image);
  document.body.appendChild(message);
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);

  intersectionObservers[0].trigger([farEntry(block, 900)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.editable_blocks_contained, 0);
});

test("mutation of a contained block reveals it without forcing layout or deep document scan", () => {
  const { document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { message, root, block, leaf } = assistantWritingRoot(FakeElement, document);
  document.body.appendChild(message);
  mutationObservers[0].trigger([{ target: message, addedNodes: [root], removedNodes: [] }]);
  intersectionObservers[0].trigger([farEntry(block, 442)]);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  block.getBoundingClientRect = () => { throw new Error("hot-path layout read"); };
  const before = document.documentQueryCount;

  mutationObservers[0].trigger([{ target: leaf, addedNodes: [], removedNodes: [] }]);

  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(document.documentQueryCount, before);
});

test("manual/idle scan can discover a deeply nested writing root without layout reads", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const wrapper = new FakeElement("section");
  wrapper.ownerDocument = document;
  const { message, block } = assistantWritingRoot(FakeElement, document);
  wrapper.appendChild(message);
  document.body.appendChild(wrapper);

  const discovered = context.__HERDR_CHATGPT_PERF__.scan();
  assert.ok(discovered >= 1);
  assert.equal(intersectionObservers[0].observed.has(block), true);
});


test("v8 discovers only the minimal heavy tool cluster", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8, outerTools: 4 });
  document.body.appendChild(outer);

  context.__HERDR_CHATGPT_PERF__.scan();

  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  assert.ok(toolObserver);
  assert.equal(toolObserver.observed.has(cluster), true);
  assert.equal(toolObserver.observed.has(outer), false);
  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-observed"), "1");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_clusters_observed, 1);
});

test("v8 does not merge separate small tool groups into a conversation-wide cluster", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const outer = new FakeElement("div", "qMYqUG_convSearchResultHighlightRoot");
  outer.ownerDocument = document;
  for (let group = 0; group < 3; group += 1) {
    const local = new FakeElement("div", "flex", "max-w-full", "flex-col", "gap-4", "grow");
    local.ownerDocument = document;
    for (let i = 0; i < 3; i += 1) {
      const tool = new FakeElement("span", "group/tool-message");
      tool.ownerDocument = document;
      local.appendChild(tool);
    }
    outer.appendChild(local);
  }
  document.body.appendChild(outer);

  context.__HERDR_CHATGPT_PERF__.scan();

  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  assert.ok(toolObserver);
  assert.equal(toolObserver.observed.size, 0);
  assert.equal(outer.getAttribute("data-herdr-tool-cluster-observed"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_clusters_observed, 0);
});

test("v8 forces discovery when continuous mutations never become quiet", () => {
  const {
    context,
    document,
    FakeElement,
    mutationObservers,
    intersectionObservers,
    runAllTimers,
    runAllIdle,
  } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);

  for (let i = 0; i < 5; i += 1) {
    mutationObservers[0].trigger([{ target: document.body, addedNodes: [outer], removedNodes: [] }]);
  }
  runAllTimers();
  runAllIdle();

  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  assert.ok(toolObserver);
  assert.equal(toolObserver.observed.has(cluster), true);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.forced_scans >= 1);
});

test("v8 discovers a heavy tool cluster directly from its mutation batch", () => {
  const { context, document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);

  mutationObservers[0].trigger([{ target: document.body, addedNodes: [outer], removedNodes: [] }]);

  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  assert.ok(toolObserver);
  assert.equal(toolObserver.observed.has(cluster), true);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.idle_scans, 0);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_discovery_batches, 1);
});

test("v8 hides a far heavy tool cluster with observer-provided exact height", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);
  context.__HERDR_CHATGPT_PERF__.scan();
  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");

  toolObserver.trigger([farEntry(cluster, 2761.5)]);

  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-hidden"), "1");
  assert.equal(cluster.style.getPropertyValue("--herdr-tool-cluster-intrinsic-size"), "2761.5px");
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_clusters_hidden, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.max_tool_cluster_height_px, 2761.5);
});

test("v8 reveals a hidden tool cluster when it reaches the viewport", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);
  context.__HERDR_CHATGPT_PERF__.scan();
  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  toolObserver.trigger([farEntry(cluster, 2761.5)]);

  toolObserver.trigger([nearEntry(cluster, 2761.5)]);

  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-hidden"), null);
  assert.ok(context.__HERDR_CHATGPT_PERF__.stats.tool_clusters_revealed >= 1);
});

test("v8 reveals and unobserves a mutating hidden tool cluster without layout reads", () => {
  const { context, document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { outer, cluster, toolNodes } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);
  context.__HERDR_CHATGPT_PERF__.scan();
  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  toolObserver.trigger([farEntry(cluster, 2761.5)]);
  cluster.getBoundingClientRect = () => { throw new Error("hot-path layout read"); };

  mutationObservers[0].trigger([{ target: toolNodes[0], addedNodes: [], removedNodes: [] }]);

  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-hidden"), null);
  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-observed"), null);
  assert.equal(toolObserver.observed.has(cluster), false);
});

test("v8 suspends tool hiding for find-in-page", () => {
  const { context, document, FakeElement, intersectionObservers } = makeContext();
  const { outer, cluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(outer);
  context.__HERDR_CHATGPT_PERF__.scan();
  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  toolObserver.trigger([farEntry(cluster, 2761.5)]);
  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-hidden"), "1");

  document.emit("keydown", { target: document.body, key: "f", metaKey: true, ctrlKey: false });
  toolObserver.trigger([farEntry(cluster, 2761.5)]);

  assert.equal(cluster.getAttribute("data-herdr-tool-cluster-hidden"), null);
});

test("settled ChatGPT tool runs fold to one count and expand without moving reply nodes", () => {
  const { context, document, FakeElement } = makeContext();
  const stack = new FakeElement("div");
  stack.ownerDocument = document;
  const firstWrap = new FakeElement("div", "contents");
  firstWrap.ownerDocument = document;
  const firstTool = new FakeElement("span", "group/tool-message");
  firstTool.ownerDocument = document;
  firstWrap.appendChild(firstTool);
  const inert = new FakeElement("div", "contents");
  inert.ownerDocument = document;
  const secondWrap = new FakeElement("div", "contents");
  secondWrap.ownerDocument = document;
  const secondTool = new FakeElement("span", "group/tool-message");
  secondTool.ownerDocument = document;
  secondWrap.appendChild(secondTool);
  const reply = new FakeElement("div");
  reply.ownerDocument = document;
  reply.setAttribute("data-message-author-role", "assistant");
  reply.textContent = "final reply";
  stack.appendChild(firstWrap);
  stack.appendChild(inert);
  stack.appendChild(secondWrap);
  stack.appendChild(reply);
  document.body.appendChild(stack);

  context.__HERDR_CHATGPT_PERF__.scan();

  const summary = stack.children.find((child) => child.getAttribute?.("data-herdr-tool-run-summary") === "1");
  assert.ok(summary);
  assert.equal(summary.textContent, "+ Tool calls × 2");
  assert.equal(firstWrap.getAttribute("data-herdr-tool-run-hidden"), "1");
  assert.equal(secondWrap.getAttribute("data-herdr-tool-run-hidden"), "1");
  assert.ok(stack.children.indexOf(summary) < stack.children.indexOf(firstWrap));
  assert.ok(stack.children.indexOf(secondWrap) < stack.children.indexOf(reply));
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_runs_folded, 1);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_run_messages_hidden, 2);

  document.emit("click", summary);
  assert.equal(summary.getAttribute("aria-expanded"), "true");
  assert.equal(firstWrap.getAttribute("data-herdr-tool-run-hidden"), null);
  assert.equal(secondWrap.getAttribute("data-herdr-tool-run-hidden"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_runs_expanded, 1);
});

test("streaming ChatGPT tool runs fold on the second card and grow incrementally", () => {
  const { context, document, FakeElement, mutationObservers } = makeContext();
  const stack = new FakeElement("div");
  stack.ownerDocument = document;
  const reply = new FakeElement("div");
  reply.ownerDocument = document;
  reply.setAttribute("data-message-author-role", "assistant");
  reply.textContent = "working";
  const stop = new FakeElement("button");
  document._querySelectorHook = (selector) => selector.includes("stop-button") ? stop : null;

  const toolWrap = () => {
    const wrap = new FakeElement("div", "contents");
    wrap.ownerDocument = document;
    const tool = new FakeElement("span", "group/tool-message");
    tool.ownerDocument = document;
    wrap.appendChild(tool);
    return wrap;
  };

  const firstWrap = toolWrap();
  stack.appendChild(firstWrap);
  stack.appendChild(reply);
  document.body.appendChild(stack);
  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(stack.children.some((child) => child.getAttribute?.("data-herdr-tool-run-summary") === "1"), false);

  const secondWrap = toolWrap();
  stack.insertBefore(secondWrap, reply);
  mutationObservers[0].trigger([{ target: stack, addedNodes: [secondWrap], removedNodes: [] }]);

  const summary = stack.children.find((child) => child.getAttribute?.("data-herdr-tool-run-summary") === "1");
  assert.ok(summary);
  assert.equal(summary.textContent, "+ Tool calls × 2");
  assert.equal(firstWrap.getAttribute("data-herdr-tool-run-hidden"), "1");
  assert.equal(secondWrap.getAttribute("data-herdr-tool-run-hidden"), "1");

  const thirdWrap = toolWrap();
  stack.insertBefore(thirdWrap, reply);
  mutationObservers[0].trigger([{ target: stack, addedNodes: [thirdWrap], removedNodes: [] }]);
  assert.equal(summary.textContent, "+ Tool calls × 3");
  assert.equal(thirdWrap.getAttribute("data-herdr-tool-run-hidden"), "1");
  assert.equal(stack.children.filter((child) => child.getAttribute?.("data-herdr-tool-run-summary") === "1").length, 1);

  document.emit("click", summary);
  assert.equal(summary.getAttribute("aria-expanded"), "true");
  const fourthWrap = toolWrap();
  stack.insertBefore(fourthWrap, reply);
  mutationObservers[0].trigger([{ target: stack, addedNodes: [fourthWrap], removedNodes: [] }]);
  assert.equal(summary.textContent, "− Tool calls × 4");
  assert.equal(fourthWrap.getAttribute("data-herdr-tool-run-hidden"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.tool_runs_folded, 1);
});

test("wrapped code viewers remain uncontained", () => {
  const { context, document, FakeElement } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, "a very long wrapped line", { wrapped: true });
  document.body.appendChild(viewer);
  context.__HERDR_CHATGPT_PERF__.scan();
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.stats.viewers_skipped, 1);
});

test("disable removes containment and disconnects all observer layers", () => {
  const { context, document, FakeElement, mutationObservers, intersectionObservers } = makeContext();
  const { viewer } = codeViewer(FakeElement, document, "a\nb");
  const { message, root, block } = assistantWritingRoot(FakeElement, document);
  const { outer: toolOuter, cluster: toolCluster } = toolClusterTree(FakeElement, document, { tools: 8 });
  document.body.appendChild(viewer);
  document.body.appendChild(message);
  document.body.appendChild(toolOuter);
  context.__HERDR_CHATGPT_PERF__.scan();
  intersectionObservers[0].trigger([farEntry(block, 442)]);
  const toolObserver = intersectionObservers.find((observer) => observer.options?.rootMargin === "0px");
  toolObserver.trigger([farEntry(toolCluster, 2761.5)]);
  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), "1");
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), "1");
  assert.equal(toolCluster.getAttribute("data-herdr-tool-cluster-hidden"), "1");

  context.__HERDR_CHATGPT_PERF__.disable();

  assert.equal(viewer.getAttribute("data-herdr-code-block-contained"), null);
  assert.equal(block.getAttribute("data-herdr-editable-block-contained"), null);
  assert.equal(toolCluster.getAttribute("data-herdr-tool-cluster-hidden"), null);
  assert.equal(toolCluster.getAttribute("data-herdr-tool-cluster-observed"), null);
  assert.equal(context.__HERDR_CHATGPT_PERF__.enabled, false);
  assert.equal(mutationObservers[0].connected, false);
  assert.equal(intersectionObservers[0].connected, false);
  assert.equal(toolObserver.connected, false);
});
