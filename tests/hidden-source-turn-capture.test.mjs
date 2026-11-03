// Hidden-tab source-turn capture (#846): a background/owned ChatGPT tab is how
// herdr opens and drives its own conversations, so the turn the runtime submits
// for it must still be reported as `h2w_turn_started` (-> `source_turn.observe`)
// even though every visible-only sampler bails on `document.hidden`.
//
// The harness slices the real capture helpers out of extension/content/wake.js,
// so these tests exercise the shipped acceptance rules rather than a
// re-implementation. `user * | Given * | When * | Then *` titles follow the
// repository test-policy contract.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

function makeHarness(ctx = {}) {
  const state = {
    now: 1_000_000,
    submitAt: 999_500,
    expectedUser: "herdr UAT：只回复 OK",
    bubbleText: "herdr UAT：只回复 OK",
    bubbleCount: 1,
    conversation: "https://chatgpt.com/c/hidden-846",
    sent: [],
    samples: 0,
    dirtyMarks: 0,
    ...ctx,
  };
  state.send = (payload) => state.sent.push(payload);
  state.sample = () => { state.samples += 1; };
  state.markDirty = () => { state.dirtyMarks += 1; };
  const start = wakeSource.indexOf("    const maybeCaptureSourceTurn = () => {");
  const end = wakeSource.indexOf("    const onTick = () => {", start);
  assert.ok(start >= 0 && end > start, "the source-turn capture helpers must remain extractable");
  const segment = wakeSource.slice(start, end);
  assert.ok(
    segment.includes("const maybeCaptureHiddenSourceTurn = () => {"),
    "the hidden capture helper must sit next to the shared capture helper",
  );
  const build = new Function("ctx", `
    let lastStartedJournalKey = "";
    let lastNoBubbleLoggedSubmitAt = 0;
    let lastUserSignature = ctx.lastUserSignature ?? "";
    let lastUserCount = ctx.lastUserCount ?? 0;
    const Date = { now: () => ctx.now };
    const ADAPTER = { getConversationKey: () => ctx.conversation };
    const console = { debug: () => {}, log: () => {} };
    const conversationHealth = { last_user_submit_at: ctx.submitAt };
    const chatGptConversationId = () => "hidden-846";
    const syncPendingSubmitAnchor = () => Number(conversationHealth.last_user_submit_at || 0);
    const pendingUserTextHint = () => ctx.expectedUser;
    const normText = (value) => String(value || "").replace(/\\s+/g, " ").trim();
    const assistantSignature = (value) => "sig:" + String(value || "").length;
    const pendingUserTextMatches = (expected, bubble) =>
      Boolean(expected && bubble)
        && (bubble.includes(expected.slice(0, 160)) || expected.includes(bubble.slice(0, 160)));
    const markLatestTurnsDirty = () => { ctx.markDirty(); };
    const roleSnapshot = (role) => {
      if (role !== "user") return { text: "", totalChars: 0, count: 0, appMentions: [] };
      ctx.sample();
      return {
        text: ctx.bubbleText,
        totalChars: String(ctx.bubbleText || "").length,
        count: ctx.bubbleCount,
        appMentions: [],
        sourceTurnText: "",
      };
    };
    const sendBg = (payload) => { ctx.send(payload); return Promise.resolve({ ok: true }); };
${segment}
    return {
      maybeCaptureSourceTurn,
      maybeCaptureHiddenSourceTurn,
      state: () => ({ lastStartedJournalKey, lastUserSignature, lastUserCount }),
    };
  `);
  return { ctx: state, ...build(state) };
}

test("user keeps a hidden-tab dispatch bindable | Given a hidden ChatGPT tab whose latest user bubble exactly matches a fresh pending-submit anchor | When the hidden interval tick runs twice | Then h2w_turn_started is reported exactly once with that exact text", () => {
  const h = makeHarness();
  const first = h.maybeCaptureHiddenSourceTurn();
  assert.equal(first, true);
  assert.equal(h.ctx.sent.length, 1);
  assert.deepEqual(
    {
      type: h.ctx.sent[0].type,
      convKey: h.ctx.sent[0].convKey,
      startedAt: h.ctx.sent[0].startedAt,
      userText: h.ctx.sent[0].userText,
    },
    {
      type: "h2w_turn_started",
      convKey: "https://chatgpt.com/c/hidden-846",
      startedAt: 999_500,
      userText: "herdr UAT：只回复 OK",
    },
  );
  // The hidden MutationObserver never marks the cache dirty, so the hidden path
  // must re-discover the latest turns before sampling.
  assert.equal(h.ctx.dirtyMarks, 1);
  // A second tick for the same submit anchor must not report it again.
  assert.equal(h.maybeCaptureHiddenSourceTurn(), false);
  assert.equal(h.ctx.sent.length, 1);
  assert.equal(h.state().lastStartedJournalKey, "999500");
});

test("user never gets a mismatched turn bound | Given a hidden ChatGPT tab whose latest user bubble differs from the pending-submit anchor text | When the hidden interval tick runs | Then nothing is reported", () => {
  const h = makeHarness({ bubbleText: "some other message entirely" });
  assert.equal(h.maybeCaptureHiddenSourceTurn(), false);
  assert.equal(h.ctx.sent.length, 0);
});

test("user never gets a stale turn bound | Given a hidden ChatGPT tab holding a pending-submit anchor older than 30s | When the hidden interval tick runs | Then nothing is reported and no DOM work happens", () => {
  const h = makeHarness({ now: 1_000_000, submitAt: 1_000_000 - 30_001 });
  assert.equal(h.maybeCaptureHiddenSourceTurn(), false);
  assert.equal(h.ctx.sent.length, 0);
  assert.equal(h.ctx.samples, 0, "a stale anchor must not trigger any page sampling");
  assert.equal(h.ctx.dirtyMarks, 0);
});

test("user pays nothing for an idle hidden tab | Given a hidden ChatGPT tab with no pending submit at all | When the hidden interval tick runs | Then nothing is reported and no DOM work happens", () => {
  const h = makeHarness({ submitAt: 0 });
  assert.equal(h.maybeCaptureHiddenSourceTurn(), false);
  assert.equal(h.ctx.sent.length, 0);
  assert.equal(h.ctx.samples, 0);
  assert.equal(h.ctx.dirtyMarks, 0);
});

test("user keeps the visible-tab capture unchanged | Given the same fresh anchor and matching bubble in a visible tab | When the shared capture helper runs directly | Then it still reports the exact turn once", () => {
  const h = makeHarness();
  assert.equal(h.maybeCaptureSourceTurn(), true);
  assert.equal(h.ctx.sent.length, 1);
  assert.equal(h.ctx.sent[0].type, "h2w_turn_started");
  assert.equal(h.maybeCaptureSourceTurn(), false);
  assert.equal(h.ctx.sent.length, 1);
});
