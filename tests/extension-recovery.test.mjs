import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { shouldDiscardRetiredSourceTab } from "../extension/continuity-core.js";

function loadClassicExtensionScripts() {
  const context = vm.createContext({
    console,
    Date,
    Math,
    crypto: { randomUUID: () => "continuity-test" },
  });
  for (const file of ["extension/context-pressure.js", "extension/conversation-health.js", "extension/recovery-controller.js", "extension/performance-core.js"]) {
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
  assert.ok(context.H2W_BROWSER_PERFORMANCE);
});

test("durable recovery barrier waits for persistence and fails closed", async () => {
  const recovery = loadClassicExtensionScripts().H2W_RECOVERY_CONTROLLER;
  let resolvePersist;
  let actions = 0;
  const pending = recovery.runAfterDurablePersistence({
    persist: () => new Promise((resolve) => { resolvePersist = resolve; }),
    action: () => { actions += 1; },
    waitMs: 0,
  });
  await Promise.resolve();
  assert.equal(actions, 0);
  resolvePersist(true);
  assert.equal(await pending, true);
  assert.equal(actions, 1);

  assert.equal(await recovery.runAfterDurablePersistence({
    persist: async () => false,
    action: () => { actions += 1; },
    waitMs: 0,
  }), false);
  assert.equal(actions, 1);

  assert.equal(await recovery.runAfterDurablePersistence({
    persist: async () => { throw new Error("storage unavailable"); },
    action: () => { actions += 1; },
    waitMs: 0,
  }), false);
  assert.equal(actions, 1);
});

test("browser performance scheduler coalesces mutation bursts", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  let now = 1000;
  let runs = 0;
  let nextTimerId = 1;
  const timers = new Map();
  const scheduler = perf.createCoalescedScheduler(() => { runs += 1; }, {
    minIntervalMs: 400,
    now: () => now,
    setTimer: (fn, delay) => {
      const id = nextTimerId++;
      timers.set(id, { fn, delay });
      return id;
    },
    clearTimer: (id) => timers.delete(id),
  });

  for (let i = 0; i < 100; i += 1) scheduler.schedule();
  assert.equal(timers.size, 1);
  const [firstId, first] = timers.entries().next().value;
  assert.equal(first.delay, 0);
  timers.delete(firstId);
  first.fn();
  assert.equal(runs, 1);

  now += 100;
  for (let i = 0; i < 100; i += 1) scheduler.schedule();
  assert.equal(timers.size, 1);
  const second = timers.values().next().value;
  assert.equal(second.delay, 300);
});

test("browser performance scheduler suspends while hidden and flushes on resume", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  let hidden = true;
  let runs = 0;
  let timers = 0;
  const scheduler = perf.createCoalescedScheduler(() => { runs += 1; }, {
    isSuspended: () => hidden,
    setTimer: () => { timers += 1; return timers; },
    clearTimer: () => {},
  });
  assert.equal(scheduler.schedule(), false);
  assert.equal(timers, 0);
  assert.equal(runs, 0);
  hidden = false;
  assert.equal(scheduler.flush(), true);
  assert.equal(runs, 1);
});

test("turn mutation filter ignores tool-card and HUD churn but keeps turn changes", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  const makeElement = ({ parent = null, matches = [], contains = [] } = {}) => ({
    nodeType: 1,
    parentElement: parent,
    matches: (selector) => matches.includes(selector),
    closest(selector) {
      if (matches.includes(selector)) return this;
      return parent?.closest?.(selector) || null;
    },
    querySelector: (selector) => contains.includes(selector) ? {} : null,
  });
  const toolSelector = perf.DEFAULT_IGNORED_TURN_MUTATION_SELECTOR;
  const turnSelector = perf.DEFAULT_TURN_MUTATION_SELECTOR;
  const assistant = makeElement({ matches: [turnSelector] });
  const tool = makeElement({ parent: assistant, matches: [toolSelector] });
  const toolChild = makeElement({ parent: tool });

  assert.equal(perf.mutationTouchesConversationTurns([{
    target: tool,
    addedNodes: [toolChild],
    removedNodes: [],
  }]), false);

  const streamedText = { nodeType: 3, parentElement: assistant };
  assert.equal(perf.mutationTouchesConversationTurns([{
    target: assistant,
    addedNodes: [streamedText],
    removedNodes: [],
  }]), true);

  const wrapper = makeElement({ contains: [turnSelector] });
  assert.equal(perf.mutationTouchesConversationTurns([{
    target: makeElement(),
    addedNodes: [wrapper],
    removedNodes: [],
  }]), true);
});

test("bounded message sampler preserves growth length while skipping tool subtrees", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  const ignoredSelector = perf.DEFAULT_IGNORED_MESSAGE_TEXT_SELECTOR;
  const text = (value) => ({ nodeType: 3, nodeValue: value, parentElement: null });
  const element = (children = [], { ignored = false, parent = null } = {}) => {
    const node = {
      nodeType: 1,
      childNodes: children,
      parentElement: parent,
      closest(selector) {
        if (ignored && selector === ignoredSelector) return this;
        return parent?.closest?.(selector) || null;
      },
    };
    for (const child of children) child.parentElement = node;
    return node;
  };
  const toolText = text("Z".repeat(5000));
  const tool = element([toolText], { ignored: true });
  const root = element([
    text("A".repeat(700)),
    tool,
    text("B".repeat(700)),
  ]);
  const sample = perf.sampleBoundedMessageText(root, { maxChars: 1024, tailChars: 256 });

  assert.equal(sample.total_chars, 1400);
  assert.equal(sample.truncated, true);
  assert.equal(sample.skipped_subtrees, 1);
  assert.ok(sample.text.length <= 1024);
  assert.ok(sample.text.startsWith("A"));
  assert.ok(sample.text.endsWith("B".repeat(256)));
  assert.doesNotMatch(sample.text, /Z/);
});

test("ui pressure classifier bands healthy, warning, high from bounded inputs", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  const classify = perf.classifyUiPressure;
  const policy = perf.DEFAULT_UI_PRESSURE_POLICY;
  assert.equal(classify({}).level, "healthy");
  assert.equal(classify({ mutationRatePerMin: 10, ticksPerMin: 10, maxTimerDriftMs: 100 }).level, "healthy");
  assert.equal(classify({ mutationRatePerMin: policy.warningMutationRatePerMin }).level, "warning");
  assert.equal(classify({ mutationRatePerMin: policy.highMutationRatePerMin }).level, "high");
  assert.equal(classify({ ticksPerMin: policy.warningTickRatePerMin }).level, "warning");
  assert.equal(classify({ ticksPerMin: policy.highTickRatePerMin }).level, "high");
  // Timer drift is the sampler-fidelity fallback signal.
  assert.equal(classify({ maxTimerDriftMs: policy.warningMaxTimerDriftMs }).level, "warning");
  assert.equal(classify({ maxTimerDriftMs: policy.highMaxTimerDriftMs }).level, "high");
  // A warning plus a high signal stays high.
  assert.equal(classify({
    mutationRatePerMin: policy.warningMutationRatePerMin,
    maxTimerDriftMs: policy.highMaxTimerDriftMs,
  }).level, "high");
  const result = classify({ mutationRatePerMin: policy.highMutationRatePerMin });
  assert.ok(result.reasons.some((r) => r.startsWith("mutation_rate_min:")));
});

test("ui pressure meter aggregates bounded counters and resets per fixed window", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  const now = 1000;
  const meter = perf.createUiPressureMeter({ windowMs: 60000, now: () => now });
  for (let i = 0; i < 100; i += 1) meter.recordMutation(1, now);
  meter.recordTick(1, now);
  meter.recordTimerDrift(120, now);
  meter.recordTimerDrift(6000, now);
  meter.recordHttpStatus(200, now);
  meter.recordHttpStatus(429, now);
  let metrics = meter.evaluate(now);
  assert.equal(metrics.drift_samples, 2);
  assert.equal(metrics.max_timer_drift_ms, 6000);
  assert.equal(metrics.http_429_count, 1);
  assert.equal(metrics.last_http_429_at, now);
  assert.equal(metrics.level, "high");
  assert.ok(metrics.mutation_rate_per_min > 0 && metrics.ticks_per_min > 0);

  // Same-window rates normalize to per-minute scale.
  const early = meter.evaluate(now + 15000);
  assert.ok(Math.abs(early.mutation_rate_per_min - 100 * 4) < 1);

  // New window resets counters while keeping the meter usable.
  const later = meter.evaluate(now + 60000);
  assert.equal(later.sampled_at, 61000);
  assert.equal(later.mutation_rate_per_min, 0);
  assert.equal(later.level, "healthy");

  // Long Task samples are informational: recorded but never raise the level.
  const fresh = perf.createUiPressureMeter({ windowMs: 60000, now: () => now });
  fresh.recordLongTask(250, now);
  const withLongTask = fresh.evaluate(now);
  assert.equal(withLongTask.long_task_count, 1);
  assert.equal(withLongTask.long_task_max_ms, 250);
  assert.equal(withLongTask.level, "healthy");
  assert.equal(withLongTask.max_timer_drift_ms, 0);
});

test("page health requires sustained server-confirmed pressure and treats 429 as backoff-only", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  const policy = {
    ...perf.DEFAULT_PAGE_HEALTH_POLICY,
    minSampleWindowMs: 15000,
    sustainedPressureMs: 30000,
    activeTurnStallMs: 30000,
    memoryQuiescentMs: 30000,
  };
  const now = 100000;
  const highUi = {
    level: "high",
    window_elapsed_ms: 20000,
    long_task_count: 5,
    long_task_max_ms: 2500,
  };

  // A hot page is not enough: an active turn must be stalled and the server
  // must prove the assistant turn already settled before render recovery.
  let health = perf.classifyPageHealth({
    ui: highUi,
    activeTurn: true,
    serverSettled: false,
    stallMs: 60000,
    highSince: 60000,
    now,
  }, policy);
  assert.notEqual(health.action, "reload");

  // The first confirmed sample merely arms the sustained-pressure timer.
  health = perf.classifyPageHealth({
    ui: highUi,
    activeTurn: true,
    serverSettled: true,
    stallMs: 60000,
    highSince: 90000,
    now,
  }, policy);
  assert.equal(health.state, "degraded");
  assert.equal(health.action, "observe");

  health = perf.classifyPageHealth({
    ui: highUi,
    activeTurn: true,
    serverSettled: true,
    stallMs: 60000,
    highSince: 60000,
    now,
  }, policy);
  assert.equal(health.state, "reload_recommended");
  assert.equal(health.action, "reload");
  assert.equal(health.reason, "render_stall");

  // Early-window rates are deliberately observational because rate scaling can
  // exaggerate the first few seconds of a new window.
  health = perf.classifyPageHealth({
    ui: { ...highUi, window_elapsed_ms: 5000 },
    activeTurn: true,
    serverSettled: true,
    stallMs: 60000,
    highSince: 60000,
    now,
  }, policy);
  assert.notEqual(health.action, "reload");

  // Critical heap pressure is eligible only while the page is quiescent and
  // only after the same sustained-pressure budget.
  health = perf.classifyPageHealth({
    ui: {},
    memory: { usedJSHeapSize: 1600 * 1024 * 1024, jsHeapSizeLimit: 2 * 1024 * 1024 * 1024 },
    quiescentMs: 60000,
    highSince: 60000,
    now,
  }, policy);
  assert.equal(health.action, "reload");
  assert.equal(health.reason, "memory_pressure");

  // Rate limiting wins over every pressure signal: never retry/reload while
  // the bounded 429 backoff is active.
  health = perf.classifyPageHealth({
    ui: highUi,
    memory: { usedJSHeapSize: 1600 * 1024 * 1024, jsHeapSizeLimit: 2 * 1024 * 1024 * 1024 },
    activeTurn: true,
    serverSettled: true,
    stallMs: 60000,
    quiescentMs: 60000,
    highSince: 60000,
    backoffUntil: now + 30000,
    now,
  }, policy);
  assert.equal(health.state, "rate_limited");
  assert.equal(health.action, "backoff");
  assert.equal(health.candidate, false);
});

test("429 backoff grows 30s to 60s and caps at 120s", () => {
  const perf = loadClassicExtensionScripts().H2W_BROWSER_PERFORMANCE;
  assert.equal(perf.rateLimitBackoffMs(1), 30000);
  assert.equal(perf.rateLimitBackoffMs(2), 60000);
  assert.equal(perf.rateLimitBackoffMs(3), 120000);
  assert.equal(perf.rateLimitBackoffMs(8), 120000);
});

test("retired source tab discard is gated on committed inactive handoff", () => {
  assert.equal(shouldDiscardRetiredSourceTab({ committed: true, sourceTabId: null, targetTabId: 2 }), false);
  assert.equal(shouldDiscardRetiredSourceTab({ committed: false, sourceTabId: 1, targetTabId: 2 }), false);
  assert.equal(shouldDiscardRetiredSourceTab({ committed: true, sourceTabId: 1, targetTabId: 2, sourceActive: true }), false);
  assert.equal(shouldDiscardRetiredSourceTab({ committed: true, sourceTabId: 1, targetTabId: 1 }), false);
  assert.equal(shouldDiscardRetiredSourceTab({ committed: true, sourceTabId: 1, targetTabId: 2, sourceActive: false }), true);
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
    "page_health_state",
    "page_health_checked_at",
    "page_health_high_since",
    "page_health_reason",
    "page_health_background_reload_attempt",
    "page_health_last_background_reload_at",
    "page_health_background_reload_executed_at",
    "network_429_count",
    "network_429_last_seen_at",
    "network_429_source",
    "network_backoff_until",
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

  const forcePolicy = {
    ...policy,
    backgroundReloadCooldownMs: 5000,
    maxBackgroundReloadAttempts: 1,
  };
  const forceSafe = {
    ...safe,
    backgroundReloadAttempts: 0,
    lastBackgroundReloadAt: null,
  };
  assert.equal(recovery.canForceTabReloadSafely(forceSafe, forcePolicy), true);
  for (const key of [
    "composerBusy",
    "composerHasHumanText",
    "streaming",
    "toolRunning",
    "permissionCardActive",
    "deliveryUnknown",
    "mutationDeliveryUncertain",
  ]) assert.equal(recovery.canForceTabReloadSafely({ ...forceSafe, [key]: true }, forcePolicy), false, `forced:${key}`);
  assert.equal(recovery.canForceTabReloadSafely({ ...forceSafe, backgroundReloadAttempts: 1 }, forcePolicy), false);
  assert.equal(recovery.canForceTabReloadSafely({ ...forceSafe, lastBackgroundReloadAt: 7000 }, forcePolicy), false);

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
    server: { ok: true, currentNodeRole: "assistant", messageId: "a1", text: "hello world", finished: true, updatedAt: 90000 },
    now,
  });
  assert.equal(synced.state, "synced");
  const ahead = recovery.classifyViewFreshness({
    dom: baseDom,
    server: { ok: true, currentNodeRole: "assistant", messageId: "a2", text: "newer answer", finished: true, updatedAt: 95000 },
    now,
  });
  assert.equal(ahead.state, "server_ahead");
  const missingAssistantDom = recovery.classifyViewFreshness({
    dom: { messageId: null, text: "", changedAt: 90000 },
    server: { ok: true, currentNodeRole: "assistant", messageId: "a2", text: "server-rendered answer", finished: true, updatedAt: 95000 },
    now,
  });
  assert.equal(missingAssistantDom.state, "server_ahead");
  const emptyAssistantDom = recovery.classifyViewFreshness({
    dom: { messageId: "a2", text: "", changedAt: 90000 },
    server: { ok: true, currentNodeRole: "assistant", messageId: "a2", text: "server-rendered answer", finished: true, updatedAt: 95000 },
    now,
  });
  assert.equal(emptyAssistantDom.state, "server_ahead");
  const previousAssistantWhileUserIsCurrent = recovery.classifyViewFreshness({
    dom: { messageId: null, text: "", changedAt: 90000 },
    server: { ok: true, currentNodeRole: "user", messageId: "a1", text: "previous answer", finished: true, updatedAt: 95000 },
    now,
  });
  assert.equal(previousAssistantWhileUserIsCurrent.state, "synced");
  const stalled = recovery.classifyViewFreshness({
    dom: { ...baseDom, changedAt: 60000 },
    server: { ok: true, currentNodeRole: "assistant", messageId: "a1", text: "hello world", finished: false, updatedAt: 60000 },
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
  assert.match(wake, /async function reloadAfterPersistingConversationState\(\)/);
  assert.match(wake, /conversationHealthPersistChain/);
  assert.match(wake, /runAfterDurablePersistence/);
  assert.match(wake, /const retryStarted = await RECOVERY_CONTROLLER\.runAfterDurablePersistence/);
  assert.match(wake, /action: \(\) => threadError\.retry\.click\(\)/);
  assert.match(wake, /if \(!retryStarted\) return true/);
  assert.match(wake, /maybeRecoverPageHealth\(\)/);
  assert.match(wake, /recordHttpStatus\(429/);
  assert.match(wake, /h2w_force_tab_reload/);
  assert.match(wake, /page_health_background_reload_attempt/);
  assert.match(wake, /network_backoff_until/);
  assert.match(wake, /const primaryReloadSpent = Number\(conversationHealth\.reload_attempt \|\| 0\)/);
  assert.match(wake, /if \(!primaryReloadSpent \|\| !lastReloadAt\) return false/);
  assert.equal((wake.match(/location\.reload\(\)/g) || []).length, 1);
  assert.ok((wake.match(/await reloadAfterPersistingConversationState\(\)/g) || []).length >= 6);
  assert.match(wake, /markRolloverRecommended/);
  assert.match(wake, /conversation-turn-/);
  assert.match(wake, /mergeMessageCountFloor/);
  assert.match(wake, /stale_view_activation_template/);
});

test("ChatGPT turn watcher caches latest turns and reuses settled turns for pressure", () => {
  const wake = fs.readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
  assert.match(wake, /rediscoverLatestTurns\(\)/);
  assert.match(wake, /\[data-message-author-role=\"user\"\], \[data-message-author-role=\"assistant\"\]/);
  assert.match(wake, /virtualTurn\?\.getAttribute\(\"data-testid\"\)/);
  assert.match(wake, /markLatestTurnsDirty\(\)/);
  assert.match(wake, /latestTurnForRole\(role\)/);
  assert.match(wake, /updateContextPressureFromSettledTurns\(/);
  assert.match(wake, /CONTEXT_PRESSURE\.mergeSettledTurns\(/);
  assert.match(wake, /uiPressure\?\.recordMutation\(\)/);
  assert.match(wake, /uiPressure\?\.recordTick\(\)/);
  assert.match(wake, /recordTimerDrift\(driftMs\)/);
  assert.match(wake, /rehydrate the latest-turn cache/);
  assert.match(wake, /sampleLatestMessageText\(el\)/);
  assert.match(wake, /let lastAsstLen = initialAssistant\.totalChars/);
  assert.match(wake, /const curLen = currentAssistant\.totalChars/);
  assert.match(wake, /function conversationHasPendingReply\(\)/);
  assert.match(wake, /const hasPendingReply = conversationHasPendingReply/);
  assert.match(wake, /event\.isTrusted/);
  assert.match(wake, /markReplyWaiting\(conversationHealth, at\)/);
  assert.match(wake, /serverSnapshotMatchesPendingTurn/);
  assert.match(wake, /maybeReportServerSettledTurn/);
  assert.match(wake, /"composer_stopped"/);
  assert.match(wake, /"pending_fallback"/);
  assert.match(wake, /userCreatedAt/);
  assert.match(wake, /serverConfirmed:\s*true/);
  assert.match(wake, /serverConfirmed\s*\?\s*\{ userEl: null, assistantEl: null \}/);
  assert.match(wake, /serverAssistantCurrent/);
  assert.match(wake, /serverSettled\s*\?\s*false\s*:\s*\(serverOpen\s*\?\s*true\s*:\s*domGenerating\)/);
  assert.match(wake, /serverCurrentNodeRole/);
  assert.doesNotMatch(wake, /hydrationGraceUntil/);
});

test("context pressure reuses settled turns for turn-end updates", () => {
  const context = loadClassicExtensionScripts();
  const pressure = context.H2W_CONTEXT_PRESSURE;
  let record = pressure.emptyContextRecord("c", 1000);
  record = pressure.mergeSettledTurns(record, [
    { id: "u1:user", role: "user", text: "hello world" },
    { id: "a1:assistant", role: "assistant", text: "a substantive answer" },
  ], 12, "chatgpt_virtual_turn_index", 1500);
  const summary = pressure.summarizeContextRecord(record);
  assert.equal(summary.observed_message_count, 2);
  assert.equal(summary.message_count_floor, 12);
  assert.equal(summary.message_floor_source, "chatgpt_virtual_turn_index");
  assert.equal(record.updated_at, 1500);
  // Monotonic floor: a lower floor is ignored and no empty merge bumps the age.
  record = pressure.mergeSettledTurns(record, [], 8, "chatgpt_virtual_turn_index", 2000);
  assert.equal(pressure.summarizeContextRecord(record).message_count_floor, 12);
  assert.equal(record.updated_at, 1500);
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
