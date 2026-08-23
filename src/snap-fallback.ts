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

/**
 * session.snapshot is useful because it carries aggregate fields that the list
 * APIs do not expose, but on some Herdr builds it can retain recently closed
 * workspaces/panes longer than workspace.list/pane.list. Treat the dedicated
 * list APIs as authoritative for those live collections whenever they answer.
 */
export async function reconcileSnapshotWithListApis(
  c: HerdrClient,
  snap: HerdrResult,
  timeoutMs = HERDR_SNAPSHOT_TIMEOUT_MS,
): Promise<HerdrResult> {
  const out: HerdrResult = { ...snap };
  const specs: Array<[method: string, key: string]> = [
    ["workspace.list", "workspaces"],
    ["pane.list", "panes"],
    ["agent.list", "agents"],
  ];
  const results = await Promise.allSettled(
    specs.map(([method]) => c.call(method, {}, timeoutMs)),
  );
  for (let i = 0; i < specs.length; i++) {
    const [, key] = specs[i];
    const result = results[i];
    if (result.status !== "fulfilled") continue;
    const value = result.value[key];
    if (Array.isArray(value)) out[key] = value;
  }
  return out;
}

/** session.snapshot with bounded wait; list-API assembly on failure. */
export async function fetchSessionSnapshot(
  c: HerdrClient,
  timeoutMs = HERDR_SNAPSHOT_TIMEOUT_MS,
): Promise<{ snap: HerdrResult; source: "snapshot" | "lists" }> {
  try {
    const r = await c.call("session.snapshot", {}, timeoutMs);
    const raw = ((r["snapshot"] ?? r) as HerdrResult) || {};
    const snap = await reconcileSnapshotWithListApis(c, raw, timeoutMs);
    return { snap, source: "snapshot" };
  } catch {
    const assembled = await snapFromListApis(c);
    if (assembled) return { snap: assembled, source: "lists" };
    throw new Error("session.snapshot and list APIs unavailable");
  }
}
