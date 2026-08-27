import { createBrowserStateStore } from "./browser-state-store.js";
import { createPinnedTarget, revalidatePinnedTarget } from "./target-pin.js";
import { ACTION_TYPES, buildActionDescriptor, classifyAction, phaseAAvailability } from "./control-actions.js";
import { controlCenterStats, createRenderCoalescer, formatActivity, formatElapsed, runtimePresentation, sortWorkspaces } from "./control-center-model.js";
import { boundedTail } from "./browser-state.js";

const TARGET_KEY = "herdrControlPinnedTarget";
const store = createBrowserStateStore();
const expandedWorkspaces = new Set();
let expansionSeeded = false;
let pinnedTarget = null;
let runtimeHealthy = false;
let hasSnapshot = false;
let eventStreamHealthy = null;
let selectedMode = ACTION_TYPES.AGENT_PROMPT;

const $ = (id) => document.getElementById(id);
const runtimeDot = $("runtimeDot");
const runtimeText = $("runtimeText");
const runtimeStats = $("runtimeStats");
const workspaceList = $("workspaceList");
const targetCard = $("targetCard");
const targetTitle = $("targetTitle");
const targetDetails = $("targetDetails");
const unpinButton = $("unpinButton");
const inspectButton = $("inspectButton");
const readTailButton = $("readTailButton");
const composer = $("composer");
const riskBadge = $("riskBadge");
const blockedReason = $("blockedReason");
const sendButton = $("sendButton");
const result = $("result");

function bg(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage(message, (response) => {
      if (chrome.runtime.lastError) resolve({ ok: false, error: chrome.runtime.lastError.message });
      else resolve(response || { ok: false, error: "empty-response" });
    });
  });
}

function statusDotClass(status) {
  if (status === "working") return "working";
  if (status === "done" || status === "idle") return "done";
  if (status === "blocked") return "blocked";
  return "";
}

function paneSummary(pane) {
  if (pane.terminal_title) return pane.terminal_title;
  if (pane.last_output) {
    const lines = String(pane.last_output).trim().split("\n").filter(Boolean);
    return lines.at(-1) || "";
  }
  return pane.agent ? `${pane.agent.name || pane.agent.kind || "agent"} ${pane.status}` : "terminal-only pane";
}

function seedExpansion(state) {
  if (expansionSeeded) return;
  const workspaces = sortWorkspaces(state.workspaces || []);
  if (!workspaces.length) return;
  for (const workspace of workspaces) {
    if (expandedWorkspaces.size < 3 || workspace.panes?.some((pane) => pane.status === "working")) {
      expandedWorkspaces.add(workspace.workspace_id);
    }
  }
  expansionSeeded = true;
}

function currentPane() {
  if (!pinnedTarget?.pane_id) return null;
  return (store.get().panes || []).find((pane) => pane.pane_id === pinnedTarget.pane_id) || null;
}

function reconcileTarget() {
  pinnedTarget = revalidatePinnedTarget(pinnedTarget, store.get().panes || []);
  return pinnedTarget;
}

function renderRuntime(state) {
  const stats = controlCenterStats(state);
  const presentation = runtimePresentation({ runtimeHealthy, eventStreamHealthy });
  runtimeDot.className = `dot ${presentation.dot}`;
  runtimeText.textContent = presentation.text;
  runtimeStats.textContent = `${stats.workspaces} workspaces · ${stats.panes} panes · ${stats.working} working`;
}

function renderWorkspaceTree(state) {
  seedExpansion(state);
  workspaceList.replaceChildren();
  const workspaces = sortWorkspaces(state.workspaces || []);
  if (!workspaces.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = runtimeHealthy ? "No Herdr panes found." : "Local runtime is unavailable.";
    workspaceList.appendChild(empty);
    return;
  }

  const fragment = document.createDocumentFragment();
  for (const workspace of workspaces) {
    const section = document.createElement("section");
    section.className = "workspace";
    section.dataset.workspaceId = workspace.workspace_id;

    const header = document.createElement("button");
    header.type = "button";
    header.className = "workspace-header";
    header.dataset.workspaceToggle = workspace.workspace_id;
    const expanded = expandedWorkspaces.has(workspace.workspace_id);

    const chevron = document.createElement("span");
    chevron.className = "chevron";
    chevron.textContent = expanded ? "▾" : "▸";
    const name = document.createElement("span");
    name.className = "workspace-name";
    name.textContent = workspace.label || workspace.workspace_id;
    const id = document.createElement("span");
    id.className = "workspace-id";
    id.textContent = `(${workspace.workspace_id})`;
    const count = document.createElement("span");
    count.className = "workspace-count";
    const workingCount = (workspace.panes || []).filter((pane) => pane.status === "working").length;
    count.textContent = `${workspace.panes?.length || 0} panes${workingCount ? ` · ${workingCount} working` : ""}`;
    header.append(chevron, name, id, count);
    section.appendChild(header);

    if (expanded) {
      const panes = document.createElement("div");
      panes.className = "panes";
      for (const pane of workspace.panes || []) {
        const row = document.createElement("div");
        row.className = `pane${pinnedTarget?.pane_id === pane.pane_id ? " pinned" : ""}`;
        row.dataset.paneId = pane.pane_id;
        row.title = "Click to pin this pane as the explicit control target";

        const first = document.createElement("div");
        first.className = "pane-line";
        const dot = document.createElement("span");
        dot.className = `dot ${statusDotClass(pane.status)}`;
        const paneId = document.createElement("span");
        paneId.className = "pane-id";
        paneId.textContent = pane.pane_id;
        const agent = document.createElement("span");
        agent.className = "pane-agent";
        agent.textContent = pane.agent?.name || pane.agent?.kind || "terminal";
        const focus = document.createElement("span");
        focus.className = "focus-badge";
        focus.textContent = pane.focused ? "focused" : "";
        const status = document.createElement("span");
        status.className = "status";
        status.textContent = pane.status;
        first.append(dot, paneId, agent, focus, status);

        const meta = document.createElement("div");
        meta.className = "pane-meta";
        const elapsed = pane.agent?.started_at ? formatElapsed(pane.agent.started_at) : "—";
        const activity = formatActivity(pane.agent?.last_activity_at || pane.last_event_at);
        meta.textContent = `${pane.cwd || pane.project_root || "cwd unavailable"} · elapsed ${elapsed} · activity ${activity}`;

        const summary = document.createElement("div");
        summary.className = "pane-summary";
        summary.textContent = paneSummary(pane);
        row.append(first, meta, summary);
        panes.appendChild(row);
      }
      section.appendChild(panes);
    }
    fragment.appendChild(section);
  }
  workspaceList.appendChild(fragment);
}

function renderTarget() {
  const target = reconcileTarget();
  const pane = currentPane();
  const hasTarget = Boolean(target?.pane_id);
  const stale = target?.stale === true;
  targetCard.classList.toggle("stale", stale);
  unpinButton.disabled = !hasTarget;
  inspectButton.disabled = !hasTarget || stale || !pane;
  readTailButton.disabled = !hasTarget || stale || !pane;

  if (!hasTarget) {
    targetTitle.textContent = "No pinned target";
    targetDetails.textContent = "Click a pane above to pin it. Herdr focus changes will not retarget this control.";
    return;
  }
  targetTitle.textContent = `${target.workspace_id || "?"} / ${target.pane_id} / ${target.agent?.name || target.agent?.kind || "terminal"}`;
  targetDetails.textContent = stale
    ? `STALE · ${target.stale_reason || "target changed"} · select the pane again before any control action`
    : `${target.status || "unknown"} · revision ${String(target.target_revision || "local").slice(0, 96)}`;
}

function renderComposerState() {
  const availability = phaseAAvailability(selectedMode, { target: pinnedTarget });
  riskBadge.textContent = classifyAction(selectedMode);
  blockedReason.textContent = availability.reason || "Read-only action available";
  sendButton.textContent = availability.enabled ? "Run" : "Dry run";
  sendButton.disabled = !pinnedTarget?.pane_id;
  document.querySelectorAll("#modeTabs [data-mode]").forEach((button) => {
    button.classList.toggle("active", button.dataset.mode === selectedMode);
  });
}

function renderAll() {
  const state = store.get();
  renderRuntime(state);
  renderTarget();
  renderWorkspaceTree(state);
  renderComposerState();
}

const coalescer = createRenderCoalescer(renderAll, {
  delayMs: 40,
  isHidden: () => document.hidden,
});
store.subscribe(() => coalescer.schedule());

document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    if (!controlPort) connectControlPort(true);
    coalescer.flush();
  }
});

async function refreshSnapshot(force = false) {
  runtimeText.textContent = "Runtime connecting…";
  const response = await bg({ type: "herdr_control_center_subscribe", force });
  if (!response?.ok || !response.state) {
    runtimeHealthy = false;
    result.textContent = response?.error || `Runtime request failed${response?.status ? ` (${response.status})` : ""}`;
    renderAll();
    return false;
  }
  runtimeHealthy = true;
  hasSnapshot = true;
  store.snapshot(response.state);
  renderAll();
  return true;
}

function handleControlMessage(message) {
  if (message?.type === "herdr_control_state" && message.state) {
    runtimeHealthy = true;
    hasSnapshot = true;
    eventStreamHealthy = true;
    store.snapshot(message.state);
  } else if (message?.type === "herdr_control_event" && message.event) {
    eventStreamHealthy = true;
    store.event(message.event);
  } else if (message?.type === "herdr_control_runtime") {
    eventStreamHealthy = message.healthy === true;
    if (message.healthy === true) runtimeHealthy = true;
    else if (!hasSnapshot) runtimeHealthy = false;
    coalescer.schedule();
  }
}

let controlPort = null;
function connectControlPort(reconcile = false) {
  if (controlPort) return;
  try {
    const port = chrome.runtime.connect({ name: "herdr-control-center" });
    controlPort = port;
    port.onMessage.addListener(handleControlMessage);
    port.onDisconnect.addListener(() => {
      if (controlPort !== port) return;
      controlPort = null;
      eventStreamHealthy = false;
      coalescer.schedule();
      setTimeout(() => {
        if (!document.hidden) connectControlPort(true);
      }, 250);
    });
    if (reconcile) void refreshSnapshot(true);
  } catch (_) {
    controlPort = null;
  }
}

workspaceList.addEventListener("click", async (event) => {
  const toggle = event.target.closest?.("[data-workspace-toggle]");
  if (toggle) {
    const workspaceId = toggle.dataset.workspaceToggle;
    if (expandedWorkspaces.has(workspaceId)) expandedWorkspaces.delete(workspaceId);
    else expandedWorkspaces.add(workspaceId);
    renderAll();
    return;
  }
  const paneNode = event.target.closest?.("[data-pane-id]");
  if (!paneNode) return;
  const pane = (store.get().panes || []).find((item) => item.pane_id === paneNode.dataset.paneId);
  if (!pane) return;
  pinnedTarget = createPinnedTarget(pane);
  await chrome.storage.local.set({ [TARGET_KEY]: pinnedTarget });
  result.textContent = `Pinned ${pinnedTarget.workspace_id || "?"} / ${pinnedTarget.pane_id}`;
  renderAll();
});

$("refreshButton").addEventListener("click", () => { void refreshSnapshot(true); });
$("collapseButton").addEventListener("click", () => {
  expandedWorkspaces.clear();
  expansionSeeded = true;
  renderAll();
});
unpinButton.addEventListener("click", async () => {
  pinnedTarget = null;
  await chrome.storage.local.remove(TARGET_KEY);
  result.textContent = "Pinned target cleared.";
  renderAll();
});
inspectButton.addEventListener("click", () => {
  const pane = currentPane();
  if (!pane) return;
  const inspect = { ...pane, last_output: pane.last_output ? boundedTail(pane.last_output, 2048) : null };
  result.textContent = JSON.stringify(inspect, null, 2);
});
readTailButton.addEventListener("click", async () => {
  if (!pinnedTarget?.pane_id || pinnedTarget.stale) return;
  readTailButton.disabled = true;
  result.textContent = "Reading bounded pane tail…";
  const response = await bg({
    type: "herdr_control_read_tail",
    pane_id: pinnedTarget.pane_id,
    lines: 40,
    max_chars: 4096,
  });
  result.textContent = response?.ok ? boundedTail(response.tail || "", 4096) || "(empty tail)" : `Read failed: ${response?.error || "unknown"}`;
  renderTarget();
});

document.querySelectorAll("#modeTabs [data-mode]").forEach((button) => {
  button.addEventListener("click", () => {
    selectedMode = button.dataset.mode;
    renderComposerState();
  });
});
sendButton.addEventListener("click", () => {
  if (!pinnedTarget?.pane_id) return;
  const text = composer.value.trim();
  const descriptor = buildActionDescriptor(selectedMode, {
    target: pinnedTarget,
    text: selectedMode === ACTION_TYPES.HERDR_METHOD ? "" : text,
    method: selectedMode === ACTION_TYPES.HERDR_METHOD ? text : null,
  });
  result.textContent = JSON.stringify(descriptor, null, 2);
});

async function start() {
  const stored = await chrome.storage.local.get(TARGET_KEY);
  pinnedTarget = stored[TARGET_KEY] || null;
  connectControlPort(false);
  renderAll();
  await refreshSnapshot();
}

void start();
