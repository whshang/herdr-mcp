import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

function loadClassicExtensionScripts() {
  const context = vm.createContext({
    console,
    Date,
    Math,
    crypto: { randomUUID: () => "continuity-test" },
  });
  for (const file of ["extension/conversation-health.js", "extension/recovery-controller.js"]) {
    const source = fs.readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
    assert.doesNotMatch(source, /^\s*(?:import|export)\s/m, `${file} must remain a classic content script`);
    new vm.Script(source, { filename: file }).runInContext(context);
  }
  return context;
}

test("conversation recovery scripts load as classic MV3 content scripts", () => {
  const context = loadClassicExtensionScripts();
  assert.ok(context.H2W_CONVERSATION_HEALTH);
  assert.ok(context.H2W_RECOVERY_CONTROLLER);
});

test("conversation health records the persisted recovery lifecycle fields", () => {
  const context = loadClassicExtensionScripts();
  const health = context.H2W_CONVERSATION_HEALTH;
  let record = health.createConversationHealth("chatgpt:project:conversation", "continuity-1");
  assert.equal(record.continuity_id, "continuity-1");
  for (const field of [
    "convKey",
    "last_user_submit_at",
    "reply_started_at",
    "last_assistant_progress_at",
    "last_turn_end_at",
    "recovery_attempt",
    "reload_attempt",
    "last_reload_at",
    "rollover_hint_at",
    "continuity_id",
  ]) assert.ok(Object.hasOwn(record, field), `missing ${field}`);

  record = health.markReplyWaiting(record, 1000);
  assert.equal(record.state, "reply_waiting");
  assert.equal(record.last_user_submit_at, 1000);
  record = health.markReplyStarted(record, 1100);
  assert.equal(record.state, "healthy");
  assert.equal(record.reply_started_at, 1100);
  record = health.markAssistantProgress(record, 1200);
  assert.equal(record.last_assistant_progress_at, 1200);
  record = health.markTurnEnded(record, 1300);
  assert.equal(record.last_turn_end_at, 1300);
});

test("reply timeout, recovery attempt, reload gate, and rollover remain fail-closed", () => {
  const context = loadClassicExtensionScripts();
  const health = context.H2W_CONVERSATION_HEALTH;
  const recovery = context.H2W_RECOVERY_CONTROLLER;
  const policy = { ...recovery.DEFAULT_RECOVERY_POLICY, replyTimeoutMs: 1000, reloadCooldownMs: 5000 };

  let record = health.markReplyWaiting(health.createConversationHealth("c"), 1000);
  assert.equal(recovery.classifyReplyTimeout(record, 1999, 1000).state, "reply_waiting");
  record = recovery.classifyReplyTimeout(record, 2000, 1000);
  assert.equal(record.state, "reply_suspect");
  assert.equal(recovery.shouldSendRecovery(record, policy, 2000), true);
  record = recovery.markRecoverySent(record, 2001);
  assert.equal(record.recovery_attempt, 1);
  assert.equal(recovery.shouldSendRecovery(record, policy, 3000), false);

  const safe = {
    composerBusy: false,
    composerHasHumanText: false,
    streaming: false,
    toolRunning: false,
    permissionCardActive: false,
    deliveryUnknown: false,
    mutationDeliveryUncertain: false,
    reloadAttempts: 0,
    lastReloadAt: null,
    now: 10000,
  };
  assert.equal(recovery.canReloadSafely(safe, policy), true);
  for (const key of [
    "composerBusy",
    "composerHasHumanText",
    "streaming",
    "toolRunning",
    "permissionCardActive",
    "deliveryUnknown",
    "mutationDeliveryUncertain",
  ]) assert.equal(recovery.canReloadSafely({ ...safe, [key]: true }, policy), false, key);
  assert.equal(recovery.canReloadSafely({ ...safe, reloadAttempts: 1 }, policy), false);
  assert.equal(recovery.canReloadSafely({ ...safe, lastReloadAt: 7000 }, policy), false);

  record = recovery.markReloaded(record, 3001);
  assert.equal(record.reload_attempt, 1);
  assert.equal(recovery.recommendRollover(record, policy), true);
});
