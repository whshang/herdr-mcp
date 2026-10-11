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
  scrollMax = 0,
  scrollY = 0,
  scrollingElement = null,
} = {}) {
  let listener = null;
  const location = new URL(url);
  const document = {
    title,
    readyState: "complete",
    body: { innerText: bodyText, textContent: bodyText },
    documentElement: { scrollHeight: scrollMax + 600 },
    scrollingElement,
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
    scrollY,
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
        overflowY: element.overflowY || "visible",
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

test("user extracts only visible text | Given an observed container with hidden, aria-hidden and password descendants | When extract runs on the container ref | Then only visible descendant text is returned", () => {
  const container = createElement({ tag: "div", text: "Visible SECRET_HIDDEN SECRET_ARIA" });
  const h = harness({ elements: [container] });
  const doc = container.ownerDocument;
  const textNode = (value) => ({ nodeType: 3, nodeValue: value });
  const child = (opts, kids) => Object.assign(createElement(opts), { nodeType: 1, ownerDocument: doc, childNodes: kids });
  container.nodeType = 1;
  container.childNodes = [
    textNode("Visible "),
    child({ tag: "span", visible: false }, [textNode("SECRET_HIDDEN")]),
    child({ tag: "span", attrs: { "aria-hidden": "true" } }, [textNode("SECRET_ARIA")]),
    child({ tag: "input", type: "password", value: "SECRET_PW" }, []),
    child({ tag: "b" }, [textNode("tail")]),
  ];
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const ref = observed.elements[0].ref;
  const result = h.send({ type: "h2w_page_assist", action: "extract", expectedOrigin: "https://app.test", generation: observed.generation, ref });
  assert.equal(result.ok, true);
  assert.equal(result.text, "Visible tail");
  assert.doesNotMatch(result.text, /SECRET/);
});

test("user sets a framework-managed checkbox | Given a checkbox whose native click toggles state and one whose state cannot change | When fill sets checked true | Then the click path is used and ok is reported only when the final state matches", () => {
  const live = createElement({ tag: "input", type: "checkbox", attrs: { "aria-label": "Live" } });
  live.checked = false;
  live.click = function () { this.clicked += 1; this.checked = !this.checked; this.events.push("click"); };
  const stuck = createElement({ tag: "input", type: "checkbox", attrs: { "aria-label": "Stuck" } });
  let stuckValue = false;
  Object.defineProperty(stuck, "checked", { get: () => stuckValue, set: () => {}, configurable: true });
  const h = harness({ elements: [live, stuck] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const refFor = (label) => observed.elements.find((e) => e.type === "checkbox" && JSON.stringify(e).includes(label)).ref;
  const liveRef = refFor("Live");
  const stuckRef = refFor("Stuck");
  const fill = (ref, generation) => h.send({ type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test", generation, ref, value: "true" });
  const ok = fill(liveRef, observed.generation);
  assert.equal(ok.ok, true);
  assert.equal(live.checked, true);
  assert.equal(live.clicked, 1);
  assert.deepEqual(live.events, ["click"]);
  const again = h.send({ type: "h2w_page_assist", action: "inspect" });
  const failed = fill(again.elements.find((e) => e.type === "checkbox" && JSON.stringify(e).includes("Stuck")).ref, again.generation);
  assert.equal(failed.ok, false);
  assert.equal(failed.error, "control_state_not_applied");
  assert.equal(stuckValue, false);
  void stuckRef;
});

test("user distinguishes a truncated dropdown observation from an absent option | Given a select whose observed option list was cut at the observation window | When fill asks for an option outside that window | Then it fails closed with a truncation-specific error and never selects the live option", () => {
  const dropdown = createElement({ tag: "select", attrs: { "aria-label": "Region" }, value: "" });
  dropdown.options = [
    { value: "", textContent: "Choose region", disabled: true },
    ...Array.from({ length: 30 }, (_, i) => ({ value: `r${i}`, textContent: `Region ${i}`, disabled: false })),
  ];
  const h = harness({ elements: [dropdown] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements[0].options.length, 24);
  assert.equal(observed.elements[0].options_truncated, true);
  const choose = (value) => h.send({
    type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: observed.elements[0].ref, value,
  });

  // "Region 27" exists in the live control but was never observed.
  const beyondWindow = choose("Region 27");
  assert.equal(beyondWindow.ok, false);
  assert.equal(beyondWindow.error, "select_option_not_observed");
  assert.equal(beyondWindow.options_observed, 24);
  assert.equal(dropdown.value, "");
  assert.deepEqual(dropdown.events, []);
  assert.equal(dropdown.focused, 0);

  // The rejection happened before any mutation, so the generation still stands.
  assert.equal(choose("Region 3").ok, true);
  assert.equal(dropdown.value, "r3");
  assert.equal(choose("Region 28").error, "stale_generation");

  // A complete observation keeps reporting an option that is genuinely absent.
  const small = createElement({ tag: "select", attrs: { "aria-label": "Small" }, value: "" });
  small.options = [{ value: "alpha", textContent: "Alpha", disabled: false }];
  const full = harness({ elements: [small] });
  const seen = full.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(seen.elements[0].options_truncated, undefined);
  const absent = full.send({
    type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test",
    generation: seen.generation, ref: seen.elements[0].ref, value: "Omega",
  });
  assert.equal(absent.error, "select_option_unavailable");
  assert.equal(small.value, "");
});

test("user scrolls an inner scroll container | Given a page whose window cannot move but whose observed content sits in a scrollable container | When scroll down is requested | Then the container moves, the result names it, and verification is still required", () => {
  const container = createElement({ tag: "div", attrs: { class: "feed-scroll" } });
  container.overflowY = "auto";
  container.scrollTop = 0;
  container.scrollHeight = 2000;
  container.clientHeight = 500;
  container.scrollBy = function ({ top }) {
    this.scrollTop = Math.max(0, Math.min(this.scrollHeight - this.clientHeight, this.scrollTop + top));
  };
  const button = createElement({ tag: "button", text: "Load more" });
  button.parentElement = container;
  const h = harness({ elements: [button], scrollMax: 0 });
  let observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.scroll.max_y, 0);
  const scroll = (direction, amount) => h.send({
    type: "h2w_page_assist", action: "scroll", expectedOrigin: "https://app.test",
    generation: observed.generation, direction, amount,
  });

  const moved = scroll("down", 400);
  assert.equal(moved.ok, true);
  assert.deepEqual([moved.before_y, moved.after_y, moved.moved, moved.at_boundary], [0, 0, true, false]);
  assert.equal(moved.scroll_container, "observed_ancestor");
  assert.equal(moved.scroll_container_tag, "div");
  assert.deepEqual([moved.scroll_container_before, moved.scroll_container_after], [0, 400]);
  assert.equal(moved.requires_verification, true);
  assert.equal(container.scrollTop, 400);
  assert.equal(scroll("down", 400).error, "stale_generation");

  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(scroll("down", 1500).scroll_container_after, 1500);
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const boundary = scroll("down", 500);
  assert.equal(boundary.ok, true);
  assert.equal(boundary.moved, false);
  assert.equal(boundary.at_boundary, true);
  assert.equal(boundary.scroll_container, undefined);
  assert.equal(container.scrollTop, 1500);
  observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(scroll("up", 200).scroll_container_after, 1300);
  assert.equal(container.scrollTop, 1300);
});

test("user scrolls a clipped document | Given a page whose only scrollable box is document.scrollingElement | When scroll down is requested | Then that element moves and is reported as the container", () => {
  const scroller = createElement({ tag: "html" });
  scroller.scrollTop = 0;
  scroller.scrollHeight = 1800;
  scroller.clientHeight = 600;
  scroller.scrollBy = function ({ top }) {
    this.scrollTop = Math.max(0, Math.min(this.scrollHeight - this.clientHeight, this.scrollTop + top));
  };
  const label = createElement({ tag: "button", text: "Deep content" });
  const h = harness({ elements: [label], scrollMax: 0, scrollingElement: scroller });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const moved = h.send({
    type: "h2w_page_assist", action: "scroll", expectedOrigin: "https://app.test",
    generation: observed.generation, direction: "down", amount: 300,
  });
  assert.equal(moved.ok, true);
  assert.deepEqual([moved.before_y, moved.after_y, moved.moved, moved.at_boundary], [0, 0, true, false]);
  assert.equal(moved.scroll_container, "document_scrolling_element");
  assert.equal(moved.scroll_container_tag, "html");
  assert.deepEqual([moved.scroll_container_before, moved.scroll_container_after], [0, 300]);
  assert.equal(moved.requires_verification, true);
  assert.equal(scroller.scrollTop, 300);
});

test("user gets one effective scroll | Given a viewport whose subpixel movement rounds to the same reported position | When scroll down is requested | Then the viewport counts as moved and no observed container is also scrolled", () => {
  const container = createElement({ tag: "div", attrs: { class: "feed-scroll" } });
  container.overflowY = "auto";
  container.scrollTop = 0;
  container.scrollHeight = 2000;
  container.clientHeight = 500;
  container.scrollBy = function ({ top }) {
    this.scrollTop = Math.max(0, Math.min(this.scrollHeight - this.clientHeight, this.scrollTop + top));
  };
  const button = createElement({ tag: "button", text: "Load more" });
  button.parentElement = container;
  const h = harness({ elements: [button], scrollMax: 0.4, scrollY: 0.2 });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.scroll.y, 0);
  const moved = h.send({
    type: "h2w_page_assist", action: "scroll", expectedOrigin: "https://app.test",
    generation: observed.generation, direction: "down", amount: 500,
  });
  assert.equal(moved.ok, true);
  assert.deepEqual([moved.before_y, moved.after_y], [0, 0]);
  assert.equal(moved.moved, true);
  assert.equal(moved.at_boundary, false);
  assert.equal(moved.scroll_container, "viewport");
  assert.deepEqual([moved.scroll_container_before, moved.scroll_container_after], [0.2, 0.4]);
  assert.equal(container.scrollTop, 0);
});
