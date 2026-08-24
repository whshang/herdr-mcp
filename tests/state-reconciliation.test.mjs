import test from "node:test";
import assert from "node:assert/strict";

import { fetchSessionSnapshot } from "../dist/snap-fallback.js";
import { SnapshotCache } from "../dist/state.js";

test("snapshot collections are reconciled with authoritative list APIs", async () => {
  const client = {
    async call(method) {
      if (method === "session.snapshot") {
        return {
          snapshot: {
            focused_pane_id: "wLive:p1",
            tabs: [{ tab_id: "wLive:t1", workspace_id: "wLive" }],
            workspaces: [{ workspace_id: "wDead", label: "stale" }],
            panes: [{ pane_id: "wDead:p1", workspace_id: "wDead" }],
            agents: [{ pane_id: "wDead:p1", workspace_id: "wDead", agent: "pi" }],
          },
        };
      }
      if (method === "workspace.list") return { workspaces: [{ workspace_id: "wLive", label: "live" }] };
      if (method === "pane.list") return { panes: [{ pane_id: "wLive:p1", workspace_id: "wLive" }] };
      if (method === "agent.list") return { agents: [] };
      throw new Error(`unexpected method ${method}`);
    },
  };

  const { snap, source } = await fetchSessionSnapshot(client, 1000);
  assert.equal(source, "snapshot");
  assert.deepEqual(snap.workspaces, [{ workspace_id: "wLive", label: "live" }]);
  assert.deepEqual(snap.panes, [{ pane_id: "wLive:p1", workspace_id: "wLive" }]);
  assert.deepEqual(snap.agents, []);
  assert.deepEqual(snap.tabs, [{ tab_id: "wLive:t1", workspace_id: "wLive" }]);
});

test("workspace_closed direct-id event removes the whole workspace scope", () => {
  const cache = new SnapshotCache({});
  cache.state = {
    workspaces: [
      { workspace_id: "wLive", label: "live" },
      { workspace_id: "wDead", label: "dead" },
    ],
    panes: [
      { pane_id: "wLive:p1", workspace_id: "wLive" },
      { pane_id: "wDead:p1", workspace_id: "wDead" },
      { pane_id: "wDead:p2", workspace_id: "wDead" },
    ],
    agents: [{ pane_id: "wDead:p1", workspace_id: "wDead", agent: "grok" }],
    tabs: [
      { tab_id: "wLive:t1", workspace_id: "wLive" },
      { tab_id: "wDead:t1", workspace_id: "wDead" },
    ],
  };
  cache.liveWorkspaceIds = new Set(["wLive", "wDead"]);

  cache.applyEvent({ event: "workspace_closed", data: { type: "workspace_closed", workspace_id: "wDead" } });
  cache.applyEvent({
    event: "workspace_created",
    data: { type: "workspace_created", workspace: { workspace_id: "wDead", label: "stale replay" } },
  });
  const snap = cache.getSnapshot();
  assert.deepEqual(snap.workspaces, [{ workspace_id: "wLive", label: "live" }]);
  assert.deepEqual(snap.panes, [{ pane_id: "wLive:p1", workspace_id: "wLive" }]);
  assert.deepEqual(snap.agents, []);
  assert.deepEqual(snap.tabs, [{ tab_id: "wLive:t1", workspace_id: "wLive" }]);
});

test("orphan events cannot resurrect a workspace missing from workspace.list", () => {
  const cache = new SnapshotCache({});
  cache.state = {
    workspaces: [{ workspace_id: "wLive", label: "live" }],
    panes: [{ pane_id: "wLive:p1", workspace_id: "wLive" }],
    agents: [],
    tabs: [{ tab_id: "wLive:t1", workspace_id: "wLive" }],
  };
  cache.liveWorkspaceIds = new Set(["wLive"]);

  cache.applyEvent({
    event: "pane_updated",
    data: { type: "pane_updated", pane: { pane_id: "wDead:p1", workspace_id: "wDead", cwd: "/tmp/dead" } },
  });
  cache.applyEvent({
    event: "workspace_updated",
    data: { type: "workspace_updated", workspace: { workspace_id: "wDead", label: "dead" } },
  });
  cache.applyEvent({
    event: "tab_updated",
    data: { type: "tab_updated", tab: { tab_id: "wDead:t1", workspace_id: "wDead" } },
  });

  const snap = cache.getSnapshot();
  assert.deepEqual(snap.workspaces, [{ workspace_id: "wLive", label: "live" }]);
  assert.deepEqual(snap.panes, [{ pane_id: "wLive:p1", workspace_id: "wLive" }]);
  assert.deepEqual(snap.tabs, [{ tab_id: "wLive:t1", workspace_id: "wLive" }]);
});

test("unknown workspace_created waits for workspace.list authority before admission", async () => {
  const client = {
    async call(method) {
      if (method === "session.snapshot") return { snapshot: { workspaces: [], panes: [], agents: [], tabs: [] } };
      if (method === "workspace.list") return { workspaces: [{ workspace_id: "wNew", label: "new" }] };
      if (method === "pane.list") return { panes: [{ pane_id: "wNew:p1", workspace_id: "wNew", cwd: "/tmp/new" }] };
      if (method === "agent.list") return { agents: [] };
      throw new Error(`unexpected method ${method}`);
    },
  };
  const cache = new SnapshotCache(client);
  cache.state = { workspaces: [], panes: [], agents: [], tabs: [] };
  cache.liveWorkspaceIds = new Set();
  cache.lastFullSnapAt = 123;

  cache.applyEvent({
    event: "workspace_created",
    data: { type: "workspace_created", workspace: { workspace_id: "wNew", label: "new" } },
  });
  assert.deepEqual(cache.getSnapshot().workspaces, []);
  assert.equal(cache.lastFullSnapAt, 0);

  await cache.bootstrap();
  assert.deepEqual(cache.getSnapshot().workspaces, [{ workspace_id: "wNew", label: "new" }]);
  cache.applyEvent({
    event: "pane_updated",
    data: { type: "pane_updated", pane: { pane_id: "wNew:p1", workspace_id: "wNew", cwd: "/tmp/new" } },
  });

  assert.equal(cache.getSnapshot().panes.length, 1);
  assert.equal(cache.getSnapshot().panes[0].workspace_id, "wNew");
});

test("explicit authoritative refresh replaces stale workspace metadata", async () => {
  const client = {
    async call(method) {
      if (method === "workspace.list") {
        return { workspaces: [{ workspace_id: "w68", label: "herdr-mcp" }] };
      }
      if (method === "pane.list") {
        return { panes: [{ pane_id: "w68:p1", workspace_id: "w68", cwd: "/repo/herdr-mcp" }] };
      }
      if (method === "agent.list") return { agents: [] };
      throw new Error(`unexpected method ${method}`);
    },
  };
  const cache = new SnapshotCache(client);
  cache.state = {
    workspaces: [{ workspace_id: "w68", label: "old-session-label" }],
    panes: [{ pane_id: "w68:p1", workspace_id: "w68", cwd: "/repo/old" }],
    agents: [],
  };
  cache.liveWorkspaceIds = new Set(["w68"]);

  await cache.refreshAuthoritative();

  assert.deepEqual(cache.workspaceViews(), [{
    id: "w68",
    label: "herdr-mcp",
    roots: ["/repo/herdr-mcp"],
  }]);
});
