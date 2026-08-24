globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.buildHudStateView = function buildHudStateView(input = {}) {
  return {
    workspace: input.workspace || null,
    agent: input.agent || null,
    conversation: input.conversation || null,
    state: input.state || "unknown",
    recovery: input.recovery || "none",
    lastEvent: input.lastEvent || null,
    labels: input.labels || {},
  };
};
