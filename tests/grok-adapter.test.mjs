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

test("Grok adapter exposes concrete direct and project chat session identity", () => {
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

  const project = harness(
    "https://grok.com/project/223e4567-e89b-42d3-a456-426614174001?ignored=1&chat=323e4567-e89b-42d3-a456-426614174002",
  );
  assert.equal(
    project.adapter.getConversationKey(),
    "https://grok.com/project/223e4567-e89b-42d3-a456-426614174001?chat=323e4567-e89b-42d3-a456-426614174002",
  );
  assert.equal(project.adapter.getNativeSessionIdentity(), "323e4567-e89b-42d3-a456-426614174002");
  assert.equal(
    project.adapter.getCanonicalConversationUrl(),
    "https://grok.com/project/223e4567-e89b-42d3-a456-426614174001?chat=323e4567-e89b-42d3-a456-426614174002",
  );

  h.location.pathname = "/";
  assert.equal(h.adapter.getConversationKey(), null);
  h.location.pathname = "/imagine";
  assert.equal(h.adapter.getConversationKey(), null);
  h.location.pathname = "/c/not-a-uuid";
  assert.equal(h.adapter.getConversationKey(), null);

  const missingProjectChat = harness("https://grok.com/project/223e4567-e89b-42d3-a456-426614174001");
  assert.equal(missingProjectChat.adapter.getConversationKey(), null);
  const invalidProject = harness(
    "https://grok.com/project/not-a-uuid?chat=323e4567-e89b-42d3-a456-426614174002",
  );
  assert.equal(invalidProject.adapter.getConversationKey(), null);
  const invalidProjectChat = harness(
    "https://grok.com/project/223e4567-e89b-42d3-a456-426614174001?chat=not-a-uuid",
  );
  assert.equal(invalidProjectChat.adapter.getConversationKey(), null);
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

test("user keeps stable Grok turn identity | Given response wrappers are replaced | When the adapter snapshots the same transcript positions | Then synthetic refs stay stable", () => {
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
      messageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
      text: "prompt",
      count: 1,
    },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getMessageSnapshot("assistant"))),
    {
      messageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:0",
      text: "answer",
      count: 1,
    },
  );

  h.set('[data-testid="user-message"]', element({
    text: "prompt",
    parentElement: element({ attrs: { id: "response-bbbbbbbb-cccc-4ddd-8eee-ffffffffffff" } }),
  }));
  h.set('[data-testid="assistant-message"]', element({
    text: "answer",
    parentElement: element({ attrs: { id: "response-22222222-3333-4444-8555-666666666666" } }),
  }));
  assert.equal(
    h.adapter.getMessageSnapshot("user").messageId,
    "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
  );
  assert.equal(
    h.adapter.getMessageSnapshot("assistant").messageId,
    "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:0",
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

test("user gets one exact Grok settled result | Given one accepted user turn | When the matching assistant result finishes | Then settlement links the exact user and assistant refs", () => {
  const h = harness();
  const userContainer = element({ attrs: { id: "response-11111111-2222-4333-8444-555555555555" } });
  const assistantContainer = element({ attrs: { id: "response-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" } });
  const user = element({
    text: "prompt",
    attrs: { "data-testid": "user-message" },
    parentElement: userContainer,
  });
  const assistant = element({
    text: "answer",
    attrs: { "data-testid": "assistant-message" },
    parentElement: assistantContainer,
  });
  h.set('[data-testid="user-message"]', user);
  h.set('[data-testid="assistant-message"]', assistant);
  h.set('[data-testid="user-message"], [data-testid="assistant-message"]', [user, assistant]);

  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getResultSettlementSnapshot(
      "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
    ))),
    {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:0",
      userMessageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
      text: "answer",
    },
  );
  assert.equal(h.adapter.getResultSettlementSnapshot("different-user"), null);
});

test("user recovers an older Grok settled result | Given a newer turn is already rendered | When settlement checks the accepted historical turn | Then it returns the matching historical assistant", () => {
  const h = harness();
  const acceptedUser = element({
    text: "accepted prompt",
    attrs: { "data-testid": "user-message" },
  });
  const acceptedAssistant = element({
    text: "accepted answer",
    attrs: { "data-testid": "assistant-message" },
  });
  const newerUser = element({
    text: "newer prompt",
    attrs: { "data-testid": "user-message" },
  });
  const newerAssistant = element({
    text: "newer answer",
    attrs: { "data-testid": "assistant-message" },
  });
  h.set('[data-testid="user-message"]', [acceptedUser, newerUser]);
  h.set('[data-testid="assistant-message"]', [acceptedAssistant, newerAssistant]);
  h.set(
    '[data-testid="user-message"], [data-testid="assistant-message"]',
    [acceptedUser, acceptedAssistant, newerUser, newerAssistant],
  );

  assert.deepEqual(
    JSON.parse(JSON.stringify(h.adapter.getResultSettlementSnapshot(
      "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
    ))),
    {
      ok: true,
      currentNodeRole: "assistant",
      finished: true,
      messageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:assistant:0",
      userMessageId: "grok-dom-v1:123e4567-e89b-12d3-a456-426614174000:user:0",
      text: "accepted answer",
    },
  );
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
