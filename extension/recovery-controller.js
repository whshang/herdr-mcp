// Recovery policy coordinator.
// Decides when to request recovery actions; it does not submit messages or reload tabs.

import { CONVERSATION_STATES, markReplySuspect } from "./conversation-health.js";

export const DEFAULT_RECOVERY_POLICY = Object.freeze({
  replyTimeoutMs: 30000,
  maxRecoveryAttempts: 1,
  maxReloadAttempts: 1,
});

export function shouldSendRecovery(record, policy = DEFAULT_RECOVERY_POLICY, now = Date.now()) {
  if (!record || record.state !== CONVERSATION_STATES.REPLY_SUSPECT) return false;
  if (record.recoveryAttempts >= policy.maxRecoveryAttempts) return false;
  return Boolean(record.lastUserSubmitAt && now - record.lastUserSubmitAt >= policy.replyTimeoutMs);
}

export function markRecoverySent(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.RECOVERY_MESSAGE_SENT,
    recoveryAttempts: (record.recoveryAttempts || 0) + 1,
    recoverySentAt: at,
  };
}

export function markRecovering(record, at = Date.now()) {
  return {
    ...record,
    state: CONVERSATION_STATES.RECOVERING,
    recoveringAt: at,
  };
}

export function classifyReplyTimeout(record, now = Date.now(), timeoutMs = DEFAULT_RECOVERY_POLICY.replyTimeoutMs) {
  if (!record?.lastUserSubmitAt) return record;
  if (now - record.lastUserSubmitAt < timeoutMs) return record;
  return markReplySuspect(record);
}

export function canReloadSafely({
  composerBusy = false,
  streaming = false,
  toolRunning = false,
  deliveryUnknown = false,
  reloadAttempts = 0,
}, policy = DEFAULT_RECOVERY_POLICY) {
  return !composerBusy
    && !streaming
    && !toolRunning
    && !deliveryUnknown
    && reloadAttempts < policy.maxReloadAttempts;
}

globalThis.H2W_RECOVERY_CONTROLLER = {
  DEFAULT_RECOVERY_POLICY,
  shouldSendRecovery,
  markRecoverySent,
  markRecovering,
  classifyReplyTimeout,
  canReloadSafely,
};
