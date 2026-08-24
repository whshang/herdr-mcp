// Pure conversation health state machine.
// UI and recovery side effects stay outside this module.

const CONVERSATION_STATES = Object.freeze({
  HEALTHY: "healthy",
  REPLY_WAITING: "reply_waiting",
  REPLY_SUSPECT: "reply_suspect",
  RECOVERY_MESSAGE_SENT: "recovery_message_sent",
  RELOAD_PENDING: "reload_pending",
  RECOVERING: "recovering",
  ROLLOVER_RECOMMENDED: "rollover_recommended",
  ROLLOVER_REQUIRED: "rollover_required",
  FAILED: "failed",
});

function createConversationHealth(convKey, continuityId = null) {
  return {
    convKey,
    continuity_id: continuityId || globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`,
    state: CONVERSATION_STATES.HEALTHY,
    last_user_submit_at: null,
    reply_started_at: null,
    last_assistant_progress_at: null,
    last_turn_end_at: null,
    recovery_attempt: 0,
    reload_attempt: 0,
    last_reload_at: null,
    rollover_hint_at: null,
  };
}

function markReplyWaiting(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.REPLY_WAITING,
    last_user_submit_at: at,
    reply_started_at: null,
    last_assistant_progress_at: null,
    last_turn_end_at: null,
    recovery_attempt: 0,
    reload_attempt: 0,
  };
}

function markReplyStarted(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.HEALTHY,
    reply_started_at: record?.reply_started_at || at,
    last_assistant_progress_at: at,
  };
}

function markAssistantProgress(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.HEALTHY,
    reply_started_at: record?.reply_started_at || at,
    last_assistant_progress_at: at,
  };
}

function markTurnEnded(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.HEALTHY,
    last_turn_end_at: at,
    recovery_attempt: 0,
    reload_attempt: 0,
  };
}

function markReplySuspect(record, reason = "reply_timeout") {
  return { ...record, state: CONVERSATION_STATES.REPLY_SUSPECT, suspect_reason: reason };
}

function markReloadPending(record, at = Date.now()) {
  return { ...record, state: CONVERSATION_STATES.RELOAD_PENDING, reload_pending_at: at };
}

function markRolloverRecommended(record, at = Date.now()) {
  return { ...record, state: CONVERSATION_STATES.ROLLOVER_RECOMMENDED, rollover_hint_at: at };
}

function markRolloverRequired(record, at = Date.now()) {
  return { ...record, state: CONVERSATION_STATES.ROLLOVER_REQUIRED, rollover_hint_at: at };
}

globalThis.H2W_CONVERSATION_HEALTH = {
  CONVERSATION_STATES,
  createConversationHealth,
  markReplyWaiting,
  markReplyStarted,
  markAssistantProgress,
  markReplySuspect,
  markReloadPending,
  markTurnEnded,
  markRolloverRecommended,
  markRolloverRequired,
};
