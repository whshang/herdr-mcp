// Single-shot ChatGPT submission: no wake path may dispatch a second provider
// mutation after an unconfirmed acknowledgement.
//
// The harness slices the real submit + performWake implementation out of
// extension/content/wake.js, so these tests exercise the shipped routing rather
// than a re-implementation. `user * | Given * | When * | Then *` titles follow
// the repository test-policy contract.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

function wakeHarness(ctx = {}) {
  const start = wakeSource.indexOf("  function captureSubmitAckBaseline(");
  const end = wakeSource.indexOf("  // ---- Delivery confirmation on SpeaksJSON sites ----", start);
  assert.ok(start >= 0 && end > start, "ChatGPT submit + performWake helpers must remain extractable");
  const segment = wakeSource.slice(start, end);
  const acceptanceStart = wakeSource.indexOf("  // The exact Unicode White_Space set Rust");
  const acceptanceEnd = wakeSource.indexOf(
    "  // sha256 digests of the exact accepted provider user text",
    acceptanceStart,
  );
  assert.ok(
    acceptanceStart >= 0 && acceptanceEnd > acceptanceStart,
    "acceptance canonicalization + exact-text matcher must remain extractable",
  );
  const acceptanceSource = wakeSource.slice(acceptanceStart, acceptanceEnd);
  const state = {
    now: 1_000_000,
    enterSubmits: 0,
    clicks: 0,
    mainWorldSubmits: 0,
    turnCalls: 0,
    ...ctx,
  };
  state.input = state.input || { innerText: state.composerText || "" };
  state.sendButton = state.sendButton || {
    disabled: false,
    click() { state.clicks += 1; },
  };
  const api = new Function("ctx", `
    const ADAPTER = {
      name: "chatgpt",
      policy: { submitAckTimeoutMs: 4000 },
      needsMainWorldInsert: true,
      inputHasContent: () => ctx.inputHasContent === true,
      getInputEl: () => ctx.input,
      getComposerTextWithoutAppPills: () => ctx.composerText || "",
      getSelectedComposerApps: () => ctx.selectedApps || [],
      getConversationKey: () => ctx.convKey || "https://chatgpt.com/c/current",
      getWatchMainWorldSelector: () => null,
      composerHasOnlyAppPills: () => false,
    };
    const normText = (value) => String(value || "").replace(/\\s+/g, " ").trim();
    const composerNorm = () => normText(ctx.composerText || "");
    const composerHasSameWake = () => ctx.composerHasSameWake === true;
    const isExtensionStaleComposer = () => false;
    const normalizeHerdrMentionAlias = (value) => String(value || "").toLowerCase();
    const currentHerdrRequiredApps = () => ctx.requiredApps || [];
    const ensureRequiredComposerApps = async () => ({ ok: true, apps: ctx.selectedApps || [] });
    const ensureTextAfterComposerAppSelection = async () => true;
    const ensureCommitted = async () => true;
    const clearComposer = async () => {};
    const waitForComposerIdle = async () => true;
    const composerBusyReason = () => null;
    const runtimeAlive = () => true;
    const wait = async (ms = 0) => { ctx.now += Math.max(1, ms || 1); };
    const isComposerGenerating = () => false;
    const findSendButton = () => ctx.sendButton || ctx.input;
    const isSendButton = (button) => Boolean(button) && button.disabled !== true;
    const dispatchEnterSubmit = () => { ctx.enterSubmits += 1; };
    const submitMainWorld = async () => { ctx.mainWorldSubmits += 1; return { ok: true }; };
    const latestTurnForRole = (role) => {
      if (role !== "user") return null;
      ctx.turnCalls += 1;
      return typeof ctx.userTurn === "function" ? ctx.userTurn(ctx.turnCalls) : null;
    };
    const document = { hidden: false };
    const MutationObserver = class { observe() {} disconnect() {} };
    const BROWSER_PERFORMANCE = null;
    const window = {
      __H2W_PERMISSION__: {
        createPermissionClicker: () => ({
          tryClick: () => (ctx.permissionHandled === true
            ? { handled: true, button: ctx.sendButton }
            : { handled: false }),
        }),
      },
    };
    const Date = { now: () => ctx.now };
    // The permission watcher schedules a bounded stop timer; keep the test
    // process from waiting on it (the submit rule under test does not need it).
    const setTimeout = () => 0;
    ${acceptanceSource}
    ${segment}
    return { performWake, submit, submitTextarea, submitAfterPermissionClick, startPermissionWatch };
  `)(state);
  return { api, ctx: state };
}

test("user avoids duplicate handoff | Given a slow Send acknowledgement | When handoff submits | Then only one provider submission is attempted", async () => {
  const { api, ctx } = wakeHarness({ composerText: "handoff prompt", userTurn: () => null });
  const result = await api.performWake({ template: "handoff prompt", handoff: true, autoAllow: false });
  assert.equal(ctx.enterSubmits, 1, "a handoff must dispatch exactly one submit primitive");
  assert.equal(ctx.clicks, 0, "the retrying click path must not be reachable");
  assert.equal(ctx.mainWorldSubmits, 0);
  assert.equal(result.ok, false);
  assert.equal(result.attempted, true);
  assert.equal(result.uncertain, true, "an unconfirmed handoff submit stays uncertain");
  assert.equal(result.committed, true);
});

test("user avoids duplicate queue delivery | Given the first submission is uncertain | When queued delivery finishes | Then no Enter or second click is dispatched", async () => {
  const { api, ctx } = wakeHarness({ composerText: "queued text", userTurn: () => null });
  const first = await api.performWake({ template: "queued text", queueInsert: true, autoAllow: false });
  assert.equal(first.uncertain, true);
  assert.equal(ctx.enterSubmits, 1);
  // A batch redelivery of the same text must not send a second provider turn.
  const second = await api.performWake({ template: "queued text", queueInsert: true, autoAllow: false });
  assert.equal(second.blocked, "submit-uncertain");
  assert.equal(second.uncertain, true);
  assert.equal(ctx.enterSubmits, 1, "queued redelivery must not dispatch another submit");
  assert.equal(ctx.clicks, 0);
});

test("user avoids duplicate recovery | Given a delayed acknowledgement | When recovery activation runs | Then only one submission is attempted", async () => {
  const { api, ctx } = wakeHarness({ composerText: "recovery probe", userTurn: () => null });
  const result = await api.performWake({ template: "recovery probe", recovery: true, autoAllow: false });
  assert.equal(ctx.enterSubmits, 1);
  assert.equal(ctx.clicks, 0);
  assert.equal(result.uncertain, true);
  const replay = await api.performWake({ template: "recovery probe", recovery: true, autoAllow: false });
  assert.equal(replay.blocked, "submit-uncertain");
  assert.equal(ctx.enterSubmits, 1, "recovery must never resend an unconfirmed probe");
});

test("user avoids duplicate generic wake | Given Send was attempted without acknowledgement | When generic wake completes | Then the result stays uncertain without a second submit", async () => {
  const { api, ctx } = wakeHarness({ composerText: "next turn", userTurn: () => null });
  const result = await api.performWake({ template: "next turn" });
  assert.equal(result.ok, false);
  assert.equal(result.uncertain, true);
  assert.equal(result.error, "submit-unconfirmed");
  assert.equal(ctx.enterSubmits, 1);
  assert.equal(ctx.mainWorldSubmits, 0);
  assert.equal(ctx.clicks, 0);
});

test("user gets a confirmed wake only from the exact accepted turn | Given the acknowledged turn matches the submitted text | When the wake completes | Then it reports success after a single submit", async () => {
  const { api, ctx } = wakeHarness({
    composerText: "next turn",
    userTurn: (call) => (call === 1 ? null : { innerText: "next turn" }),
  });
  const result = await api.performWake({ template: "next turn" });
  assert.equal(ctx.enterSubmits, 1);
  assert.equal(result.ok, true);
  assert.equal(result.uncertain, undefined);
});

test("user keeps an uncertain ChatGPT submit terminal across paths | Given one unconfirmed submit of a template | When another ChatGPT wake path wants the same text | Then the second attempt is refused", async () => {
  const { api, ctx } = wakeHarness({ composerText: "shared text", userTurn: () => null });
  await api.performWake({ template: "shared text", handoff: true, autoAllow: false });
  assert.equal(ctx.enterSubmits, 1);
  for (const data of [
    { template: "shared text", queueInsert: true, autoAllow: false },
    { template: "shared text", recovery: true, autoAllow: false },
    { template: "shared text" },
  ]) {
    const result = await api.performWake(data);
    assert.equal(result.blocked, "submit-uncertain");
    assert.equal(result.uncertain, true);
  }
  assert.equal(ctx.enterSubmits, 1, "no ChatGPT path may resend an uncertain submit");
});

test("user never gets a duplicate ChatGPT submit from the legacy entrypoint | Given ChatGPT with delayed acknowledgement | When the legacy submit entrypoint runs | Then it dispatches once and reports failure", async () => {
  const { api, ctx } = wakeHarness({ composerText: "next turn", userTurn: () => null });
  const sent = await api.submit();
  assert.equal(sent, false);
  assert.equal(ctx.enterSubmits, 1);
  assert.equal(ctx.clicks, 0);
  assert.equal(ctx.mainWorldSubmits, 0);
  // submitTextarea is only reachable for non-ChatGPT adapters, but even if a
  // caller reaches it the ChatGPT guard keeps it single-shot.
  const textareaSent = await api.submitTextarea();
  assert.equal(textareaSent, false);
  assert.equal(ctx.enterSubmits, 2);
  assert.equal(ctx.clicks, 0);
});

test("user never gets a second submit primitive after a permission click | Given the Send click after a permission click is not acknowledged | When the follow-up submit runs | Then only the click is dispatched and it stays unconfirmed", async () => {
  const start = wakeSource.indexOf("  async function submitAfterPermissionClick()");
  const end = wakeSource.indexOf("  async function submitTextarea()", start);
  assert.ok(start >= 0 && end > start, "submitAfterPermissionClick must remain extractable");
  const helper = wakeSource.slice(start, end);
  assert.doesNotMatch(helper, /dispatchEnterSubmit/, "the Enter fallback must be removed");
  assert.match(helper, /btn\.click\(\)/);

  const { api, ctx } = wakeHarness({
    composerText: "next turn",
    userTurn: () => null,
    permissionHandled: true,
  });
  await api.startPermissionWatch(1000);
  const accepted = await api.submitAfterPermissionClick();
  assert.equal(accepted, false, "an unconfirmed permission-click submit stays unconfirmed");
  assert.equal(ctx.clicks, 1, "the permission follow-up dispatches exactly one Send click");
  assert.equal(ctx.enterSubmits, 0, "an unconfirmed permission-click submit must never send Enter");
  assert.equal(ctx.mainWorldSubmits, 0);
});
