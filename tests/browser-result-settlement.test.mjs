import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const backgroundSource = readFileSync(path.join(__dirname, "..", "extension", "background.js"), "utf8");
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");

const settlementStart = wakeSource.indexOf("  async function reportBrowserResultSettlement(serverSnapshot) {");
const settlementEnd = wakeSource.indexOf("  // Browser Registry identity cached by the page script", settlementStart);
assert.ok(settlementStart >= 0 && settlementEnd > settlementStart, "settlement helper must remain extractable");
const settlementSource = wakeSource.slice(settlementStart, settlementEnd);

const postStart = backgroundSource.indexOf("async function postBrowserDispatchResult({");
const postEnd = backgroundSource.indexOf("\nasync function observeBrowserConversation({", postStart);
assert.ok(postStart >= 0 && postEnd > postStart, "background settlement post must remain extractable");
const postSource = backgroundSource.slice(postStart, postEnd);

function settlementHarness({ sessionRef = "br_session_1", generation = 7, accepted = null, sends = [] } = {}) {
  const acceptedDispatchAssignments = new Map();
  if (accepted) {
    acceptedDispatchAssignments.set(sessionRef, {
      generation,
      acceptedUserMessageRef: accepted,
      reportedAssistantRef: null,
    });
  }
  const sent = [];
  const sendBg = async (payload) => {
    sent.push(payload);
    const next = sends.shift();
    if (typeof next === "function") return next(payload);
    return next || { ok: true };
  };
  const report = new Function(
    "registeredBrowserSessionRef",
    "registeredBrowserGeneration",
    "acceptedDispatchAssignments",
    "sendBg",
    "ADAPTER",
    `${settlementSource}; return reportBrowserResultSettlement;`,
  )(sessionRef, generation, acceptedDispatchAssignments, sendBg, { name: "chatgpt" });
  return { report, sent, acceptedDispatchAssignments };
}

test("finalized turn reports exact provider/session/generation/user/assistant identity and text", async () => {
  const { report, sent } = settlementHarness({ sessionRef: "br_abc", generation: 7 });
  const ok = await report({
    ok: true,
    finished: true,
    messageId: "assistant-msg-1",
    userMessageId: "user-msg-1",
    text: "  final worker answer  ",
  });
  assert.equal(ok, true);
  assert.deepEqual(sent, [{
    type: "h2w_browser_result",
    provider: "chatgpt",
    session_ref: "br_abc",
    generation: 7,
    accepted_user_message_ref: "user-msg-1",
    assistant_message_ref: "assistant-msg-1",
    assistant_text: "final worker answer",
  }]);
});

test("settlement skips instead of guessing when exact identity is missing", async () => {
  const noUser = settlementHarness({ sessionRef: "br_abc", generation: 7 });
  assert.equal(await noUser.report({ ok: true, finished: true, messageId: "assistant-1" }), false);
  assert.equal(noUser.sent.length, 0);

  const noAssistant = settlementHarness({ sessionRef: "br_abc", generation: 7 });
  assert.equal(await noAssistant.report({ ok: true, finished: true, userMessageId: "user-1" }), false);
  assert.equal(noAssistant.sent.length, 0);

  const unsettled = settlementHarness({ sessionRef: "br_abc", generation: 7 });
  assert.equal(await unsettled.report({
    ok: true,
    finished: false,
    messageId: "assistant-1",
    userMessageId: "user-1",
    text: "still generating",
  }), false);
  assert.equal(unsettled.sent.length, 0);

  const unbound = settlementHarness({ sessionRef: null, generation: 7 });
  assert.equal(await unbound.report({
    ok: true,
    finished: true,
    messageId: "assistant-1",
    userMessageId: "user-1",
    text: "no registered session",
  }), false);
  assert.equal(unbound.sent.length, 0);
});

test("accepted dispatch identity is the fallback when a snapshot omits the user message id", async () => {
  const { report, sent } = settlementHarness({
    sessionRef: "br_abc",
    generation: 7,
    accepted: "accepted-user-1",
  });
  const ok = await report({
    ok: true,
    finished: true,
    messageId: "assistant-1",
    text: "worker answer",
  });
  assert.equal(ok, true);
  assert.equal(sent[0].accepted_user_message_ref, "accepted-user-1");
});

test("repeated settlement for one assistant message is locally idempotent", async () => {
  const { report, sent } = settlementHarness({ sessionRef: "br_abc", generation: 7 });
  const snapshot = {
    ok: true,
    finished: true,
    messageId: "assistant-1",
    userMessageId: "user-1",
    text: "worker answer",
  };
  assert.equal(await report(snapshot), true);
  assert.equal(await report(snapshot), true);
  assert.equal(sent.length, 1);
});

test("settlement has no physical tab focus dependency", () => {
  assert.doesNotMatch(settlementSource, /document\.hidden|tabs\.update|tabs\.get|active:\s*true|chrome\.tabs/);
  assert.match(settlementSource, /sendBg\(\{/);
});

test("background posts trusted result settlement through the existing registry IPC", async () => {
  const calls = [];
  const postBrowserRegistry = async (payload) => {
    calls.push(payload);
    return { ok: true, result_settled: true, replayed: false };
  };
  const post = new Function(
    "postBrowserRegistry",
    `${postSource}; return postBrowserDispatchResult;`,
  )(postBrowserRegistry);
  const result = await post({
    provider: "chatgpt",
    session_ref: "br_abc",
    expected_generation: 7,
    accepted_user_message_ref: "user-1",
    assistant_message_ref: "assistant-1",
    assistant_text: "worker answer",
  });
  assert.deepEqual(calls, [{
    operation: "dispatch.result",
    provider: "chatgpt",
    session_ref: "br_abc",
    expected_generation: 7,
    accepted_user_message_ref: "user-1",
    assistant_message_ref: "assistant-1",
    assistant_text: "worker answer",
  }]);
  assert.equal(result.ok, true);
  assert.equal(result.result_settled, true);
});

test("background handles h2w_browser_result and never blocks on tab focus", () => {
  assert.match(backgroundSource, /msg\?\.type === "h2w_browser_result"/);
  const handlerStart = backgroundSource.indexOf('msg?.type === "h2w_browser_result"');
  const handlerEnd = backgroundSource.indexOf('if (msg?.type === "h2w_turn_ended")', handlerStart);
  assert.ok(handlerStart >= 0 && handlerEnd > handlerStart, "result handler must remain extractable");
  const handler = backgroundSource.slice(handlerStart, handlerEnd);
  assert.match(handler, /postBrowserDispatchResult\(/);
  assert.match(handler, /browser_result_fields_incomplete/);
  assert.doesNotMatch(handler, /document\.hidden|chrome\.tabs/);
});
