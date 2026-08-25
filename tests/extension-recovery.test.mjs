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
  for (const file of ["extension/context-pressure.js", "extension/conversation-health.js", "extension/recovery-controller.js"]) {
    const source = fs.readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
    assert.doesNotMatch(source, /^\s*(?:import|export)\s/m, `${file} must remain a classic content script`);
    new vm.Script(source, { filename: file }).runInContext(context);
  }
  return context;
}

test("conversation recovery scripts load as classic MV3 content scripts", () => {
  const context = loadClassicExtensionScripts();
  assert.ok(context.H2W_CONTEXT_PRESSURE);
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
    "freshness_state",
    "freshness_checked_at",
    "server_latest_at",
    "page_latest_at",
    "freshness_delta_ms",
    "server_message_id",
    "page_message_id",
    "stale_refresh_attempt",
    "stale_activation_attempt",
    "thread_error_retry_attempt",
    "thread_error_reload_attempt",
    "thread_error_last_seen_at",
    "reload_reason",
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
  assert.equal(recovery.canRolloverSafely(record, {}, policy), true);
  assert.equal(recovery.canRolloverSafely(record, { streaming: true }, policy), false);
  assert.equal(recovery.canRolloverSafely(record, { toolRunning: true }, policy), false);
  assert.equal(recovery.canRolloverSafely(record, { permissionCardActive: true }, policy), false);
  assert.equal(recovery.canRolloverSafely(record, { tabReachable: false }, policy), false);
  assert.equal(recovery.canRolloverSafely({ ...record, last_assistant_progress_at: 3002 }, {}, policy), false);
});

test("observed assistant progress cancels a stale ChatGPT reply timeout", () => {
  const context = loadClassicExtensionScripts();
  const health = context.H2W_CONVERSATION_HEALTH;
  const recovery = context.H2W_RECOVERY_CONTROLLER;
  let record = health.markReplyWaiting(health.createConversationHealth("chatgpt-project-conversation"), 1000);
  record = health.markAssistantProgress(record, 1500);
  assert.equal(record.state, "healthy");
  assert.equal(record.last_assistant_progress_at, 1500);
  assert.equal(recovery.classifyReplyTimeout(record, 120000, 1000).state, "healthy");
});

test("freshness classification distinguishes synced, server-ahead, and server-stalled views", () => {
  const context = loadClassicExtensionScripts();
  const recovery = context.H2W_RECOVERY_CONTROLLER;
  const now = 100000;
  const baseDom = { messageId: "a1", text: "hello world", changedAt: 90000 };
  const synced = recovery.classifyViewFreshness({
    dom: baseDom,
    server: { ok: true, messageId: "a1", text: "hello world", finished: true, updatedAt: 90000 },
    now,
  });
  assert.equal(synced.state, "synced");
  const ahead = recovery.classifyViewFreshness({
    dom: baseDom,
    server: { ok: true, messageId: "a2", text: "newer answer", finished: true, updatedAt: 95000 },
    now,
  });
  assert.equal(ahead.state, "server_ahead");
  const stalled = recovery.classifyViewFreshness({
    dom: { ...baseDom, changedAt: 60000 },
    server: { ok: true, messageId: "a1", text: "hello world", finished: false, updatedAt: 60000 },
    now,
  }, { ...recovery.DEFAULT_RECOVERY_POLICY, assistantStallMs: 30000, serverStallMs: 30000 });
  assert.equal(stalled.state, "server_stalled");
});

test("ChatGPT turn watcher wires assistant progress, settled turns, and explicit rollover reasons", () => {
  const wake = fs.readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
  assert.match(wake, /markAssistantProgressIfActive\(\)/);
  assert.match(wake, /markObservedTurnEnded\(endedAt\)/);
  assert.match(wake, /trigger:\s*"context_pressure"/);
  assert.match(wake, /trigger:\s*"recovery_exhausted"/);
  assert.match(wake, /backend-api\/conversation/);
  assert.match(wake, /maybeRefreshStaleView\(\)/);
  assert.match(wake, /regenerate-thread-error-button/);
  assert.match(wake, /maybeRecoverExplicitThreadError\(\)/);
  assert.match(wake, /thread_error_server_ahead/);
  assert.match(wake, /thread_error_delivery_unknown/);
  assert.match(wake, /startsWith\("thread_error_"\)/);
  assert.match(wake, /markRolloverRecommended/);
  assert.match(wake, /conversation-turn-/);
  assert.match(wake, /mergeMessageCountFloor/);
  assert.match(wake, /stale_view_activation_template/);
});

test("context pressure uses conservative text and virtual-turn thresholds", () => {
  const context = loadClassicExtensionScripts();
  const pressure = context.H2W_CONTEXT_PRESSURE;
  assert.equal(pressure.EFFECTIVE_CONTEXT_TOKENS, 128000);
  assert.equal(pressure.USABLE_TEXT_BUDGET_TOKENS, 96000);
  assert.equal(pressure.evaluateContextPressure({ estimatedTextTokens: 56000 }).state, "context_warning");
  assert.equal(pressure.evaluateContextPressure({ estimatedTextTokens: 64000 }).state, "handoff_prepare");
  assert.equal(pressure.evaluateContextPressure({ estimatedTextTokens: 72000 }).state, "rollover_recommended");
  assert.equal(pressure.evaluateContextPressure({ estimatedTextTokens: 80000 }).state, "rollover_required");
  assert.equal(pressure.evaluateContextPressure({ messageCount: 32 }).state, "context_warning");
  assert.equal(pressure.evaluateContextPressure({ messageCount: 40 }).state, "handoff_prepare");
  assert.equal(pressure.evaluateContextPressure({ messageCount: 46 }).state, "rollover_recommended");
  assert.equal(pressure.evaluateContextPressure({ messageCount: 50 }).state, "rollover_required");
  assert.equal(pressure.evaluateContextPressure({ messageCount: 60 }).state, "high_risk");
});

test("context pressure persists metadata only and proactive rollover is fail-closed", () => {
  const context = loadClassicExtensionScripts();
  const pressure = context.H2W_CONTEXT_PRESSURE;
  let record = pressure.emptyContextRecord("c", 1000);
  record = pressure.mergeObservedTurns(record, [
    { id: "u1", role: "user", text: "hello world" },
    { id: "a1", role: "assistant", text: "answer" },
  ]);
  assert.equal(JSON.stringify(record).includes("hello world"), false);
  assert.equal(pressure.summarizeContextRecord(record).message_count, 2);

  record = pressure.mergeMessageCountFloor(record, 51, "chatgpt_virtual_turn_index", 2000);
  record = pressure.mergeMessageCountFloor(record, 12, "chatgpt_virtual_turn_index", 3000);
  const summarized = pressure.summarizeContextRecord(record);
  assert.equal(summarized.observed_message_count, 2);
  assert.equal(summarized.message_count_floor, 51);
  assert.equal(summarized.message_count, 51);
  assert.equal(summarized.message_floor_source, "chatgpt_virtual_turn_index");
  assert.equal(summarized.state, "rollover_required");
  assert.equal(summarized.recommendation, "auto_rollover");

  const base = {
    pressure: pressure.evaluateContextPressure({ messageCount: 50 }),
    runtimeHealth: "healthy",
    bound: true,
    canHandoff: true,
    projectConversation: true,
    quiescent: true,
    deliveryUncertain: false,
    mutationPending: false,
    handoffStatus: null,
  };
  assert.equal(pressure.shouldAutoRollover(base), true);
  assert.equal(pressure.shouldAutoRollover({ ...base, mutationPending: true }), false);
  assert.equal(pressure.shouldAutoRollover({ ...base, handoffStatus: "summary_requested" }), false);
});
