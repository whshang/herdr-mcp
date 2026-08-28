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
    `${labels.tip_state || "State"}: ${stateLabel}`,
    `${labels.tip_recovery || "Recovery"}: ${recoveryLabel}`,
    state.lastEvent ? `${labels.tip_last_event || "Last event"}: ${state.lastEvent}` : null,
  ].filter(Boolean).join("\n");
};
