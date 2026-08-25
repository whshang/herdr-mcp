// context-pressure.js — conservative, observable conversation pressure policy.
//
// ChatGPT does not expose a reliable backend token counter to the extension.
// Keep this policy explicitly estimated: count only user/assistant text that the
// page exposes, deduplicate stable turn ids, and use a conservative tokenizer-
// compatible estimator. Raw MCP payload bytes are intentionally excluded.
(function initContextPressure(global) {
  "use strict";

  const CONTEXT_PRESSURE_VERSION = 2;
  const EFFECTIVE_CONTEXT_TOKENS = 128000;
  // Browser-visible user/assistant text is only part of ChatGPT's effective
  // context. Project instructions, system messages and MCP/tool payloads are
  // not reliably visible to the extension, so keep a deliberate reserve.
  const USABLE_TEXT_BUDGET_TOKENS = 96000;
  const MAX_TRACKED_TURNS = 3000;

  const DEFAULT_CONTEXT_POLICY = Object.freeze({
    effectiveContextTokens: EFFECTIVE_CONTEXT_TOKENS,
    usableTextBudgetTokens: USABLE_TEXT_BUDGET_TOKENS,
    warningTokens: 56000,
    prepareTokens: 64000,
    recommendTokens: 72000,
    autoRolloverTokens: 80000,
    highRiskTokens: 92000,
    messageWarningCount: 32,
    messagePrepareCount: 40,
    messageRecommendCount: 46,
    messageAutoRolloverCount: 50,
    messageHighRiskCount: 60,
    turnWarningCount: 20,
    turnPrepareCount: 24,
    turnRecommendCount: 27,
    turnAutoRolloverCount: 30,
    turnHighRiskCount: 36,
    ageWarningMs: 12 * 60 * 60 * 1000,
    autoRetryCooldownMs: 60000,
  });

  const CONTINUITY_STATES = Object.freeze({
    HEALTHY: "healthy",
    CONTEXT_WARNING: "context_warning",
    HANDOFF_PREPARE: "handoff_prepare",
    ROLLOVER_RECOMMENDED: "rollover_recommended",
    ROLLOVER_REQUIRED: "rollover_required",
    HIGH_RISK: "high_risk",
  });

  const STATE_RANK = Object.freeze({
    [CONTINUITY_STATES.HEALTHY]: 0,
    [CONTINUITY_STATES.CONTEXT_WARNING]: 1,
    [CONTINUITY_STATES.HANDOFF_PREPARE]: 2,
    [CONTINUITY_STATES.ROLLOVER_RECOMMENDED]: 3,
    [CONTINUITY_STATES.ROLLOVER_REQUIRED]: 4,
    [CONTINUITY_STATES.HIGH_RISK]: 5,
  });

  function maxState(left, right) {
    return (STATE_RANK[right] || 0) > (STATE_RANK[left] || 0) ? right : left;
  }

  function positiveInt(value) {
    const n = Number(value);
    return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
  }

  function codePointLength(value) {
    return Array.from(String(value || "")).length;
  }

  function textFingerprint(value) {
    const text = String(value || "");
    let hash = 2166136261;
    for (let i = 0; i < text.length; i += 1) {
      hash ^= text.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return `${text.length}:${hash >>> 0}`;
  }

  /**
   * Conservative o200k-compatible estimate for browser-only use.
   *
   * Shipping the full o200k BPE table would add several MB to this otherwise
   * tiny MV3 extension. This estimator deliberately errs high for source code
   * and punctuation while treating CJK characters close to one token each.
   * The UI and policy always label the result as an estimate.
   */
  function estimateO200kCompatibleTokens(input) {
    const text = String(input || "");
    if (!text) return 0;
    let tokens = 0;
    const parts = text.match(/[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\u3040-\u30ff\uac00-\ud7af]+|[A-Za-z]+|\d+|\s+|[^A-Za-z\d\s]/gu) || [];
    for (const part of parts) {
      if (/^[\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\u3040-\u30ff\uac00-\ud7af]+$/u.test(part)) {
        tokens += Math.ceil(codePointLength(part) * 0.95);
      } else if (/^[A-Za-z]+$/.test(part)) {
        tokens += Math.ceil(part.length / 4);
      } else if (/^\d+$/.test(part)) {
        tokens += Math.ceil(part.length / 3);
      } else if (/^\s+$/.test(part)) {
        tokens += Math.ceil(part.length / 12);
      } else {
        tokens += Math.ceil(codePointLength(part) / 2);
      }
    }
    return Math.max(1, tokens);
  }

  function stateForTokens(tokens, policy = DEFAULT_CONTEXT_POLICY) {
    const n = positiveInt(tokens);
    if (n >= policy.highRiskTokens) return CONTINUITY_STATES.HIGH_RISK;
    if (n >= policy.autoRolloverTokens) return CONTINUITY_STATES.ROLLOVER_REQUIRED;
    if (n >= policy.recommendTokens) return CONTINUITY_STATES.ROLLOVER_RECOMMENDED;
    if (n >= policy.prepareTokens) return CONTINUITY_STATES.HANDOFF_PREPARE;
    if (n >= policy.warningTokens) return CONTINUITY_STATES.CONTEXT_WARNING;
    return CONTINUITY_STATES.HEALTHY;
  }

  function evaluateContextPressure(input = {}, policy = DEFAULT_CONTEXT_POLICY) {
    const estimatedTextTokens = positiveInt(input.estimatedTextTokens);
    const messageCount = positiveInt(input.messageCount);
    const turnCount = positiveInt(input.turnCount);
    const firstObservedAt = positiveInt(input.firstObservedAt);
    const now = positiveInt(input.now) || Date.now();
    let state = stateForTokens(estimatedTextTokens, policy);
    const reasons = [];

    if (estimatedTextTokens >= policy.warningTokens) reasons.push(`estimated_text_tokens:${estimatedTextTokens}`);
    if (messageCount >= policy.messageWarningCount) reasons.push(`message_count:${messageCount}`);
    if (turnCount >= policy.turnWarningCount) reasons.push(`turn_count:${turnCount}`);
    if (firstObservedAt && now - firstObservedAt >= policy.ageWarningMs) reasons.push(`conversation_age_ms:${now - firstObservedAt}`);

    // ChatGPT virtualizes old DOM nodes and hides system/tool payloads, so
    // message/turn counts are first-class pressure signals rather than warning
    // decoration. The virtual list's absolute conversation-turn index gives us
    // a monotonic message-count floor even when only a handful of nodes remain
    // mounted in the page.
    if (messageCount >= policy.messageHighRiskCount) state = maxState(state, CONTINUITY_STATES.HIGH_RISK);
    else if (messageCount >= policy.messageAutoRolloverCount) state = maxState(state, CONTINUITY_STATES.ROLLOVER_REQUIRED);
    else if (messageCount >= policy.messageRecommendCount) state = maxState(state, CONTINUITY_STATES.ROLLOVER_RECOMMENDED);
    else if (messageCount >= policy.messagePrepareCount) state = maxState(state, CONTINUITY_STATES.HANDOFF_PREPARE);
    else if (messageCount >= policy.messageWarningCount) state = maxState(state, CONTINUITY_STATES.CONTEXT_WARNING);

    if (turnCount >= policy.turnHighRiskCount) state = maxState(state, CONTINUITY_STATES.HIGH_RISK);
    else if (turnCount >= policy.turnAutoRolloverCount) state = maxState(state, CONTINUITY_STATES.ROLLOVER_REQUIRED);
    else if (turnCount >= policy.turnRecommendCount) state = maxState(state, CONTINUITY_STATES.ROLLOVER_RECOMMENDED);
    else if (turnCount >= policy.turnPrepareCount) state = maxState(state, CONTINUITY_STATES.HANDOFF_PREPARE);
    else if (turnCount >= policy.turnWarningCount) state = maxState(state, CONTINUITY_STATES.CONTEXT_WARNING);

    if (state === CONTINUITY_STATES.HEALTHY && reasons.length) state = CONTINUITY_STATES.CONTEXT_WARNING;

    const recommendation = [CONTINUITY_STATES.ROLLOVER_REQUIRED, CONTINUITY_STATES.HIGH_RISK].includes(state)
      ? "auto_rollover"
      : state === CONTINUITY_STATES.ROLLOVER_RECOMMENDED
        ? "rollover_now"
        : state === CONTINUITY_STATES.HANDOFF_PREPARE
          ? "prepare_handoff"
          : state === CONTINUITY_STATES.CONTEXT_WARNING
            ? "conversation_long"
            : "none";

    return {
      version: CONTEXT_PRESSURE_VERSION,
      state,
      estimated_text_tokens: estimatedTextTokens,
      effective_context_tokens: policy.effectiveContextTokens,
      usable_text_budget_tokens: policy.usableTextBudgetTokens,
      usage_ratio: estimatedTextTokens / policy.usableTextBudgetTokens,
      message_count: messageCount,
      turn_count: turnCount,
      reasons,
      recommendation,
    };
  }

  function emptyContextRecord(convKey, now = Date.now()) {
    return {
      version: CONTEXT_PRESSURE_VERSION,
      convKey: String(convKey || ""),
      first_observed_at: now,
      updated_at: now,
      turns: {},
      message_count_floor: 0,
      message_floor_source: null,
      last_auto_attempt_at: null,
      auto_transfer_id: null,
      last_rollover_at: null,
      rollover_reason: null,
    };
  }

  function normalizedObservation(raw) {
    const id = String(raw?.id || "").trim();
    const role = raw?.role === "user" || raw?.role === "assistant" ? raw.role : null;
    if (!id || !role) return null;
    const text = String(raw?.text || "");
    if (!text.trim()) return null;
    return {
      id,
      role,
      chars: text.length,
      token_estimate: estimateO200kCompatibleTokens(text),
      fingerprint: textFingerprint(text),
    };
  }

  function mergeObservedTurns(record, observations = [], now = Date.now()) {
    const base = record && typeof record === "object"
      ? { ...record, turns: { ...(record.turns || {}) } }
      : emptyContextRecord("", now);
    base.version = CONTEXT_PRESSURE_VERSION;
    if (!positiveInt(base.first_observed_at)) base.first_observed_at = now;

    let changed = false;
    for (const raw of observations) {
      const observation = normalizedObservation(raw);
      if (!observation) continue;
      const previous = base.turns[observation.id];
      if (previous?.role === observation.role
        && previous?.fingerprint === observation.fingerprint
        && previous?.token_estimate === observation.token_estimate) continue;
      base.turns[observation.id] = observation;
      changed = true;
    }

    const entries = Object.entries(base.turns);
    if (entries.length > MAX_TRACKED_TURNS) {
      // Preserve the newest insertion-order ids. 3k ids is far above normal
      // chat length while keeping storage bounded. No text is persisted.
      for (const [id] of entries.slice(0, entries.length - MAX_TRACKED_TURNS)) delete base.turns[id];
      changed = true;
    }
    if (changed) base.updated_at = now;
    return base;
  }

  function mergeMessageCountFloor(record, floor, source = "virtual_turn_index", now = Date.now()) {
    const base = record && typeof record === "object"
      ? { ...record, turns: { ...(record.turns || {}) } }
      : emptyContextRecord("", now);
    base.version = CONTEXT_PRESSURE_VERSION;
    const nextFloor = positiveInt(floor);
    const previousFloor = positiveInt(base.message_count_floor);
    if (nextFloor <= previousFloor) return base;
    base.message_count_floor = nextFloor;
    base.message_floor_source = String(source || "virtual_turn_index");
    base.updated_at = now;
    return base;
  }

  /**
   * Minimal incremental turn-end update for the browser turn watcher: merge
   * just the settled user/assistant observations (already read at settle time)
   * plus the settled turn's own monotonic virtual floor, instead of rescanning
   * the full conversation DOM. Public contract of this module is unchanged; the
   * full-history merge remains available through mergeObservedTurns.
   */
  function mergeSettledTurns(record, settledTurns = [], floor = 0, source = "chatgpt_virtual_turn_index", now = Date.now()) {
    let next = mergeObservedTurns(record, settledTurns, now);
    if (positiveInt(floor) > 0) next = mergeMessageCountFloor(next, floor, source, now);
    return next;
  }

  function summarizeContextRecord(record, policy = DEFAULT_CONTEXT_POLICY, now = Date.now()) {
    const turns = Object.values(record?.turns || {});
    const estimatedTextTokens = turns.reduce((sum, turn) => sum + positiveInt(turn?.token_estimate), 0);
    const observedMessageCount = turns.length;
    const messageCount = Math.max(observedMessageCount, positiveInt(record?.message_count_floor));
    const turnCount = turns.filter((turn) => turn?.role === "user").length;
    return {
      ...evaluateContextPressure({
        estimatedTextTokens,
        messageCount,
        turnCount,
        firstObservedAt: record?.first_observed_at,
        now,
      }, policy),
      first_observed_at: record?.first_observed_at || null,
      updated_at: record?.updated_at || null,
      observed_message_count: observedMessageCount,
      message_count_floor: positiveInt(record?.message_count_floor),
      message_floor_source: record?.message_floor_source || null,
      last_auto_attempt_at: record?.last_auto_attempt_at || null,
      auto_transfer_id: record?.auto_transfer_id || null,
      last_rollover_at: record?.last_rollover_at || null,
      rollover_reason: record?.rollover_reason || null,
    };
  }

  function markAutoAttempt(record, transferId = null, reason = "context_pressure", now = Date.now()) {
    return {
      ...(record || emptyContextRecord("", now)),
      last_auto_attempt_at: now,
      auto_transfer_id: transferId || record?.auto_transfer_id || null,
      rollover_reason: String(reason || "context_pressure"),
      updated_at: now,
    };
  }

  function markRolloverCommitted(record, transferId = null, now = Date.now()) {
    return {
      ...(record || emptyContextRecord("", now)),
      last_rollover_at: now,
      auto_transfer_id: transferId || record?.auto_transfer_id || null,
      updated_at: now,
    };
  }

  function isActiveHandoffStatus(status) {
    return ["summary_requested", "summary_ready", "target_opening", "seed_submitting", "seed_uncertain"].includes(String(status || ""));
  }

  function shouldAutoRollover(input = {}, policy = DEFAULT_CONTEXT_POLICY) {
    const pressure = input.pressure || {};
    const state = String(pressure.state || "");
    if (![CONTINUITY_STATES.ROLLOVER_REQUIRED, CONTINUITY_STATES.HIGH_RISK].includes(state)) return false;
    if (String(input.runtimeHealth || "healthy") !== "healthy") return false;
    if (!input.bound || !input.canHandoff || !input.projectConversation) return false;
    if (!input.quiescent || input.deliveryUncertain || input.mutationPending) return false;
    if (isActiveHandoffStatus(input.handoffStatus)) return false;
    const now = positiveInt(input.now) || Date.now();
    const lastAttemptAt = positiveInt(input.lastAutoAttemptAt);
    if (lastAttemptAt && now - lastAttemptAt < policy.autoRetryCooldownMs) return false;
    return true;
  }

  global.H2W_CONTEXT_PRESSURE = Object.freeze({
    CONTEXT_PRESSURE_VERSION,
    EFFECTIVE_CONTEXT_TOKENS,
    USABLE_TEXT_BUDGET_TOKENS,
    DEFAULT_CONTEXT_POLICY,
    CONTINUITY_STATES,
    estimateO200kCompatibleTokens,
    textFingerprint,
    evaluateContextPressure,
    emptyContextRecord,
    mergeObservedTurns,
    mergeMessageCountFloor,
    mergeSettledTurns,
    summarizeContextRecord,
    markAutoAttempt,
    markRolloverCommitted,
    shouldAutoRollover,
  });
})(globalThis);
