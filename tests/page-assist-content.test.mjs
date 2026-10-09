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
  children = [],
} = {}) {
  const attributes = new Map(Object.entries(attrs));
  const events = [];
  const element = {
    nodeType: 1,
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
    childNodes: children.map((child) => (
      typeof child === "string"
        ? { nodeType: 3, nodeValue: child }
        : child
    )),
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
  scrollMax = 0,
} = {}) {
  let listener = null;
  const location = new URL(url);
  const document = {
    title,
    readyState: "complete",
    body: { innerText: bodyText, textContent: bodyText },
    documentElement: { scrollHeight: scrollMax + 600 },
    querySelectorAll() { return elements; },
  };
  const assignOwner = (element) => {
    element.ownerDocument = document;
    for (const child of element.childNodes || []) {
      if (child && child.nodeType === 1) assignOwner(child);
    }
  };
  for (const element of elements) assignOwner(element);

  const context = vm.createContext({
    console,
    URL,
    Date,
    Event: TestEvent,
    setTimeout,
    document,
    location,
    scrollY: 0,
    innerHeight: 600,
    scrollBy({ top }) {
      this.scrollY = Math.max(0, Math.min(scrollMax, this.scrollY + top));
    },
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

test("user scrolls the viewport | Given a bounded observed page | When scrolling by direction and pixels | Then movement and a boundary are reported without reusing stale generation", () => {
  const h = harness({ scrollMax: 1250 });
  let observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.scroll.y, 0);
  assert.equal(observed.scroll.max_y, 1250);
  const scroll = (direction, amount, expectedOrigin = "https://app.test") => h.send({
    type: "h2w_page_assist", action: "scroll", generation: observed.generation,
    expectedOrigin, direction, amount,
  });
  assert.equal(scroll("down", 1600).error, "scroll_params_invalid");
  assert.equal(scroll("down", 250, "https://other.test").error, "origin_mismatch");
  const moved = scroll("down", 500);
  assert.equal(moved.ok, true);
  assert.deepEqual([moved.before_y, moved.after_y, moved.moved, moved.at_boundary], [0, 500, true, false]);
  assert.equal(scroll("down", 300).error, "stale_generation");
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.scroll.y, 500);
  assert.equal(scroll("down", 1000).after_y, 1250);
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const boundary = scroll("down", 300);
  assert.equal(boundary.ok, true);
  assert.equal(boundary.moved, false);
  assert.equal(boundary.at_boundary, true);
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(scroll("up", 500).after_y, 750);
});

test("user extracts text from one observed control | Given a bounded safe DOM element | When an exact generation ref is requested | Then the result is clipped without exposing markup or attributes", () => {
  const text = "Story paragraph ".repeat(500);
  const element = createElement({ tag: "button", text });
  const secret = createElement({ tag: "input", type: "password", attrs: { "aria-label": "Secret" } });
  const h = harness({ elements: [element, secret] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 1);
  const extraction = (params = {}) => h.send({
    type: "h2w_page_assist", action: "extract", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: observed.elements[0].ref,
    maxChars: 180, ...params,
  });
  const result = extraction();
  assert.equal(result.ok, true);
  assert.equal(result.text.length, 180);
  assert.equal(result.truncated, true);
  assert.ok(!Object.hasOwn(result, "outerHTML"));
  assert.ok(!Object.hasOwn(result, "attributes"));
  assert.equal(extraction({ maxChars: 9000 }).error, "extract_params_invalid");
  assert.equal(extraction({ expectedOrigin: "https://other.test" }).error, "origin_mismatch");
  assert.equal(extraction({ generation: "old" }).error, "stale_generation");
  assert.equal(extraction({ ref: `ref_${observed.generation}_999` }).error, "element_detached");
  assert.equal(extraction({ selector: "button" }).error, "disallowed_parameter");
  const next = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.notEqual(next.generation, observed.generation);
  assert.equal(extraction().error, "stale_generation");
});

test("user extracts allowlisted structural fields | Given a visible nonsensitive element | When opt-in fields are requested | Then only bounded tag and role tokens are returned and unsupported names fail closed", () => {
  const link = createElement({
    tag: "a",
    text: "Open details",
    attrs: {
      href: "/details/42?token=SECRET#frag",
      role: "LINK",
      "aria-label": "Open details",
      title: "Details title",
    },
  });
  const widget = createElement({ tag: "my-widget", text: "Widget", attrs: { role: "li\nnk" } });
  const overlongRole = createElement({ tag: "button", text: "Long", attrs: { role: "a".repeat(40) } });
  const unicodeRole = createElement({ tag: "button", text: "Unicode", attrs: { role: "l\u00e4nk" } });
  const input = createElement({ tag: "input", type: "text", attrs: { placeholder: "Name" } });
  const h = harness({ elements: [link, widget, overlongRole, unicodeRole, input] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 5);
  const extract = (ref, params = {}) => h.send({
    type: "h2w_page_assist", action: "extract", expectedOrigin: "https://app.test",
    generation: observed.generation, ref, maxChars: 200, ...params,
  });

  const linkRef = observed.elements[0].ref;
  const structured = extract(linkRef, { fields: ["tag", "role"] });
  assert.equal(structured.ok, true);
  assert.deepEqual({ ...structured.fields }, { tag: "a", role: "link" });
  assert.ok(!Object.hasOwn(structured.fields, "href"));
  assert.ok(!Object.hasOwn(structured.fields, "aria_label"));
  assert.ok(!Object.hasOwn(structured.fields, "title"));
  assert.ok(!Object.hasOwn(structured, "outerHTML"));
  assert.ok(!Object.hasOwn(structured.fields, "class"));
  assert.ok(!structured.text.includes("SECRET"));

  // Structural tokens are lowercased and bounded; a non-token role is omitted.
  assert.deepEqual({ ...extract(observed.elements[1].ref, { fields: ["tag", "role"] }).fields }, { tag: "my-widget" });
  assert.deepEqual({ ...extract(observed.elements[2].ref, { fields: ["tag", "role"] }).fields }, { tag: "button" });
  assert.deepEqual({ ...extract(observed.elements[3].ref, { fields: ["role"] }).fields }, {});

  // Attribute text, links and unknown names are rejected, not silently ignored.
  for (const fields of [
    ["href"], ["aria_label"], ["title"], ["outerHTML"], ["attributes"], ["Tag"],
    [], ["tag", "tag"], ["tag", "role", "href"], "role", [1], ["tag", "role", "aria_label"],
  ]) {
    assert.equal(extract(linkRef, { fields }).error, "extract_fields_invalid");
  }

  // Omitting fields preserves the legacy text-only shape.
  const textOnly = extract(linkRef);
  assert.equal(textOnly.ok, true);
  assert.equal(textOnly.text, "Open details");
  assert.ok(!Object.hasOwn(textOnly, "fields"));
  assert.equal(extract(linkRef, { generation: "old", fields: ["tag"] }).error, "stale_generation");

  // An element that becomes sensitive or hidden after observation fails closed.
  input.type = "password";
  assert.equal(extract(observed.elements[4].ref, { fields: ["tag"] }).error, "unsupported_element");
});

test("user extracts visible text safely | Given a container with hidden or sensitive descendants | When extraction runs | Then hidden and sensitive descendant text is excluded and hidden textContent is never inferred", () => {
  const hiddenText = createElement({ tag: "div", text: "", children: ["SECRET-HIDDEN"] });
  hiddenText.textContent = "SECRET-HIDDEN";
  const hiddenPassword = createElement({ tag: "input", type: "password", value: "hunter2", visible: false });
  const passwordContainer = createElement({ tag: "div", text: "", children: [hiddenPassword] });
  passwordContainer.textContent = "hunter2";
  const otp = createElement({ tag: "span", text: "123456", attrs: { autocomplete: "one-time-code" } });
  const codeContainer = createElement({ tag: "div", text: "Enter code: 123456", children: ["Enter code: ", otp] });
  const h = harness({ elements: [hiddenText, passwordContainer, codeContainer] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 3);
  const extract = (ref, params = {}) => h.send({
    type: "h2w_page_assist", action: "extract", expectedOrigin: "https://app.test",
    generation: observed.generation, ref, maxChars: 200, ...params,
  });

  // innerText is empty but textContent contains hidden text: never infer it.
  const hiddenResult = extract(observed.elements[0].ref);
  assert.equal(hiddenResult.ok, true);
  assert.equal(hiddenResult.text, "");
  assert.ok(!hiddenResult.text.includes("SECRET-HIDDEN"));

  // A hidden password child contributes no text.
  const passwordResult = extract(observed.elements[1].ref);
  assert.equal(passwordResult.ok, true);
  assert.equal(passwordResult.text, "");
  assert.ok(!passwordResult.text.includes("hunter2"));

  // A visible sensitive descendant is filtered even with opt-in structure.
  const codeResult = extract(observed.elements[2].ref, { fields: ["tag", "role"] });
  assert.equal(codeResult.ok, true);
  assert.equal(codeResult.text, "Enter code:");
  assert.ok(!codeResult.text.includes("123456"));
  assert.deepEqual({ ...codeResult.fields }, { tag: "div" });
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
