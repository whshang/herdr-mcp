import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const kernelSource = readFileSync(new URL("../extension/content/browser-page-kernel.js", import.meta.url), "utf8");
const compatibilitySource = readFileSync(new URL("../extension/content/page-assist.js", import.meta.url), "utf8");

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
    hasAttribute(name) { return attributes.has(name); },
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
    readyState: "complete",
    body: { innerText: bodyText, textContent: bodyText },
    querySelectorAll() { return elements; },
  };
  for (const element of elements) element.ownerDocument = document;

  const context = vm.createContext({
    console,
    URL,
    Date,
    Event: TestEvent,
    setTimeout,
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
  vm.runInContext(kernelSource, context, { filename: "browser-page-kernel.js" });
  assert.equal(typeof context.H2W_BROWSER_PAGE_KERNEL?.handleAction, "function");
  vm.runInContext(compatibilitySource, context, { filename: "page-assist.js" });
  assert.equal(typeof listener, "function");

  return {
    send(message) {
      let response;
      const handled = listener(message, {}, (value) => { response = value; });
      assert.equal(handled, false);
      return response;
    },
    sendAsync(message) {
      return new Promise((resolve) => {
        const handled = listener(message, {}, resolve);
        assert.equal(handled, true);
      });
    },
  };
}

test("Page Assist inspect exposes only visible non-sensitive elements through opaque refs", () => {
  const safeButton = createElement({ tag: "button", type: "submit", text: "Continue" });
  const safeLink = createElement({ tag: "a", text: "Details", attrs: { href: "/details" } });
  const newTabLink = createElement({ tag: "a", text: "More details", attrs: { href: "/more", target: "_blank" } });
  const logoutLink = createElement({ tag: "a", text: "Account", attrs: { href: "/logout" } });
  const reveal = createElement({
    tag: "button",
    type: "button",
    text: "More",
    attrs: { "aria-expanded": "false" },
  });
  const safeInput = createElement({
    tag: "input", type: "text",
    attrs: { placeholder: "Name", id: "display-name", "data-testid": "profile-name", class: "form-field primary" },
  });
  const password = createElement({ tag: "input", type: "password", attrs: { placeholder: "Password" } });
  const otp = createElement({ tag: "input", type: "text", attrs: { autocomplete: "one-time-code" } });
  const card = createElement({ tag: "input", type: "text", attrs: { autocomplete: "cc-number" } });
  const hidden = createElement({ tag: "button", text: "Hidden", visible: false });
  const disabled = createElement({ tag: "button", text: "Disabled", disabled: true });
  const h = harness({
    bodyText: "x".repeat(200),
    elements: [safeButton, safeLink, newTabLink, logoutLink, reveal, safeInput, password, otp, card, hidden, disabled],
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
  assert.equal(result.elements.length, 6);
  assert.equal(result.elements.map((item) => item.text).join("|"), "Continue|Details|More details|Account|More|Name");
  assert.equal(result.elements[0].type, "submit");
  assert.equal(result.elements[5].tag, "input");
  assert.equal(result.elements[5].id, "display-name");
  assert.equal(result.elements[5].data_testid, "profile-name");
  assert.deepEqual(Array.from(result.elements[5].classes), ["form-field", "primary"]);
  assert.equal(result.elements[5].contenteditable, undefined);
  assert.equal(result.elements[0].fast_path, undefined);
  assert.equal(result.elements[1].fast_path, "same_origin_navigation");
  assert.equal(result.elements[2].fast_path, undefined);
  assert.equal(result.elements[3].fast_path, undefined);
  assert.equal(result.elements[4].fast_path, "reveal");
  assert.ok(result.elements.every((item) => item.ref.startsWith(`ref_${result.generation}_`)));
  assert.ok(result.elements.every((item) => !Object.hasOwn(item, "selector") && !Object.hasOwn(item, "path")));

  const linkClick = h.send({
    type: "h2w_page_assist",
    action: "click",
    expectedOrigin: "https://app.test",
    generation: result.generation,
    ref: result.elements[1].ref,
  });
  assert.equal(linkClick.ok, true);
  assert.equal(linkClick.navigation_url, "https://app.test/details");
});

test("user diagnoses an id-less composer | Given a visible non-sensitive ProseMirror textbox | When observing DOM structure | Then only bounded structural attributes are returned", () => {
  const textbox = createElement({
    tag: "div",
    text: "",
    contentEditable: true,
    attrs: {
      role: "textbox",
      "aria-label": "New chat",
      class: "ProseMirror editor another other fifth",
      "data-testid": "chat-composer",
      id: "invalid:raw-id",
    },
  });
  const h = harness({ elements: [textbox] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect", expectedOrigin: "https://app.test" });
  assert.equal(observed.elements.length, 1);
  assert.equal(observed.elements[0].tag, "div");
  assert.equal(observed.elements[0].role, "textbox");
  assert.equal(observed.elements[0].contenteditable, true);
  assert.equal(observed.elements[0].id, undefined);
  assert.equal(observed.elements[0].data_testid, "chat-composer");
  assert.deepEqual(Array.from(observed.elements[0].classes), ["ProseMirror", "editor", "another", "other"]);
  assert.ok(!Object.hasOwn(observed.elements[0], "outerHTML"));
  assert.ok(!Object.hasOwn(observed.elements[0], "selector"));
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

test("user selects an exact native dropdown option | Given a visible single-select control | When filling by value or label | Then only an unambiguous enabled option is selected", () => {
  const dropdown = createElement({ tag: "select", attrs: { "aria-label": "Country" }, value: "" });
  dropdown.options = [
    { value: "", textContent: "Choose country", disabled: true },
    { value: "jp", textContent: "Japan", disabled: false },
    { value: "us", textContent: "United States", disabled: false },
    { value: "xx", textContent: "Disabled", disabled: true },
  ];
  const h = harness({ elements: [dropdown] });

  let observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements[0].tag, "select");
  assert.deepEqual(Array.from(observed.elements[0].options), ["Japan", "United States"]);
  assert.equal(observed.elements[0].options_truncated, undefined);
  const choose = (value) => h.send({
    type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: observed.elements[0].ref, value,
  });

  const missing = choose("France");
  assert.equal(missing.error, "select_option_unavailable");
  assert.equal(dropdown.value, "");
  assert.deepEqual(dropdown.events, []);
  const disabled = choose("Disabled");
  assert.equal(disabled.error, "select_option_unavailable");
  assert.equal(dropdown.focused, 0);

  dropdown.options.push({ value: "fr", textContent: "France", disabled: false });
  assert.equal(choose("France").error, "select_option_unavailable");
  dropdown.options.pop();

  const selected = choose("United States");
  assert.equal(selected.ok, true);
  assert.equal(dropdown.value, "us");
  assert.deepEqual(dropdown.events, ["input", "change"]);
  assert.equal(choose("Japan").error, "stale_generation");

  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements[0].selected_option, "United States");
  assert.equal(choose("jp").ok, true);
  assert.equal(dropdown.value, "jp");

  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements[0].selected_option, "Japan");
  dropdown.options[2].textContent = "Changed dynamically";
  assert.equal(choose("United States").error, "select_option_stale");
  dropdown.options[2].textContent = "United States";
  dropdown.options.push({ value: "jp", textContent: "Japan Duplicate", disabled: false });
  assert.equal(choose("jp").error, "select_option_ambiguous");
  assert.equal(dropdown.value, "jp");

  dropdown.multiple = true;
  assert.equal(choose("us").error, "multiple_select_unsupported");
  assert.equal(dropdown.events.length, 4);

  dropdown.multiple = false;
  dropdown.options.push(...Array.from({ length: 28 }, (_, i) => ({
    value: `opaque_private_${i}`, textContent: `Visible ${i}`, disabled: false,
  })));
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements[0].options.length, 24);
  assert.equal(observed.elements[0].options_truncated, true);
  assert.ok(!JSON.stringify(observed.elements[0]).includes("opaque_private_"));
});

test("user explicitly sets checkable state | Given observed native checkbox and radio controls | When fill specifies true or false | Then state is verified without accidental toggles", () => {
  const checkbox = createElement({ tag: "input", type: "checkbox", attrs: { "aria-label": "Notify me" } });
  const radio = createElement({ tag: "input", type: "radio", attrs: { "aria-label": "Email" } });
  checkbox.checked = false;
  radio.checked = false;
  const h = harness({ elements: [checkbox, radio] });
  let observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const checkRef = observed.elements.find((e) => e.type === "checkbox");
  const radioRef = observed.elements.find((e) => e.type === "radio");
  assert.equal(checkRef.checked, false);
  assert.equal(radioRef.checked, false);
  function fill(ref, value) {
    return h.send({
      type: "h2w_page_assist", action: "fill",
      expectedOrigin: "https://app.test",
      generation: observed.generation, ref, value,
    });
  }
  assert.equal(fill(checkRef.ref, "yes").error, "checkable_state_required");
  assert.equal(fill(checkRef.ref, "false").error, "control_already_in_state");
  assert.deepEqual(checkbox.events, []);
  assert.equal(fill(checkRef.ref, "true").ok, true);
  assert.equal(checkbox.checked, true);
  assert.deepEqual(checkbox.events, ["input", "change"]);
  assert.equal(fill(checkRef.ref, "false").error, "stale_generation");
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.find((e) => e.type === "checkbox").checked, true);
  assert.equal(fill(observed.elements.find((e) => e.type === "checkbox").ref, "false").ok, true);
  assert.equal(checkbox.checked, false);

  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(fill(observed.elements.find((e) => e.type === "radio").ref, "false").error, "radio_uncheck_unsupported");
  assert.deepEqual(radio.events, []);
  assert.equal(fill(observed.elements.find((e) => e.type === "radio").ref, "true").ok, true);
  assert.equal(radio.checked, true);
  assert.deepEqual(radio.events, ["input", "change"]);
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.find((e) => e.type === "radio").checked, true);
  assert.equal(fill(observed.elements.find((e) => e.type === "radio").ref, "true").error, "control_already_in_state");
  assert.deepEqual(radio.events, ["input", "change"]);
});

test("user verifies a generic page postcondition | Given an observed same-origin page | When a bounded expect runs | Then document, URL, and text conditions settle without a mutation", async () => {
  const h = harness({
    url: "https://app.test/orders",
    bodyText: "Orders ready",
  });
  const observed = h.send({
    type: "h2w_page_assist",
    action: "observe",
    expectedOrigin: "https://app.test",
    maxChars: 64,
  });
  assert.equal(observed.ok, true);
  assert.equal(observed.url, "https://app.test/orders");

  const ready = await h.sendAsync({
    type: "h2w_page_assist",
    action: "expect",
    expectedOrigin: "https://app.test",
    condition: "document_ready",
    timeoutMs: 100,
  });
  assert.equal(ready.ok, true);
  assert.equal(ready.condition, "document_ready");

  const text = await h.sendAsync({
    type: "h2w_page_assist",
    action: "expect",
    expectedOrigin: "https://app.test",
    condition: "text_present",
    value: "Orders ready",
    timeoutMs: 100,
  });
  assert.equal(text.ok, true);

  const missing = await h.sendAsync({
    type: "h2w_page_assist",
    action: "expect",
    expectedOrigin: "https://app.test",
    condition: "text_present",
    value: "never here",
    timeoutMs: 0,
  });
  assert.equal(missing.ok, false);
  assert.equal(missing.error, "expect_timeout");
});

test("user waits for an exact visible control | Given dynamic and sensitive page elements | When a bounded element expect runs | Then only an observed safe label can satisfy the wait", async () => {
  const secure = createElement({ tag: "input", type: "password", attrs: { "aria-label": "Secret" } });
  const hidden = createElement({ tag: "button", text: "Hidden button", visible: false });
  const existing = createElement({ tag: "button", text: "Continue" });
  const elements = [secure, hidden, existing];
  const h = harness({ elements });
  const expect = (condition, value, timeoutMs = 0) => h.sendAsync({
    type: "h2w_page_assist", action: "expect",
    expectedOrigin: "https://app.test", condition, value, timeoutMs,
  });
  assert.equal((await expect("element_present", "Continue")).ok, true);
  assert.equal((await expect("element_absent", "Continue")).error, "expect_timeout");
  assert.equal((await expect("element_absent", "Secret")).ok, true);
  assert.equal((await expect("element_absent", "Hidden button")).ok, true);
  assert.equal((await expect("element_present", "#password")).error, "expect_timeout");
  assert.equal((await expect("element_present", " Continue ")).error, "expect_value_invalid");

  const introduced = createElement({ tag: "button", text: "Publish" });
  introduced.ownerDocument = existing.ownerDocument;
  setTimeout(() => elements.push(introduced), 75);
  const appeared = await expect("element_present", "Publish", 350);
  assert.equal(appeared.ok, true);
  assert.ok(appeared.elapsed_ms >= 50);
  elements.pop();
  assert.equal((await expect("element_absent", "Publish")).ok, true);

  // A capped scan is not proof of absence in a dense application DOM.
  for (let i = 0; i < 129; i += 1) {
    const el = createElement({ tag: "button", text: `Other ${i}` });
    el.ownerDocument = existing.ownerDocument;
    elements.push(el);
  }
  assert.equal((await expect("element_absent", "Missing")).error, "element_scan_truncated");
  assert.equal((await expect("element_present", "Missing")).error, "element_scan_truncated");
});

test("Page Assist fails closed inside a cross-origin iframe", () => {
  const h = harness({ topOrigin: "https://parent.test" });
  const result = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(result.ok, false);
  assert.equal(result.error, "cross_origin_iframe_blocked");
});
