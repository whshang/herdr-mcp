import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/page-assist.js", import.meta.url), "utf8");

class TestEvent {
  constructor(type, options = {}) {
    this.type = type;
    this.bubbles = options.bubbles === true;
  }
}

function createElement({
  tag = "button",
  type = "",
  text = "",
  value = "",
  attrs = {},
  visible = true,
  disabled = false,
  readOnly = false,
  contentEditable = false,
} = {}) {
  const attributes = new Map(Object.entries(attrs));
  const events = [];
  const element = {
    tagName: tag.toUpperCase(),
    type,
    innerText: text,
    textContent: text,
    value,
    disabled,
    readOnly,
    isConnected: true,
    isContentEditable: contentEditable,
    offsetParent: visible ? {} : null,
    ownerDocument: null,
    clicked: 0,
    focused: 0,
    events,
    getAttribute(name) {
      if (name === "contenteditable" && contentEditable) return "true";
      return attributes.get(name) ?? null;
    },
    getBoundingClientRect() {
      return visible ? { width: 120, height: 28 } : { width: 0, height: 0 };
    },
    scrollIntoView() {},
    focus() { this.focused += 1; },
    click() { this.clicked += 1; },
    dispatchEvent(event) {
      events.push(event.type);
      return true;
    },
  };
  return element;
}

function harness({
  url = "https://app.test/path",
  title = "Test page",
  bodyText = "Visible page text",
  elements = [],
  topOrigin = null,
} = {}) {
  let listener = null;
  const location = new URL(url);
  const document = {
    title,
    body: { innerText: bodyText, textContent: bodyText },
    querySelectorAll() { return elements; },
  };
  for (const element of elements) element.ownerDocument = document;

  const context = vm.createContext({
    console,
    URL,
    Date,
    Event: TestEvent,
    document,
    location,
    chrome: {
      runtime: {
        onMessage: {
          addListener(fn) { listener = fn; },
        },
      },
    },
    getComputedStyle(element) {
      const hidden = element.offsetParent === null;
      return {
        display: hidden ? "none" : "block",
        visibility: hidden ? "hidden" : "visible",
        opacity: hidden ? "0" : "1",
        position: "static",
      };
    },
  });
  context.self = context;
  context.top = topOrigin
    ? { location: { origin: topOrigin } }
    : context;
  vm.runInContext(source, context, { filename: "page-assist.js" });
  assert.equal(typeof listener, "function");

  return {
    send(message) {
      let response;
      const handled = listener(message, {}, (value) => { response = value; });
      assert.equal(handled, false);
      return response;
    },
  };
}

test("Page Assist inspect exposes only visible non-sensitive elements through opaque refs", () => {
  const safeButton = createElement({ tag: "button", text: "Continue" });
  const safeInput = createElement({ tag: "input", type: "text", attrs: { placeholder: "Name" } });
  const password = createElement({ tag: "input", type: "password", attrs: { placeholder: "Password" } });
  const otp = createElement({ tag: "input", type: "text", attrs: { autocomplete: "one-time-code" } });
  const card = createElement({ tag: "input", type: "text", attrs: { autocomplete: "cc-number" } });
  const hidden = createElement({ tag: "button", text: "Hidden", visible: false });
  const disabled = createElement({ tag: "button", text: "Disabled", disabled: true });
  const h = harness({
    bodyText: "x".repeat(200),
    elements: [safeButton, safeInput, password, otp, card, hidden, disabled],
  });

  const result = h.send({
    type: "h2w_page_assist",
    action: "inspect",
    expectedOrigin: "https://app.test",
    maxChars: 32,
  });
  assert.equal(result.ok, true);
  assert.equal(result.origin, "https://app.test");
  assert.equal(result.text.length, 32);
  assert.equal(result.elements.length, 2);
  assert.equal(result.elements.map((item) => item.text).join("|"), "Continue|Name");
  assert.ok(result.elements.every((item) => item.ref.startsWith(`ref_${result.generation}_`)));
  assert.ok(result.elements.every((item) => !Object.hasOwn(item, "selector") && !Object.hasOwn(item, "path")));
});

test("Page Assist click requires the current generation and invalidates refs after one action", () => {
  const button = createElement({ tag: "button", text: "Run" });
  const h = harness({ elements: [button] });
  const inspected = h.send({ type: "h2w_page_assist", action: "inspect", maxChars: 100 });
  const ref = inspected.elements[0].ref;

  const wrongOrigin = h.send({
    type: "h2w_page_assist",
    action: "click",
    expectedOrigin: "https://other.test",
    generation: inspected.generation,
    ref,
  });
  assert.equal(wrongOrigin.ok, false);
  assert.equal(wrongOrigin.error, "origin_mismatch");

  const forbidden = h.send({
    type: "h2w_page_assist",
    action: "click",
    expectedOrigin: "https://app.test",
    generation: inspected.generation,
    ref,
    selector: "#run",
  });
  assert.equal(forbidden.ok, false);
  assert.equal(forbidden.error, "disallowed_parameter");

  const clicked = h.send({
    type: "h2w_page_assist",
    action: "click",
    expectedOrigin: "https://app.test",
    generation: inspected.generation,
    ref,
  });
  assert.equal(clicked.ok, true);
  assert.equal(button.clicked, 1);

  const replay = h.send({
    type: "h2w_page_assist",
    action: "click",
    expectedOrigin: "https://app.test",
    generation: inspected.generation,
    ref,
  });
  assert.equal(replay.ok, false);
  assert.equal(replay.error, "stale_generation");
  assert.equal(button.clicked, 1);
});

test("Page Assist fill is bounded and invalidates the generation", () => {
  const input = createElement({ tag: "input", type: "text", attrs: { placeholder: "Message" } });
  const h = harness({ elements: [input] });
  const inspected = h.send({ type: "h2w_page_assist", action: "inspect" });
  const ref = inspected.elements[0].ref;

  const filled = h.send({
    type: "h2w_page_assist",
    action: "fill",
    expectedOrigin: "https://app.test",
    generation: inspected.generation,
    ref,
    value: "z".repeat(12000),
  });
  assert.equal(filled.ok, true);
  assert.equal(input.value.length, 10000);
  assert.deepEqual(input.events, ["input", "change"]);

  const replay = h.send({
    type: "h2w_page_assist",
    action: "fill",
    expectedOrigin: "https://app.test",
    generation: inspected.generation,
    ref,
    value: "again",
  });
  assert.equal(replay.ok, false);
  assert.equal(replay.error, "stale_generation");
});

test("Page Assist fails closed inside a cross-origin iframe", () => {
  const h = harness({ topOrigin: "https://parent.test" });
  const result = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(result.ok, false);
  assert.equal(result.error, "cross_origin_iframe_blocked");
});
