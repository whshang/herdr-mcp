globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderReadonlyHud = function renderReadonlyHud(element, state) {
  if (!element) return;
  element.textContent = state.state || "unknown";
  element.title = globalThis.H2W_HUD.renderHudTooltip(state);
};
