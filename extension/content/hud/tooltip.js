globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderHudTooltip = function renderHudTooltip(state = {}) {
  const labels = state.labels || {};
  const stateKey = state.state === "healthy" ? "ready"
    : state.state === "reply_waiting" ? "working"
      : state.state === "reply_suspect" ? "recovering"
        : state.state;
  const stateLabel = labels.states?.[stateKey] || stateKey || labels.states?.unknown || "unknown";
  const recoveryLabel = state.recovery === "none"
    ? (labels.none || "none")
    : (labels.states?.[state.recovery] || state.recovery);
  return [
    state.workspace ? `${labels.tip_workspace || "Workspace"}: ${state.workspace}` : null,
    state.agent ? `${labels.tip_agent || "Agent"}: ${state.agent}` : null,
    state.conversation ? `${labels.tip_conversation || "Conversation"}: ${state.conversation}` : null,
    `${labels.tip_state || "State"}: ${stateLabel}`,
    `${labels.tip_recovery || "Recovery"}: ${recoveryLabel}`,
    state.lastEvent ? `${labels.tip_last_event || "Last event"}: ${state.lastEvent}` : null,
  ].filter(Boolean).join("\n");
};
