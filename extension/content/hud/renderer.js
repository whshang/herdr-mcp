globalThis.H2W_HUD = globalThis.H2W_HUD || {};
globalThis.H2W_HUD.renderReadonlyHud = function renderReadonlyHud(element, state) {
  if (!element) return;
  const current = String(state.state || "unknown");
  const stateKey = current === "healthy" ? "ready"
    : current === "reply_waiting" ? "working"
      : current === "reply_suspect" ? "recovering"
        : current;
  const label = state.labels?.states?.[stateKey] || stateKey;
  const tasks = state.tasks || {};
  const taskParts = [];
  const running = Math.max(0, Number(tasks.running) || 0);
  const completed = Math.max(0, Number(tasks.completed) || 0);
  const blocked = Math.max(0, Number(tasks.blocked) || 0);
  const failed = Math.max(0, Number(tasks.failed) || 0);
  if (running) taskParts.push(`${running} ${state.labels?.states?.working || "working"}`);
  if (completed) taskParts.push(`${completed} ${state.labels?.states?.done || "done"}`);
  if (blocked) taskParts.push(`${blocked} ${state.labels?.states?.blocked || "blocked"}`);
  if (failed) taskParts.push(`${failed} failed`);
  element.textContent = `Herdr ● ${label}${taskParts.length ? ` · ${taskParts.join(" · ")}` : ""}`;
  element.title = globalThis.H2W_HUD.renderHudTooltip(state);
};
