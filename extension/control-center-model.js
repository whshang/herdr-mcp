export function controlCenterStats(state = {}) {
  const workspaces = Array.isArray(state.workspaces) ? state.workspaces : [];
  const panes = Array.isArray(state.panes) ? state.panes : [];
  const working = panes.filter((pane) => pane.status === "working").length;
  return {
    workspaces: workspaces.length,
    panes: panes.length,
    working,
  };
}

export function runtimePresentation({ runtimeHealthy = false, eventStreamHealthy = null } = {}) {
  if (!runtimeHealthy) return { dot: "offline", text: "Runtime unavailable" };
  if (eventStreamHealthy === false) {
    return { dot: "working", text: "Runtime healthy · event stream reconnecting" };
  }
  return { dot: "healthy", text: "Runtime healthy" };
}

export function formatElapsed(startedAt, nowMs = Date.now()) {
  const started = Date.parse(startedAt || "");
  if (!Number.isFinite(started)) return "—";
  const seconds = Math.max(0, Math.floor((nowMs - started) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

export function formatActivity(at, nowMs = Date.now()) {
  const value = Date.parse(at || "");
  if (!Number.isFinite(value)) return "—";
  const seconds = Math.max(0, Math.floor((nowMs - value) / 1000));
  if (seconds < 10) return "now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}

export function sortWorkspaces(workspaces = []) {
  return [...workspaces].sort((left, right) => {
    const leftWorking = left.panes?.some((pane) => pane.status === "working") ? 0 : 1;
    const rightWorking = right.panes?.some((pane) => pane.status === "working") ? 0 : 1;
    if (leftWorking !== rightWorking) return leftWorking - rightWorking;
    return String(left.label || left.workspace_id).localeCompare(String(right.label || right.workspace_id));
  });
}

export function createRenderCoalescer(render, {
  delayMs = 40,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
  isHidden = () => false,
} = {}) {
  let timer = null;
  let pending = false;
  const flush = () => {
    if (timer !== null) clearTimer(timer);
    timer = null;
    if (!pending || isHidden()) return false;
    pending = false;
    render();
    return true;
  };
  return {
    schedule() {
      pending = true;
      if (isHidden() || timer !== null) return false;
      timer = setTimer(() => {
        timer = null;
        if (isHidden()) return;
        if (!pending) return;
        pending = false;
        render();
      }, delayMs);
      return true;
    },
    flush,
    cancel() {
      pending = false;
      if (timer !== null) clearTimer(timer);
      timer = null;
    },
  };
}
