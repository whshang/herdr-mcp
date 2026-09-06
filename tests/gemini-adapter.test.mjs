import test from "node:test";
import assert from "node:assert/strict";
import { createHash, webcrypto } from "node:crypto";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/injector/gemini.js", import.meta.url), "utf8");

function element({ text = "", attrs = {}, visible = true } = {}) {
  return {
    innerText: text,
    textContent: text,
    visible,
    getAttribute(name) { return attrs[name] ?? null; },
  };
}

function harness(url = "https://gemini.google.com/app/abc123") {
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
  });
  context.globalThis = context;
  vm.runInContext(source, context, { filename: "gemini.js" });
  return {
    adapter: window.__H2W_ADAPTER__,
    location,
    set(selector, values) {
      selectors.set(selector, Array.isArray(values) ? values : [values]);
    },
  };
}

test("Gemini adapter exposes only concrete /app session identity", () => {
  const h = harness("https://gemini.google.com/app/abc_123-XYZ?hl=en");
  assert.equal(h.adapter.name, "gemini");
  assert.equal(h.adapter.getConversationKey(), "https://gemini.google.com/app/abc_123-XYZ");
  assert.equal(h.adapter.getNativeSessionIdentity(), "abc_123-XYZ");
  assert.equal(h.adapter.getCanonicalConversationUrl(), "https://gemini.google.com/app/abc_123-XYZ");

  h.location.pathname = "/app";
  assert.equal(h.adapter.getConversationKey(), null);
  assert.equal(h.adapter.getNativeSessionIdentity(), null);

  h.location.pathname = "/gem/abc";
  assert.equal(h.adapter.getConversationKey(), null);
});

test("Gemini adapter uses bounded semantic composer and generation observations", () => {
  const h = harness();
  const composer = element();
  const send = element({ attrs: { "aria-label": "Send message" } });
  const stop = element({ attrs: { "aria-label": "Stop response" } });
  h.set('div.ql-editor.textarea[contenteditable="true"]', composer);
  h.set('button[aria-label="Send message"]', send);
  h.set('button[aria-label*="Stop" i]', stop);

  assert.equal(h.adapter.getInputEl(), composer);
  assert.equal(h.adapter.getWatchMainWorldSelector(), 'div.ql-editor.textarea[contenteditable="true"]');
  const sendCandidates = h.adapter.getSendButtonCandidates();
  const stopCandidates = h.adapter.getStopButtonCandidates();
  assert.equal(sendCandidates.length, 1);
  assert.equal(sendCandidates[0], send);
  assert.equal(stopCandidates.length, 1);
  assert.equal(stopCandidates[0], stop);
  assert.equal(h.adapter.isGenerationInProgress(), true);
});

test("Gemini adapter observes user/assistant baselines without inventing provider ids", () => {
  const h = harness();
  const firstUser = element({ text: "first prompt" });
  const latestUser = element({ text: "second prompt" });
  const assistant = element({ text: "answer", attrs: { "data-message-id": "local-dom-id" } });
  h.set("user-query", [firstUser, latestUser]);
  h.set("model-response", [assistant]);

  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("user"))),
    { messageId: null, text: "second prompt", count: 2 },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    { messageId: "local-dom-id", text: "answer", count: 1 },
  );
});

test("Gemini adapter hashes Google account email before returning native identity", async () => {
  const h = harness();
  const account = element({
    attrs: { "aria-label": "Google Account: Example User (User.Name+Gemini@example.com)" },
  });
  h.set('a[aria-label*="Google Account"]', account);

  const expected = createHash("sha256")
    .update("user.name+gemini@example.com")
    .digest("hex");
  assert.equal(
    await h.adapter.getAccountNativeIdentity(),
    `google-account-sha256:${expected}`,
  );
});
