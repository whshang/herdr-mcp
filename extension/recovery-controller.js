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
  backgroundReloadCooldownMs: 180000,
  maxRecoveryAttempts: 1,
  maxReloadAttempts: 1,
  maxBackgroundReloadAttempts: 1,
  probeCorrelationWindowMs: 60000,
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
  const serverCurrentAssistant = String(server.currentNodeRole || "") === "assistant";

  // ChatGPT can leave an empty/missing assistant DOM after the server has
  // already accepted and rendered the assistant turn. Treat that as a stale
  // local view only when the server confirms the current node is assistant;
  // otherwise the latest server assistant may simply belong to an older turn.
  if (serverCurrentAssistant && serverText) {
    const pageAssistantMissing = !pageId && !pageText;
    const pageAssistantEmpty = pageId === serverId && !pageText;
    if (pageAssistantMissing || pageAssistantEmpty) {
      return { state: "server_ahead", deltaMs, serverLatestAt, pageLatestAt };
    }
  }

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

// Recovery probe authorization.
//
// A recovery probe is a NEW user turn in the live conversation, so timing alone
// must never authorize one: a long-thinking model, a pending tool call, and a
// dropped turn all look alike to a timer, and the current turn's provider node
// can still be the user message while the model reasons. The single authorizing
// fact is a provider-closed current turn that produced no assistant output
// anywhere. Everything else - busy page, unreadable snapshot, still-open turn, a
// provider tip that is not the current turn's assistant - fails closed. Explicit
// provider error surfaces (ChatGPT send timeout / disconnected / thread error
// cards) keep their own bounded recovery paths and do not depend on this gate.
function recoveryProbeAllowed(evidence, since = 0) {
  if (!evidence) return null;
  const { dom = {}, server = {} } = evidence;
  if (dom.streaming || dom.toolRunning || dom.composerBusy || dom.permissionCardActive) return null;
  if (server?.ok !== true || server.finished !== true) return null;
  // The snapshot walks back from the provider tip, so a terminal assistant
  // message is only this turn's outcome when the assistant node IS the tip.
  if (String(server.currentNodeRole || "") !== "assistant" || !String(server.messageId || "")) return null;
  // It must also belong to the turn this record waits for. Without that link a
  // failed or uncertain Send leaves the previous terminal turn as the tip, and
  // its emptiness would authorize a second user turn.
  const submittedAt = Number(since || 0);
  const userAt = Number(server.userCreatedAt || 0);
  if (!submittedAt || !userAt || !String(server.userMessageId || "")
    || Math.abs(userAt - submittedAt) > DEFAULT_RECOVERY_POLICY.probeCorrelationWindowMs) return null;
  if (String(dom.assistantText || "").trim() || String(server.text || "").trim()) return null;
  return "provider_settled_without_output";
}

function shouldSendRecovery(record, policy = DEFAULT_RECOVERY_POLICY, now = Date.now(), evidence = null) {
  if (!record || record.state !== RECOVERY_STATES.REPLY_SUSPECT) return false;
  if ((record.recovery_attempt || 0) >= policy.maxRecoveryAttempts) return false;
  // Re-checked at send time: a suspect state persisted by an earlier build must
  // not authorize a probe against fresher live evidence.
  if (!recoveryProbeAllowed(evidence, record.last_user_submit_at)) return false;
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

function classifyReplyTimeout(record, now = Date.now(), timeoutMs = DEFAULT_RECOVERY_POLICY.replyTimeoutMs, evidence = null) {
  if (!record) return record;
  const allowed = recoveryProbeAllowed(evidence, record.last_user_submit_at);
  // Keep the suspect label only while the proof still holds, so the HUD stops
  // reporting a reply timeout as soon as the turn is proven live again.
  if (record.state === RECOVERY_STATES.REPLY_SUSPECT) {
    return allowed ? record : { ...record, state: RECOVERY_STATES.REPLY_WAITING, suspect_reason: null };
  }
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
  if (!recoveryProbeAllowed(evidence, startedAt)) return record;
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

function canForceTabReloadSafely({
  composerBusy = false,
  composerHasHumanText = false,
  streaming = false,
  toolRunning = false,
  permissionCardActive = false,
  deliveryUnknown = false,
  mutationDeliveryUncertain = false,
  backgroundReloadAttempts = 0,
  lastBackgroundReloadAt = null,
  now = Date.now(),
}, policy = DEFAULT_RECOVERY_POLICY) {
  return !composerBusy
    && !composerHasHumanText
    && !streaming
    && !toolRunning
    && !permissionCardActive
    && !deliveryUnknown
    && !mutationDeliveryUncertain
    && (!lastBackgroundReloadAt || now - lastBackgroundReloadAt >= policy.backgroundReloadCooldownMs)
    && backgroundReloadAttempts < policy.maxBackgroundReloadAttempts;
}

async function runAfterDurablePersistence({
  persist,
  action,
  waitMs = 0,
  waitFn = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
} = {}) {
  if (typeof persist !== "function" || typeof action !== "function") return false;
  let persisted = false;
  try {
    persisted = (await persist()) === true;
  } catch (_) {
    persisted = false;
  }
  if (!persisted) return false;
  if (waitMs > 0) {
    try {
      await waitFn(waitMs);
    } catch (_) {
      return false;
    }
  }
  try {
    action();
    return true;
  } catch (_) {
    return false;
  }
}

globalThis.H2W_RECOVERY_CONTROLLER = {
  DEFAULT_RECOVERY_POLICY,
  recoveryProbeAllowed,
  shouldSendRecovery,
  markRecoverySent,
  markRecovering,
  markReloaded,
  recommendRollover,
  canRolloverSafely,
  classifyReplyTimeout,
  canReloadSafely,
  canForceTabReloadSafely,
  classifyViewFreshness,
  runAfterDurablePersistence,
};
