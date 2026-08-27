export const ACTION_TYPES = Object.freeze({
  INSPECT: "inspect",
  READ_TAIL: "read_tail",
  AGENT_PROMPT: "agent_prompt",
  STEER: "steer",
  HERDR_METHOD: "herdr_method",
  TERMINAL_TEXT: "terminal_text",
  TERMINAL_INPUT: "terminal_input",
  TERMINAL_KEYS: "terminal_keys",
  INTERRUPT: "interrupt",
});

export const ACTION_RISK = Object.freeze({
  READ: "READ",
  MUTATION: "MUTATION",
  TERMINAL_MUTATION: "TERMINAL_MUTATION",
  PROVIDER_STEER: "PROVIDER_STEER",
  UNKNOWN: "UNKNOWN",
});

const RISKS = Object.freeze({
  [ACTION_TYPES.INSPECT]: ACTION_RISK.READ,
  [ACTION_TYPES.READ_TAIL]: ACTION_RISK.READ,
  [ACTION_TYPES.AGENT_PROMPT]: ACTION_RISK.MUTATION,
  [ACTION_TYPES.STEER]: ACTION_RISK.PROVIDER_STEER,
  [ACTION_TYPES.HERDR_METHOD]: ACTION_RISK.UNKNOWN,
  [ACTION_TYPES.TERMINAL_TEXT]: ACTION_RISK.TERMINAL_MUTATION,
  [ACTION_TYPES.TERMINAL_INPUT]: ACTION_RISK.TERMINAL_MUTATION,
  [ACTION_TYPES.TERMINAL_KEYS]: ACTION_RISK.TERMINAL_MUTATION,
  [ACTION_TYPES.INTERRUPT]: ACTION_RISK.MUTATION,
});

const PHASE_A_REASONS = Object.freeze({
  [ACTION_TYPES.AGENT_PROMPT]: "Waiting for Rust control plane reliability",
  [ACTION_TYPES.STEER]: "Provider steer unsupported in Phase A",
  [ACTION_TYPES.HERDR_METHOD]: "Herdr mutation methods are dry-run only in Phase A",
  [ACTION_TYPES.TERMINAL_TEXT]: "Terminal mutation is disabled in Phase A",
  [ACTION_TYPES.TERMINAL_INPUT]: "Terminal mutation is disabled in Phase A",
  [ACTION_TYPES.TERMINAL_KEYS]: "Terminal mutation is disabled in Phase A",
  [ACTION_TYPES.INTERRUPT]: "Reliability required before interrupt is enabled",
});

export function classifyAction(type) {
  return RISKS[type] || ACTION_RISK.UNKNOWN;
}

export function phaseAAvailability(type, context = {}) {
  const risk = classifyAction(type);
  if (risk === ACTION_RISK.READ) {
    if (!context.target && type === ACTION_TYPES.READ_TAIL) {
      return { enabled: false, mode: "read", reason: "Pin a pane first" };
    }
    if (context.target?.stale) {
      return { enabled: false, mode: "read", reason: "Target stale" };
    }
    return { enabled: true, mode: "read", reason: null };
  }
  return {
    enabled: false,
    mode: "dry_run",
    reason: PHASE_A_REASONS[type] || "Unsupported action",
  };
}

export function buildActionDescriptor(type, { target = null, text = "", method = null, args = null } = {}) {
  const risk = classifyAction(type);
  const availability = phaseAAvailability(type, { target });
  return {
    phase: "A",
    action: type,
    risk,
    target: target ? {
      workspace_id: target.workspace_id || null,
      pane_id: target.pane_id || null,
      target_revision: target.target_revision || null,
      stale: target.stale === true,
    } : null,
    args: {
      ...(text ? { text: String(text) } : {}),
      ...(method ? { method: String(method) } : {}),
      ...(args && typeof args === "object" ? args : {}),
    },
    executable: availability.enabled,
    execution_mode: availability.mode,
    blocked_reason: availability.reason,
  };
}
