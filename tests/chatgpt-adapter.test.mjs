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
    location: { origin: "https://chatgpt.com", pathname: "/c/current" },
    // Browser globals the adapter legitimately uses; the vm context does not
    // provide them by itself.
    AbortController,
    setTimeout,
    clearTimeout,
    ...(fetch ? { fetch } : {}),
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

test("user registers the signed-in account inside a ChatGPT Project | Given a Project session whose top-level id is the workspace while user.id matches /me | When the account is observed | Then the user id is the canonical identity and no identity conflict is reported", async () => {
  const payloads = {
    "/backend-api/me": { id: "user-abc", account: { id: "ws-workspace" } },
    "/api/auth/session": {
      id: "ws-workspace",
      user: { id: "user-abc" },
      account: { id: "ws-workspace" },
    },
  };
  const fetch = async (url) => ({ ok: true, json: async () => payloads[url] });
  const adapter = loadAdapterWithDocument({}, fetch);

  // Shape 1: the session's top-level id is the workspace/account id and shadows
  // a matching user.id. Only the user-namespace id may become the identity.
  const projectScoped = await adapter.getAccountObservation();
  assert.equal(projectScoped.nativeIdentity, "user-abc", "a workspace top-level id must not shadow the session user id");
  assert.equal(projectScoped.reason, undefined, "the same signed-in account is not an identity conflict");
  assert.equal(projectScoped.email, null, "no email is still a registerable identity when the user ids agree");
  assert.deepEqual(
    JSON.parse(JSON.stringify(projectScoped)),
    { nativeIdentity: "user-abc", email: null },
    "an accepted observation keeps the historical identity/email contract: the shape is a refusal diagnostic",
  );

  // The same Project route can answer /me with the workspace id on top and the
  // user id nested; the user-namespace id still wins.
  payloads["/backend-api/me"] = { id: "ws-workspace", user: { id: "user-abc" } };
  const nestedMe = await adapter.getAccountObservation();
  assert.equal(nestedMe.nativeIdentity, "user-abc");

  // Shape 2: the session carries only its workspace id. It stays in the
  // cross-check, and a positive account-id agreement is the only proof that
  // accepts /me's user id.
  payloads["/backend-api/me"] = { id: "user-abc", account: { id: "ws-workspace" } };
  delete payloads["/api/auth/session"].user;
  delete payloads["/api/auth/session"].id;
  const accountOnlySession = await adapter.getAccountObservation();
  assert.equal(accountOnlySession.nativeIdentity, "user-abc");
  assert.equal(accountOnlySession.identityVerification, "account_id_agreement",
    "a workspace-only session is accepted only through the account-id agreement proof");
});

test("user refuses an unprovable Project identity namespace | Given a workspace-only session and a /me that cannot prove the same account | When the account is observed | Then nothing is published and the refusal names the shape", async () => {
  const payloads = {
    "/backend-api/me": { id: "old-user" },
    "/api/auth/session": { account: { id: "new-workspace" } },
  };
  const fetch = async (url) => ({ ok: true, json: async () => payloads[url] });
  const adapter = loadAdapterWithDocument({}, fetch);

  // The session names no user id, so the endpoints share no user-namespace id to
  // compare, and /me exposes no account id to agree on either: the stale /me user
  // id must never be published on its own.
  const unprovable = await adapter.getAccountObservation();
  assert.equal(unprovable.nativeIdentity, null, "dropping an endpoint out of the cross-check would publish a stale /me");
  assert.equal(unprovable.email, null);
  assert.equal(unprovable.reason, "native_id_unconfirmed_namespace");
  assert.equal(unprovable.idShape.id_comparison, "single_user_id_present");
  assert.equal(unprovable.idShape.me_user_id, "me_user_id_present");
  assert.equal(unprovable.idShape.session_user_id, "session_user_id_absent");
  assert.equal(unprovable.idShape.me_account_id, "me_account_id_absent");
  assert.equal(unprovable.idShape.session_top_id, "session_top_id_absent");

  // A workspace-shaped top-level id is still not a user id.
  payloads["/api/auth/session"] = { id: "new-workspace", account: { id: "new-workspace" } };
  const workspaceTopId = await adapter.getAccountObservation();
  assert.equal(workspaceTopId.nativeIdentity, null);
  assert.equal(workspaceTopId.idShape.session_top_id, "session_top_id_equals_account");

  // A shared account id proves the same account, but contradictory emails still
  // refuse.
  payloads["/backend-api/me"] = { id: "old-user", email: "one@example.com", account: { id: "new-workspace" } };
  payloads["/api/auth/session"] = {
    user: { id: "new-user", email: "two@example.com" },
    account: { id: "new-workspace" },
  };
  const emailConflict = await adapter.getAccountObservation();
  assert.equal(emailConflict.nativeIdentity, null);
  assert.equal(emailConflict.reason, "native_id_conflict_different_email");
});

test("user refuses a genuinely different Project login | Given /me and the session name different users under one workspace id | When the account is observed | Then no identity is published and the conflict reason stays precise", async () => {
  const payloads = {
    "/backend-api/me": { id: "user-abc" },
    "/api/auth/session": {
      id: "ws-workspace",
      user: { id: "user-other" },
      account: { id: "ws-workspace" },
    },
  };
  const fetch = async (url) => ({ ok: true, json: async () => payloads[url] });
  const adapter = loadAdapterWithDocument({}, fetch);

  // Shape 3: two real user ids differ. #847's fix cannot and must not reconcile
  // this, so it stays fail-closed with the same precise enums the UAT recorded.
  const mismatch = await adapter.getAccountObservation();
  assert.equal(mismatch.nativeIdentity, null, "a real user-id mismatch must still fail closed before submit");
  assert.equal(mismatch.email, null);
  assert.equal(mismatch.reason, "native_id_conflict_unverified_email");
  assert.equal(mismatch.idRelation, "me_differs_session_user_and_account_id");
  assert.equal(mismatch.bearerRelation, "bearer_token_unavailable");
  assert.equal(mismatch.idShape.id_comparison, "user_ids_differ");
  assert.equal(mismatch.idShape.session_top_id, "session_top_id_equals_account");

  payloads["/backend-api/me"].email = "one@example.com";
  payloads["/api/auth/session"].user.email = "two@example.com";
  const differentEmails = await adapter.getAccountObservation();
  assert.equal(differentEmails.nativeIdentity, null);
  assert.equal(differentEmails.reason, "native_id_conflict_different_email");

  // The same user id in both endpoints is never a conflict, even when /me also
  // reports the workspace account id.
  payloads["/api/auth/session"].user.id = "user-abc";
  payloads["/api/auth/session"].user.email = "one@example.com";
  const consistent = await adapter.getAccountObservation();
  assert.equal(consistent.nativeIdentity, "user-abc");
  assert.equal(consistent.email, "one@example.com");
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

test("user reads a bearer-confirmed identity in the user namespace | Given the bearer re-read answers with a workspace top-level id beside the user id | When the conflict is resolved | Then the session user is adopted instead of a workspace id", async () => {
  const payloads = {
    "/backend-api/me": { id: "old-identity" },
    "/api/auth/session": {
      user: { id: "current-user", email: "current@example.com" },
      account: { id: "current-account" },
      accessToken: "test-token-not-for-logging",
    },
  };
  // The bearer re-read hits the same endpoint as the cookie /me but with the
  // session token, and a workspace-shaped top-level id must not be mistaken for
  // the user there either.
  const fetch = async (url, options = {}) => {
    if (url === "/backend-api/me" && options.headers?.authorization) {
      return { ok: true, json: async () => ({ id: "current-account", user: { id: "current-user" } }) };
    }
    return { ok: true, json: async () => payloads[url] };
  };
  const adapter = loadAdapterWithDocument({}, fetch);
  const observed = await adapter.getAccountObservation();
  assert.equal(observed.bearerRelation, "bearer_matches_session_user");
  assert.equal(observed.nativeIdentity, "current-user");
  assert.equal(observed.email, "current@example.com");
  assert.equal(observed.identityVerification, "bearer_verified_session_user");
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

test("user avoids hammering a throttled identity endpoint | Given /backend-api/me returns HTTP 429 with Retry-After | When account identity is observed twice in a row | Then the second authenticated endpoint is never read and nothing is read inside the cooldown", async () => {
  const requests = [];
  let rateLimited = true;
  const fetch = async (url) => {
    requests.push(url);
    return rateLimited
      ? { ok: false, status: 429, headers: { get: (key) => key.toLowerCase() === "retry-after" ? "45" : null } }
      : { ok: true, status: 200, json: async () => ({ id: "current-user" }) };
  };
  const adapter = loadAdapterWithDocument({}, fetch);

  // Measure from before the call so the lower-bound assertion cannot lose a
  // millisecond to scheduling between arming the cooldown and checking it.
  const armedAtOrAfter = Date.now();
  assert.equal(await adapter.getAccountNativeIdentity(), null, "a throttled probe reports no identity");
  assert.deepEqual(requests, ["/backend-api/me"], "the first 429 must short-circuit the second authenticated fetch");
  assert.equal(await adapter.getAccountNativeIdentity(), null);
  assert.equal(requests.length, 1, "back-to-back identity probes must not re-read inside the cooldown");
  assert.ok(adapter._accountIdentity429Until - armedAtOrAfter >= 45000, "Retry-After is honoured as a lower bound");

  // The cooldown is a bounded delay, not a permanent identity blackout.
  adapter._accountIdentity429Until = Date.now() - 1;
  rateLimited = false;
  assert.equal(await adapter.getAccountNativeIdentity(), "current-user", "identity probing resumes after the cooldown");
  assert.deepEqual(requests.slice(1), ["/backend-api/me", "/api/auth/session"],
    "the resumed observation cross-checks both authenticated endpoints");
});

test("user cools the secondary identity endpoint | Given /backend-api/me has no user id and /api/auth/session returns HTTP 429 | When a new identity probe arrives | Then both authenticated endpoints are read once and repeated probes are suppressed", async () => {
  const requests = [];
  const fetch = async (url) => {
    requests.push(url);
    return url === "/backend-api/me"
      ? { ok: true, status: 200, json: async () => ({}) }
      : { ok: false, status: 429, headers: { get: () => "30" } };
  };
  const adapter = loadAdapterWithDocument({}, fetch);

  assert.equal(await adapter.getAccountNativeIdentity(), null);
  assert.deepEqual(requests, ["/backend-api/me", "/api/auth/session"]);
  assert.equal(await adapter.getAccountNativeIdentity(), null);
  assert.equal(requests.length, 2, "the cooldown must cover both authenticated endpoints");
});

test("user observes an account inside the identity cooldown | Given /backend-api/me returned HTTP 429 | When a registration observation runs inside the cooldown | Then no authenticated endpoint is read again and the observation is neutral", async () => {
  const requests = [];
  const fetch = async (url) => {
    requests.push(url);
    return { ok: false, status: 429, headers: { get: () => "45" } };
  };
  const adapter = loadAdapterWithDocument({}, fetch);

  assert.equal(
    JSON.stringify(await adapter.getAccountObservation()),
    JSON.stringify({ nativeIdentity: null, email: null }),
    "a throttled observation reports no identity and no display email",
  );
  assert.deepEqual(requests, ["/backend-api/me"]);
  assert.equal(
    JSON.stringify(await adapter.getAccountObservation()),
    JSON.stringify({ nativeIdentity: null, email: null }),
  );
  assert.equal(requests.length, 1, "the cooldown also gates the observation entrypoint");
});

test("user bounds an untrusted Retry-After | Given the identity endpoint returns HTTP 429 with Retry-After 999999 | When the probe computes its cooldown | Then the delay stays inside the bounded policy instead of trusting the header", async () => {
  const fetch = async () => ({ ok: false, status: 429, headers: { get: () => "999999" } });
  const adapter = loadAdapterWithDocument({}, fetch);

  assert.equal(await adapter.getAccountNativeIdentity(), null);
  const remainingMs = adapter._accountIdentity429Until - Date.now();
  assert.ok(remainingMs > 0 && remainingMs <= 120000, `cooldown ${remainingMs}ms must stay bounded`);
});

test("user cools the identity probe after a throttled bearer check | Given the cookie endpoints disagree and the bearer verification returns HTTP 429 | When the next observation runs | Then the bearer check arms the cooldown and no identity read happens inside it", async () => {
  const requests = [];
  const fetch = async (url, options = {}) => {
    requests.push(options.headers?.authorization ? `${url}#bearer` : url);
    if (url === "/backend-api/me" && options.headers?.authorization) {
      return { ok: false, status: 429, headers: { get: () => "20" } };
    }
    if (url === "/backend-api/me") return { ok: true, json: async () => ({ id: "cookie-user" }) };
    return {
      ok: true,
      json: async () => ({
        user: { id: "session-user", email: "current@example.com" },
        accessToken: "test-token-not-for-logging",
      }),
    };
  };
  const adapter = loadAdapterWithDocument({}, fetch);
  const observed = await adapter.getAccountObservation();
  assert.equal(observed.nativeIdentity, null);
  assert.equal(observed.bearerRelation, "bearer_rate_limited");
  assert.deepEqual(requests, ["/backend-api/me", "/api/auth/session", "/backend-api/me#bearer"]);
  const armedMs = adapter._accountIdentity429Until - Date.now();
  assert.ok(armedMs > 0 && armedMs <= 120000, `a bearer 429 must arm a bounded cooldown, got ${armedMs}ms`);

  const throttled = await adapter.getAccountObservation();
  assert.equal(throttled.nativeIdentity, null);
  assert.equal(throttled.email, null, "a throttled observation stays neutral");
  assert.equal(requests.length, 3, "no authenticated identity read may run inside the cooldown");
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
