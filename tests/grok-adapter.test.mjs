import test from "node:test";
import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/injector/grok.js", import.meta.url), "utf8");
const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

function element({ text = "", attrs = {}, visible = true, parentElement = null } = {}) {
  return {
    innerText: text,
    textContent: text,
    visible,
    offsetParent: visible ? {} : null,
    parentElement,
    getAttribute(name) { return attrs[name] ?? null; },
  };
}

function harness(url = "https://grok.com/c/123e4567-e89b-12d3-a456-426614174000") {
  const selectors = new Map();
  const document = {
    querySelector(selector) {
      return (selectors.get(selector) || [])[0] || null;
    },
    querySelectorAll(selector) {
      return selectors.get(selector) || [];
    },
  };
  const location = new URL(url);
  const window = {};
  let sessionPayload = {
    status: "authenticated",
    session: { userId: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" },
  };
  class BaseAdapter {
    elementVisible(candidate) { return Boolean(candidate?.visible); }
  }
  const context = vm.createContext({
    BaseAdapter,
    URL,
    TextEncoder,
    Uint8Array,
    crypto: webcrypto,
    globalThis: null,
    document,
    location,
    window,
    CSS: { escape: (value) => String(value) },
    fetch: async (input) => {
      assert.equal(String(input), "/api/auth/session");
      return {
        ok: true,
        async json() { return sessionPayload; },
      };
    },
  });
  context.globalThis = context;
  vm.runInContext(source, context, { filename: "grok.js" });
  return {
    adapter: window.__H2W_ADAPTER__,
    location,
    set(selector, values) {
      selectors.set(selector, Array.isArray(values) ? values : [values]);
    },
    setSessionPayload(value) { sessionPayload = value; },
  };
}

test("Grok adapter exposes only concrete /c UUID session identity", () => {
  const h = harness("https://grok.com/c/123e4567-e89b-12d3-a456-426614174000?rid=ignored");
  assert.equal(h.adapter.name, "grok");
  assert.equal(
    h.adapter.getConversationKey(),
    "https://grok.com/c/123e4567-e89b-12d3-a456-426614174000",
  );
  assert.equal(h.adapter.getNativeSessionIdentity(), "123e4567-e89b-12d3-a456-426614174000");
  assert.equal(
    h.adapter.getCanonicalConversationUrl(),
    "https://grok.com/c/123e4567-e89b-12d3-a456-426614174000",
  );

  h.location.pathname = "/";
  assert.equal(h.adapter.getConversationKey(), null);
  h.location.pathname = "/imagine";
  assert.equal(h.adapter.getConversationKey(), null);
  h.location.pathname = "/c/not-a-uuid";
  assert.equal(h.adapter.getConversationKey(), null);
});

test("Grok adapter uses bounded semantic composer, turn, and generation selectors", () => {
  const h = harness();
  const composer = element();
  const send = element({ attrs: { "data-testid": "chat-submit" } });
  const stop = element({ attrs: { "aria-label": "Stop response" } });
  const firstUser = element({ text: "first prompt" });
  const latestUser = element({ text: "second prompt", attrs: { "data-message-id": "u2" } });
  const assistant = element({ text: "answer", attrs: { "data-message-id": "a1" } });

  h.set('[data-testid="chat-input"] div.ProseMirror[role="textbox"]', composer);
  h.set('button[data-testid="chat-submit"]', send);
  h.set('button[aria-label*="Stop" i]', stop);
  h.set('[data-testid="user-message"]', [firstUser, latestUser]);
  h.set('[data-testid="assistant-message"]', assistant);

  assert.equal(h.adapter.getInputEl(), composer);
  assert.equal(h.adapter.getSendButtonCandidates()[0], send);
  assert.equal(h.adapter.isGenerationInProgress(), true);
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("user"))),
    { messageId: "u2", text: "second prompt", count: 2 },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    { messageId: "a1", text: "answer", count: 1 },
  );
});

test("Grok adapter recovers stable role-scoped turn ids from response ancestors", () => {
  const h = harness();
  const userContainer = element({ attrs: { id: "response-11111111-2222-4333-8444-555555555555" } });
  const assistantContainer = element({ attrs: { id: "response-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" } });
  const user = element({ text: "prompt", parentElement: userContainer });
  const assistant = element({ text: "answer", parentElement: assistantContainer });
  h.set('[data-testid="user-message"]', user);
  h.set('[data-testid="assistant-message"]', assistant);

  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("user"))),
    {
      messageId: "11111111-2222-4333-8444-555555555555-user",
      text: "prompt",
      count: 1,
    },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    {
      messageId: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee-assistant",
      text: "answer",
      count: 1,
    },
  );
});

test("Grok adapter hashes same-origin session userId before returning native identity", async () => {
  const h = harness();
  const userId = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
  const expected = createHash("sha256").update(userId).digest("hex");
  assert.equal(
    await h.adapter.getAccountNativeIdentity(),
    `grok-account-sha256:${expected}`,
  );

  h.setSessionPayload({ status: "authenticated", session: { userId: "not-a-uuid" } });
  assert.equal(await h.adapter.getAccountNativeIdentity(), null);
});

test("Grok must reuse provider-neutral account and single-attempt browser actuation paths", () => {
  const accountStart = wakeSource.indexOf("async function browserAccountNativeIdentity()");
  const accountEnd = wakeSource.indexOf("async function registerCurrentConversation", accountStart);
  const accountSource = wakeSource.slice(accountStart, accountEnd);
  assert.match(accountSource, /\["gemini",\s*"claude",\s*"grok"\]\.includes\(ADAPTER\.name\)/);
  assert.match(accountSource, /ADAPTER\.getAccountNativeIdentity/);

  const actuationStart = wakeSource.indexOf("async function performBrowserActuationCommand(command)");
  const actuationEnd = wakeSource.indexOf("chrome.runtime.onMessage.addListener", actuationStart);
  const actuationSource = wakeSource.slice(actuationStart, actuationEnd);
  assert.match(actuationSource, /\["chatgpt",\s*"gemini",\s*"claude",\s*"grok"\]\.includes\(ADAPTER\.name\)/);
  assert.doesNotMatch(actuationSource, /ADAPTER\.name\s*===\s*"grok"/);
});
