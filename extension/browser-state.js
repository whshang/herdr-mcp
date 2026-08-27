// Browser Control Plane Phase A: normalize local runtime state into one UI model.
// This module is pure and deliberately independent from chrome.* APIs.

function stringOrNull(value) {
  return typeof value === "string" && value ? value : null;
}

function paneIdOf(value) {
  return stringOrNull(value?.pane_id) || stringOrNull(value?.pane) || stringOrNull(value?.id);
}

function workspaceIdOf(value) {
  return stringOrNull(value?.workspace_id) || stringOrNull(value?.workspace) || stringOrNull(value?.id);
}

function agentStatus(value) {
  const raw = value?.status;
  if (typeof raw === "string" && raw) return raw;
  if (raw && typeof raw === "object" && typeof raw.status === "string") return raw.status;
  return "unknown";
}

function longestMatchingRoot(cwd, roots = []) {
  if (!cwd) return null;
  return roots
    .filter((root) => typeof root === "string" && root && (cwd === root || cwd.startsWith(`${root}/`)))
    .sort((a, b) => b.length - a.length)[0] || null;
}

function normalizeAgent(agent) {
  if (!agent) return null;
  return {
    name: stringOrNull(agent.name) || stringOrNull(agent.agent),
    kind: stringOrNull(agent.kind) || stringOrNull(agent.agent),
    status: agentStatus(agent),
    started_at: stringOrNull(agent.started_at),
    last_activity_at: stringOrNull(agent.last_activity_at) || stringOrNull(agent.at),
    seq: Number.isFinite(agent.seq) ? agent.seq : (Number.isFinite(agent.state_change_seq) ? agent.state_change_seq : null),
  };
}

function normalizePane(pane, agent, workspace) {
  const paneId = paneIdOf(pane);
  const workspaceId = workspaceIdOf(pane) || workspaceIdOf(agent) || workspaceIdOf(workspace);
  const normalizedAgent = normalizeAgent(agent || pane?.agent || null);
  const cwd = stringOrNull(pane?.cwd) || stringOrNull(pane?.foreground_cwd) || stringOrNull(agent?.cwd);
  const roots = Array.isArray(workspace?.roots) ? workspace.roots : [];
  return {
    workspace_id: workspaceId,
    tab_id: stringOrNull(pane?.tab_id),
    pane_id: paneId,
    cwd,
    project_root: stringOrNull(pane?.project_root) || longestMatchingRoot(cwd, roots),
    terminal_title: stringOrNull(pane?.terminal_title) || stringOrNull(pane?.terminal_title_stripped) || stringOrNull(agent?.terminal_title),
    focused: pane?.focused === true,
    agent: normalizedAgent,
    status: normalizedAgent?.status || "terminal-only",
    current_summary: stringOrNull(pane?.current_summary) || null,
    last_output: null,
    last_event_at: stringOrNull(agent?.at) || null,
  };
}

export function rebuildWorkspaceViews(view = {}) {
  const panes = Array.isArray(view.panes) ? view.panes : [];
  const workspaceRows = Array.isArray(view.workspace_rows) ? view.workspace_rows : [];
  const tabs = Array.isArray(view.tabs) ? view.tabs : [];
  const ids = new Set([
    ...workspaceRows.map(workspaceIdOf).filter(Boolean),
    ...panes.map((pane) => pane.workspace_id).filter(Boolean),
  ]);
  return {
    ...view,
    workspaces: [...ids].map((workspaceId) => {
      const row = workspaceRows.find((workspace) => workspaceIdOf(workspace) === workspaceId) || {};
      return {
        workspace_id: workspaceId,
        label: stringOrNull(row.label) || workspaceId,
        roots: Array.isArray(row.roots) ? [...row.roots] : [],
        panes: panes.filter((pane) => pane.workspace_id === workspaceId),
        tabs: tabs.filter((tab) => workspaceIdOf(tab) === workspaceId),
      };
    }),
  };
}

export function normalizeBrowserState(snapshot = {}) {
  const workspaceRows = Array.isArray(snapshot.workspaces) ? snapshot.workspaces : [];
  const rawPanes = Array.isArray(snapshot.panes) ? snapshot.panes : [];
  const rawAgents = Array.isArray(snapshot.agents) ? snapshot.agents : [];
  const tabs = Array.isArray(snapshot.tabs) ? snapshot.tabs.map((tab) => ({ ...tab })) : [];
  const agentByPane = new Map(rawAgents.map((agent) => [paneIdOf(agent), agent]).filter(([id]) => id));
  const workspaceById = new Map(workspaceRows.map((workspace) => [workspaceIdOf(workspace), workspace]).filter(([id]) => id));

  const panes = rawPanes
    .map((pane) => {
      const paneId = paneIdOf(pane);
      if (!paneId) return null;
      const agent = agentByPane.get(paneId) || null;
      const workspaceId = workspaceIdOf(pane) || workspaceIdOf(agent);
      return normalizePane(pane, agent, workspaceById.get(workspaceId) || null);
    })
    .filter(Boolean);

  // Some older snapshots can expose an agent whose pane is not present in the
  // panes array. Keep it visible as a best-effort pane row until reconcile.
  for (const agent of rawAgents) {
    const paneId = paneIdOf(agent);
    if (!paneId || panes.some((pane) => pane.pane_id === paneId)) continue;
    const workspaceId = workspaceIdOf(agent);
    panes.push(normalizePane({ pane_id: paneId, workspace_id: workspaceId }, agent, workspaceById.get(workspaceId) || null));
  }

  return rebuildWorkspaceViews({
    protocol: snapshot.protocol || null,
    boot_id: snapshot.boot_id || null,
    state_seq: snapshot.state_seq ?? snapshot.seq ?? null,
    server_time: snapshot.server_time || null,
    workspace_rows: workspaceRows.map((workspace) => ({ ...workspace })),
    tabs,
    panes,
    workspaces: [],
  });
}

export function applyBrowserEvent(view = {}, event = {}) {
  const type = event.type || event.event;
  const panes = [...(view.panes || [])];
  const id = paneIdOf(event) || paneIdOf(event.pane);
  const index = id ? panes.findIndex((pane) => pane.pane_id === id) : -1;

  if ((type === "pane_removed" || type === "agent_gone") && id) {
    return rebuildWorkspaceViews({ ...view, panes: panes.filter((pane) => pane.pane_id !== id) });
  }

  if ((type === "pane_upsert" || type === "pane_updated" || type === "pane_created") && id) {
    const raw = event.pane && typeof event.pane === "object" ? event.pane : event;
    const previous = index >= 0 ? panes[index] : null;
    const workspaceId = workspaceIdOf(raw) || previous?.workspace_id || workspaceIdOf(event);
    const row = (view.workspace_rows || []).find((workspace) => workspaceIdOf(workspace) === workspaceId) || null;
    const next = {
      ...(previous || normalizePane({ pane_id: id, workspace_id: workspaceId }, null, row)),
      ...normalizePane({ ...(previous || {}), ...raw, pane_id: id, workspace_id: workspaceId }, previous?.agent || null, row),
      last_output: previous?.last_output || null,
      current_summary: previous?.current_summary || null,
      last_event_at: event.at || previous?.last_event_at || null,
    };
    if (raw.status && !next.agent) next.status = raw.status;
    if (index >= 0) panes[index] = next;
    else panes.push(next);
    return rebuildWorkspaceViews({ ...view, panes });
  }

  if (["agent_working", "agent_settled"].includes(type) && id) {
    const previous = index >= 0 ? panes[index] : normalizePane({
      pane_id: id,
      workspace_id: workspaceIdOf(event),
      cwd: event.cwd,
      terminal_title: event.terminal_title,
    }, event, null);
    const nextAgent = normalizeAgent({
      ...(previous.agent || {}),
      ...event,
      name: event.agent || event.name || previous.agent?.name,
      kind: event.kind || event.agent || previous.agent?.kind,
      status: event.status,
      started_at: event.started_at || previous.agent?.started_at,
      last_activity_at: event.last_activity_at || event.at || previous.agent?.last_activity_at,
    });
    const next = {
      ...previous,
      workspace_id: workspaceIdOf(event) || previous.workspace_id,
      cwd: event.cwd || previous.cwd,
      terminal_title: event.terminal_title || previous.terminal_title,
      agent: nextAgent,
      status: nextAgent?.status || previous.status,
      last_event_at: event.at || previous.last_event_at || null,
    };
    if (index >= 0) panes[index] = next;
    else panes.push(next);
    return rebuildWorkspaceViews({ ...view, panes });
  }

  if (type === "agent_output" && id) {
    if (index < 0) return view;
    panes[index] = {
      ...panes[index],
      last_output: boundedTail(event.output, 2048),
      last_event_at: event.at || panes[index].last_event_at || null,
    };
    return rebuildWorkspaceViews({ ...view, panes });
  }

  if ((type === "workspace_upsert" || type === "workspace_updated") && workspaceIdOf(event)) {
    const workspaceId = workspaceIdOf(event);
    const workspaceRows = [...(view.workspace_rows || [])];
    const at = workspaceRows.findIndex((workspace) => workspaceIdOf(workspace) === workspaceId);
    const row = event.workspace && typeof event.workspace === "object" ? event.workspace : event;
    if (at >= 0) workspaceRows[at] = { ...workspaceRows[at], ...row, id: workspaceId };
    else workspaceRows.push({ ...row, id: workspaceId });
    return rebuildWorkspaceViews({ ...view, workspace_rows: workspaceRows });
  }

  if (type === "workspace_removed" && workspaceIdOf(event)) {
    const workspaceId = workspaceIdOf(event);
    return rebuildWorkspaceViews({
      ...view,
      workspace_rows: (view.workspace_rows || []).filter((workspace) => workspaceIdOf(workspace) !== workspaceId),
      panes: panes.filter((pane) => pane.workspace_id !== workspaceId),
      tabs: (view.tabs || []).filter((tab) => workspaceIdOf(tab) !== workspaceId),
    });
  }

  return view;
}

// Backward-compatible pure test helper from the first Phase A model.
export function applyPaneEvent(view = {}, event = {}) {
  const pane = event.pane && typeof event.pane === "object" ? event.pane : event;
  return applyBrowserEvent(view, {
    ...event,
    type: event.type || "pane_updated",
    pane,
    pane_id: event.pane_id || paneIdOf(pane),
  });
}

export function boundedTail(text, limit = 2048) {
  const max = Math.max(0, Number(limit) || 0);
  const value = String(text || "");
  return value.length <= max ? value : value.slice(-max);
}

export const browserStateInternals = Object.freeze({ paneIdOf, workspaceIdOf });
