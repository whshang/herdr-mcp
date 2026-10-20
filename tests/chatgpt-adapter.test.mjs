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
  assert.equal(mismatch.reason, "native_id_conflict_different_email");
  payloads["/api/auth/session"].account = { id: "prior-login" };
  const accountIdMatchesMe = await adapter.getAccountObservation();
  assert.equal(accountIdMatchesMe.nativeIdentity, null, "do not silently reconcile different identity namespaces");
  assert.equal(accountIdMatchesMe.idRelation, "me_matches_session_account_id");
  assert.equal(accountIdMatchesMe.emailProof, "emails_differ");
  payloads["/api/auth/session"].account.id = "other-account";
  const accountIdDiffersMe = await adapter.getAccountObservation();
  assert.equal(accountIdDiffersMe.idRelation, "me_differs_session_user_and_account_id");
  assert.equal(accountIdDiffersMe.emailProof, "emails_differ");
  delete payloads["/api/auth/session"].account;
  const noAccountId = await adapter.getAccountObservation();
  assert.equal(noAccountId.idRelation, "me_differs_session_user_without_account_id");

  payloads["/api/auth/session"].user.email = "prior@example.com";
  const sameEmailDifferentId = await adapter.getAccountObservation();
  assert.equal(sameEmailDifferentId.nativeIdentity, null);
  assert.equal(sameEmailDifferentId.reason, "native_id_conflict_matching_email");
  payloads["/api/auth/session"].user.email = "current@example.com";

  payloads["/api/auth/session"].user.id = "prior-login";
  const emailConflict = await adapter.getAccountObservation();
  assert.equal(emailConflict.nativeIdentity, null, "conflicting verified emails also fail closed");
  assert.equal(emailConflict.email, null);
  assert.equal(emailConflict.reason, "email_conflict");
  payloads["/api/auth/session"].user.email = "prior@example.com";
  const consistent = await adapter.getAccountObservation();
  assert.equal(consistent.nativeIdentity, "prior-login");
  assert.equal(consistent.email, "prior@example.com");
});

test("user safely adopts a bearer-verified session identity | Given conflicting cookie and session IDs | When the active bearer proves the session user | Then the canonical account identity is bounded and cache-safe", async () => {
  let bearerCalls = 0;
  const payloads = {
    "/backend-api/me": { id: "old-identity" },
    "/api/auth/session": {
      user: { id: "current-user", email: "current@example.com" },
      account: { id: "current-account" },
      accessToken: "test-token-not-for-logging",
    },
  };
  const fetch = async (url, options = {}) => {
    if (url === "/backend-api/me" && options.headers?.authorization) {
      bearerCalls++;
      assert.equal(options.headers.authorization, "Bearer test-token-not-for-logging");
      return { ok: true, json: async () => ({ id: "current-user" }) };
    }
    return { ok: true, json: async () => payloads[url] };
  };
  const adapter = loadAdapterWithDocument({}, fetch);
  const observed = await adapter.getAccountObservation();
  assert.equal(observed.nativeIdentity, "current-user", "bearer-proven active session user becomes the canonical identity");
  assert.equal(observed.email, "current@example.com");
  assert.equal(observed.identityVerification, "bearer_verified_session_user");
  assert.equal(observed.bearerRelation, "bearer_matches_session_user");
  assert.equal(observed.emailProof, "me_email_missing");
  assert.equal(bearerCalls, 1);
  const again = await adapter.getAccountObservation();
  assert.equal(again.nativeIdentity, "current-user");
  assert.equal(again.bearerRelation, "bearer_matches_session_user");
  assert.equal(bearerCalls, 1, "do not hammer /backend-api/me on every registration poll");

  payloads["/api/auth/session"].user.id = "new-session-user";
  const changed = await adapter.getAccountObservation();
  assert.equal(changed.nativeIdentity, null);
  assert.equal(changed.bearerRelation, "bearer_differs_from_session");
  assert.equal(bearerCalls, 2, "a changed authenticated identity invalidates the cached proof");
});

test("user refuses unproven bearer login identity | Given a mismatched cookie user | When bearer verification fails or session email is unsafe | Then no account is registered", async () => {
  const payloads = {
    "/backend-api/me": { id: "stale-cookie-user" },
    "/api/auth/session": {
      user: { id: "live-session-user", email: "current@example.com" },
      accessToken: "private-token",
    },
  };
  let bearerStatus = 403;
  const fetch = async (url, options = {}) => {
    if (url === "/backend-api/me" && options.headers?.authorization) {
      return bearerStatus === 200
        ? { ok: true, json: async () => ({ id: "live-session-user" }) }
        : { ok: false, status: bearerStatus };
    }
    return { ok: true, json: async () => payloads[url] };
  };
  const adapter = loadAdapterWithDocument({}, fetch);
  const denied = await adapter.getAccountObservation();
  assert.equal(denied.nativeIdentity, null);
  assert.equal(denied.bearerRelation, "bearer_not_authorized");

  payloads["/api/auth/session"].user.id = "different-active-user";
  bearerStatus = 429;
  const limited = await adapter.getAccountObservation();
  assert.equal(limited.nativeIdentity, null);
  assert.equal(limited.bearerRelation, "bearer_rate_limited");

  payloads["/backend-api/me"].email = "different@example.com";
  bearerStatus = 200;
  payloads["/api/auth/session"].user.id = "live-session-user";
  const disagree = await adapter.getAccountObservation();
  assert.equal(disagree.nativeIdentity, null, "different authenticated emails must reject even a valid session bearer");

  delete payloads["/backend-api/me"].email;
  delete payloads["/api/auth/session"].user.email;
  payloads["/api/auth/session"].user.id = "changed-session-user";
  const noEmail = await adapter.getAccountObservation();
  assert.equal(noEmail.nativeIdentity, null, "bearer proof alone cannot publish an unlabeled account");
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
