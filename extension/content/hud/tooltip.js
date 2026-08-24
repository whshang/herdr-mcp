globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderHudTooltip = function renderHudTooltip(state = {}) {
  return [
    state.workspace ? `Workspace: ${state.workspace}` : null,
    state.agent ? `Agent: ${state.agent}` : null,
    `State: ${state.state || "unknown"}`,
    `Recovery: ${state.recovery || "none"}`,
  ].filter(Boolean).join("\n");
};
