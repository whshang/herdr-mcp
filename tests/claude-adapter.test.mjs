import test from "node:test";
import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/injector/claude.js", import.meta.url), "utf8");
const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

function element({ text = "", attrs = {}, visible = true } = {}) {
  return {
    innerText: text,
    textContent: text,
    visible,
    offsetParent: visible ? {} : null,
    getAttribute(name) { return attrs[name] ?? null; },
  };
}

function harness(url = "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000") {
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
  let accountPayload = { account: { email_address: "User.Name+Claude@example.com" } };
  let accountEndpointOk = true;
  const localStorageValues = new Map();
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
    localStorage: {
      getItem(key) { return localStorageValues.get(String(key)) ?? null; },
    },
    window,
    fetch: async (input) => {
      assert.equal(String(input), "/api/auth/current_account");
      return {
        ok: accountEndpointOk,
        async json() { return accountPayload; },
      };
    },
  });
  context.globalThis = context;
  vm.runInContext(source, context, { filename: "claude.js" });
  return {
    adapter: window.__H2W_ADAPTER__,
    location,
    set(selector, values) {
      selectors.set(selector, Array.isArray(values) ? values : [values]);
    },
    setAccountPayload(value) { accountPayload = value; },
    setAccountEndpointOk(value) { accountEndpointOk = value === true; },
    setLocalStorage(key, value) {
      if (value == null) localStorageValues.delete(String(key));
      else localStorageValues.set(String(key), String(value));
    },
  };
}

test("Claude adapter exposes only concrete /chat UUID session identity", () => {
  const h = harness("https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000?from=history");
  assert.equal(h.adapter.name, "claude");
  assert.equal(
    h.adapter.getConversationKey(),
    "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000",
  );
  assert.equal(h.adapter.getNativeSessionIdentity(), "123e4567-e89b-12d3-a456-426614174000");
  assert.equal(
    h.adapter.getCanonicalConversationUrl(),
    "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000",
  );

  h.location.pathname = "/new";
  assert.equal(h.adapter.getConversationKey(), null);
  assert.equal(h.adapter.getNativeSessionIdentity(), null);

  h.location.pathname = "/project/123e4567-e89b-12d3-a456-426614174000";
  assert.equal(h.adapter.getConversationKey(), null);

  h.location.pathname = "/chat/not-a-uuid";
  assert.equal(h.adapter.getConversationKey(), null);
});

test("Claude adapter uses bounded semantic composer, message, and generation selectors", () => {
  const h = harness();
  const composer = element();
  const send = element({ attrs: { "aria-label": "Send message" } });
  const stop = element({ attrs: { "aria-label": "Stop response" } });
  const firstUser = element({ text: "first prompt" });
  const latestUser = element({ text: "second prompt" });
  const assistant = element({ text: "answer", attrs: { "data-message-id": "local-assistant-id" } });

  h.set('[data-testid="chat-input"][contenteditable="true"]', composer);
  h.set('button[aria-label="Send message"]', send);
  h.set('button[aria-label*="Stop" i]', stop);
  h.set('[data-testid="user-message"]', [firstUser, latestUser]);
  h.set('.font-claude-response', assistant);

  assert.equal(h.adapter.getInputEl(), composer);
  assert.equal(h.adapter.getSendButtonCandidates()[0], send);
  assert.equal(h.adapter.isGenerationInProgress(), true);
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("user"))),
    { messageId: null, text: "second prompt", count: 2 },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    { messageId: "local-assistant-id", text: "answer", count: 1 },
  );
});

test("user gets scoped Claude transcript identity | Given stable transcript row metadata | When user and adjacent assistant snapshots are read | Then refs stay session role and ordinal scoped", () => {
  const h = harness();
  const makeTranscriptMessage = ({ role, index, text, streaming = false, rsIndex = index }) => {
    const article = element({ attrs: {
      "aria-posinset": String(index + 1),
      "aria-setsize": "4",
    } });
    const row = element({ attrs: {
      "data-testid": "transcript-row",
      "data-index": String(index),
      "data-rs-index": String(rsIndex),
      "data-perf-row": role === "user" ? "human" : "assistant",
      "data-perf-row-streaming": streaming ? "true" : "false",
    } });
    const message = element({ text });
    message.closest = (selector) => {
      if (selector === '[data-testid="transcript-row"]') return row;
      if (selector === '[role="article"]') return article;
      return null;
    };
    row.querySelector = (selector) => {
      if (role === "user" && selector.includes('data-testid="user-message"')) return message;
      if (role === "assistant" && selector.includes('data-testid="assistant-message"')) return message;
      return null;
    };
    return { article, row, message };
  };

  const user = makeTranscriptMessage({ role: "user", index: 2, text: "accepted prompt" });
  const assistant = makeTranscriptMessage({ role: "assistant", index: 3, text: "final answer" });
  h.set('[data-testid="user-message"]', user.message);
  h.set('.font-claude-response', assistant.message);
  h.set('[data-testid="transcript-row"]', [user.row, assistant.row]);

  const acceptedRef = "claude-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:2";
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("user"))),
    { messageId: acceptedRef, text: "accepted prompt", count: 1 },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    {
      messageId: "claude-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:3",
      text: "final answer",
      count: 1,
    },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getResultSettlementSnapshot(acceptedRef))),
    {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: "claude-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:3",
      userMessageId: acceptedRef,
      text: "final answer",
    },
  );

  const mismatched = makeTranscriptMessage({
    role: "user",
    index: 2,
    rsIndex: 9,
    text: "untrusted prompt",
  });
  h.set('[data-testid="user-message"]', mismatched.message);
  assert.equal(h.adapter.getMessageSnapshot("user").messageId, null);
  h.set('[data-testid="transcript-row"]', [mismatched.row, assistant.row]);
  assert.equal(h.adapter.getResultSettlementSnapshot(acceptedRef), null);
});

test("Claude adapter hashes current-account email before returning native identity", async () => {
  const h = harness();
  const expected = createHash("sha256")
    .update("user.name+claude@example.com")
    .digest("hex");
  assert.equal(
    await h.adapter.getAccountNativeIdentity(),
    `claude-account-sha256:${expected}`,
  );

  h.setAccountPayload({ account: {} });
  assert.equal(await h.adapter.getAccountNativeIdentity(), null);
});

test("Claude adapter falls back to two matching validated account UUID hints", async () => {
  const h = harness();
  const accountUuid = "123e4567-e89b-42d3-a456-426614174000";
  const expected = createHash("sha256").update(accountUuid).digest("hex");
  h.setAccountEndpointOk(false);
  h.setLocalStorage("__qk_hint_account_uuid", accountUuid.toUpperCase());
  h.setLocalStorage("rq-cache-confirmed-account", accountUuid);
  assert.equal(
    await h.adapter.getAccountNativeIdentity(),
    `claude-account-sha256:${expected}`,
  );

  h.setLocalStorage("rq-cache-confirmed-account", "223e4567-e89b-42d3-a456-426614174000");
  assert.equal(await h.adapter.getAccountNativeIdentity(), null);

  h.setLocalStorage("rq-cache-confirmed-account", "not-a-uuid");
  assert.equal(await h.adapter.getAccountNativeIdentity(), null);
});

test("Claude reuses the provider-neutral account and single-attempt browser actuation path", () => {
  const accountStart = wakeSource.indexOf("async function browserAccountNativeIdentity()");
  const accountEnd = wakeSource.indexOf("async function registerCurrentConversation", accountStart);
  const accountSource = wakeSource.slice(accountStart, accountEnd);
  assert.match(accountSource, /\[[^\]]*"claude"[^\]]*\]\.includes\(ADAPTER\.name\)/);
  assert.match(accountSource, /ADAPTER\.getAccountNativeIdentity/);
  assert.doesNotMatch(accountSource, /ADAPTER\.name\s*===\s*"claude"/);

  const actuationStart = wakeSource.indexOf("async function performBrowserActuationCommand(command)");
  const actuationEnd = wakeSource.indexOf("chrome.runtime.onMessage.addListener", actuationStart);
  const actuationSource = wakeSource.slice(actuationStart, actuationEnd);
  assert.match(actuationSource, /\[[^\]]*"claude"[^\]]*\]\.includes\(ADAPTER\.name\)/);
  assert.doesNotMatch(actuationSource, /ADAPTER\.name\s*===\s*"claude"/);
});
