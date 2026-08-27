// popup.js — bind UI: workspace label + pane stats (incl. agentless terminals)
import { detectOrLoadLocale, getLocale, t, onLocaleReady } from "./i18n.js";

const $ = (id) => document.getElementById(id);
const STATUS_COLOR = { idle: "#9ca3af", working: "#d97706", done: "#16a34a", blocked: "#dc2626", unknown: "#6b7280", terminal: "#6b7280" };

async function bg(msg) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage(msg, (resp) => {
      if (chrome.runtime.lastError) resolve(null);
      else resolve(resp);
    });
  });
}

async function activeTab() {
  return new Promise((resolve) => {
    chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => resolve(tabs[0] || null));
  });
}

let toastTimer = null;
let popupState = null;
let popupTab = null;
function showToast(text, kind = "err") {
  const el = $("toast");
  el.textContent = text;
  el.className = kind;
  el.style.display = "block";
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.style.display = "none"; }, 5000);
}

/** Seconds from number input: empty → fallback; <=0 → 0 (off). */
function parseSecInput(v, fallback) {
  const n = Number(v);
  if (v === "" || v === undefined || v === null) return fallback;
  if (!Number.isFinite(n)) return fallback;
  if (n <= 0) return 0;
  return Math.min(Math.floor(n), 86400);
}

function basename(p) {
  return String(p || "").replace(/\/+$/, "").split("/").filter(Boolean).pop() || "";
}

function workspaceMetaMap(workspaces) {
  const m = new Map();
  for (const w of workspaces || []) {
    if (w?.id) m.set(w.id, w);
  }
  return m;
}

/** Group panes by workspace (agents alone miss agentless terminals). */
function groupPanesByWorkspace(panes, agents, workspaces) {
  const map = new Map();
  const ensure = (ws) => {
    if (!map.has(ws)) map.set(ws, { panes: [], agents: [] });
    return map.get(ws);
  };
  for (const w of workspaces || []) {
    if (w?.id) ensure(w.id);
  }
  for (const p of panes || []) {
    const ws = p.workspace || (typeof p.id === "string" && p.id.includes(":") ? p.id.split(":")[0] : null);
    if (!ws) continue;
    ensure(ws).panes.push(p);
  }
  for (const a of agents || []) {
    const ws = a.workspace || (typeof a.pane === "string" && a.pane.includes(":") ? a.pane.split(":")[0] : null);
    if (!ws) continue;
    ensure(ws).agents.push(a);
  }
  return map;
}

function titleForWorkspace(wsId, meta) {
  const label = (meta?.label || "").trim();
  if (label) return `${label} (${wsId})`;
  const roots = meta?.roots || [];
  const folders = [];
  for (const r of roots) {
    const b = basename(r);
    if (b && !folders.includes(b)) folders.push(b);
  }
  if (folders.length) return `${folders.slice(0, 2).join("+")} (${wsId})`;
  return wsId;
}

function summarize(group) {
  const panes = group.panes || [];
  const agents = group.agents || [];
  const working = agents.filter((a) => a.status === "working").length
    || panes.filter((p) => p.agent?.status === "working").length;
  const withAgent = panes.filter((p) => p.agent?.name).length;
  const terminalOnly = Math.max(0, panes.length - withAgent);
  return {
    paneCount: panes.length || agents.length,
    working,
    terminalOnly,
  };
}

function sessionBindingMap(sessionBindings) {
  const m = new Map();
  for (const b of sessionBindings || []) {
    if (b?.workspace_id) m.set(b.workspace_id, b);
  }
  return m;
}

function applyStaticI18n() {
  document.documentElement.lang = getLocale() === "zh" ? "zh-CN" : getLocale();
  document.title = t("popup_title");
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    if (key) el.textContent = t(key);
  });
  const opts = $("openOptions");
  if (opts) opts.textContent = t("options_link");
}

async function bindWorkspace(tab, ws, group, meta, title) {
  if (!tab?.id) return;
  const sample = (group.agents || []).find((a) => a.status === "working")
    || (group.panes || []).find((p) => p.agent?.status === "working")
    || (group.panes || [])[0]
    || (group.agents || [])[0];
  const paneId = sample?.pane || sample?.id || null;
  const agentName = sample?.name || sample?.agent?.name || null;
  const r = await bg({
    type: "h2w_bind",
    tabId: tab.id,
    workspace_id: ws,
    workspace_label: title,
    workspace_label_raw: meta?.label || null,
    roots: meta?.roots || [],
    pane: paneId,
    agent: agentName,
  });
  if (r?.ok) {
    showToast(`✓ ${t("bound")} ${r.workspace_label || title}`, "ok");
    refresh();
  } else if (r?.error === "conversation-unavailable") {
    showToast(t("bind_need_chat"));
  } else if (r?.error === "already-bound") {
    showToast(t("already_bound_ws"));
  } else {
    showToast(`${t("bind_failed")}: ${r?.error || "?"}`);
  }
}

async function refresh() {
  applyStaticI18n();
  const tab = await activeTab();
  const st = await bg({ type: "h2w_state", tabId: tab?.id }) || {};
  popupTab = tab;
  popupState = st;
  const agentsResp = await bg({ type: "h2w_agents" });
  const wsMeta = workspaceMetaMap(agentsResp?.workspaces);
  const groups = groupPanesByWorkspace(agentsResp?.panes, agentsResp?.agents, agentsResp?.workspaces);
  const boundByWs = sessionBindingMap(st.sessionBindings);

  const srv = $("srvStatus");
  if (agentsResp?.ok) {
    const nWs = groups.size;
    const nPanes = [...groups.values()].reduce((n, g) => n + (g.panes?.length || 0), 0);
    srv.innerHTML = `<span class="ok">● ${t("online")}</span> <span class="muted">${t("popup_service_stats", { workspaces: nWs, panes: nPanes })}</span>`;
  } else if (agentsResp?.status === 401) {
    srv.innerHTML = `<span class="err">● 401</span>`;
    $("agents").innerHTML = `<div class="err">${t("token_mismatch")}</div>
      <div style="margin-top:4px"><button id="openOptsBtn">${t("open_options")}</button></div>`;
    $("openOptsBtn")?.addEventListener("click", () => chrome.runtime.openOptionsPage());
    return;
  } else {
    srv.innerHTML = `<span class="err">● ${t("unreachable")}${agentsResp?.status ? ` (${agentsResp.status})` : ""}</span>`;
  }
  $("automationModeStatus").textContent = st.config?.automationMode === "project_auto"
    ? t("automation_mode_project")
    : t("automation_mode_manual");
  const quickAutomation = $("automationQuickToggle");
  if (quickAutomation) {
    const active = st.config?.enabled === true;
    quickAutomation.disabled = !tab?.id || !st.convInfo;
    quickAutomation.dataset.enabled = active ? "1" : "0";
    quickAutomation.textContent = t(active ? "hud_automation_on" : "hud_automation_off");
    quickAutomation.classList.toggle("primary", active);
  }
  $("progressTickSec").value = String(st.config?.progressTickSec ?? 60);
  const llmEl = $("llmJudgeStatus");
  if (llmEl) {
    const cfgOn = !!st.config?.llmJudgeConfigured;
    const last = st.idleNudgeLast;
    if (!cfgOn) {
      llmEl.textContent = t("llm_status_off");
    } else if (!last) {
      llmEl.textContent = t("llm_status_ready");
    } else {
      const ago = Math.max(0, Math.round((Date.now() - (last.at || 0)) / 1000));
      const raw = last.raw != null ? t("llm_status_raw", { raw: JSON.stringify(String(last.raw)).slice(0, 32) }) : "";
      llmEl.textContent = t("llm_status_last", { reason: last.reason || "?", ago: String(ago) }) + raw;
    }
  }

  const conv = $("convInfo");
  if (st.convInfo) {
    const short = st.convInfo.convKey.replace(/^https?:\/\//, "");
    conv.textContent = `${st.convInfo.site} · ${short}`;
  } else {
    conv.innerHTML = `<span class="err">${t("unsupported_tab")}</span>
      <div class="hintbox">${t("open_supported_chat")}
      <div class="muted">${(tab && tab.url || "?").slice(0, 72)}</div></div>`;
  }

  const box = $("agents");
  if (!agentsResp?.ok) {
    box.textContent = agentsResp?.error || t("no_herdr");
  } else if (!groups.size) {
    box.textContent = t("no_workspaces");
  } else if (!st.convInfo) {
    box.textContent = t("bind_need_chat");
  } else {
    box.innerHTML = "";
    const entries = [...groups.entries()].sort(([a], [b]) => {
      const ab = boundByWs.has(a) ? 0 : 1;
      const bb = boundByWs.has(b) ? 0 : 1;
      if (ab !== bb) return ab - bb;
      return titleForWorkspace(a, wsMeta.get(a)).localeCompare(titleForWorkspace(b, wsMeta.get(b)));
    });
    for (const [ws, group] of entries) {
      const meta = wsMeta.get(ws);
      const title = titleForWorkspace(ws, meta);
      const sum = summarize(group);
      const bound = boundByWs.get(ws);
      const row = document.createElement("div");
      row.className = bound ? "agent bound" : "agent";
      const dotColor = sum.working > 0 ? STATUS_COLOR.working : STATUS_COLOR.idle;
      const main = document.createElement("div");
      main.className = "agent-main";
      main.innerHTML = `<div class="title"><span class="dot" style="background:${dotColor}" title="${sum.working > 0 ? t("ws_working") : t("ws_idle")}"></span><b>${title}</b></div>
        <div class="meta">${t("popup_workspace_stats", {
          panes: sum.paneCount,
          working: sum.working,
          terminal: sum.terminalOnly ? t("popup_terminal_suffix", { count: sum.terminalOnly }) : "",
        })}</div>`;
      row.appendChild(main);
      if (bound) {
        const btn = document.createElement("button");
        btn.className = "danger sm";
        btn.textContent = t("unbind");
        btn.addEventListener("click", async (e) => {
          e.stopPropagation();
          await bg({ type: "h2w_unbind", convKey: st.convInfo.convKey, workspace_id: ws });
          refresh();
        });
        row.appendChild(btn);
      } else {
        const btn = document.createElement("button");
        btn.className = "primary sm";
        btn.textContent = t("bind_action");
        btn.addEventListener("click", async (e) => {
          e.stopPropagation();
          await bindWorkspace(tab, ws, group, meta, title);
        });
        row.appendChild(btn);
        row.addEventListener("click", () => { void bindWorkspace(tab, ws, group, meta, title); });
      }
      box.appendChild(row);
    }
  }
}

$("progressTickSec").addEventListener("change", async (e) => {
  const sec = parseSecInput(e.target.value, 60);
  e.target.value = String(sec);
  await bg({ type: "h2w_set_config", config: { progressTickSec: sec } });
});
$("openOptions").addEventListener("click", (e) => { e.preventDefault(); chrome.runtime.openOptionsPage(); });
$("automationQuickToggle").addEventListener("click", async (event) => {
  if (!popupTab?.id || !popupState?.convInfo) return;
  const button = event.currentTarget;
  const enabled = button.dataset.enabled !== "1";
  button.disabled = true;
  const response = await bg({ type: "h2w_popup_set_automation", tabId: popupTab.id, enabled });
  if (!response?.ok) {
    showToast(`${t("hud_automation_update_failed")}: ${response?.error || "?"}`);
  }
  await refresh();
});
$("openControlCenter").addEventListener("click", () => {
  if (!chrome.sidePanel?.open) {
    showToast("Control Center is unavailable in this browser.");
    return;
  }
  // Keep the call in the direct click handler so Chrome preserves the user
  // gesture required by sidePanel.open().
  chrome.sidePanel.open({ windowId: chrome.windows.WINDOW_ID_CURRENT })
    .catch((error) => showToast(String(error?.message || error)));
});

onLocaleReady(() => { void refresh(); });
void detectOrLoadLocale();
