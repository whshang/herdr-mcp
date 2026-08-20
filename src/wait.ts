/**
 * herdr_wait: event-driven agent lifecycle wait, Cloudflare-safe (90s chunks).
 */
import { HerdrClient, HerdrError, HerdrEvent, HerdrResult } from "./herdr.js";

const SETTLED = new Set(["idle", "blocked", "done"]);
const CLOUDFLARE_SAFE_SEC = 90;

interface AgentFields { name: string | null; pane: string | null; workspace: string | null; status: string | null }

interface Candidate {
  name: string | null;
  pane: string | null;
  workspace: string | null;
  status: string | null;
}

type AgentResolution =
  | { kind: "match"; pane: string | null; status: string | null }
  | { kind: "ambiguous"; candidates: Candidate[] };

function agentFields(obj: HerdrEvent | HerdrResult): AgentFields {
  const agent = obj["agent"];
  const name = typeof agent === "string" ? agent : null;
  const pane = typeof obj["pane_id"] === "string" ? obj["pane_id"] : null;
  const workspace = typeof obj["workspace_id"] === "string" ? obj["workspace_id"] : null;
  let status = obj["agent_status"] ?? obj["status"];
  if (typeof status === "object" && status !== null && "status" in (status as object)) {
    status = (status as Record<string, unknown>)["status"];
  }
  return { name, pane, workspace, status: typeof status === "string" ? status : null };
}

async function readSummary(client: HerdrClient, target: string, status: string | null, readTarget?: string): Promise<Record<string, unknown>> {
  // Always read from the resolved pane/name (never the raw target, which may
  // be a workspace:name form the herdr socket does not understand).
  const rt = readTarget ?? target;
  try {
    const r = await client.call("agent.read", {
      target: rt, source: "recent_unwrapped", lines: 120, strip_ansi: true,
    }, 10000);
    const read = (r["read"] ?? r) as Record<string, unknown>;
    const text = read["content"] ?? read["text"] ?? read["output"];
    return {
      ok: true, status, target, read_target: rt,
      pane_id: read["pane_id"],
      output: typeof text === "string" ? text.slice(0, 4000) : read,
    };
  } catch (e) {
    const err = e instanceof HerdrError ? { code: e.code, message: e.message } : { code: "unknown", message: "read failed" };
    return { ok: true, status, target, read_target: rt, read_error: err };
  }
}

/**
 * Resolve a herdr_wait target to a single agent (or signal ambiguity).
 *
 * Reads from `getState` when provided (shared SnapshotCache — the same source
 * herdr_inspect uses), else falls back to a direct client snapshot. This is the
 * A-2 single-source-of-truth guarantee: wait and inspect never disagree.
 *
 * Resolution order:
 *  1. If the target matches a pane_id (e.g. "wH:p1") it is unambiguous.
 *  2. If the target contains ":" but is not a pane_id, treat it as
 *     "workspace:name" (e.g. "wH:omp") — the unique agent with that name in
 *     that workspace.
 *  3. Otherwise it is a bare name (e.g. "omp"): collect ALL matches; exactly
 *     one is used, more than one is ambiguous.
 */
async function findAgent(
  client: HerdrClient,
  target: string,
  getState?: () => HerdrResult,
): Promise<AgentResolution> {
  try {
    const snap = getState ? getState() : await client.snapshot();
    const agents = snap["agents"];
    if (!Array.isArray(agents)) return { kind: "match", pane: null, status: null };
    const candidates: Candidate[] = [];
    for (const a of agents) {
      if (typeof a !== "object" || a === null) continue;
      const f = agentFields(a as HerdrResult);
      candidates.push({ name: f.name, pane: f.pane, workspace: f.workspace, status: f.status });
    }

    // 1) pane_id: unambiguous direct match.
    if (target.includes(":")) {
      const byPane = candidates.filter((c) => c.pane === target);
      if (byPane.length === 1) {
        return { kind: "match", pane: byPane[0].pane, status: byPane[0].status };
      }
      // 2) workspace:name (e.g. "wH:omp").
      const sep = target.indexOf(":");
      const ws = target.slice(0, sep);
      const name = target.slice(sep + 1);
      const byWsName = candidates.filter((c) => c.workspace === ws && c.name === name);
      if (byWsName.length === 1) {
        return { kind: "match", pane: byWsName[0].pane, status: byWsName[0].status };
      }
      if (byWsName.length > 1) {
        return { kind: "ambiguous", candidates: byWsName };
      }
      return { kind: "match", pane: null, status: null };
    }

    // 3) bare name — all matches.
    const byName = candidates.filter((c) => c.name === target);
    if (byName.length === 1) {
      return { kind: "match", pane: byName[0].pane, status: byName[0].status };
    }
    if (byName.length > 1) {
      return { kind: "ambiguous", candidates: byName };
    }
    return { kind: "match", pane: null, status: null };
  } catch {
    return { kind: "match", pane: null, status: null };
  }
}

function ambiguousResult(target: string, candidates: Candidate[]): Record<string, unknown> {
  return {
    ok: false,
    reason: "ambiguous_target",
    target,
    candidates: candidates.map((c) => ({
      name: c.name,
      pane: c.pane,
      workspace: c.workspace,
      status: c.status,
    })),
    hint: "multiple agents share this name — use pane_id (e.g. wH:p1) or workspace:name (e.g. wH:omp) to disambiguate",
  };
}

export async function waitForAgent(
  client: HerdrClient,
  target: string,
  until: string[],
  timeoutSec: number,
  getState?: () => HerdrResult,
): Promise<Record<string, unknown>> {
  const untilSet = until.length > 0 ? new Set(until) : SETTLED;
  const chunkSec = Math.min(timeoutSec, CLOUDFLARE_SAFE_SEC);
  const start = Date.now();

  // fast path: already settled?
  const found = await findAgent(client, target, getState);
  if (found.kind === "ambiguous") {
    return ambiguousResult(target, found.candidates);
  }
  if (found.status && untilSet.has(found.status)) {
    return readSummary(client, target, found.status, found.pane ?? undefined);
  }
  const paneId = found.pane ?? target;

  // subscribe to this pane's agent_status_changed until settle or chunk timeout
  try {
    for await (const ev of client.subscribe(
      [{ type: "pane.agent_status_changed", pane_id: paneId }],
      chunkSec,
    )) {
      const f = agentFields(ev);
      if (f.pane !== paneId) continue;
      if (f.status && untilSet.has(f.status)) {
        return readSummary(client, target, f.status, paneId);
      }
      if ((Date.now() - start) / 1000 > chunkSec) break;
    }
  } catch (e) {
    const err = e instanceof HerdrError ? { code: e.code, message: e.message } : { code: "unknown" };
    return { ok: false, reason: "subscribe_error", ...err, target };
  }

  // chunk exhausted — still_running, caller re-calls to extend
  const now = await findAgent(client, target, getState);
  if (now.kind === "ambiguous") {
    return ambiguousResult(target, now.candidates);
  }
  return {
    ok: false,
    reason: "still_running",
    target,
    status: now.status,
    until: [...untilSet].sort(),
    elapsed_ms: Date.now() - start,
    hint: "call herdr_wait again to continue waiting",
  };
}
