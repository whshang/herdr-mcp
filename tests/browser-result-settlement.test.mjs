import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const backgroundSource = readFileSync(path.join(__dirname, "..", "extension", "background.js"), "utf8");
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");

const settlementStart = wakeSource.indexOf("  async function reportBrowserResultSettlement(serverSnapshot,");
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

function observationHarness({ accepted = true, recovered = null, snapshots = [], sends = [] } = {}) {
  let now = 10000;
  let route = "chatgpt:/c/worker";
  let probes = 0;
  let callback;
  const sent = [];
  const assignments = new Map();
  if (accepted) assignments.set("br_worker", { generation: 7, acceptedUserMessageRef: "user-1", reportedAssistantRef: null });
  const routeStart = wakeSource.indexOf("  function startConversationRouteWatch() {");
  const routeEnd = wakeSource.indexOf("  function assistantSignature", routeStart);
  const observe = new Function(
    "registeredBrowserSessionRef", "registeredBrowserGeneration", "registeredConvKey",
    "acceptedDispatchAssignments", "sendBg", "ADAPTER", "fetchChatGptConversationSnapshot",
    "Date", "setInterval", "document", "window", "recovered",
    `${settlementSource}\n${wakeSource.slice(routeStart, routeEnd)}\nrestoreBrowserResultAssignment(recovered); startConversationRouteWatch(); return observeBrowserResultSettlement;`,
  )("br_worker", 7, route, assignments, async (payload) => {
    sent.push(payload);
    return sends.shift() || { ok: true };
  }, { name: "chatgpt", getConversationKey: () => route }, async () => {
    probes += 1;
    const snapshot = snapshots.shift();
    return typeof snapshot === "function" ? snapshot() : snapshot;
  }, { now: () => now }, (fn) => { callback = fn; }, { hidden: true, addEventListener() {} }, { addEventListener() {} }, recovered);
  return { observe, sent, assignments, tick: () => callback(), advance: (ms = 5000) => { now += ms; }, navigate: () => { route = "chatgpt:/c/other"; }, probes: () => probes };
}

const completedWorkerSnapshot = {
  ok: true, currentNodeRole: "assistant", finished: true,
  messageId: "assistant-1", userMessageId: "user-1", text: "worker answer",
};

test("route watch settles a hidden browser worker without native binding or automation", async () => {
  const h = observationHarness({ snapshots: [{ ...completedWorkerSnapshot, finished: false }, completedWorkerSnapshot] });
  h.tick();
  await new Promise(setImmediate);
  assert.equal(h.sent.length, 0);
  h.advance();
  h.tick();
  await new Promise(setImmediate);
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].accepted_user_message_ref, "user-1");
  h.advance();
  h.tick();
  await new Promise(setImmediate);
  assert.equal(h.sent.length, 1);
  assert.equal(h.probes(), 2);
});

test("accepted worker result retries a failed acknowledgement and throttles probes", async () => {
  const h = observationHarness({ snapshots: [completedWorkerSnapshot, completedWorkerSnapshot], sends: [{ ok: false }, { ok: true }] });
  assert.equal(await h.observe(), false);
  assert.equal(await h.observe(), false);
  assert.equal(h.probes(), 1);
  h.advance(10000);
  assert.equal(await h.observe(), true);
  assert.equal(h.sent.length, 2);
});

test("reopened worker recovers exact finalized result without an in-memory assignment", async () => {
  const h = observationHarness({ accepted: false, recovered: { generation: 6, accepted_user_message_ref: "user-1" }, snapshots: [completedWorkerSnapshot] });
  assert.equal(await h.observe(), true);
  assert.equal(h.sent[0].session_ref, "br_worker");
  assert.equal(h.sent[0].generation, 6);
});

test("in-flight result observation cannot cross a conversation route change", async () => {
  let resolve;
  const h = observationHarness({ snapshots: [() => new Promise((done) => { resolve = done; })] });
  const first = h.observe();
  h.advance();
  assert.equal(await h.observe(), false);
  assert.equal(h.probes(), 1);
  h.navigate();
  resolve(completedWorkerSnapshot);
  assert.equal(await first, false);
  assert.equal(h.sent.length, 0);
});

test("an older result ACK preserves a newer accepted worker assignment", async () => {
  let resolve;
  const h = settlementHarness({ sessionRef: "br_worker", generation: 7, accepted: "user-1", sends: [() => new Promise((done) => { resolve = done; })] });
  const report = h.report(completedWorkerSnapshot);
  const newer = { generation: 7, acceptedUserMessageRef: "user-2", reportedAssistantRef: null };
  h.acceptedDispatchAssignments.set("br_worker", newer);
  resolve({ ok: true });
  assert.equal(await report, true);
  assert.deepEqual(h.acceptedDispatchAssignments.get("br_worker"), newer);
});

test("ordinary registered pages with no assignment perform no provider probes", async () => {
  const h = observationHarness({ accepted: false });
  for (let i = 0; i < 100; i += 1) { await h.observe(); h.advance(60000); }
  assert.equal(h.probes(), 0);
  assert.equal(h.sent.length, 0);
});

test("recovered assignment survives more than three transient result failures", async () => {
  const h = observationHarness({ accepted: false, recovered: { generation: 6, accepted_user_message_ref: "user-1" }, snapshots: Array(5).fill(completedWorkerSnapshot), sends: [...Array(4).fill({ ok: false, error: "browser-registry-http-503" }), { ok: true }] });
  for (let i = 0; i < 4; i += 1) { assert.equal(await h.observe(), false); h.advance(60000); }
  assert.equal(await h.observe(), true);
  assert.equal(h.sent.length, 5);
  assert.ok(h.sent.every((message) => message.generation === 6));
});

test("persistent identity rejection stops retries but a new assignment can proceed", async () => {
  const h = observationHarness({ snapshots: [...Array(3).fill(completedWorkerSnapshot), { ...completedWorkerSnapshot, userMessageId: "user-2", messageId: "assistant-2" }], sends: [...Array(3).fill({ ok: false, error: "browser_dispatch_result_unmatched" }), { ok: true }] });
  for (let i = 0; i < 10; i += 1) { await h.observe(); h.advance(60000); }
  assert.equal(h.probes(), 3);
  assert.equal(h.sent.length, 3);
  h.assignments.set("br_worker", { generation: 7, acceptedUserMessageRef: "user-2", reportedAssistantRef: null });
  assert.equal(await h.observe(), true);
  assert.equal(h.sent.length, 4);
});
