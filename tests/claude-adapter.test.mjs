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
    fetch: async (input) => {
      assert.equal(String(input), "/api/auth/current_account");
      return {
        ok: true,
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

test("Claude reuses the provider-neutral account and single-attempt browser actuation path", () => {
  const accountStart = wakeSource.indexOf("async function browserAccountNativeIdentity()");
  const accountEnd = wakeSource.indexOf("async function registerCurrentConversation", accountStart);
  const accountSource = wakeSource.slice(accountStart, accountEnd);
  assert.match(accountSource, /\["gemini",\s*"claude"\]\.includes\(ADAPTER\.name\)/);
  assert.match(accountSource, /ADAPTER\.getAccountNativeIdentity/);

  const actuationStart = wakeSource.indexOf("async function performBrowserActuationCommand(command)");
  const actuationEnd = wakeSource.indexOf("chrome.runtime.onMessage.addListener", actuationStart);
  const actuationSource = wakeSource.slice(actuationStart, actuationEnd);
  assert.match(actuationSource, /\["chatgpt",\s*"gemini",\s*"claude"\]\.includes\(ADAPTER\.name\)/);
  assert.doesNotMatch(actuationSource, /ADAPTER\.name\s*===\s*"claude"/);
});
