// Pure conversation health state machine.
// UI and recovery side effects stay outside this module.

export const CONVERSATION_STATES = Object.freeze({
  HEALTHY: "healthy",
  REPLY_WAITING: "reply_waiting",
  REPLY_SUSPECT: "reply_suspect",
  RECOVERY_MESSAGE_SENT: "recovery_message_sent",
  RECOVERING: "recovering",
  ROLLOVER_RECOMMENDED: "rollover_recommended",
  ROLLOVER_REQUIRED: "rollover_required",
  FAILED: "failed",
});

export function createConversationHealth(convKey) {
  return {
    convKey,
    state: CONVERSATION_STATES.HEALTHY,
    lastUserSubmitAt: null,
    replyStartedAt: null,
    lastAssistantProgressAt: null,
    recoveryAttempts: 0,
    reloadAttempts: 0,
  };
}

export function markReplyWaiting(record, at = Date.now()) {
  return { ...record, state: CONVERSATION_STATES.REPLY_WAITING, lastUserSubmitAt: at };
}

export function markReplyStarted(record, at = Date.now()) {
  return { ...record, state: CONVERSATION_STATES.HEALTHY, replyStartedAt: at, lastAssistantProgressAt: at };
}

export function markReplySuspect(record, reason = "reply_timeout") {
  return { ...record, state: CONVERSATION_STATES.REPLY_SUSPECT, suspectReason: reason };
}

globalThis.H2W_CONVERSATION_HEALTH = {
  CONVERSATION_STATES,
  createConversationHealth,
  markReplyWaiting,
  markReplyStarted,
  markReplySuspect,
};
