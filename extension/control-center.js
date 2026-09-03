import { createBrowserStateStore } from "./browser-state-store.js";
import { createPinnedTarget, revalidatePinnedTarget } from "./target-pin.js";
import { ACTION_TYPES, ACTION_RISK, actionModesForTarget, buildActionDescriptor, classifyAction } from "./control-actions.js";
import {
  controlCenterStats,
  createRenderCoalescer,
  formatElapsed,
  runtimePresentation,
  sortWorkspaces,
  workspaceAggregateStatus,
  workspaceRowsForPage,
} from "./control-center-model.js";
import { boundedTail } from "./browser-state.js";
import { detectOrLoadLocale, getLocale, t } from "./i18n.js";

const TARGET_KEY = "herdrControlPinnedTarget";
const EXPANDED_WORKSPACES_KEY = "herdrControlExpandedWorkspaces";
const DEVICE_PANEL_COLLAPSED_KEY = "herdrControlDevicePanelCollapsed";
const store = createBrowserStateStore();
const expandedWorkspaces = new Set();
let expansionSeeded = false;
let pinnedTarget = null;
let runtimeHealthy = false;
let hasSnapshot = false;
let eventStreamHealthy = null;
let selectedMode = null;
let pageContext = { loading: true, tabId: null, windowId: null, response: null, error: null };
let pageContextRefreshSeq = 0;
let fleetContext = { loading: true, response: null, error: null, updatedAt: 0 };
let devicePanelCollapsed = false;
let bindingMutationWorkspaceId = null;
let actionInFlight = false;

const $ = (id) => document.getElementById(id);
const runtimeDot = $("runtimeDot");
const runtimeText = $("runtimeText");
const runtimeStats = $("runtimeStats");
const deviceSummary = $("deviceSummary");
const deviceToggleButton = $("deviceToggleButton");
const deviceChevron = $("deviceChevron");
const devicePanelBody = $("devicePanelBody");
const deviceList = $("deviceList");
const deviceHelp = $("deviceHelp");
const workspaceList = $("workspaceList");
const controlDock = $("controlDock");
const pageContextCard = $("pageContextCard");
const pageContextTitle = $("pageContextTitle");
const pageContextMeta = $("pageContextMeta");
const pageContextHelp = $("pageContextHelp");
const targetCard = $("targetCard");
const targetTitle = $("targetTitle");
const targetKindBadge = $("targetKindBadge");
const targetDetails = $("targetDetails");
const unpinButton = $("unpinButton");
const inspectButton = $("inspectButton");
const readTailButton = $("readTailButton");
const agentQuickActions = $("agentQuickActions");
const interruptButton = $("interruptButton");
const composer = $("composer");
const composerFooter = $("composerFooter");
const riskBadge = $("riskBadge");
const blockedReason = $("blockedReason");
const actionHeading = $("actionHeading");
const modeTabs = $("modeTabs");
const modeHelp = $("modeHelp");
const sendButton = $("sendButton");
const actionModeBadge = $("actionModeBadge");
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

function fleetStatusDot(device) {
  if (device?.authorization === "revoked" || device?.connection === "offline") return "offline";
  if (device?.authorization === "suspended" || device?.connection === "stale") return "stale";
  if (device?.connection === "online" && device?.health === "ok") return "healthy";
  return "";
}

function deviceAuthorizationLabel(value) {
  const key = `cc_device_authorization_${String(value || "unknown")}`;
  const label = t(key);
  return label === key ? String(value || t("cc_status_unknown")) : label;
}

function deviceConnectionLabel(value) {
  const key = `cc_device_connection_${String(value || "offline")}`;
  const label = t(key);
  return label === key ? String(value || t("cc_status_unknown")) : label;
}

function deviceLastSeenLabel(value) {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms < 0) return t("cc_device_last_seen_unknown");
  if (ms < 10_000) return t("cc_time_now");
  if (ms < 60_000) return t("cc_time_seconds_ago", { value: Math.floor(ms / 1000) });
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return t("cc_time_minutes_ago", { value: minutes });
  return t("cc_time_hours_ago", { value: Math.floor(minutes / 60) });
}

function fleetFailureText(response) {
  const code = String(response?.code || "");
  if (code === "device_inventory_admin_required") return t("cc_devices_owner_required");
  if (response?.http_status === 404 || code === "not_found" || code === "device_inventory_platform_unsupported") {
    return t("cc_devices_runtime_unavailable");
  }
  return t("cc_devices_unavailable", { error: response?.error || code || "unknown" });
}

function renderDevicePanelCollapse() {
  devicePanelBody.hidden = devicePanelCollapsed;
  deviceToggleButton.setAttribute("aria-expanded", String(!devicePanelCollapsed));
  deviceToggleButton.title = t(devicePanelCollapsed ? "cc_devices_expand" : "cc_devices_collapse");
  deviceChevron.textContent = devicePanelCollapsed ? "›" : "⌄";
}

async function persistDevicePanelCollapse() {
  try {
    await chrome.storage.local.set({ [DEVICE_PANEL_COLLAPSED_KEY]: devicePanelCollapsed });
  } catch (_) { /* UI state remains valid for this panel lifetime. */ }
}

function renderFleet() {
  deviceList.replaceChildren();
  deviceHelp.hidden = true;
  deviceHelp.className = "device-help";
  deviceHelp.textContent = "";

  if (fleetContext.loading) {
    const loading = document.createElement("div");
    loading.className = "device-empty";
    loading.textContent = t("cc_devices_loading");
    deviceList.appendChild(loading);
    deviceSummary.textContent = "";
    return;
  }

  const response = fleetContext.response;
  if (!response?.ok) {
    deviceSummary.textContent = "";
    const empty = document.createElement("div");
    empty.className = "device-empty";
    empty.textContent = t("cc_devices_not_available");
    deviceList.appendChild(empty);
    deviceHelp.hidden = false;
    deviceHelp.classList.add("error");
    deviceHelp.textContent = fleetFailureText(response || { error: fleetContext.error });
    return;
  }

  // Revoked identities are authorization tombstones, not current fleet members.
  // Edge filters them too; keep this defensive filter for older/stale runtimes.
  const devices = Array.isArray(response.devices)
    ? response.devices.filter((device) => device?.authorization !== "revoked")
    : [];
  devices.sort((a, b) => {
    const rank = (device) => device.connection === "online" ? 0
      : device.connection === "stale" ? 1 : 2;
    return rank(a) - rank(b) || String(a.name || a.device_id).localeCompare(String(b.name || b.device_id));
  });
  const online = devices.filter((device) => device.authorization === "active" && device.connection === "online").length;
  deviceSummary.textContent = t("cc_devices_summary", { online, total: devices.length });
  const observedAt = Number(response.observed_at_ms || 0);
  deviceSummary.title = observedAt > 0
    ? t("cc_devices_observed", { value: new Date(observedAt).toLocaleTimeString() })
    : "";

  if (!devices.length) {
    const empty = document.createElement("div");
    empty.className = "device-empty";
    empty.textContent = t("cc_devices_empty");
    deviceList.appendChild(empty);
    return;
  }

  const localDeviceId = String(response.local?.device_id || "");
  for (const device of devices) {
    const row = document.createElement("div");
    row.className = "device-row";

    const dot = document.createElement("span");
    dot.className = `dot ${fleetStatusDot(device)}`;

    const main = document.createElement("div");
    main.className = "device-main";
    const nameLine = document.createElement("div");
    nameLine.className = "device-name-line";
    const name = document.createElement("span");
    name.className = "device-name";
    name.textContent = device.name || device.device_id || t("cc_status_unknown");
    nameLine.appendChild(name);
    if (localDeviceId && device.device_id === localDeviceId) {
      const current = document.createElement("span");
      current.className = "device-this";
      current.textContent = t("cc_device_this");
      nameLine.appendChild(current);
    }
    const meta = document.createElement("div");
    meta.className = "device-meta";
    const runtime = [device.runtime_version, device.runtime_generation].filter(Boolean).join(" · ") || t("cc_status_unknown");
    const staleGeneration = localDeviceId === device.device_id && response.local?.link_generation_stale === true
      ? ` · ${t("cc_device_generation_stale")}` : "";
    meta.textContent = `${deviceAuthorizationLabel(device.authorization)} · ${deviceConnectionLabel(device.connection)} · ${String(device.health || t("cc_status_unknown"))} · ${runtime}${staleGeneration}`;
    main.append(nameLine, meta);

    const lastSeen = document.createElement("span");
    lastSeen.className = "device-last-seen";
    lastSeen.textContent = deviceLastSeenLabel(device.last_seen_ago_ms);
    lastSeen.title = device.device_id || "";
    row.append(dot, main, lastSeen);
    deviceList.appendChild(row);
  }
}

async function refreshFleet() {
  fleetContext = { ...fleetContext, loading: true, error: null };
  renderFleet();
  const response = await bg({ type: "herdr_control_fleet" });
  fleetContext = {
    loading: false,
    response,
    error: response?.error || null,
    updatedAt: Date.now(),
  };
  renderFleet();
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

function siteLabel(site) {
  if (site === "chatgpt") return "ChatGPT";
  if (site === "claude" || site === "claude.ai") return "Claude";
  if (site === "z.ai" || site === "zai") return "z.ai";
  if (site === "deepseek") return "DeepSeek";
  return site || t("cc_page_unknown_site");
}

function shortIdentity(value, max = 18) {
  const text = String(value || "");
  return text.length > max ? `${text.slice(0, 8)}…${text.slice(-6)}` : text;
}

function pageContextInfo() {
  return pageContext.response?.convInfo || null;
}

function pageContextBindings() {
  return Array.isArray(pageContext.response?.sessionBindings) ? pageContext.response.sessionBindings : [];
}

function pageContextBindingIds() {
  return new Set(pageContextBindings().map((binding) => String(binding?.workspace_id || "")).filter(Boolean));
}

function renderPageContext(state) {
  const info = pageContextInfo();
  const bindings = pageContextBindings();
  const supported = Boolean(info?.convKey);

  if (pageContext.loading) {
    pageContextCard.hidden = true;
    return;
  }

  if (!supported) {
    pageContextCard.hidden = true;
    return;
  }

  pageContextCard.hidden = false;
  pageContextTitle.textContent = siteLabel(info.site);
  pageContextMeta.textContent = bindings.length
    ? t("cc_page_binding_count", { count: bindings.length })
    : t("cc_page_unbound");
  const identity = [];
  if (info.project_id) identity.push(t("cc_page_project_id", { value: shortIdentity(info.project_id, 48) }));
  if (info.conversation_id) identity.push(t("cc_page_conversation_id", { value: shortIdentity(info.conversation_id, 48) }));
  pageContextMeta.title = identity.join(" · ");
  pageContextHelp.hidden = !pageContext.error;
  pageContextHelp.textContent = pageContext.error || "";
}

async function refreshPageContext() {
  const seq = ++pageContextRefreshSeq;
  pageContext = { ...pageContext, loading: true, error: null };
  renderPageContext(store.get());
  let tabs = [];
  try { tabs = await chrome.tabs.query({ active: true, currentWindow: true }); }
  catch (error) {
    if (seq !== pageContextRefreshSeq) return;
    pageContext = { loading: false, tabId: null, windowId: null, response: null, error: String(error?.message || error) };
    renderAll();
    return;
  }
  const tab = tabs[0] || null;
  if (seq !== pageContextRefreshSeq) return;
  if (!tab?.id) {
    pageContext = { loading: false, tabId: null, windowId: tab?.windowId || null, response: null, error: null };
    renderAll();
    return;
  }
  const response = await bg({ type: "h2w_state", tabId: tab.id });
  if (seq !== pageContextRefreshSeq) return;
  pageContext = {
    loading: false,
    tabId: tab.id,
    windowId: tab.windowId ?? null,
    response,
    error: response?.error || null,
  };
  renderAll();
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

function restoreExpansionPreference(value) {
  if (!Array.isArray(value)) return false;
  expandedWorkspaces.clear();
  for (const workspaceId of value) {
    const id = String(workspaceId || "").trim();
    if (id) expandedWorkspaces.add(id);
  }
  expansionSeeded = true;
  return true;
}

async function persistExpansionPreference() {
  expansionSeeded = true;
  try {
    await chrome.storage.local.set({
      [EXPANDED_WORKSPACES_KEY]: [...expandedWorkspaces].sort(),
    });
  } catch (_) { /* UI state remains valid for this panel lifetime. */ }
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
  controlDock.remove();
  controlDock.hidden = true;
  workspaceList.replaceChildren();
  const pageBoundIds = pageContextBindingIds();
  const workspaces = workspaceRowsForPage(state.workspaces || [], pageContextBindings());
  if (!workspaces.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = runtimeHealthy ? t("cc_no_panes") : t("cc_runtime_unavailable");
    workspaceList.appendChild(empty);
    return;
  }

  const fragment = document.createDocumentFragment();
  const pageSupported = Boolean(pageContextInfo()?.convKey && pageContext.tabId && !pageContext.loading);
  const bindingBusy = Boolean(bindingMutationWorkspaceId);
  for (const workspace of workspaces) {
    const workspaceId = String(workspace.workspace_id);
    const workspaceName = workspace.label || workspaceId;
    const contextBound = pageBoundIds.has(workspaceId);
    const section = document.createElement("section");
    section.className = `workspace${contextBound ? " context-bound" : ""}`;
    section.dataset.workspaceId = workspaceId;

    const header = document.createElement("div");
    header.className = "workspace-header";
    const expanded = expandedWorkspaces.has(workspaceId);

    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "workspace-toggle";
    toggle.dataset.workspaceToggle = workspaceId;
    toggle.setAttribute("aria-expanded", String(expanded));

    const chevron = document.createElement("span");
    chevron.className = "chevron";
    chevron.textContent = expanded ? "▾" : "▸";
    const stateDot = document.createElement("span");
    stateDot.className = `dot ${statusDotClass(workspaceAggregateStatus(workspace))}`;
    const name = document.createElement("span");
    name.className = "workspace-name";
    name.textContent = workspaceName;
    const id = document.createElement("span");
    id.className = "workspace-id";
    id.textContent = workspaceId;
    const count = document.createElement("span");
    count.className = "workspace-count";
    const workingCount = (workspace.panes || []).filter((pane) => pane.status === "working").length;
    const paneCount = workspace.panes?.length || 0;
    count.textContent = t("cc_workspace_count", { panes: paneCount });
    count.title = t("cc_workspace_count_detail", { panes: paneCount, working: workingCount });
    count.setAttribute("aria-label", count.title);
    toggle.append(chevron, stateDot, name, id, count);

    header.appendChild(toggle);
    if (pageSupported) {
      const bindingToggle = document.createElement("button");
      bindingToggle.type = "button";
      bindingToggle.className = `workspace-binding-toggle${bindingMutationWorkspaceId === workspaceId ? " binding-busy" : ""}`;
      bindingToggle.dataset.workspaceBindingAction = workspaceId;
      bindingToggle.setAttribute("aria-pressed", String(contextBound));
      bindingToggle.disabled = bindingBusy;
      bindingToggle.textContent = bindingMutationWorkspaceId === workspaceId
        ? t("cc_workspace_binding_updating")
        : (contextBound ? t("cc_workspace_bound") : t("cc_workspace_bind"));
      bindingToggle.title = bindingBusy
        ? t("cc_workspace_binding_busy_reason")
        : contextBound
          ? t("cc_workspace_unbind_hint", { workspace: workspaceName })
          : t("cc_workspace_bind_hint", { workspace: workspaceName });
      bindingToggle.setAttribute("aria-label", contextBound
        ? t("cc_workspace_unbind_aria", { workspace: workspaceName })
        : t("cc_workspace_bind_aria", { workspace: workspaceName }));
      header.appendChild(bindingToggle);
    }
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
        if (pinnedTarget?.pane_id === pane.pane_id) {
          controlDock.hidden = false;
          row.appendChild(controlDock);
        }
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
  const interruptDescriptor = buildActionDescriptor(ACTION_TYPES.INTERRUPT, { target });
  agentQuickActions.hidden = !hasTarget || !target?.agent;
  interruptButton.disabled = actionInFlight || !interruptDescriptor.executable;
  const readBlockedReason = !hasTarget
    ? t("cc_pin_first")
    : stale
      ? t("cc_target_stale_disabled_reason")
      : !pane ? t("cc_target_unavailable_reason") : "";
  inspectButton.title = readBlockedReason || t("cc_inspect_state");
  readTailButton.title = readBlockedReason || t("cc_read_tail");
  interruptButton.title = actionInFlight
    ? t("cc_action_busy_reason")
    : interruptDescriptor.executable ? "" : t("cc_stop_unavailable_reason");

  if (!hasTarget) {
    targetTitle.textContent = "";
    targetDetails.textContent = "";
    targetKindBadge.hidden = true;
    targetKindBadge.className = "target-kind-badge";
    actionHeading.textContent = "";
    agentQuickActions.hidden = true;
    controlDock.hidden = true;
    return;
  }
  const kind = target.agent ? "agent" : "terminal";
  targetKindBadge.hidden = true;
  targetKindBadge.className = `target-kind-badge ${kind}`;
  actionHeading.textContent = "";
  targetTitle.textContent = target.agent
    ? t("cc_agent_actions_for", { agent: target.agent?.name || target.agent?.kind || "Agent" })
    : t("cc_terminal_actions");
  targetDetails.textContent = stale ? t("cc_target_stale_short") : statusLabel(target.status);
}

function modePresentation(mode) {
  if (mode === ACTION_TYPES.STEER) {
    return { help: "cc_mode_steer_help", placeholder: "cc_placeholder_steer" };
  }
  if (mode === ACTION_TYPES.HERDR_METHOD) {
    return { help: "cc_mode_herdr_help", placeholder: "cc_placeholder_herdr" };
  }
  if ([ACTION_TYPES.TERMINAL_TEXT, ACTION_TYPES.TERMINAL_INPUT].includes(mode)) {
    return { help: "cc_mode_terminal_help", placeholder: "cc_placeholder_terminal" };
  }
  return { help: "cc_mode_prompt_help", placeholder: "cc_placeholder_prompt" };
}

function modeLabelKey(mode) {
  if (mode === ACTION_TYPES.STEER) return "cc_mode_steer";
  if ([ACTION_TYPES.TERMINAL_TEXT, ACTION_TYPES.TERMINAL_INPUT].includes(mode)) return "cc_mode_terminal";
  return "cc_mode_prompt";
}

function renderModeTabs(modes) {
  modeTabs.replaceChildren();
  modeTabs.hidden = modes.length === 0;
  for (const mode of modes) {
    const button = document.createElement("button");
    button.type = "button";
    button.dataset.mode = mode;
    button.textContent = t(modeLabelKey(mode));
    button.classList.toggle("active", mode === selectedMode);
    button.addEventListener("click", () => {
      selectedMode = mode;
      renderComposerState();
    });
    modeTabs.appendChild(button);
  }
}

function riskLabel(mode) {
  const risk = classifyAction(mode);
  if (risk === ACTION_RISK.PROVIDER_STEER) return t("cc_risk_provider");
  if (risk === ACTION_RISK.TERMINAL_MUTATION) return t("cc_risk_terminal");
  if (risk === ACTION_RISK.UNKNOWN) return t("cc_risk_herdr");
  return t("cc_risk_write");
}

function controlOutcomeLabel(outcome) {
  const key = `cc_outcome_${String(outcome || "failed")}`;
  const label = t(key);
  return label === key ? String(outcome || "failed") : label;
}

function controlOutcomeText(response) {
  const outcome = String(response?.outcome || (response?.ok ? "submitted" : "failed"));
  const lines = [controlOutcomeLabel(outcome)];
  const reason = response?.detail?.capability?.reason
    || response?.detail?.reason
    || response?.message
    || response?.error;
  if (reason && response?.ok !== true) lines.push(String(reason));
  if (outcome === "uncertain") lines.push(t("cc_control_uncertain_hint"));
  return lines.join("\n");
}

async function applyControlResponse(response) {
  result.textContent = controlOutcomeText(response);
  if (response?.outcome === "stale_target") {
    pinnedTarget = { ...pinnedTarget, stale: true, stale_reason: "target_revision_changed" };
    await chrome.storage.local.set({ [TARGET_KEY]: pinnedTarget });
    await refreshSnapshot();
  }
}

function renderComposerState() {
  const modes = actionModesForTarget(pinnedTarget);
  if (!modes.includes(selectedMode)) selectedMode = modes[0] || null;
  renderModeTabs(modes);
  const hasMode = Boolean(selectedMode);
  composer.hidden = !hasMode;
  composerFooter.hidden = !hasMode;
  actionModeBadge.hidden = true;
  if (!hasMode) {
    modeHelp.textContent = t("cc_pin_first");
    sendButton.disabled = true;
    return;
  }
  const mode = modePresentation(selectedMode);
  const descriptor = buildActionDescriptor(selectedMode, { target: pinnedTarget });
  riskBadge.textContent = riskLabel(selectedMode);
  modeHelp.textContent = t(mode.help);
  composer.placeholder = t(mode.placeholder);
  actionModeBadge.textContent = descriptor.executable ? t("cc_live_badge") : t("cc_preview_badge");
  actionModeBadge.classList.toggle("live", descriptor.executable);
  if (actionInFlight) blockedReason.textContent = t("cc_action_busy_reason");
  else if (!pinnedTarget?.pane_id) blockedReason.textContent = t("cc_pin_first");
  else if (pinnedTarget.stale) blockedReason.textContent = t("cc_target_stale_short");
  else if (descriptor.executable) blockedReason.textContent = "";
  else blockedReason.textContent = t("cc_preview_only_reason");
  sendButton.textContent = descriptor.executable
    ? (selectedMode === ACTION_TYPES.STEER
      ? t("cc_execute_steer")
      : selectedMode === ACTION_TYPES.TERMINAL_INPUT
        ? t("cc_execute_terminal")
        : t("cc_execute_prompt"))
    : t("cc_preview_action");
  sendButton.disabled = actionInFlight || !pinnedTarget?.pane_id || pinnedTarget?.stale === true;
  sendButton.title = sendButton.disabled ? blockedReason.textContent : "";

}

async function mutateWorkspaceBinding(workspaceId) {
  const info = pageContextInfo();
  if (!info?.convKey || !pageContext.tabId || !workspaceId || bindingMutationWorkspaceId) return;
  const currentlyBound = pageContextBindingIds().has(String(workspaceId));
  const workspace = (store.get().workspaces || []).find((row) => String(row.workspace_id) === String(workspaceId));
  if (!currentlyBound && !workspace) return;
  bindingMutationWorkspaceId = String(workspaceId);
  renderAll();

  const response = currentlyBound
    ? await bg({ type: "h2w_unbind", convKey: info.convKey, workspace_id: workspaceId })
    : await bg({
      type: "h2w_bind",
      tabId: pageContext.tabId,
      convKey: info.convKey,
      workspace_id: workspaceId,
      workspace_label: workspace.label || workspaceId,
      local_project_key: workspace.local_project_key || null,
      device_id: workspace.device_id || null,
    });
  const actionError = !response?.ok && response?.error !== "already-bound"
    ? t(currentlyBound ? "cc_page_unbind_failed" : "cc_page_bind_failed", { error: response?.error || "unknown" })
    : null;

  await refreshPageContext();
  bindingMutationWorkspaceId = null;
  if (actionError) pageContext.error = actionError;
  renderAll();
}

try {
  chrome.tabs.onActivated.addListener(() => { void refreshPageContext(); });
  chrome.tabs.onUpdated.addListener((_tabId, changeInfo, tab) => {
    if (tab?.active && (changeInfo?.url || changeInfo?.status === "complete")) void refreshPageContext();
  });
} catch (_) {}

function renderAll() {
  const state = store.get();
  renderDevicePanelCollapse();
  renderRuntime(state);
  renderPageContext(state);
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
    if (!fleetContext.updatedAt || Date.now() - fleetContext.updatedAt > 30_000) void refreshFleet();
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
  } else if (message?.type === "herdr_control_binding_changed") {
    void refreshPageContext();
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
  if (event.target.closest?.("#controlDock")) return;
  const bindingAction = event.target.closest?.("[data-workspace-binding-action]");
  if (bindingAction) {
    await mutateWorkspaceBinding(bindingAction.dataset.workspaceBindingAction);
    return;
  }
  const toggle = event.target.closest?.("[data-workspace-toggle]");
  if (toggle) {
    const workspaceId = toggle.dataset.workspaceToggle;
    if (expandedWorkspaces.has(workspaceId)) expandedWorkspaces.delete(workspaceId);
    else expandedWorkspaces.add(workspaceId);
    void persistExpansionPreference();
    renderAll();
    return;
  }
  const paneNode = event.target.closest?.("[data-pane-id]");
  if (!paneNode) return;
  const pane = (store.get().panes || []).find((item) => item.pane_id === paneNode.dataset.paneId);
  if (!pane) return;
  const previousPaneId = pinnedTarget?.pane_id || null;
  if (previousPaneId === pane.pane_id && pinnedTarget?.stale !== true) {
    pinnedTarget = null;
    composer.value = "";
    await chrome.storage.local.remove(TARGET_KEY);
    renderAll();
    return;
  }
  const previousKind = pinnedTarget?.pane_id ? (pinnedTarget.agent ? "agent" : "terminal") : null;
  const nextTarget = createPinnedTarget(pane);
  const nextKind = nextTarget.agent ? "agent" : "terminal";
  if (previousPaneId !== nextTarget.pane_id || previousKind !== nextKind) composer.value = "";
  pinnedTarget = nextTarget;
  await chrome.storage.local.set({ [TARGET_KEY]: pinnedTarget });
  result.textContent = "";
  renderAll();
});

$("refreshButton").addEventListener("click", () => {
  void Promise.all([refreshSnapshot(true), refreshFleet(), refreshPageContext()]);
});
$("collapseButton").addEventListener("click", () => {
  expandedWorkspaces.clear();
  devicePanelCollapsed = true;
  void persistExpansionPreference();
  void persistDevicePanelCollapse();
  renderAll();
});
deviceToggleButton.addEventListener("click", () => {
  devicePanelCollapsed = !devicePanelCollapsed;
  void persistDevicePanelCollapse();
  renderDevicePanelCollapse();
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

interruptButton.addEventListener("click", async () => {
  if (!pinnedTarget?.pane_id || pinnedTarget.stale || actionInFlight) return;
  if (!confirm(t("cc_interrupt_confirm"))) return;
  const descriptor = buildActionDescriptor(ACTION_TYPES.INTERRUPT, { target: pinnedTarget });
  if (!descriptor.executable) return;
  actionInFlight = true;
  renderAll();
  result.textContent = t("cc_interrupt_sending");
  const response = await bg({
    type: "herdr_control_action",
    request: {
      action: descriptor.action,
      target: descriptor.target,
      args: descriptor.args,
      idempotency_key: `browser-control:${crypto.randomUUID()}`,
    },
  });
  await applyControlResponse(response);
  actionInFlight = false;
  renderAll();
});

sendButton.addEventListener("click", async () => {
  if (!pinnedTarget?.pane_id || pinnedTarget.stale || actionInFlight) return;
  const text = composer.value.trim();
  const descriptor = buildActionDescriptor(selectedMode, {
    target: pinnedTarget,
    text: selectedMode === ACTION_TYPES.HERDR_METHOD ? "" : text,
    method: selectedMode === ACTION_TYPES.HERDR_METHOD ? text : null,
  });
  if (!descriptor.executable) {
    result.textContent = JSON.stringify(descriptor, null, 2);
    return;
  }
  if (!text && [ACTION_TYPES.AGENT_PROMPT, ACTION_TYPES.STEER, ACTION_TYPES.TERMINAL_INPUT].includes(selectedMode)) {
    result.textContent = t("cc_control_text_required");
    return;
  }

  actionInFlight = true;
  renderComposerState();
  result.textContent = t("cc_control_sending");
  const request = {
    action: descriptor.action,
    target: descriptor.target,
    args: descriptor.args,
    idempotency_key: `browser-control:${crypto.randomUUID()}`,
  };
  const response = await bg({ type: "herdr_control_action", request });
  await applyControlResponse(response);
  if (response?.ok && selectedMode === ACTION_TYPES.TERMINAL_INPUT) composer.value = "";
  actionInFlight = false;
  renderAll();
});

async function start() {
  await detectOrLoadLocale();
  applyStaticI18n();
  document.documentElement.classList.remove("i18n-pending");
  const stored = await chrome.storage.local.get([
    TARGET_KEY,
    EXPANDED_WORKSPACES_KEY,
    DEVICE_PANEL_COLLAPSED_KEY,
  ]);
  pinnedTarget = stored[TARGET_KEY] || null;
  restoreExpansionPreference(stored[EXPANDED_WORKSPACES_KEY]);
  devicePanelCollapsed = stored[DEVICE_PANEL_COLLAPSED_KEY] === true;
  connectControlPort(false);
  renderAll();
  renderFleet();
  await Promise.all([refreshSnapshot(), refreshPageContext(), refreshFleet()]);
}

void start();
