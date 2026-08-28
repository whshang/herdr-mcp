import { createBrowserStateStore } from "./browser-state-store.js";
import { createPinnedTarget, revalidatePinnedTarget } from "./target-pin.js";
import { ACTION_TYPES, ACTION_RISK, buildActionDescriptor, classifyAction } from "./control-actions.js";
import { controlCenterStats, createRenderCoalescer, formatElapsed, runtimePresentation, sortWorkspaces } from "./control-center-model.js";
import { boundedTail } from "./browser-state.js";
import { detectOrLoadLocale, getLocale, t } from "./i18n.js";

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
const modeHelp = $("modeHelp");
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

function isNativeHostError(error) {
  return /native messaging host|native host|specified native messaging host|forbidden/i.test(String(error || ""));
}

function runtimeErrorPresentation(response) {
  const raw = String(response?.error || "").trim();
  if (isNativeHostError(raw)) return { message: t("native_host_help"), detail: raw };
  return {
    message: raw || t("cc_runtime_request_failed", {
      status: response?.status ? ` (${response.status})` : "",
    }),
    detail: raw,
  };
}

function statusDotClass(status) {
  if (status === "working") return "working";
  if (status === "done" || status === "idle") return "done";
  if (status === "blocked") return "blocked";
  return "";
}

function statusLabel(status) {
  if (status === "working") return t("cc_status_working");
  if (status === "done") return t("cc_status_done");
  if (status === "idle") return t("cc_status_idle");
  if (status === "blocked") return t("cc_status_blocked");
  if (status === "terminal-only") return t("cc_status_terminal");
  return t("cc_status_unknown");
}

function activityLabel(at, nowMs = Date.now()) {
  const value = Date.parse(at || "");
  if (!Number.isFinite(value)) return "—";
  const seconds = Math.max(0, Math.floor((nowMs - value) / 1000));
  if (seconds < 10) return t("cc_time_now");
  if (seconds < 60) return t("cc_time_seconds_ago", { value: seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("cc_time_minutes_ago", { value: minutes });
  return t("cc_time_hours_ago", { value: Math.floor(minutes / 60) });
}

function paneSummary(pane) {
  if (pane.terminal_title) return pane.terminal_title;
  if (pane.last_output) {
    const lines = String(pane.last_output).trim().split("\n").filter(Boolean);
    return lines.at(-1) || "";
  }
  return pane.agent
    ? `${pane.agent.name || pane.agent.kind || "agent"} · ${statusLabel(pane.status)}`
    : t("cc_status_terminal");
}

function applyStaticI18n() {
  const locale = getLocale();
  document.documentElement.lang = locale === "zh" ? "zh-CN" : locale;
  document.title = t("cc_title");
  document.querySelectorAll("[data-i18n]").forEach((element) => {
    const key = element.getAttribute("data-i18n");
    if (key) element.textContent = t(key);
  });
  document.querySelectorAll("[data-i18n-aria]").forEach((element) => {
    const key = element.getAttribute("data-i18n-aria");
    if (key) element.setAttribute("aria-label", t(key));
  });
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
  runtimeText.textContent = !runtimeHealthy
    ? t("cc_runtime_unavailable")
    : eventStreamHealthy === false
      ? t("cc_runtime_reconnecting")
      : t("cc_runtime_healthy");
  runtimeStats.textContent = t("cc_stats", stats);
}

function renderWorkspaceTree(state) {
  seedExpansion(state);
  workspaceList.replaceChildren();
  const workspaces = sortWorkspaces(state.workspaces || []);
  if (!workspaces.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = runtimeHealthy ? t("cc_no_panes") : t("cc_runtime_unavailable");
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
    count.textContent = t("cc_workspace_count", {
      panes: workspace.panes?.length || 0,
      working: workingCount,
    });
    header.append(chevron, name, id, count);
    section.appendChild(header);

    if (expanded) {
      const panes = document.createElement("div");
      panes.className = "panes";
      for (const pane of workspace.panes || []) {
        const row = document.createElement("div");
        row.className = `pane${pinnedTarget?.pane_id === pane.pane_id ? " pinned" : ""}`;
        row.dataset.paneId = pane.pane_id;
        row.title = t("cc_pin_hint");

        const first = document.createElement("div");
        first.className = "pane-line";
        const dot = document.createElement("span");
        dot.className = `dot ${statusDotClass(pane.status)}`;
        const paneId = document.createElement("span");
        paneId.className = "pane-id";
        paneId.textContent = pane.pane_id;
        const agent = document.createElement("span");
        agent.className = "pane-agent";
        agent.textContent = pane.agent?.name || pane.agent?.kind || t("cc_terminal");
        const focus = document.createElement("span");
        focus.className = "focus-badge";
        focus.textContent = pane.focused ? t("cc_focused") : "";
        const status = document.createElement("span");
        status.className = "status";
        status.textContent = statusLabel(pane.status);
        first.append(dot, paneId, agent, focus, status);

        const meta = document.createElement("div");
        meta.className = "pane-meta";
        const elapsed = pane.agent?.started_at ? formatElapsed(pane.agent.started_at) : "—";
        const activity = activityLabel(pane.agent?.last_activity_at || pane.last_event_at);
        meta.textContent = t("cc_meta", {
          cwd: pane.cwd || pane.project_root || "—",
          elapsed,
          activity,
        });

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

function staleReasonLabel(reason) {
  if (reason === "pane_removed") return t("cc_stale_removed");
  if (reason === "target_revision_changed") return t("cc_stale_replaced");
  return reason || t("cc_status_unknown");
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
    targetTitle.textContent = t("cc_no_target");
    targetDetails.textContent = t("cc_target_help");
    return;
  }
  targetTitle.textContent = `${target.workspace_id || "?"} / ${target.pane_id} / ${target.agent?.name || target.agent?.kind || t("cc_terminal")}`;
  targetDetails.textContent = stale
    ? t("cc_target_stale", { reason: staleReasonLabel(target.stale_reason) })
    : t("cc_target_current", {
      status: statusLabel(target.status),
      revision: String(target.target_revision || "local").slice(0, 96),
    });
}

function modePresentation(mode) {
  if (mode === ACTION_TYPES.STEER) {
    return { help: "cc_mode_steer_help", placeholder: "cc_placeholder_steer" };
  }
  if (mode === ACTION_TYPES.HERDR_METHOD) {
    return { help: "cc_mode_herdr_help", placeholder: "cc_placeholder_herdr" };
  }
  if (mode === ACTION_TYPES.TERMINAL_TEXT) {
    return { help: "cc_mode_terminal_help", placeholder: "cc_placeholder_terminal" };
  }
  return { help: "cc_mode_prompt_help", placeholder: "cc_placeholder_prompt" };
}

function riskLabel(mode) {
  const risk = classifyAction(mode);
  if (risk === ACTION_RISK.PROVIDER_STEER) return t("cc_risk_provider");
  if (risk === ACTION_RISK.TERMINAL_MUTATION) return t("cc_risk_terminal");
  if (risk === ACTION_RISK.UNKNOWN) return t("cc_risk_herdr");
  return t("cc_risk_write");
}

function renderComposerState() {
  const mode = modePresentation(selectedMode);
  riskBadge.textContent = riskLabel(selectedMode);
  modeHelp.textContent = t(mode.help);
  composer.placeholder = t(mode.placeholder);
  if (!pinnedTarget?.pane_id) blockedReason.textContent = t("cc_pin_first");
  else if (pinnedTarget.stale) blockedReason.textContent = t("cc_target_stale_short");
  else blockedReason.textContent = t("cc_preview_only_reason");
  sendButton.textContent = t("cc_preview_action");
  sendButton.disabled = !pinnedTarget?.pane_id || pinnedTarget?.stale === true;
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
  runtimeText.textContent = t("cc_runtime_connecting");
  const response = await bg({ type: "herdr_control_center_subscribe", force });
  if (!response?.ok || !response.state) {
    runtimeHealthy = false;
    const failure = runtimeErrorPresentation(response);
    result.textContent = failure.message;
    result.title = failure.detail || "";
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
  result.textContent = t("cc_pinned", {
    workspace: pinnedTarget.workspace_id || "?",
    pane: pinnedTarget.pane_id,
  });
  renderAll();
});

$("refreshButton").addEventListener("click", () => { void refreshSnapshot(true); });
$("collapseButton").addEventListener("click", () => {
  expandedWorkspaces.clear();
  expansionSeeded = true;
  renderAll();
});
$("settingsButton").addEventListener("click", () => chrome.runtime.openOptionsPage());
unpinButton.addEventListener("click", async () => {
  pinnedTarget = null;
  await chrome.storage.local.remove(TARGET_KEY);
  result.textContent = t("cc_target_cleared");
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
  result.textContent = t("cc_reading_tail");
  const response = await bg({
    type: "herdr_control_read_tail",
    pane_id: pinnedTarget.pane_id,
    lines: 40,
    max_chars: 4096,
  });
  result.textContent = response?.ok
    ? boundedTail(response.tail || "", 4096) || t("cc_empty_tail")
    : t("cc_read_failed", { error: response?.error || "unknown" });
  renderTarget();
});

document.querySelectorAll("#modeTabs [data-mode]").forEach((button) => {
  button.addEventListener("click", () => {
    selectedMode = button.dataset.mode;
    renderComposerState();
  });
});
sendButton.addEventListener("click", () => {
  if (!pinnedTarget?.pane_id || pinnedTarget.stale) return;
  const text = composer.value.trim();
  const descriptor = buildActionDescriptor(selectedMode, {
    target: pinnedTarget,
    text: selectedMode === ACTION_TYPES.HERDR_METHOD ? "" : text,
    method: selectedMode === ACTION_TYPES.HERDR_METHOD ? text : null,
  });
  result.textContent = JSON.stringify(descriptor, null, 2);
});

async function start() {
  await detectOrLoadLocale();
  applyStaticI18n();
  document.documentElement.classList.remove("i18n-pending");
  const stored = await chrome.storage.local.get(TARGET_KEY);
  pinnedTarget = stored[TARGET_KEY] || null;
  connectControlPort(false);
  renderAll();
  await refreshSnapshot();
}

void start();
