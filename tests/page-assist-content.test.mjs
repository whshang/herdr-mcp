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
    setAttribute(name, value) { attributes.set(name, String(value)); },
    getBoundingClientRect() {
      return this.offsetParent
        ? { left: 20, top: 30, right: 140, bottom: 58, width: 120, height: 28 }
        : { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 };
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
  activeElement = null,
  extraGlobals = {},
  scrollY = 0,
  scrollingElement = null,
} = {}) {
  let listener = null;
  const location = new URL(url);
  const document = {
    title,
    readyState: "complete",
    activeElement,
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
    ...extraGlobals,
    scrollY,
    innerHeight: 600,
    innerWidth: 1000,
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
        cursor: element.cursor || "auto",
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

test("user hovers a bounded visible card | Given a CSS-hidden edit button | When an observed hover-only ref is prepared | Then the revealed button receives its own ref only after a new observation", () => {
  const card = createElement({
    tag: "div", text: "Existing evaluation", attrs: { class: "evaluationCard__sample" },
  });
  const edit = createElement({
    tag: "button", text: "Edit evaluation", visible: false,
    attrs: { class: "ui-button ui-button-icon ui-button-default ui-button-sm edit-icon" },
  });
  const password = createElement({ tag: "input", type: "password", value: "private" });
  const h = harness({ elements: [card, edit, password] });
  const first = h.send({ type: "h2w_page_assist", action: "observe" });
  assert.equal(first.ok, true);
  assert.equal(first.elements.length, 1);
  assert.equal(first.elements[0].hover_only, true);
  assert.equal(first.elements[0].text, "Existing evaluation");
  assert.equal(JSON.stringify(first).includes("Edit evaluation"), false);
  assert.equal(JSON.stringify(first).includes("private"), false);
  const rejected = h.send({
    type: "h2w_page_assist", action: "click", generation: first.generation,
    ref: first.elements[0].ref,
  });
  assert.equal(rejected.error, "hover_only_target");
  assert.equal(card.clicked, 0);
  const pressed = h.send({
    type: "h2w_page_assist", action: "press", generation: first.generation,
    ref: first.elements[0].ref, key: "Enter",
  });
  assert.equal(pressed.error, "hover_only_target");
  const prepared = h.send({
    type: "h2w_page_assist", action: "hover", generation: first.generation,
    ref: first.elements[0].ref,
  });
  assert.equal(prepared.ok, true);
  assert.equal(prepared.hover_point.x, 80);
  assert.equal(prepared.hover_point.y, 44);
  assert.equal(h.send({
    type: "h2w_page_assist", action: "hover", generation: first.generation,
    ref: first.elements[0].ref,
  }).error, "stale_generation");
  edit.offsetParent = {};
  const after = h.send({ type: "h2w_page_assist", action: "observe" });
  assert.equal(after.elements.length, 2);
  const editable = after.elements.find((element) => element.text === "Edit evaluation");
  assert.ok(editable?.ref);
  assert.equal(editable.hover_only, undefined);
  assert.ok(editable.classes.includes("edit-icon"));
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

// extract_table inspects rendered content, so its fixtures need a small DOM
// model: nodes carry the harness document (the kernel refuses nodes without
// one), a real childNodes tree for the visible-text projection, and
// offsetParent / bounding boxes for the visibility checks.
function tableDomNode({ doc, tag = "td", type = "", text = "", attrs = {}, visible = true, children = [] }) {
  const node = createElement({ tag, type, text: "", attrs, visible });
  node.ownerDocument = doc;
  node.nodeType = 1;
  node.childNodes = [...(text ? [{ nodeType: 3, nodeValue: text }] : []), ...children];
  if (children.length > 0) node.children = children;
  if (!visible) {
    // An unrendered element reports its markup text through innerText; that
    // fallback is exactly what the visible-only projection must not use.
    node.innerText = tableRenderedText(node);
    node.textContent = node.innerText;
  }
  return node;
}

function tableDomRow({ doc, cells, visible = true, attrs = {} }) {
  const row = tableDomNode({ doc, tag: "tr", visible, attrs });
  row.children = cells;
  return row;
}

function tableRenderedText(node) {
  return (node.childNodes || []).map((child) => child.nodeValue ?? tableRenderedText(child)).join("");
}

function attachTableRows(table, rows) {
  table.nodeType = 1;
  table.childNodes = [];
  table.querySelectorAll = (selector) => (selector === "tr" ? rows : []);
  table.innerText = rows.map((row) => row.children.map(tableRenderedText).join(" ")).join("\n");
  return table;
}

function tableHarness({ rows, extraElements = [] }) {
  const table = createElement({ tag: "table", text: "", visible: true });
  const hiddenTable = createElement({ tag: "table", text: "hidden", visible: false });
  const h = harness({ elements: [table, hiddenTable, ...extraElements] });
  attachTableRows(table, rows(table.ownerDocument));
  return { h, table };
}

test("user extracts one observed table | Given a page with a visible table ref | When extract_table requests bounded rows | Then headers and cell text return without markup or hidden values", () => {
  const longCell = "v".repeat(400);
  const button = createElement({ tag: "button", text: "Filter" });
  const { h } = tableHarness({
    extraElements: [button],
    rows: (doc) => [
      tableDomRow({ doc, cells: [tableDomNode({ doc, tag: "th", text: "Name" }), tableDomNode({ doc, tag: "th", text: "Qty" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Widget" }), tableDomNode({ doc, text: "3" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: longCell }), tableDomNode({ doc, text: "0" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Gadget" }), tableDomNode({ doc, text: "7" })] }),
    ],
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 2);
  const tableRef = observed.elements.find((item) => item.tag === "table")?.ref;
  const buttonRef = observed.elements.find((item) => item.tag === "button")?.ref;
  assert.ok(tableRef);

  const extractTable = (params = {}) => h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: tableRef, ...params,
  });
  const result = extractTable();
  assert.equal(result.ok, true);
  assert.deepEqual(Array.from(result.headers), ["Name", "Qty"]);
  assert.equal(result.rows.length, 3);
  assert.deepEqual(Array.from(result.rows[0]), ["Widget", "3"]);
  assert.equal(result.rows[1][0].length, 200);
  assert.equal(result.rows_truncated, false);
  assert.ok(!Object.hasOwn(result, "outerHTML"));
  assert.ok(!Object.hasOwn(result, "attributes"));

  const clipped = extractTable({ maxRows: 2 });
  assert.equal(clipped.rows.length, 2);
  assert.equal(clipped.rows_truncated, true);

  assert.equal(extractTable({ maxRows: 101 }).error, "extract_table_params_invalid");
  assert.equal(extractTable({ maxRows: 0 }).error, "extract_table_params_invalid");
  assert.equal(extractTable({ selector: "table" }).error, "disallowed_parameter");
  assert.equal(extractTable({ expectedOrigin: "https://other.test" }).error, "origin_mismatch");
  assert.equal(extractTable({ generation: "old" }).error, "stale_generation");
  assert.equal(extractTable({ ref: `ref_${observed.generation}_999` }).error, "element_detached");
  assert.equal(h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: buttonRef,
  }).error, "unsupported_element");

  const reobserved = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.notEqual(reobserved.generation, observed.generation);
  assert.equal(extractTable().error, "stale_generation");
});

test("user relies on table extraction rejecting every non-row parameter | Given an observed visible table | When extract_table receives a query-shaped, script-shaped or mutation parameter | Then each is rejected before any read", () => {
  const { h } = tableHarness({
    rows: (doc) => [
      tableDomRow({ doc, cells: [tableDomNode({ doc, tag: "th", text: "Name" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Widget" })] }),
    ],
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const tableRef = observed.elements.find((item) => item.tag === "table")?.ref;
  const extractTable = (params = {}) => h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: tableRef, ...params,
  });
  const forbidden = ["selector", "css", "xpath", "js", "script", "eval", "code", "cookie", "storage", "cdp"];
  const rejected = forbidden.map((key) => extractTable({ [key]: "table td" }));
  assert.deepEqual(Array.from(rejected, (entry) => entry.error), forbidden.map(() => "disallowed_parameter"));
  // The kernel owns the query/script surface only; mutation, condition and
  // idempotency parameters are rejected by the Runtime before this route, so an
  // extra field can never widen the read here.
  const bounded = extractTable({ maxRows: 1, unknownField: "ignored" });
  assert.equal(bounded.ok, true);
  assert.equal(bounded.rows.length, 1);
  assert.ok(!Object.hasOwn(bounded, "op_id"));
  assert.ok(!Object.hasOwn(bounded, "delivery_state"));
  assert.ok(!Object.hasOwn(bounded, "value"));
});

test("user reads merged table cells once per occurrence | Given rows with rowspan and colspan cells | When extract_table returns bounded rows | Then each spanned cell appears only in the row that declares it", () => {
  const { h } = tableHarness({
    rows: (doc) => [
      tableDomRow({ doc, cells: [tableDomNode({ doc, tag: "th", text: "Item" }), tableDomNode({ doc, tag: "th", text: "Qty" })] }),
      tableDomRow({
        doc,
        cells: [
          tableDomNode({ doc, text: "Group", attrs: { rowspan: "2" } }),
          tableDomNode({ doc, text: "Widget" }),
          tableDomNode({ doc, text: "3" }),
        ],
      }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Gadget" }), tableDomNode({ doc, text: "7" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Spanning note", attrs: { colspan: "2" } })] }),
    ],
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const tableRef = observed.elements.find((item) => item.tag === "table")?.ref;
  const result = h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: tableRef,
  });
  assert.equal(result.ok, true);
  assert.deepEqual(Array.from(result.headers), ["Item", "Qty"]);
  assert.equal(result.rows.length, 3);
  assert.deepEqual(Array.from(result.rows[0]), ["Group", "Widget", "3"]);
  assert.deepEqual(Array.from(result.rows[1]), ["Gadget", "7"]);
  assert.deepEqual(Array.from(result.rows[2]), ["Spanning note"]);
  assert.deepEqual(Array.from(result.rows, (row) => row.length), [3, 2, 1], "spanned cells are not replicated into later rows");
  assert.equal(result.rows[1].includes("Group"), false);
  assert.equal(result.rows_truncated, false);
});

test("user reads only rendered table cells | Given a visible table with hidden, aria-hidden, sensitive and password content | When extract_table returns rows | Then excluded cells are empty, hidden rows are absent and no secret text is returned", () => {
  const { h } = tableHarness({
    rows: (doc) => [
      tableDomRow({ doc, cells: [tableDomNode({ doc, tag: "th", text: "Name" }), tableDomNode({ doc, tag: "th", text: "Qty" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "Widget" }), tableDomNode({ doc, text: "SECRET_HIDDEN_CELL", visible: false })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "SECRET_ARIA_CELL", attrs: { "aria-hidden": "true" } }), tableDomNode({ doc, text: "9" })] }),
      tableDomRow({
        doc,
        cells: [
          tableDomNode({
            doc,
            text: "Visible",
            children: [
              { nodeType: 3, nodeValue: " " },
              tableDomNode({ doc, tag: "span", text: "SECRET_HIDDEN_DESC", visible: false }),
              tableDomNode({ doc, tag: "span", text: "SECRET_ARIA_DESC", attrs: { "aria-hidden": "true" } }),
              tableDomNode({ doc, tag: "input", type: "password", text: "SECRET_PASSWORD" }),
              tableDomNode({ doc, tag: "b", text: "tail" }),
            ],
          }),
          tableDomNode({ doc, text: "1" }),
        ],
      }),
      tableDomRow({ doc, visible: false, cells: [tableDomNode({ doc, text: "SECRET_HIDDEN_ROW" }), tableDomNode({ doc, text: "4" })] }),
      tableDomRow({ doc, cells: [tableDomNode({ doc, text: "SECRET_SENSITIVE_CELL", attrs: { autocomplete: "current-password" } }), tableDomNode({ doc, text: "2" })] }),
    ],
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const tableRef = observed.elements.find((item) => item.tag === "table")?.ref;
  const result = h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: tableRef,
  });
  assert.equal(result.ok, true);
  assert.deepEqual(Array.from(result.headers), ["Name", "Qty"]);
  assert.equal(result.rows.length, 4, "the hidden row is not returned");
  assert.deepEqual(Array.from(result.rows[0]), ["Widget", ""], "a hidden cell keeps its column position");
  assert.deepEqual(Array.from(result.rows[1]), ["", "9"]);
  assert.deepEqual(Array.from(result.rows[2]), ["Visible tail", "1"]);
  assert.deepEqual(Array.from(result.rows[3]), ["", "2"]);
  assert.doesNotMatch(JSON.stringify(result), /SECRET/);
});

test("user clips a long observed table | Given a table with more data rows than max_rows | When extract_table requests bounded rows | Then only max_rows rows return and rows_truncated states whether more exist", () => {
  const { h } = tableHarness({
    rows: (doc) => [
      tableDomRow({ doc, cells: [tableDomNode({ doc, tag: "th", text: "Name" })] }),
      ...Array.from({ length: 4 }, (_unused, index) => tableDomRow({
        doc, cells: [tableDomNode({ doc, text: `row-${index}` })],
      })),
    ],
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const tableRef = observed.elements.find((item) => item.tag === "table")?.ref;
  const extractTable = (params = {}) => h.send({
    type: "h2w_page_assist", action: "extract_table", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: tableRef, ...params,
  });
  const full = extractTable();
  assert.equal(full.rows.length, 4);
  assert.equal(full.rows_truncated, false, "the header row is not a data row");
  const exact = extractTable({ maxRows: 4 });
  assert.equal(exact.rows.length, 4);
  assert.equal(exact.rows_truncated, false, "an exact fit is not truncated");
  const clipped = extractTable({ maxRows: 3 });
  assert.equal(clipped.rows.length, 3);
  assert.equal(clipped.rows_truncated, true);
  assert.deepEqual(Array.from(clipped.rows[2]), ["row-2"]);
  const single = extractTable({ maxRows: 1 });
  assert.equal(single.rows.length, 1);
  assert.equal(single.rows_truncated, true);
  assert.deepEqual(Array.from(single.rows[0]), ["row-0"]);
});

test("user presses one bounded key | Given an observed page with a focused control | When a vocabulary key is pressed with or without a ref | Then key events reach one target and the generation expires", () => {
  class FakeKeyboardEvent {
    constructor(type, options = {}) {
      this.type = type;
      this.key = options.key;
      this.code = options.code;
    }
  }
  const events = [];
  const record = (label, element) => ({
    ...element,
    label,
    dispatchEvent(event) {
      events.push({ target: label, type: event.type, key: event.key, code: event.code });
      return true;
    },
  });
  const composer = record("composer", createElement({
    tag: "div", text: "", contentEditable: true, attrs: { "aria-label": "Composer" },
  }));
  const focused = record("focused", createElement({
    tag: "input", type: "text", attrs: { placeholder: "Search" },
  }));
  const h = harness({
    elements: [composer],
    activeElement: focused,
    extraGlobals: { KeyboardEvent: FakeKeyboardEvent },
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 1);

  const press = (params = {}) => h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: observed.generation, key: "Enter", ...params,
  });
  const withoutRef = press();
  assert.equal(withoutRef.ok, true);
  assert.equal(withoutRef.key, "Enter");
  assert.ok(!Object.hasOwn(withoutRef, "ref"));
  assert.deepEqual(events.map((event) => `${event.target}:${event.type}`), [
    "focused:keydown", "focused:keypress", "focused:keyup",
  ]);
  assert.deepEqual(
    events.filter((event) => event.type === "keydown").map((event) => `${event.key}/${event.code}`),
    ["Enter/Enter"],
  );
  assert.equal(withoutRef.requires_verification, true);

  const reobserved = h.send({ type: "h2w_page_assist", action: "inspect" });
  events.length = 0;
  const withRef = press({ generation: reobserved.generation, ref: reobserved.elements[0].ref, key: "ArrowDown" });
  assert.equal(withRef.ok, true);
  assert.equal(withRef.ref, reobserved.elements[0].ref);
  assert.deepEqual(events.map((event) => `${event.target}:${event.type}:${event.key}`), [
    "composer:keydown:ArrowDown", "composer:keypress:ArrowDown", "composer:keyup:ArrowDown",
  ]);

  const fresh = h.send({ type: "h2w_page_assist", action: "inspect" });
  const invalid = (params = {}) => h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: fresh.generation, ...params,
  });
  assert.equal(invalid({ key: "ctrl+a" }).error, "press_params_invalid");
  assert.equal(invalid({ key: "enter" }).error, "press_params_invalid");
  assert.equal(invalid({ key: "" }).error, "press_params_invalid");
  assert.equal(invalid({ key: "Enter", value: "text" }).error, "press_params_invalid");
  assert.equal(invalid({ key: "Enter", selector: "input" }).error, "disallowed_parameter");
  assert.equal(invalid({ key: "Enter", expectedOrigin: "https://other.test" }).error, "origin_mismatch");
  assert.equal(invalid({ key: "Enter", generation: "old" }).error, "stale_generation");
  assert.equal(invalid({ key: "Enter", ref: `ref_${fresh.generation}_999` }).error, "element_detached");
  events.length = 0;
  const afterPress = press({ generation: fresh.generation });
  assert.equal(afterPress.ok, true);
  assert.ok(!Object.hasOwn(afterPress, "value"));
  assert.equal(press({ generation: fresh.generation }).error, "stale_generation");
});

test("user presses every documented key | Given an observed focused control | When each vocabulary key is pressed | Then the exact key and code pair reaches that control once", () => {
  class FakeKeyboardEvent {
    constructor(type, options = {}) {
      this.type = type;
      this.key = options.key;
      this.code = options.code;
    }
  }
  const events = [];
  const focused = createElement({ tag: "input", type: "text", attrs: { placeholder: "Search" } });
  focused.dispatchEvent = (event) => {
    events.push(`${event.type}:${event.key}/${event.code}`);
    return true;
  };
  const h = harness({
    elements: [],
    activeElement: focused,
    extraGlobals: { KeyboardEvent: FakeKeyboardEvent },
  });
  const vocabulary = {
    Enter: ["Enter", "Enter"],
    Escape: ["Escape", "Escape"],
    Tab: ["Tab", "Tab"],
    Backspace: ["Backspace", "Backspace"],
    Delete: ["Delete", "Delete"],
    ArrowUp: ["ArrowUp", "ArrowUp"],
    ArrowDown: ["ArrowDown", "ArrowDown"],
    ArrowLeft: ["ArrowLeft", "ArrowLeft"],
    ArrowRight: ["ArrowRight", "ArrowRight"],
    Home: ["Home", "Home"],
    End: ["End", "End"],
    PageUp: ["PageUp", "PageUp"],
    PageDown: ["PageDown", "PageDown"],
    Space: [" ", "Space"],
  };
  for (const [key, [expectedKey, expectedCode]] of Object.entries(vocabulary)) {
    events.length = 0;
    const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
    const result = h.send({
      type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
      generation: observed.generation, key,
    });
    assert.equal(result.ok, true, `${key} must be inside the fixed vocabulary`);
    assert.equal(result.key, key);
    assert.deepEqual(events, [
      `keydown:${expectedKey}/${expectedCode}`,
      `keypress:${expectedKey}/${expectedCode}`,
      `keyup:${expectedKey}/${expectedCode}`,
    ]);
  }
  assert.ok(focused.focused >= Object.keys(vocabulary).length, "each press focuses the target before dispatching");
});

test("user cannot press an out-of-vocabulary key | Given an observed focused control | When press receives anything outside the fixed vocabulary | Then nothing is dispatched and the observation stays valid", () => {
  const events = [];
  const focused = createElement({ tag: "input", type: "text", attrs: { placeholder: "Search" } });
  focused.dispatchEvent = (event) => {
    events.push(event.type);
    return true;
  };
  const h = harness({
    elements: [],
    activeElement: focused,
    extraGlobals: {
      KeyboardEvent: class {
        constructor(type, options = {}) {
          this.type = type;
          this.key = options.key;
        }
      },
    },
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const press = (params = {}) => h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: observed.generation, ...params,
  });
  const rejected = [
    "a", "F5", "ENTER", "enter", "Enter ", " Enter", "Space ", "Ctrl+Enter",
    "Shift+Tab", "Meta+A", "PageDown2", 13, null, {}, true, "",
  ];
  for (const key of rejected) {
    assert.equal(press({ key }).error, "press_params_invalid", `${String(key)} must be rejected`);
  }
  assert.equal(press().error, "press_params_invalid", "a missing key is rejected");
  assert.equal(press({ key: "Enter", value: "typed text" }).error, "press_params_invalid");
  assert.deepEqual(events, [], "no key event reaches the page for a rejected key");
  assert.equal(press({ key: "Escape" }).ok, true, "the observation survives every rejected key");
  assert.deepEqual(events, ["keydown", "keypress", "keyup"]);
});

test("user cannot press through an unsafe target | Given an observed page whose refs later become hidden, disabled, sensitive or detached | When press targets each ref | Then every rejection is reported before any key event", () => {
  const events = [];
  const target = {
    ...createElement({ tag: "button", text: "Next" }),
    dispatchEvent(event) {
      events.push(event.type);
      return true;
    },
  };
  const hide = createElement({ tag: "button", text: "Hide me" });
  const disable = createElement({ tag: "button", text: "Disable me" });
  const sensitive = createElement({ tag: "input", type: "text", attrs: { placeholder: "Search" } });
  const detach = createElement({ tag: "button", text: "Detach me" });
  const h = harness({
    elements: [target, hide, disable, sensitive, detach],
    extraGlobals: {
      KeyboardEvent: class {
        constructor(type, options = {}) {
          this.type = type;
          this.key = options.key;
        }
      },
    },
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 5);
  const refFor = (tag) => observed.elements.find((item) => item.tag === tag)?.ref;
  const refs = {
    target: refFor("button"),
    hide: observed.elements.filter((item) => item.tag === "button")[1].ref,
    disable: observed.elements.filter((item) => item.tag === "button")[2].ref,
    sensitive: refFor("input"),
    detach: observed.elements.filter((item) => item.tag === "button")[3].ref,
  };
  const press = (params = {}) => h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: observed.generation, key: "Enter", ...params,
  });

  hide.offsetParent = null;
  assert.equal(press({ ref: refs.hide }).error, "unsupported_element");
  disable.disabled = true;
  assert.equal(press({ ref: refs.disable }).error, "unsupported_element");
  sensitive.type = "password";
  assert.equal(press({ ref: refs.sensitive }).error, "unsupported_element");
  detach.isConnected = false;
  assert.equal(press({ ref: refs.detach }).error, "element_detached");
  assert.equal(press({ ref: `ref_${observed.generation}_999` }).error, "element_detached");

  const reobserved = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.notEqual(reobserved.generation, observed.generation);
  assert.equal(h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: reobserved.generation, key: "Enter", ref: refs.target,
  }).error, "invalid_ref");
  assert.deepEqual(events, []);

  const delivered = h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: reobserved.generation, key: "Enter",
    ref: reobserved.elements.find((item) => item.tag === "button")?.ref,
  });
  assert.equal(delivered.ok, true);
  assert.deepEqual(events, ["keydown", "keypress", "keyup"]);
  assert.equal(h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: reobserved.generation, key: "Enter",
  }).error, "stale_generation", "a delivered press expires the observation");
});

test("user cannot press without a key target | Given a page with no focused control and a cross-origin frame | When press runs without a usable ref | Then press_unavailable or cross_origin_iframe_blocked is reported", () => {
  const h = harness({ elements: [] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: observed.generation, key: "Enter",
  }).error, "press_unavailable", "a body without dispatchEvent is not a key target");

  const crossFrame = harness({ elements: [], topOrigin: "https://other.test" });
  assert.equal(crossFrame.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: "pa_any_1", key: "Enter",
  }).error, "cross_origin_iframe_blocked");
});

test("user sees an uncertain press | Given a focused control whose key dispatch throws | When press runs | Then delivery is unknown and the observation is not expired", () => {
  const focused = createElement({ tag: "input", type: "text", attrs: { placeholder: "Search" } });
  const h = harness({
    elements: [],
    activeElement: focused,
    extraGlobals: {
      KeyboardEvent: class {
        constructor() {
          throw new Error("page blocked synthetic keys");
        }
      },
    },
  });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const press = (params = {}) => h.send({
    type: "h2w_page_assist", action: "press", expectedOrigin: "https://app.test",
    generation: observed.generation, key: "Enter", ...params,
  });
  assert.equal(press().error, "press_delivery_unknown");
  assert.equal(press({ key: "not-a-key" }).error, "press_params_invalid",
    "the unconfirmed press leaves the generation current so the caller can re-observe instead of replaying");
});

test("user diagnoses an id-less composer | Given a visible non-sensitive ProseMirror textbox | When observing DOM structure | Then only bounded structural attributes are returned", () => {
  const textbox = createElement({
    tag: "div",
    text: "",
    contentEditable: true,
    attrs: {
      role: "textbox",
      "aria-label": "New chat",
      class: "ProseMirror editor another other fifth sixth seventh eighth ninth",
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
  assert.deepEqual(Array.from(observed.elements[0].classes), ["ProseMirror", "editor", "another", "other", "fifth", "sixth", "seventh", "eighth"]);
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
  const container = createElement({ tag: "div", attrs: { role: "button" }, text: "Visible SECRET_HIDDEN SECRET_ARIA" });
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

test("user fills a rich-text composer | Given a contenteditable editor that may ignore raw text writes | When fill runs | Then success is reported only after the text reads back", () => {
  const value = "hello  claude\u00a0composer";
  function fillOnly(element) {
    const h = harness({ elements: [element] });
    const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
    assert.equal(observed.elements.length, 1);
    return h.send({
      type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test",
      generation: observed.generation, ref: observed.elements[0].ref, value,
    });
  }
  // ProseMirror-like: re-renders its own model and drops a raw innerText write.
  const prose = createElement({ tag: "div", contentEditable: true, attrs: { "aria-label": "Prose editor" } });
  Object.defineProperty(prose, "innerText", { get: () => "", set: () => {}, configurable: true });
  Object.defineProperty(prose, "textContent", { get: () => "", set: () => {}, configurable: true });
  const rejected = fillOnly(prose);
  assert.equal(rejected.ok, false);
  assert.equal(rejected.error, "fill_not_applied");
  assert.equal(rejected.applied, false);

  const plain = createElement({ tag: "div", contentEditable: true, attrs: { "aria-label": "Plain editor" } });
  assert.equal(fillOnly(plain).ok, true);
});

test("user fills a rich-text composer through the extension | Given the extension requests a MAIN-world fill | When fill targets a contenteditable editor | Then the kernel only tags the element and hands the value back without writing", () => {
  const editor = createElement({ tag: "div", contentEditable: true, attrs: { "aria-label": "Prose editor" } });
  const h = harness({ elements: [editor] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  const handoff = h.send({
    type: "h2w_page_assist", action: "fill", expectedOrigin: "https://app.test",
    generation: observed.generation, ref: observed.elements[0].ref, value: "hello claude", mainWorldFill: true,
  });
  assert.equal(handoff.ok, true);
  assert.match(handoff.main_world_fill_token, /^f[0-9a-z]+$/);
  assert.equal(handoff.value, "hello claude");
  assert.equal(editor.getAttribute("data-h2w-fill-token"), handoff.main_world_fill_token);
  assert.notEqual(editor.innerText, "hello claude");
});

test("user verifies observed controls | Given a textarea and semantic radio | When ref-scoped expect reads their actual state | Then values and checked state are verified without returning field contents", async () => {
  const input = createElement({ tag: "textarea", value: "actual draft" });
  const radio = createElement({ tag: "div", attrs: { role: "radio", "aria-checked": "true" } });
  const option = createElement({ tag: "div", attrs: { role: "option", "aria-selected": "true" } });
  const focusable = createElement({ tag: "div", attrs: { tabindex: "0", class: "custom-tag-selected" } });
  const pointer = Object.assign(createElement({ tag: "span", text: "Safe action" }), { cursor: "pointer" });
  const label = Object.assign(createElement({ tag: "label", text: "Choice" }), { control: radio });
  const h = harness({ elements: [input, radio, option, focusable, pointer, label, createElement({ tag: "div", text: "Broad ancestor" }), createElement({ tag: "input", type: "password" })] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 6);
  for (const [condition, ref, value] of [["input_value_equals", observed.elements[0].ref, "actual draft"], ["input_value_contains", observed.elements[0].ref, "draft"], ["element_checked", observed.elements[1].ref, undefined], ["element_selected", observed.elements[2].ref, undefined], ["element_class_contains", observed.elements[3].ref, "custom-tag-selected"], ["element_visible", observed.elements[0].ref, undefined], ["element_enabled", observed.elements[0].ref, undefined]]) {
    const result = await h.sendAsync({ type: "h2w_page_assist", action: "expect", generation: observed.generation, ref, condition, value });
    assert.equal(result.ok, true);
    assert.equal(result.value, undefined);
  }
  const invalid = await h.sendAsync({ type: "h2w_page_assist", action: "expect", generation: observed.generation, ref: observed.elements[3].ref, condition: "element_class_contains", value: "span.tag" });
  assert.equal(invalid.error, "expect_value_invalid");
});
test("user distinguishes clipped input preview from full input value | Given a long safe textarea value | When observe and scoped expect run | Then only the preview is exposed and the exact full value is verified", async () => {
  const fullValue = `Draft ${"x".repeat(220)} final`;
  const long = createElement({ tag: "textarea", value: fullValue });
  const short = createElement({ tag: "textarea", value: "Short draft" });
  const password = createElement({ tag: "input", type: "password", value: "private-value" });
  const h = harness({ elements: [long, short, password] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });

  assert.equal(observed.elements.length, 2);
  assert.equal(observed.elements[0].text, fullValue.slice(0, 120));
  assert.equal(observed.elements[0].text_truncated, true);
  assert.equal(observed.elements[1].text, "Short draft");
  assert.equal(observed.elements[1].text_truncated, undefined);
  assert.equal(JSON.stringify(observed).includes("final"), false);
  assert.equal(JSON.stringify(observed).includes("private-value"), false);

  const matched = await h.sendAsync({
    type: "h2w_page_assist", action: "expect", condition: "input_value_equals",
    generation: observed.generation, ref: observed.elements[0].ref, value: fullValue,
  });
  assert.equal(matched.ok, true);
  assert.equal(matched.value, undefined);
});

test("user rejects expired control assertions | Given an observed input whose generation has been replaced | When scoped expect uses its old ref | Then it fails closed", async () => {
  const h = harness({ elements: [createElement({ tag: "input", value: "draft" })] });
  const old = h.send({ type: "h2w_page_assist", action: "inspect" });
  h.send({ type: "h2w_page_assist", action: "inspect" });
  const result = await h.sendAsync({ type: "h2w_page_assist", action: "expect", generation: old.generation, ref: old.elements[0].ref, condition: "input_value_equals", value: "draft" });
  assert.equal(result.error, "stale_generation");
});

test("user finds controls after inert markup | Given 128 inert spans before a visible Cancel button | When element presence and absence are checked | Then inert candidates do not consume the control budget", async () => {
  const inert = Array.from({ length: 128 }, () => createElement({ tag: "span", text: "inert" }));
  const h = harness({ elements: [...inert, createElement({ text: "Cancel" }), createElement({ tag: "input", type: "password", attrs: { "aria-label": "Secret" } })] });
  for (const [condition, value] of [["element_present", "Cancel"], ["element_absent", "Missing"], ["element_absent", "Secret"]]) {
    const result = await h.sendAsync({ type: "h2w_page_assist", action: "expect", condition, value });
    assert.equal(result.ok, true);
  }
});

test("user cannot reuse refs across documents | Given two page instances observed in the same millisecond | When a ref from the first is clicked in the second | Then generation fencing rejects it", () => {
  class FixedDate extends Date { static now() { return 123456789; } }
  const a = harness({ elements: [createElement({ text: "First" })], extraGlobals: { Date: FixedDate } });
  const target = createElement({ text: "Second" });
  const b = harness({ elements: [target], extraGlobals: { Date: FixedDate } });
  const first = a.send({ type: "h2w_page_assist", action: "inspect" });
  const second = b.send({ type: "h2w_page_assist", action: "inspect" });
  assert.notEqual(first.generation, second.generation);
  const result = b.send({ type: "h2w_page_assist", action: "click", generation: first.generation, ref: first.elements[0].ref });
  assert.equal(result.error, "stale_generation");
  assert.equal(target.clicked, 0);
});

test("user clicks a nested custom choice | Given a bounded generic tag in a dialog | When its observed ref is clicked | Then the delegated choice handler receives the click", async () => {
  const dialog = createElement({ tag: "div", attrs: { role: "dialog" } });
  const choice = createElement({ tag: "span", text: "Experience mismatch", attrs: { class: "ui__tag outlined" } });
  choice.children = [createElement({ tag: "span", text: "Experience mismatch" })];
  choice.closest = (selector) => selector === 'form, dialog, [role="dialog"]' ? dialog : null;
  choice.querySelector = () => null;
  choice.click = function () { this.clicked += 1; dialog.clicked += 1; };
  const h = harness({ elements: [choice] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(observed.elements.length, 1);
  assert.equal(observed.elements[0].text, "Experience mismatch");
  const present = await h.sendAsync({ type: "h2w_page_assist", action: "expect", condition: "element_present", value: "Experience mismatch" });
  assert.equal(present.ok, true);
  const result = h.send({ type: "h2w_page_assist", action: "click", generation: observed.generation, ref: observed.elements[0].ref });
  assert.equal(result.ok, true);
  assert.equal(dialog.clicked, 1);
  h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.equal(h.send({ type: "h2w_page_assist", action: "click", generation: observed.generation, ref: observed.elements[0].ref }).error, "stale_generation");
});

test("user observes only bounded choice controls | Given inert tags and unsafe custom containers | When inspect scans them before Cancel | Then they receive no refs and do not consume the control budget", () => {
  const inert = Array.from({ length: 128 }, () => createElement({ tag: "span", text: "Decorative", attrs: { class: "tag" } }));
  const inertDiv = createElement({ tag: "div", text: "Inert", attrs: { class: "layout-container" } });
  inertDiv.getBoundingClientRect = () => { throw new Error("inert div must not trigger geometry checks"); };
  inertDiv.querySelector = () => { throw new Error("inert div must not trigger descendant checks"); };
  const broad = createElement({ tag: "div", text: "Broad choice", attrs: { class: "choice" } });
  broad.closest = () => ({});
  broad.getBoundingClientRect = () => ({ width: 900, height: 300 });
  const sensitive = createElement({ tag: "span", text: "Sensitive choice", attrs: { class: "chip" } });
  sensitive.closest = () => ({});
  sensitive.querySelector = () => createElement({ tag: "input", type: "password" });
  const h = harness({ elements: [...inert, inertDiv, broad, sensitive, createElement({ text: "Cancel" })] });
  const observed = h.send({ type: "h2w_page_assist", action: "inspect" });
  assert.deepEqual(Array.from(observed.elements, (el) => el.text), ["Cancel"]);
});
