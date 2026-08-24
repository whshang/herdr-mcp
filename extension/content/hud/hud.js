globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.updateReadonlyHud = function updateReadonlyHud(element, input) {
  globalThis.H2W_HUD.renderReadonlyHud(
    element,
    globalThis.H2W_HUD.buildHudStateView(input),
  );
};
