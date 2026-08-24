// Recovery policy coordinator.
// Decides when to request recovery actions; it does not submit messages or reload tabs.

const HEALTH = globalThis.H2W_CONVERSATION_HEALTH || {};
const RECOVERY_STATES = HEALTH.CONVERSATION_STATES || {};
const recoveryMarkReplySuspect = HEALTH.markReplySuspect || ((record) => record);

const DEFAULT_RECOVERY_POLICY = Object.freeze({
  replyTimeoutMs: 30000,
  assistantStallMs: 30000,
  serverStallMs: 60000,
  freshnessProbeIntervalMs: 15000,
  reloadCooldownMs: 60000,
  maxRecoveryAttempts: 1,
  maxReloadAttempts: 1,
});

function classifyViewFreshness({ dom = {}, server = {}, now = Date.now() } = {}, policy = DEFAULT_RECOVERY_POLICY) {
  if (!server?.ok || !server?.messageId) return { state: "unknown", deltaMs: null };
  const serverLatestAt = Number(server.updatedAt || server.createdAt || 0) || null;
  const pageLatestAt = Number(dom.messageAt || dom.changedAt || 0) || null;
  const deltaMs = serverLatestAt && pageLatestAt ? serverLatestAt - pageLatestAt : null;
  const serverId = String(server.messageId || "");
  const pageId = String(dom.messageId || "");
  const serverText = String(server.text || "").replace(/\s+/g, " ").trim();
  const pageText = String(dom.text || "").replace(/\s+/g, " ").trim();

  if (serverId && pageId && serverId !== pageId) {
    return { state: "server_ahead", deltaMs, serverLatestAt, pageLatestAt };
  }
  if (serverText && pageText && serverText.length > pageText.length + 24
    && serverText.slice(0, Math.min(pageText.length, 160)) === pageText.slice(0, 160)) {
    return { state: "server_ahead", deltaMs, serverLatestAt, pageLatestAt };
  }
  const lastActivityAt = Math.max(Number(serverLatestAt || 0), Number(dom.changedAt || 0));
  if (server.finished === false && lastActivityAt && now - lastActivityAt >= policy.serverStallMs) {
    return { state: "server_stalled", deltaMs, serverLatestAt, pageLatestAt };
  }
  return { state: "synced", deltaMs, serverLatestAt, pageLatestAt };
}

function shouldSendRecovery(record, policy = DEFAULT_RECOVERY_POLICY, now = Date.now()) {
  if (!record || record.state !== RECOVERY_STATES.REPLY_SUSPECT) return false;
  if ((record.recovery_attempt || 0) >= policy.maxRecoveryAttempts) return false;
  return Boolean(record.last_user_submit_at && now - record.last_user_submit_at >= policy.replyTimeoutMs);
}

function markRecoverySent(record, at = Date.now()) {
  return {
    ...record,
    state: RECOVERY_STATES.RECOVERY_MESSAGE_SENT,
    recovery_attempt: (record.recovery_attempt || 0) + 1,
    recovery_sent_at: at,
  };
}

function markRecovering(record, at = Date.now()) {
  return {
    ...record,
    state: RECOVERY_STATES.RECOVERING,
    recovering_at: at,
  };
}

function markReloaded(record, at = Date.now()) {
  return {
    ...record,
    state: RECOVERY_STATES.RECOVERING,
    reload_attempt: (record.reload_attempt || 0) + 1,
    last_reload_at: at,
  };
}

function recommendRollover(record, policy = DEFAULT_RECOVERY_POLICY) {
  return Boolean(record
    && (record.recovery_attempt || 0) >= policy.maxRecoveryAttempts
    && (record.reload_attempt || 0) >= policy.maxReloadAttempts
    && record.state !== RECOVERY_STATES.HEALTHY);
}

function canRolloverSafely(record, {
  composerBusy = false,
  composerHasHumanText = false,
  streaming = false,
  toolRunning = false,
  permissionCardActive = false,
  tabReachable = true,
} = {}, policy = DEFAULT_RECOVERY_POLICY) {
  if (!recommendRollover(record, policy)) return false;
  if (composerBusy || composerHasHumanText || streaming || toolRunning || permissionCardActive) return false;
  if (!tabReachable) return false;
  const lastReloadAt = Number(record?.last_reload_at || 0);
  const lastProgressAt = Number(record?.last_assistant_progress_at || 0);
  if (lastReloadAt && lastProgressAt > lastReloadAt) return false;
  return true;
}

function classifyReplyTimeout(record, now = Date.now(), timeoutMs = DEFAULT_RECOVERY_POLICY.replyTimeoutMs) {
  if (!record) return record;
  let startedAt = null;
  let reason = "reply_timeout";
  if (record.state === RECOVERY_STATES.REPLY_WAITING) {
    startedAt = record.last_user_submit_at;
  } else if (record.state === RECOVERY_STATES.RECOVERY_MESSAGE_SENT) {
    startedAt = record.recovery_sent_at;
    reason = "recovery_timeout";
  } else {
    return record;
  }
  if (!startedAt || now - startedAt < timeoutMs) return record;
  return recoveryMarkReplySuspect(record, reason);
}

function canReloadSafely({
  composerBusy = false,
  composerHasHumanText = false,
  streaming = false,
  toolRunning = false,
  permissionCardActive = false,
  deliveryUnknown = false,
  mutationDeliveryUncertain = false,
  reloadAttempts = 0,
  lastReloadAt = null,
  now = Date.now(),
}, policy = DEFAULT_RECOVERY_POLICY) {
  return !composerBusy
    && !composerHasHumanText
    && !streaming
    && !toolRunning
    && !permissionCardActive
    && !deliveryUnknown
    && !mutationDeliveryUncertain
    && (!lastReloadAt || now - lastReloadAt >= policy.reloadCooldownMs)
    && reloadAttempts < policy.maxReloadAttempts;
}

globalThis.H2W_RECOVERY_CONTROLLER = {
  DEFAULT_RECOVERY_POLICY,
  shouldSendRecovery,
  markRecoverySent,
  markRecovering,
  markReloaded,
  recommendRollover,
  canRolloverSafely,
  classifyReplyTimeout,
  canReloadSafely,
  classifyViewFreshness,
};
