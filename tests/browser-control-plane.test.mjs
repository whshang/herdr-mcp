import assert from "node:assert/strict";
import test from "node:test";
import { performance } from "node:perf_hooks";
import {
  normalizeBrowserState,
  applyBrowserEvent,
  applyPaneEvent,
  boundedTail,
} from "../extension/browser-state.js";
import {
  createPinnedTarget,
  revalidatePinnedTarget,
  focusChangedDoesNotRetarget,
} from "../extension/target-pin.js";
import {
  ACTION_TYPES,
  ACTION_RISK,
  classifyAction,
  phaseAAvailability,
  buildActionDescriptor,
} from "../extension/control-actions.js";
import {
  controlCenterStats,
  createRenderCoalescer,
  runtimePresentation,
  workspaceAggregateStatus,
  workspaceRowsForPage,
} from "../extension/control-center-model.js";
import { createBrowserStateStore } from "../extension/browser-state-store.js";

function baseSnapshot() {
  return {
    server_time: "2026-08-27T09:00:00Z",
    workspaces: [{ id: "w1", label: "repo", roots: ["/repo"] }],
    panes: [
      { pane_id: "w1:p1", workspace_id: "w1", cwd: "/repo", terminal_title: "pi - repo" },
      { pane_id: "w1:p2", workspace_id: "w1", cwd: "/repo/tools" },
    ],
    agents: [{
      pane: "w1:p1",
      workspace: "w1",
      agent: "pi",
      name: "pi",
      status: "working",
      cwd: "/repo",
      started_at: "2026-08-27T08:55:00Z",
      last_activity_at: "2026-08-27T08:59:59Z",
      seq: 9,
    }],
  };
}

test("snapshot -> BrowserStateView normalizes current Rust push shape", () => {
  const view = normalizeBrowserState(baseSnapshot());
  assert.equal(view.workspaces.length, 1);
  assert.equal(view.workspaces[0].workspace_id, "w1");
  assert.equal(view.panes[0].agent.name, "pi");
  assert.equal(view.panes[0].project_root, "/repo");
  assert.equal(view.panes[0].status, "working");
});

test("BrowserStateStore UI adapter supports snapshot/get/event subscription", () => {
  const store = createBrowserStateStore();
  let emissions = 0;
  store.subscribe(() => { emissions += 1; });
  store.snapshot(baseSnapshot());
  assert.equal(store.get().panes.length, 2);
  store.event({ type: "agent_settled", pane: "w1:p1", workspace: "w1", agent: "pi", status: "done" });
  assert.equal(store.get().panes[0].status, "done");
  assert.equal(emissions, 2);
});

test("incremental pane/agent event updates existing row", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "agent_settled",
    pane: "w1:p1",
    workspace: "w1",
    agent: "pi",
    status: "done",
    at: "2026-08-27T09:01:00Z",
  });
  assert.equal(view.panes.find((pane) => pane.pane_id === "w1:p1").status, "done");
  assert.equal(view.panes.length, 2);
});

test("pane create adds a row and workspace projection", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "pane_created",
    pane: { pane_id: "w1:p3", workspace_id: "w1", cwd: "/repo/new" },
  });
  assert.equal(view.panes.length, 3);
  assert.equal(view.workspaces[0].panes.length, 3);
  assert.equal(view.panes.at(-1).status, "terminal-only");
});

test("push pane_upsert payload creates terminal-only and agent panes", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "pane_upsert",
    pane: "w1:p3",
    pane_id: "w1:p3",
    workspace: "w1",
    pane_data: { pane_id: "w1:p3", workspace_id: "w1", cwd: "/repo/terminal", agent: null },
  });
  assert.equal(view.panes.find((pane) => pane.pane_id === "w1:p3").status, "terminal-only");

  view = applyBrowserEvent(view, {
    type: "pane_upsert",
    pane: "w1:p4",
    pane_id: "w1:p4",
    workspace: "w1",
    pane_data: {
      pane_id: "w1:p4",
      workspace_id: "w1",
      cwd: "/repo/agent",
      agent: "pi",
      agent_status: "working",
    },
  });
  const agentPane = view.panes.find((pane) => pane.pane_id === "w1:p4");
  assert.equal(agentPane.agent.name, "pi");
  assert.equal(agentPane.status, "working");
});

test("push pane_upsert clears an exited agent while retaining the terminal pane", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "pane_upsert",
    pane: "w1:p1",
    pane_id: "w1:p1",
    workspace: "w1",
    pane_data: { pane_id: "w1:p1", workspace_id: "w1", cwd: "/repo", agent: null },
  });
  const pane = view.panes.find((item) => item.pane_id === "w1:p1");
  assert.equal(pane.agent, null);
  assert.equal(pane.status, "terminal-only");
});

test("focused pane upsert clears stale focus from its workspace peers", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "pane_upsert",
    pane: "w1:p1",
    workspace: "w1",
    pane_data: { pane_id: "w1:p1", workspace_id: "w1", cwd: "/repo", focused: true },
  });
  assert.equal(view.panes.find((pane) => pane.pane_id === "w1:p1").focused, true);
  view = applyBrowserEvent(view, {
    type: "pane_upsert",
    pane: "w1:p2",
    workspace: "w1",
    pane_data: { pane_id: "w1:p2", workspace_id: "w1", cwd: "/repo/tools", focused: true },
  });
  assert.equal(view.panes.find((pane) => pane.pane_id === "w1:p1").focused, false);
  assert.equal(view.panes.find((pane) => pane.pane_id === "w1:p2").focused, true);
});

test("pane remove removes a row", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, { type: "pane_removed", pane_id: "w1:p2" });
  assert.equal(view.panes.length, 1);
  assert.equal(view.panes.some((pane) => pane.pane_id === "w1:p2"), false);
});

test("push workspace lifecycle updates and removes workspace projection", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "workspace_upsert",
    workspace: "w2",
    workspace_data: { workspace_id: "w2", label: "repo-two" },
  });
  assert.equal(view.workspaces.find((workspace) => workspace.workspace_id === "w2").label, "repo-two");
  view = applyBrowserEvent(view, { type: "workspace_removed", workspace: "w2" });
  assert.equal(view.workspaces.some((workspace) => workspace.workspace_id === "w2"), false);
});

test("working -> done preserves agent identity", () => {
  let view = normalizeBrowserState(baseSnapshot());
  view = applyBrowserEvent(view, {
    type: "agent_settled",
    pane: "w1:p1",
    workspace: "w1",
    agent: "pi",
    status: "done",
  });
  const pane = view.panes.find((item) => item.pane_id === "w1:p1");
  assert.equal(pane.agent.name, "pi");
  assert.equal(pane.status, "done");
});

test("terminal-only pane remains visible", () => {
  const view = normalizeBrowserState(baseSnapshot());
  const pane = view.panes.find((item) => item.pane_id === "w1:p2");
  assert.equal(pane.agent, null);
  assert.equal(pane.status, "terminal-only");
});

test("target pin captures explicit pane identity", () => {
  const pane = normalizeBrowserState(baseSnapshot()).panes[0];
  const target = createPinnedTarget(pane, null, () => new Date("2026-08-27T09:00:00Z"));
  assert.equal(target.workspace_id, "w1");
  assert.equal(target.pane_id, "w1:p1");
  assert.match(target.target_revision, /w1:p1/);
  assert.equal(target.stale, false);
});

test("focus change does not retarget explicit pin", () => {
  const target = createPinnedTarget(normalizeBrowserState(baseSnapshot()).panes[0]);
  const after = focusChangedDoesNotRetarget(target, { focused_pane: "w1:p2" });
  assert.equal(after.pane_id, "w1:p1");
  assert.equal(after, target);
});

test("pinned pane removed -> stale", () => {
  const view = normalizeBrowserState(baseSnapshot());
  const target = createPinnedTarget(view.panes[0]);
  const next = revalidatePinnedTarget(target, view.panes.filter((pane) => pane.pane_id !== "w1:p1"));
  assert.equal(next.stale, true);
  assert.equal(next.stale_reason, "pane_removed");
});

test("reconnect snapshot revalidates unchanged target", () => {
  const first = normalizeBrowserState(baseSnapshot());
  const target = createPinnedTarget(first.panes[0]);
  const reconnect = normalizeBrowserState(baseSnapshot());
  const next = revalidatePinnedTarget(target, reconnect.panes);
  assert.equal(next.stale, false);
  assert.equal(next.pane_id, "w1:p1");
});

test("cwd changes do not stale the same pinned pane and agent session", () => {
  const first = normalizeBrowserState(baseSnapshot());
  const target = createPinnedTarget(first.panes[0]);
  const changed = baseSnapshot();
  changed.panes[0] = { ...changed.panes[0], cwd: "/repo/subdir" };
  changed.agents[0] = { ...changed.agents[0], cwd: "/repo/subdir" };
  const next = revalidatePinnedTarget(target, normalizeBrowserState(changed).panes);
  assert.equal(next.stale, false);
});

test("reconnect detects same pane occupied by a new agent session", () => {
  const first = normalizeBrowserState(baseSnapshot());
  const target = createPinnedTarget(first.panes[0]);
  const changed = baseSnapshot();
  changed.agents[0] = { ...changed.agents[0], started_at: "2026-08-27T09:02:00Z" };
  const next = revalidatePinnedTarget(target, normalizeBrowserState(changed).panes);
  assert.equal(next.stale, true);
  assert.equal(next.stale_reason, "target_revision_changed");
});

test("no duplicate pane rows after repeated upsert", () => {
  let view = normalizeBrowserState(baseSnapshot());
  for (let i = 0; i < 20; i += 1) {
    view = applyPaneEvent(view, {
      type: "pane_updated",
      pane_id: "w1:p1",
      pane: { pane_id: "w1:p1", workspace_id: "w1", cwd: "/repo" },
    });
  }
  assert.equal(view.panes.filter((pane) => pane.pane_id === "w1:p1").length, 1);
});

test("action risk classification covers browser control action types", () => {
  assert.equal(classifyAction(ACTION_TYPES.INSPECT), ACTION_RISK.READ);
  assert.equal(classifyAction(ACTION_TYPES.READ_TAIL), ACTION_RISK.READ);
  assert.equal(classifyAction(ACTION_TYPES.AGENT_PROMPT), ACTION_RISK.MUTATION);
  assert.equal(classifyAction(ACTION_TYPES.STEER), ACTION_RISK.PROVIDER_STEER);
  assert.equal(classifyAction(ACTION_TYPES.HERDR_METHOD), ACTION_RISK.UNKNOWN);
  assert.equal(classifyAction(ACTION_TYPES.TERMINAL_INPUT), ACTION_RISK.TERMINAL_MUTATION);
});

test("trusted prompt and steer probe are executable while high-risk modes stay preview-only", () => {
  const target = createPinnedTarget(normalizeBrowserState(baseSnapshot()).panes[0]);
  assert.equal(phaseAAvailability(ACTION_TYPES.AGENT_PROMPT, { target }).enabled, true);
  const prompt = buildActionDescriptor(ACTION_TYPES.AGENT_PROMPT, { target, text: "keep compatibility" });
  assert.equal(prompt.phase, "control-v1");
  assert.equal(prompt.executable, true);
  assert.equal(prompt.execution_mode, "trusted_extension");
  assert.equal(prompt.target.pane_id, "w1:p1");
  assert.equal(prompt.args.text, "keep compatibility");

  const steer = buildActionDescriptor(ACTION_TYPES.STEER, { target, text: "do not change schema" });
  assert.equal(steer.executable, true);
  assert.equal(steer.execution_mode, "provider_probe");

  const terminal = buildActionDescriptor(ACTION_TYPES.TERMINAL_INPUT, { target, text: "rm -rf /" });
  assert.equal(terminal.executable, false);
  assert.equal(terminal.execution_mode, "dry_run");
});

test("runtime authoritative target revision survives normalization and pinning", () => {
  const snapshot = baseSnapshot();
  snapshot.boot_id = "boot-1";
  snapshot.panes[0] = {
    ...snapshot.panes[0],
    revision: 7,
    target_revision: "btr1_authoritative",
    agent_session: { agent: "pi", source: "herdr:pi", kind: "path", value: "/tmp/session.jsonl" },
    control_capabilities: { agent_prompt: { available: true }, steer: { available: false, outcome: "unsupported_provider" } },
  };
  const pane = normalizeBrowserState(snapshot).panes[0];
  assert.equal(pane.target_revision, "btr1_authoritative");
  assert.equal(pane.revision, 7);
  assert.equal(pane.agent_session.value, "/tmp/session.jsonl");
  const target = createPinnedTarget(pane);
  assert.equal(target.target_revision, "btr1_authoritative");
  assert.equal(target.control_capabilities.steer.outcome, "unsupported_provider");
});

test("READ tail requires a live pinned target", () => {
  assert.equal(phaseAAvailability(ACTION_TYPES.READ_TAIL, { target: null }).enabled, false);
  const target = createPinnedTarget(normalizeBrowserState(baseSnapshot()).panes[0]);
  assert.equal(phaseAAvailability(ACTION_TYPES.READ_TAIL, { target }).enabled, true);
  assert.equal(phaseAAvailability(ACTION_TYPES.READ_TAIL, { target: { ...target, stale: true } }).enabled, false);
});

test("bounded tail never retains unbounded terminal history", () => {
  const source = "x".repeat(100_000) + "THE-END";
  const tail = boundedTail(source, 2048);
  assert.equal(tail.length, 2048);
  assert.equal(tail.endsWith("THE-END"), true);
});

test("render coalescer collapses event bursts into one render", () => {
  let renders = 0;
  let callback = null;
  const coalescer = createRenderCoalescer(() => { renders += 1; }, {
    setTimer: (fn) => { callback = fn; return 1; },
    clearTimer: () => {},
  });
  for (let i = 0; i < 100; i += 1) coalescer.schedule();
  assert.equal(renders, 0);
  callback();
  assert.equal(renders, 1);
});

test("hidden panel defers DOM work and flushes once visible", () => {
  let hidden = true;
  let renders = 0;
  const coalescer = createRenderCoalescer(() => { renders += 1; }, { isHidden: () => hidden });
  for (let i = 0; i < 50; i += 1) coalescer.schedule();
  assert.equal(renders, 0);
  hidden = false;
  assert.equal(coalescer.flush(), true);
  assert.equal(renders, 1);
});

test("runtime presentation distinguishes snapshot health from event reconnect", () => {
  assert.deepEqual(runtimePresentation({ runtimeHealthy: false, eventStreamHealthy: false }), {
    dot: "offline",
    text: "Runtime unavailable",
  });
  assert.deepEqual(runtimePresentation({ runtimeHealthy: true, eventStreamHealthy: false }), {
    dot: "working",
    text: "Runtime healthy · event stream reconnecting",
  });
  assert.deepEqual(runtimePresentation({ runtimeHealthy: true, eventStreamHealthy: true }), {
    dot: "healthy",
    text: "Runtime healthy",
  });
});

test("workspace aggregate status exposes the highest-priority live state", () => {
  assert.equal(workspaceAggregateStatus({ panes: [{ status: "done" }, { status: "working" }] }), "working");
  assert.equal(workspaceAggregateStatus({ panes: [{ status: "done" }, { status: "blocked" }] }), "blocked");
  assert.equal(workspaceAggregateStatus({ panes: [{ status: "idle" }, { status: "done" }] }), "idle");
  assert.equal(workspaceAggregateStatus({ panes: [{ status: "done" }] }), "done");
  assert.equal(workspaceAggregateStatus({ panes: [] }), "unknown");
});

test("workspace rows merge current-page binding with live state and preserve stale unbind targets", () => {
  const live = [
    { workspace_id: "w1", label: "Alpha", panes: [{ status: "done" }] },
    { workspace_id: "w2", label: "Beta", panes: [{ status: "working" }] },
    { workspace_id: "w3", label: "Gamma", panes: [] },
  ];
  const rows = workspaceRowsForPage(live, [
    { workspace_id: "w1", workspace_label: "Alpha" },
    { workspace_id: "w9", workspace_label: "Closed workspace" },
    { workspace_id: "w9", workspace_label: "Closed workspace" },
  ]);
  assert.deepEqual(rows.map((row) => row.workspace_id), ["w1", "w9", "w2", "w3"]);
  assert.equal(rows[1].binding_missing, true);
  assert.equal(rows[1].label, "Closed workspace");
  assert.deepEqual(live.map((row) => row.workspace_id), ["w1", "w2", "w3"], "input order is not mutated");
});

test("50-pane state and incremental burst stay small and fast", () => {
  const panes = Array.from({ length: 50 }, (_, index) => ({
    pane_id: `w${Math.floor(index / 10)}:p${index}`,
    workspace_id: `w${Math.floor(index / 10)}`,
    cwd: `/repo/${index}`,
  }));
  const workspaces = Array.from({ length: 5 }, (_, index) => ({ id: `w${index}`, label: `repo-${index}`, roots: [`/repo`] }));
  const agents = panes.filter((_, index) => index % 3 === 0).map((pane) => ({
    pane: pane.pane_id,
    workspace: pane.workspace_id,
    agent: "pi",
    status: "working",
  }));
  const started = performance.now();
  let view = normalizeBrowserState({ workspaces, panes, agents });
  for (let index = 0; index < 200; index += 1) {
    const pane = panes[index % panes.length];
    view = applyBrowserEvent(view, {
      type: index % 2 ? "agent_working" : "agent_settled",
      pane: pane.pane_id,
      workspace: pane.workspace_id,
      agent: "pi",
      status: index % 2 ? "working" : "done",
    });
  }
  const elapsed = performance.now() - started;
  assert.equal(view.panes.length, 50);
  assert.equal(controlCenterStats(view).panes, 50);
  assert.ok(elapsed < 500, `model work took ${elapsed.toFixed(1)}ms`);
});
