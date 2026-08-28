import { createBrowserStateStore } from "./browser-state-store.js";
import { createPinnedTarget, revalidatePinnedTarget } from "./target-pin.js";
import { ACTION_TYPES, ACTION_RISK, buildActionDescriptor, classifyAction } from "./control-actions.js";
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
const store = createBrowserStateStore();
const expandedWorkspaces = new Set();
let expansionSeeded = false;
let pinnedTarget = null;
let runtimeHealthy = false;
let hasSnapshot = false;
let eventStreamHealthy = null;
let selectedMode = ACTION_TYPES.AGENT_PROMPT;
let pageContext = { loading: true, tabId: null, windowId: null, response: null, error: null };
let pageContextRefreshSeq = 0;
let bindingMutationWorkspaceId = null;

const $ = (id) => document.getElementById(id);
const runtimeDot = $("runtimeDot");
const runtimeText = $("runtimeText");
const runtimeStats = $("runtimeStats");
const workspaceList = $("workspaceList");
const pageContextCard = $("pageContextCard");
const pageContextTitle = $("pageContextTitle");
const pageContextMeta = $("pageContextMeta");
const pageHandoffButton = $("pageHandoffButton");
const pageContextHelp = $("pageContextHelp");
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
  pageContextCard.classList.toggle("unsupported", !supported);

  if (pageContext.loading) {
    pageContextTitle.textContent = t("cc_page_loading");
    pageContextMeta.textContent = "";
    pageContextMeta.title = "";
    pageContextHelp.textContent = t("cc_page_context_help");
    pageHandoffButton.disabled = true;
    pageHandoffButton.textContent = t("cc_page_handoff");
    return;
  }

  if (!supported) {
    pageContextTitle.textContent = t("cc_page_unsupported");
    pageContextMeta.textContent = pageContext.error || t("cc_page_unsupported_meta");
    pageContextMeta.title = "";
    pageContextHelp.textContent = t("cc_page_handoff_unavailable_help");
    pageHandoffButton.disabled = true;
    pageHandoffButton.textContent = t("cc_page_handoff");
    return;
  }

  const site = siteLabel(info.site);
  if (info.project_id && info.conversation_id) pageContextTitle.textContent = t("cc_page_project_conversation", { site });
  else if (info.project_id) pageContextTitle.textContent = t("cc_page_project_home", { site });
  else pageContextTitle.textContent = t("cc_page_conversation", { site });

  pageContextMeta.textContent = bindings.length
    ? t("cc_page_binding_count", { count: bindings.length })
    : t("cc_page_unbound");
  const identity = [];
  if (info.project_id) identity.push(t("cc_page_project_id", { value: shortIdentity(info.project_id, 48) }));
  if (info.conversation_id) identity.push(t("cc_page_conversation_id", { value: shortIdentity(info.conversation_id, 48) }));
  pageContextMeta.title = identity.join(" · ");

  const handoffStatus = String(pageContext.response?.handoff?.status || "");
  const handoffPageSupported = (
    (info.site === "chatgpt" && Boolean(info.project_id) && Boolean(info.conversation_id))
    || (info.site === "z.ai" && Boolean(info.conversation_id))
  );
  const canHandoff = bindings.length > 0 && handoffPageSupported;
  const workspaceBusy = bindings.some((binding) => (
    String(binding?.status || "") === "working" || Number(binding?.working_count || 0) > 0
  ));
  const transferBusy = ["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(handoffStatus);
  pageHandoffButton.disabled = !canHandoff || workspaceBusy || transferBusy;
  pageHandoffButton.textContent = workspaceBusy && canHandoff
    ? t("cc_page_handoff_busy")
    : (pageContext.response?.handoff?.can_resume === true
      ? t("cc_page_handoff_resume")
      : (transferBusy ? t("cc_page_handoff_working") : t("cc_page_handoff")));

  let handoffHelp = t("cc_page_context_help");
  if (!handoffPageSupported) {
    if (info.site === "chatgpt") handoffHelp = t("cc_page_handoff_project_required");
    else if (info.site === "z.ai") handoffHelp = t("cc_page_handoff_conversation_required");
    else handoffHelp = t("cc_page_handoff_unavailable_help");
  } else if (!bindings.length) {
    handoffHelp = t("cc_page_handoff_binding_required");
  } else if (workspaceBusy) {
    handoffHelp = t("cc_page_handoff_busy_help");
  }
  pageContextHelp.textContent = pageContext.error || pageContext.notice || handoffHelp;
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
    const bindingMissing = workspace.binding_missing === true;
    const section = document.createElement("section");
    section.className = `workspace${contextBound ? " context-bound" : ""}${bindingMissing ? " binding-missing" : ""}`;
    section.dataset.workspaceId = workspaceId;

    const header = document.createElement("div");
    header.className = "workspace-header";
    const expanded = !bindingMissing && expandedWorkspaces.has(workspaceId);

    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "workspace-toggle";
    toggle.dataset.workspaceToggle = workspaceId;
    toggle.disabled = bindingMissing;
    toggle.setAttribute("aria-expanded", String(expanded));

    const chevron = document.createElement("span");
    chevron.className = "chevron";
    chevron.textContent = bindingMissing ? "·" : (expanded ? "▾" : "▸");
    const stateDot = document.createElement("span");
    stateDot.className = bindingMissing ? "dot stale" : `dot ${statusDotClass(workspaceAggregateStatus(workspace))}`;
    const name = document.createElement("span");
    name.className = "workspace-name";
    name.textContent = workspaceName;
    const id = document.createElement("span");
    id.className = "workspace-id";
    id.textContent = `(${workspaceId})`;
    const count = document.createElement("span");
    count.className = "workspace-count";
    const workingCount = (workspace.panes || []).filter((pane) => pane.status === "working").length;
    count.textContent = bindingMissing
      ? t("cc_workspace_not_visible")
      : t("cc_workspace_count", {
        panes: workspace.panes?.length || 0,
        working: workingCount,
      });
    toggle.append(chevron, stateDot, name, id, count);

    const bindingToggle = document.createElement("button");
    bindingToggle.type = "button";
    bindingToggle.className = `workspace-binding-toggle${bindingMutationWorkspaceId === workspaceId ? " binding-busy" : ""}`;
    bindingToggle.dataset.workspaceBindingAction = workspaceId;
    bindingToggle.setAttribute("aria-pressed", String(contextBound));
    bindingToggle.disabled = !pageSupported || bindingBusy;
    bindingToggle.textContent = bindingMutationWorkspaceId === workspaceId
      ? t("cc_workspace_binding_updating")
      : (contextBound ? t("cc_workspace_bound") : t("cc_workspace_bind"));
    bindingToggle.title = !pageSupported
      ? t("cc_workspace_binding_disabled")
      : (contextBound
        ? t("cc_workspace_unbind_hint", { workspace: workspaceName })
        : t("cc_workspace_bind_hint", { workspace: workspaceName }));
    bindingToggle.setAttribute("aria-label", contextBound
      ? t("cc_workspace_unbind_aria", { workspace: workspaceName })
      : t("cc_workspace_bind_aria", { workspace: workspaceName }));
    header.append(toggle, bindingToggle);
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

function handoffFailureMessage(error) {
  const code = String(error || "unknown");
  if (code === "workspace_busy") return t("cc_page_handoff_busy_help");
  if (code === "binding_required") return t("cc_page_handoff_binding_required");
  return t("cc_page_handoff_failed", { error: code });
}

pageHandoffButton.addEventListener("click", async () => {
  if (!pageContext.tabId || pageHandoffButton.disabled) return;
  pageHandoffButton.disabled = true;
  pageContextHelp.textContent = t("cc_page_handoff_starting");
  const response = await bg({ type: "h2w_handoff_start", tabId: pageContext.tabId, trigger: "manual" });
  const actionMessage = response?.ok
    ? t("cc_page_handoff_started")
    : handoffFailureMessage(response?.error);
  await refreshPageContext();
  pageContext.notice = actionMessage;
  renderPageContext(store.get());
});

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
  await refreshPageContext();
}

void start();
