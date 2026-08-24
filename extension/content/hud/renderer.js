globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderReadonlyHud = function renderReadonlyHud(element, state) {
  if (!element) return;
  const current = String(state.state || "unknown");
  const label = current === "healthy" ? "ready"
    : current === "reply_waiting" ? "working"
      : current === "reply_suspect" ? "recovering"
        : current;
  element.textContent = `Herdr ● ${label}`;
  element.title = globalThis.H2W_HUD.renderHudTooltip(state);
};
