import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/injector/chatgpt.js", import.meta.url), "utf8");
const legacySelector = '#prompt-textarea[contenteditable="true"]';
const projectSelector = 'div.ProseMirror[role="textbox"][contenteditable="true"]';

test("user can create a ChatGPT Project chat | Given an id-less composer | When the adapter locates it | Then only one visible editable textbox is accepted", () => {
  const selectors = new Map();
  let adapter;
  const document = {
    querySelector(selector) { return selectors.get(selector)?.[0] || null; },
    querySelectorAll(selector) { return selectors.get(selector) || []; },
  };
  class BaseAdapter {
    elementVisible(element) { return element?.visible === true; }
  }
  const registerH2WAdapter = (registered) => { adapter = registered; };
  const location = { origin: "https://chatgpt.com", pathname: "/g/g-p-test/project" };
  vm.runInNewContext(source, { BaseAdapter, registerH2WAdapter, document, location }, {
    filename: "chatgpt.js",
  });

  const project = { id: "", visible: true };
  selectors.set(projectSelector, [project]);
  assert.equal(adapter.getInputEl(), project);
  assert.equal(adapter.getWatchMainWorldSelector(), projectSelector);

  const legacy = { id: "prompt-textarea", visible: true };
  selectors.set(legacySelector, [legacy]);
  assert.equal(adapter.getInputEl(), legacy);
  assert.equal(adapter.getWatchMainWorldSelector(), legacySelector);

  selectors.delete(legacySelector);
  selectors.set(projectSelector, [project, { id: "", visible: true }]);
  assert.equal(adapter.getInputEl(), null);
  assert.equal(adapter.getWatchMainWorldSelector(), null);

  selectors.set(projectSelector, [{ id: "", visible: false }]);
  assert.equal(adapter.getInputEl(), null);

  location.pathname = "/g/g-p-test/c/local-chatgpt%3Abdf4ff94-c1cd-45f7-b141-e9218f0fdd00";
  assert.equal(adapter.getConversationKey(), null);
  location.pathname = "/c/local-chatgpt:bdf4ff94-c1cd-45f7-b141-e9218f0fdd00";
  assert.equal(adapter.getConversationKey(), null);
  location.pathname = "/g/g-p-test/c/01234567-89ab-4cde-8f01-23456789abcd";
  assert.equal(adapter.getConversationKey(), "https://chatgpt.com/g/g-p-test/c/01234567-89ab-4cde-8f01-23456789abcd");
});

// ---- Minimal DOM model for the composer App picker and accepted-message identity ----

function makeEl({ tag = "div", attrs = {}, children = [], visible = true, text = "" } = {}) {
  const element = {
    tagName: String(tag).toUpperCase(),
    attrs: { ...attrs },
    children: [],
    parent: null,
    visible,
    ownText: text,
    offsetWidth: visible ? 10 : 0,
    offsetHeight: visible ? 10 : 0,
    disabled: false,
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null;
    },
    hasAttribute(name) {
      return Object.prototype.hasOwnProperty.call(this.attrs, name);
    },
    get textContent() {
      return this.ownText + this.children.map((child) => child.textContent).join("");
    },
    contains(node) {
      for (let current = node; current; current = current.parent) {
        if (current === this) return true;
      }
      return false;
    },
    matches(selector) {
      return selector.split(",").some((part) => matchesSimple(this, part.trim()));
    },
    closest(selector) {
      for (let current = this; current; current = current.parent) {
        if (current.matches(selector)) return current;
      }
      return null;
    },
    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      const parts = selector.split(",").map((part) => part.trim()).filter(Boolean);
      const out = [];
      const walk = (node) => {
        for (const child of node.children) {
          if (parts.some((part) => matchesSimple(child, part)) && !out.includes(child)) out.push(child);
          walk(child);
        }
      };
      walk(this);
      return out;
    },
    id() {
      return this.attrs.id || "";
    },
  };
  for (const child of children) {
    child.parent = element;
    element.children.push(child);
  }
  return element;
}

function matchesSimple(element, selector) {
  let rest = selector;
  while (rest) {
    if (rest.startsWith("#")) {
      const match = rest.match(/^#([A-Za-z0-9_-]+)/);
      if (!match) return false;
      if (element.attrs.id !== match[1]) return false;
      rest = rest.slice(match[0].length);
      continue;
    }
    const attr = rest.match(/^\[([A-Za-z0-9_-]+)(?:([*^$]?=)"([^"]*)")?\]/);
    if (attr) {
      if (!element.hasAttribute(attr[1])) return false;
      if (attr[3] !== undefined) {
        const value = String(element.getAttribute(attr[1]) || "");
        if (attr[2] === "*" ? !value.includes(attr[3]) : value !== attr[3]) return false;
      }
      rest = rest.slice(attr[0].length);
      continue;
    }
    const tag = rest.match(/^[A-Za-z][A-Za-z0-9-]*/);
    if (tag) {
      if (element.tagName !== tag[0].toUpperCase()) return false;
      rest = rest.slice(tag[0].length);
      continue;
    }
    const cls = rest.match(/^\.([A-Za-z0-9_-]+)/);
    if (cls) {
      const classes = String(element.getAttribute("class") || "").split(/\s+/);
      if (!classes.includes(cls[1])) return false;
      rest = rest.slice(cls[0].length);
      continue;
    }
    return false;
  }
  return true;
}

function loadAdapterWithDocument(document, fetch = undefined) {
  let adapter;
  class BaseAdapter {
    elementVisible(element) { return element?.visible === true; }
  }
  vm.runInNewContext(source, {
    BaseAdapter,
    registerH2WAdapter: (registered) => { adapter = registered; },
    document,
    fetch,
    AbortController,
    setTimeout,
    clearTimeout,
    location: { origin: "https://chatgpt.com", pathname: "/c/current" },
  }, { filename: "chatgpt.js" });
  return adapter;
}

const composerSelector = 'div.ProseMirror[role="textbox"][contenteditable="true"]';

function appPickerDocument({ popupId = "app-popup", popup = null, extraRoots = [] } = {}) {
  const composer = makeEl({
    tag: "div",
    attrs: {
      role: "textbox",
      contenteditable: "true",
      class: "ProseMirror",
      ...(popupId ? { "aria-controls": popupId } : {}),
    },
  });
  const popupEl = popup ?? makeEl({ attrs: { id: popupId } });
  const document = {
    querySelector(selector) {
      return this.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      const nodes = [composer, popupEl, ...extraRoots];
      return nodes.filter((node) => node.matches(selector));
    },
    getElementById(id) {
      return [composer, popupEl, ...extraRoots].find((node) => node.id() === id) || null;
    },
  };
  return { document, composer, popup: popupEl };
}

test("user app selection ignores unrelated controls | Given a sidebar button and a composer-owned popup option share the keyword | When candidates are resolved | Then only the composer-owned exact option is returned", () => {
  const exact = makeEl({ attrs: { role: "option", "data-keyword": "herdr" }, visible: true });
  const substring = makeEl({ attrs: { role: "option", "data-keyword": "herdr-tools" }, visible: true });
  // A sidebar popover option whose only "match" would be substring/loose text.
  const foreign = makeEl({ tag: "button", attrs: { role: "option" }, text: "herdr", visible: true });
  const { document } = appPickerDocument({
    popup: makeEl({ attrs: { id: "app-popup" }, children: [exact, substring] }),
    extraRoots: [makeEl({ attrs: { class: "popover" }, children: [foreign] })],
  });
  const adapter = loadAdapterWithDocument(document);
  // Spread into host-realm arrays: the adapter runs inside a vm context.
  assert.deepEqual([...adapter.getComposerAppCandidates("herdr")], [exact]);
  assert.equal(adapter.getComposerAppCandidates("herd").length, 0, "substring text is never a keyword match");
  assert.deepEqual([...adapter.getComposerAppCandidates("herdr-tools")], [substring]);
});

test("user app search fails closed without a composer-owned popup | Given a matching option exists on the page but the composer owns no popup | When candidates are resolved | Then nothing is returned", () => {
  const foreign = makeEl({ attrs: { role: "option", "data-keyword": "herdr" }, visible: true });
  const { document } = appPickerDocument({
    popupId: null,
    extraRoots: [makeEl({ attrs: { role: "menu" }, children: [foreign] })],
  });
  const adapter = loadAdapterWithDocument(document);
  assert.equal(adapter.getComposerAppCandidates("herdr").length, 0);
  assert.equal(adapter.getComposerAppSuggestionButtons("herdr").length, 0);
});

test("user rejects inconsistent ChatGPT account identity | Given authenticated endpoints name different users | When the adapter observes login | Then registration identity is unavailable", async () => {
  const payloads = {
    "/backend-api/me": { id: "prior-login", email: "prior@example.com" },
    "/api/auth/session": { user: { id: "new-login", email: "current@example.com" } },
  };
  const fetch = async (url) => ({ ok: true, json: async () => payloads[url] });
  const adapter = loadAdapterWithDocument({}, fetch);
  const mismatch = await adapter.getAccountObservation();
  assert.equal(mismatch.nativeIdentity, null);
  assert.equal(mismatch.email, null);

  payloads["/api/auth/session"].user.id = "prior-login";
  const emailConflict = await adapter.getAccountObservation();
  assert.equal(emailConflict.nativeIdentity, null, "conflicting verified emails also fail closed");
  assert.equal(emailConflict.email, null);
  payloads["/api/auth/session"].user.email = "prior@example.com";
  const consistent = await adapter.getAccountObservation();
  assert.equal(consistent.nativeIdentity, "prior-login");
  assert.equal(consistent.email, "prior@example.com");
});

test("user never accepts plain text as an App identity | Given an accepted turn with only the text @herdr | When the accepted message identity is read | Then no App is reported", () => {
  const hidden = makeEl({ attrs: { role: "menu", "data-keyword": "herdr" } });
  const plain = makeEl({
    attrs: { "data-message-id": "user-plain" },
    children: [makeEl({ attrs: { "data-message-author-role": "user" }, text: "@herdr hello" })],
  });
  const pathless = makeEl({
    attrs: { "data-message-id": "user-pathless" },
    children: [makeEl({
      attrs: { "data-message-author-role": "user" },
      children: [makeEl({
        tag: "span",
        attrs: { "data-appearance": "inline-mention", "app-mention-name": "herdr" },
      })],
    })],
  });
  const withPath = makeEl({
    attrs: { "data-message-id": "user-with-path" },
    children: [makeEl({
      attrs: { "data-message-author-role": "user" },
      children: [makeEl({
        tag: "span",
        attrs: {
          "data-appearance": "inline-mention",
          "app-mention-name": "herdr",
          "app-mention-path": "connectors/herdr",
        },
      })],
    })],
  });
  const ambiguousA = makeEl({ attrs: { "data-message-id": "user-dup" } });
  const ambiguousB = makeEl({ attrs: { "data-message-id": "user-dup" } });
  const document = {
    querySelector: () => null,
    querySelectorAll(selector) {
      return [plain, pathless, withPath, ambiguousA, ambiguousB, hidden].filter((node) => node.matches(selector));
    },
    getElementById: () => null,
  };
  const adapter = loadAdapterWithDocument(document);
  const identityOf = (id) => [...adapter.getAcceptedUserAppIdentity(id)].map((app) => ({
    name: app.name,
    path: app.path,
  }));
  assert.deepEqual(identityOf("user-plain"), [], "plain-text @herdr is not an App selection");
  assert.deepEqual(identityOf("user-pathless"), [], "an App mention without a provider path never counts");
  assert.deepEqual(identityOf("user-with-path"), [{ name: "herdr", path: "connectors/herdr" }]);
  assert.deepEqual(identityOf("user-dup"), [], "an ambiguous carrier of the same id fails closed");
  assert.deepEqual(identityOf(""), []);
});
