/**
 * Avoid hanging on session.snapshot — prefer list APIs when the daemon control
 * plane is slow or returns TaskGroup (same strategy as ctmc: stable tool surface
 * must not depend on one fragile aggregate call).
 */
import { HerdrClient, HerdrResult } from "./herdr.js";
import { HERDR_SNAPSHOT_TIMEOUT_MS } from "./timeouts.js";

export async function snapFromListApis(c: HerdrClient): Promise<HerdrResult | null> {
  try {
    const wsR = await c.call("workspace.list", {}, HERDR_SNAPSHOT_TIMEOUT_MS);
    const workspaces = Array.isArray(wsR["workspaces"]) ? (wsR["workspaces"] as unknown[]) : [];
    let panes: unknown[] = [];
    let agents: unknown[] = [];
    try {
      const pR = await c.call("pane.list", {}, HERDR_SNAPSHOT_TIMEOUT_MS);
      if (Array.isArray(pR["panes"])) panes = pR["panes"] as unknown[];
    } catch { /* optional */ }
    try {
      const aR = await c.call("agent.list", {}, HERDR_SNAPSHOT_TIMEOUT_MS);
      if (Array.isArray(aR["agents"])) agents = aR["agents"] as unknown[];
    } catch { /* optional */ }
    if (workspaces.length === 0 && panes.length === 0 && agents.length === 0) return null;
    return { type: "assembled_from_lists", workspaces, panes, agents };
  } catch {
    return null;
  }
}

/** session.snapshot with bounded wait; list-API assembly on failure. */
export async function fetchSessionSnapshot(
  c: HerdrClient,
  timeoutMs = HERDR_SNAPSHOT_TIMEOUT_MS,
): Promise<{ snap: HerdrResult; source: "snapshot" | "lists" }> {
  try {
    const r = await c.call("session.snapshot", {}, timeoutMs);
    const snap = ((r["snapshot"] ?? r) as HerdrResult) || {};
    return { snap, source: "snapshot" };
  } catch {
    const assembled = await snapFromListApis(c);
    if (assembled) return { snap: assembled, source: "lists" };
    throw new Error("session.snapshot and list APIs unavailable");
  }
}
