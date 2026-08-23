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

  cache.applyEvent({ event: "workspace_closed", data: { type: "workspace_closed", workspace_id: "wDead" } });
  const snap = cache.getSnapshot();
  assert.deepEqual(snap.workspaces, [{ workspace_id: "wLive", label: "live" }]);
  assert.deepEqual(snap.panes, [{ pane_id: "wLive:p1", workspace_id: "wLive" }]);
  assert.deepEqual(snap.agents, []);
  assert.deepEqual(snap.tabs, [{ tab_id: "wLive:t1", workspace_id: "wLive" }]);
});
