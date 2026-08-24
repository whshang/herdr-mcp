globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.buildHudStateView = function buildHudStateView(input = {}) {
  return {
    workspace: input.workspace || null,
    agent: input.agent || null,
    state: input.state || "unknown",
    recovery: input.recovery || "none",
  };
};
