// ChatGPT (2026-10) no longer renders [data-message-author-role="user"]; user
// turns are div[data-user-message-bubble="true"] with the text in an inner
// .whitespace-pre-wrap node (#729). These tests exercise the shipped wake.js
// user-turn helpers and the ChatGPT adapter against both DOM generations.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
const adapterSource = readFileSync(new URL("../extension/content/injector/chatgpt.js", import.meta.url), "utf8");

function el({ tag = "div", attrs = {}, classes = [], text = "", children = [] } = {}) {
  const node = {
    tagName: tag.toUpperCase(),
    attrs: { ...attrs },
    classes: new Set(classes),
    ownText: text,
    children: [],
    parent: null,
    getAttribute(name) { return Object.hasOwn(this.attrs, name) ? this.attrs[name] : null; },
    get textContent() { return this.ownText + this.children.map((child) => child.textContent).join(""); },
    get innerText() { return this.textContent; },
    matches(selector) { return selector.split(",").some((part) => matchesSimple(this, part.trim())); },
    closest(selector) {
      for (let current = this; current; current = current.parent) if (current.matches(selector)) return current;
      return null;
    },
    querySelectorAll(selector) {
      const out = [];
      const walk = (parent) => {
        for (const child of parent.children) {
          if (child.matches(selector)) out.push(child);
          walk(child);
        }
      };
      walk(this);
      return out;
    },
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
  };
  for (const child of children) { child.parent = node; node.children.push(child); }
  return node;
}

function matchesSimple(node, selector) {
  let rest = selector;
  if (!rest) return false;
  const tag = rest.match(/^[A-Za-z][A-Za-z0-9-]*/);
  if (tag) {
    if (node.tagName !== tag[0].toUpperCase()) return false;
    rest = rest.slice(tag[0].length);
  }
  while (rest) {
    const cls = rest.match(/^\.([A-Za-z0-9_-]+)/);
    if (cls) {
      if (!node.classes.has(cls[1])) return false;
      rest = rest.slice(cls[0].length);
      continue;
    }
    const attr = rest.match(/^\[([A-Za-z0-9_-]+)(?:(\^?=)"([^"]*)")?\]/);
    if (attr) {
      const value = node.getAttribute(attr[1]);
      if (value === null) return false;
      if (attr[3] !== undefined && (attr[2] === "^=" ? !value.startsWith(attr[3]) : value !== attr[3])) return false;
      rest = rest.slice(attr[0].length);
      continue;
    }
    return false;
  }
  return true;
}

function documentOf(root) {
  return { querySelectorAll: (selector) => root.querySelectorAll(selector), querySelector: (s) => root.querySelector(s) };
}

const HELPERS_START = "  // ---- ChatGPT user-turn selectors ----";
const HELPERS_END = "\n  function observedConversationTurns()";

function loadWakeHelpers(document) {
  const start = wakeSource.indexOf(HELPERS_START);
  const end = wakeSource.indexOf(HELPERS_END, start);
  assert.ok(start >= 0 && end > start, "the user-turn helpers must stay extractable from wake.js");
  const body = `${wakeSource.slice(start, end)}
  ({ conversationTurnRole, userTurnTextRoot, conversationTurnNodesForRole });`;
  return vm.runInNewContext(body, { document });
}

function bubble(text, attrs = {}) {
  return el({
    attrs: { "data-user-message-bubble": "true", ...attrs },
    children: [el({ classes: ["text-size-chat", "whitespace-pre-wrap"], text })],
  });
}

test("user turn is found in the 2026-10 ChatGPT DOM | Given only data-user-message-bubble user turns | When wake.js looks up user turns | Then the bubbles are user turns and text comes from the inner pre-wrap node", () => {
  const first = bubble("first question");
  const latest = bubble("latest question");
  const assistant = el({ attrs: { "data-message-author-role": "assistant" }, text: "answer" });
  const root = el({
    children: [
      el({ attrs: { "data-content-search-unit-key": "a:user" }, children: [first] }),
      assistant,
      el({ attrs: { "data-content-search-unit-key": "b:user" }, children: [latest] }),
    ],
  });
  const helpers = loadWakeHelpers(documentOf(root));
  const users = helpers.conversationTurnNodesForRole("user");
  assert.equal(users.length, 2);
  assert.equal(users[1], latest);
  assert.equal(helpers.conversationTurnRole(latest), "user");
  assert.equal(helpers.conversationTurnRole(assistant), "assistant");
  assert.equal(helpers.userTurnTextRoot(latest).textContent, "latest question");
  assert.equal(helpers.userTurnTextRoot(assistant), assistant, "non-bubble roots are unchanged");
  assert.equal(helpers.conversationTurnNodesForRole("assistant").length, 1);
});

test("user turn keeps the legacy selector first | Given a legacy author-role user turn that also wraps a bubble | When wake.js counts user turns | Then the turn is counted once via the legacy node", () => {
  const nested = bubble("legacy text");
  const legacy = el({ attrs: { "data-message-author-role": "user" }, children: [nested] });
  const helpers = loadWakeHelpers(documentOf(el({ children: [legacy] })));
  const users = helpers.conversationTurnNodesForRole("user");
  assert.equal(users.length, 1);
  assert.equal(users[0], legacy);
  assert.equal(helpers.conversationTurnRole(nested), null, "a covered bubble is not a second user turn");
});

function loadAdapter(document) {
  let adapter;
  class BaseAdapter { elementVisible() { return true; } }
  vm.runInNewContext(adapterSource, {
    BaseAdapter,
    registerH2WAdapter: (registered) => { adapter = registered; },
    document,
    location: { origin: "https://chatgpt.com", pathname: "/c/current" },
    AbortController,
    setTimeout,
    clearTimeout,
  }, { filename: "chatgpt.js" });
  return adapter;
}

test("user App keywords are read from the bubble DOM | Given only data-user-message-bubble user turns | When the adapter reads the latest user App keywords | Then the latest bubble's App pill is reported", () => {
  const pill = el({ attrs: { "data-inline-selection-pill": "", "data-symbol": "ecosystemMention", "data-keyword": "Herdr" } });
  const older = bubble("older");
  const latest = el({ attrs: { "data-user-message-bubble": "true" }, children: [pill] });
  const adapter = loadAdapter(documentOf(el({ children: [older, latest] })));
  assert.deepEqual([...adapter.getLatestUserAppKeywords()], ["herdr"]);
});

test("user accepted App identity resolves through a bubble carrier | Given a data-message-id carrier holding a bubble with an App mention | When the accepted identity is read | Then the provider mention is reported", () => {
  const mention = el({ tag: "span", attrs: { "data-appearance": "inline-mention", "app-mention-name": "herdr", "app-mention-path": "connectors/herdr" } });
  const carrier = el({ attrs: { "data-message-id": "u1" }, children: [el({ attrs: { "data-user-message-bubble": "true" }, children: [mention] })] });
  const adapter = loadAdapter(documentOf(el({ children: [carrier] })));
  const identity = [...adapter.getAcceptedUserAppIdentity("u1")].map((app) => ({ name: app.name, path: app.path }));
  assert.deepEqual(identity, [{ name: "herdr", path: "connectors/herdr" }]);
});
