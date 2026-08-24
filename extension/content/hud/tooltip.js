globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderHudTooltip = function renderHudTooltip(state = {}) {
  return [
    state.workspace ? `Workspace: ${state.workspace}` : null,
    state.agent ? `Agent: ${state.agent}` : null,
    state.conversation ? `Conversation: ${state.conversation}` : null,
    `State: ${state.state || "unknown"}`,
    `Recovery: ${state.recovery || "none"}`,
    state.lastEvent ? `Last event: ${state.lastEvent}` : null,
  ].filter(Boolean).join("\n");
};
