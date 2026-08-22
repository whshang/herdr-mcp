/**
 * A-2: SnapshotCache — the single source of truth for agents/panes/workspaces/tabs.
 *
 * Bootstraps with `session.snapshot`, then keeps a live `events.subscribe`
 * connection and applies events incrementally. On transport break it reconnects
 * and re-snapshots; a 30s TTL fallback forces a fresh snapshot even if events
 * keep flowing (guards against a diverged cache).
 *
 * herdr_inspect and herdr_wait both read from this cache, eliminating the
 * P1-J class of inconsistency (same pane: inspect says working, wait says done).
 *
 * Event structure (verified live against the daemon — do NOT guess):
 *   {"event":"pane_updated","data":{"type":"pane_updated","pane":{<full PaneInfo>}}}
 * `data.pane` is a FULL post-change PaneInfo, so any pane event is an upsert.
 */
import { HerdrClient, HerdrEvent, HerdrResult } from "./herdr.js";
import { fetchSessionSnapshot } from "./snap-fallback.js";

const RESUBSCRIBE_SEC = 25;   // subscribe chunk (daemon may not send on idle)
const TTL_MS = 30_000;        // force a full re-snapshot at least this often

export interface AgentView {
  name: string | null;
  pane: string | null;
  status: string | null;
  workspace: string | null;
  cwd: string | null;
  started_at: string | null;
  last_activity_at: string | null;
  state_change_seq?: unknown;
  terminal_title?: unknown;
  session_ref?: unknown;
}

function parseSessionStarted(sessionValue: unknown): string | null {
  if (typeof sessionValue !== "string") return null;
  // herdr session filenames use DASHES in the time: 2026-08-14T07-29-14-679Z_<id>.jsonl
  const m = sessionValue.match(/(\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}(?:-\d+)?Z)/);
  if (!m) return null;
  const raw = m[1];
  // Normalize 07-29-14-679Z -> 07:29:14.679Z (readable ISO-ish)
  const parts = raw.match(/^(\d{4}-\d{2}-\d{2}T)(\d{2})-(\d{2})-(\d{2})(?:-(\d+))?Z$/);
  if (!parts) return raw;
  const ms = parts[6] ? "." + parts[6] : "";
  return parts[1] + parts[2] + ":" + parts[3] + ":" + parts[4] + ms + "Z";
}

export interface DigestEvent {
  cursor: number;
  at: string;
  type: string;
  workspace_id?: string;
  pane_id?: string;
  pane?: Record<string, unknown>;
  workspace?: Record<string, unknown>;
  tab?: Record<string, unknown>;
}

const DIGEST_HISTORY_MAX = 2048;

export class SnapshotCache {
  private readonly client: HerdrClient;
  private state: HerdrResult = {};
  private readonly digestHistory: DigestEvent[] = [];
  private digestCursor = 0;
  private readonly listeners = new Set<(ev: HerdrEvent) => void>();
  private lastActivityByPane = new Map<string, number>();
  private lastFullSnapAt = 0;
  private started = false;
  private loopError: Error | null = null;
  private _eventCount = 0;
  private _lastEventAt = 0;
  private readyResolve: (() => void) | null = null;
  private readyPromise: Promise<void>;

  constructor(client: HerdrClient) {
    this.client = client;
    this.readyPromise = new Promise<void>((resolve) => { this.readyResolve = resolve; });
  }

  /** Number of events applied since start (diagnostics / smoke test). */
  get eventCount(): number {
    return this._eventCount;
  }

  get lastEventAt(): number {
    return this._lastEventAt;
  }

  /** Begin bootstrap + background event loop. Idempotent. */
  start(): void {
    if (this.started) return;
    this.started = true;
    void this.runLoop();
  }

  get lastError(): Error | null {
    return this.loopError;
  }

  /** Snapshot of the cache (synchronously available after first start()). */
  getSnapshot(): HerdrResult {
    return this.state;
  }

  /** Resolves after the first successful bootstrap snapshot (or stays pending). */
  whenReady(): Promise<void> {
    return this.readyPromise;
  }

  /**
   * Subscribe to the normalized event stream AFTER each event is applied to
   * the cache (state already updated when listeners run). Returns an
   * unsubscribe function. Used by the /push SSE hub to fan out agent status
   * transitions without opening a second daemon subscription per client.
   */
  onEvent(fn: (ev: HerdrEvent) => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  private async runLoop(): Promise<void> {
    for (;;) {
      try {
        await this.bootstrap();
        this.readyResolve?.();
        this.readyResolve = null;
        this.lastFullSnapAt = Date.now();
        const subs = this.buildSubscriptions();
        for await (const ev of this.client.subscribe(subs, RESUBSCRIBE_SEC)) {
          this.applyEvent(ev);
          if (Date.now() - this.lastFullSnapAt > TTL_MS) break; // TTL fallback
        }
      } catch (e) {
        this.loopError = e instanceof Error ? e : new Error(String(e));
      }
      await new Promise<void>((r) => setTimeout(r, 250)); // back off before reconnect
    }
  }

  /** Full refresh from session.snapshot (bootstrap / reconnect / TTL). */
  private async bootstrap(): Promise<void> {
    const { snap } = await fetchSessionSnapshot(this.client);
    const prevAgents = (this.state["agents"] as unknown[]) ?? [];
    const prevLastAct = new Map(this.lastActivityByPane);
    this.state = snap;
    // Preserve last_activity timestamps for agents that persist across snapshots.
    for (const a of prevAgents) {
      const rec = a as Record<string, unknown>;
      if (typeof rec["pane_id"] === "string" && prevLastAct.has(rec["pane_id"] as string)) {
        this.lastActivityByPane.set(rec["pane_id"] as string, prevLastAct.get(rec["pane_id"] as string)!);
      }
    }
  }

  private buildSubscriptions(): { type: string; pane_id: string }[] {
    const panes = (this.state["panes"] as unknown[]) ?? [];
    const ids = new Set<string>();
    for (const p of panes) {
      const rec = (p ?? {}) as Record<string, unknown>;
      if (typeof rec["pane_id"] === "string") ids.add(rec["pane_id"] as string);
    }
    const agents = (this.state["agents"] as unknown[]) ?? [];
    for (const a of agents) {
      const rec = (a ?? {}) as Record<string, unknown>;
      if (typeof rec["pane_id"] === "string") ids.add(rec["pane_id"] as string);
    }

    const anchor = ids.size > 0 ? [...ids][0] : (typeof this.state["focused_pane_id"] === "string" ? this.state["focused_pane_id"] as string : "");
    if (!anchor) return [];
    const types = [
      "pane.updated", "pane.created", "pane.closed", "pane.focused", "pane.moved",
      "pane.exited", "pane.agent_detected", "pane.scroll_changed",
      "workspace.created", "workspace.updated", "workspace.closed", "workspace.focused", "workspace.renamed",
      "tab.created", "tab.closed", "tab.focused", "tab.renamed", "tab.moved",
    ];
    const subs = types.map((type) => ({ type, pane_id: anchor }));
    // pane.agent_status_changed is PANE-SCOPED on the daemon (verified: a
    // pane_id-less subscription delivers nothing, and only the subscribed
    // pane's transitions arrive). The /push hub and herdr_since digest depend
    // on live working→settled transitions for ARBITRARY agents, so subscribe
    // it for every pane, not just the anchor.
    for (const pid of ids) {
      subs.push({ type: "pane.agent_status_changed", pane_id: pid });
    }
    return subs;
  }

  /** Apply one event to the cache (verified live structure: data.pane full PaneInfo). */
  private applyEvent(ev: HerdrEvent): void {
    this._eventCount++;
    this._lastEventAt = Date.now();
    const data = (ev["data"] ?? ev) as Record<string, unknown>;
    const evType = (ev["event"] ?? data["type"]) as string | undefined;
    const pane = (data["pane"] ?? undefined) as Record<string, unknown> | undefined;
    const ws = (data["workspace"] ?? undefined) as Record<string, unknown> | undefined;
    const tab = (data["tab"] ?? undefined) as Record<string, unknown> | undefined;
    // ❺: append a compact event to the bounded digest history for herdr_since.
    this.digestCursor++;
    const dig: DigestEvent = { cursor: this.digestCursor, at: new Date(this._lastEventAt).toISOString(), type: evType ?? "unknown" };
    if (pane && typeof pane["pane_id"] === "string") dig.pane_id = pane["pane_id"] as string;
    if (pane) dig.pane = pane;
    if (ws && typeof ws["workspace_id"] === "string") { dig.workspace_id = ws["workspace_id"] as string; dig.workspace = ws; }
    if (tab && typeof tab["tab_id"] === "string") dig.tab = tab;
    this.digestHistory.push(dig);
    if (this.digestHistory.length > DIGEST_HISTORY_MAX) this.digestHistory.splice(0, this.digestHistory.length - DIGEST_HISTORY_MAX);

    const now = Date.now();

    // Pane events: upsert pane + its agent (data.pane is the post-change snapshot).
    // Close events arrive as {"data":{"pane_id":...,"type":"pane_closed",...},"event":"pane_closed"}
    // — pane_id sits DIRECTLY on data, there is NO data.pane object. Removal must
    // accept both shapes; otherwise the closed pane stays in the cache until the
    // next periodic full snapshot (measured: up to ~25s stale view).
    const closedPid = typeof pane?.["pane_id"] === "string" ? (pane["pane_id"] as string)
      : typeof data["pane_id"] === "string" ? (data["pane_id"] as string) : null;
    if (closedPid && (evType === "pane_closed" || evType === "pane.exited" || evType === "pane_closed_event")) {
      this.removePane(closedPid);
      return;
    }
    if (pane && typeof pane["pane_id"] === "string") {
      const pid = pane["pane_id"] as string;
      this.lastActivityByPane.set(pid, now);
      const panes = this.ensureArray("panes");
      const idx = panes.findIndex((p) => (p as Record<string, unknown>)["pane_id"] === pid);
      if (idx >= 0) panes[idx] = pane;
      else panes.push(pane);

      const agentName = typeof pane["agent"] === "string" ? pane["agent"] : null;
      if (agentName) {
        const agents = this.ensureArray("agents");
        const aIdx = agents.findIndex((a) => (a as Record<string, unknown>)["pane_id"] === pid);
        const agentRec: Record<string, unknown> = {
          agent: agentName, pane_id: pid, workspace_id: pane["workspace_id"],
          agent_status: pane["agent_status"], cwd: pane["cwd"] ?? pane["foreground_cwd"],
          terminal_title: pane["terminal_title"], state_change_seq: pane["state_change_seq"],
          agent_session: pane["agent_session"],
        };
        if (aIdx >= 0) agents[aIdx] = agentRec;
        else agents.push(agentRec);
      } else if (evType === "pane_closed" || !pane["agent"]) {
        const agents = this.ensureArray("agents");
        const aIdx = agents.findIndex((a) => (a as Record<string, unknown>)["pane_id"] === pid);
        if (aIdx >= 0) agents.splice(aIdx, 1);
      }
    }

    // Workspace events.
    if (ws && typeof ws["workspace_id"] === "string") {
      const wid = ws["workspace_id"] as string;
      if (evType === "workspace_closed") {
        const wss = this.ensureArray("workspaces");
        const i = wss.findIndex((w) => (w as Record<string, unknown>)["workspace_id"] === wid);
        if (i >= 0) wss.splice(i, 1);
      } else {
        const wss = this.ensureArray("workspaces");
        const i = wss.findIndex((w) => (w as Record<string, unknown>)["workspace_id"] === wid);
        if (i >= 0) wss[i] = ws;
        else wss.push(ws);
      }
    }

    // Tab events.
    if (tab && typeof tab["tab_id"] === "string") {
      const tid = tab["tab_id"] as string;
      if (evType === "tab_closed") {
        const tabs = this.ensureArray("tabs");
        const i = tabs.findIndex((t) => (t as Record<string, unknown>)["tab_id"] === tid);
        if (i >= 0) tabs.splice(i, 1);
      } else {
        const tabs = this.ensureArray("tabs");
        const i = tabs.findIndex((t) => (t as Record<string, unknown>)["tab_id"] === tid);
        if (i >= 0) tabs[i] = tab;
        else tabs.push(tab);
      }
    }

    // Fan out to /push hub listeners (best-effort; a broken listener must
    // never break the cache loop).
    for (const fn of this.listeners) {
      try {
        fn(ev);
      } catch {
        /* listener error is not a cache error */
      }
    }
  }

  private removePane(pid: string): void {
    const panes = (this.state["panes"] as unknown[]) ?? [];
    const i = panes.findIndex((p) => (p as Record<string, unknown>)["pane_id"] === pid);
    if (i >= 0) panes.splice(i, 1);
    const agents = (this.state["agents"] as unknown[]) ?? [];
    const a = agents.findIndex((x) => (x as Record<string, unknown>)["pane_id"] === pid);
    if (a >= 0) agents.splice(a, 1);
    this.lastActivityByPane.delete(pid);
  }

  private ensureArray(key: string): Record<string, unknown>[] {
    const arr = (this.state[key] as unknown[]) ?? [];
    if (!Array.isArray(this.state[key])) this.state[key] = arr;
    return arr as Record<string, unknown>[];
  }

  workspaceViews(): { id: string; label: string | null; roots: string[] }[] {
    const wss = (this.state["workspaces"] as unknown[]) ?? [];
    const agents = this.agentViews();
    const out: { id: string; label: string | null; roots: string[] }[] = [];
    for (const w of wss) {
      const rec = (w ?? {}) as Record<string, unknown>;
      const id = typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string)
        : typeof rec["id"] === "string" ? (rec["id"] as string) : null;
      if (!id) continue;
      const label = typeof rec["label"] === "string" && rec["label"] ? (rec["label"] as string) : null;
      const roots: string[] = [];
      const projects = (rec["projects"] as unknown[]) ?? [];
      for (const p of projects) {
        const pr = (p ?? {}) as Record<string, unknown>;
        if (typeof pr["root"] === "string" && !roots.includes(pr["root"] as string)) {
          roots.push(pr["root"] as string);
        }
      }
      if (!roots.length && typeof rec["cwd"] === "string") roots.push(rec["cwd"] as string);
      if (!roots.length) {
        for (const a of agents) {
          if (a.workspace === id && a.cwd && !roots.includes(a.cwd)) roots.push(a.cwd);
        }
      }
      if (!roots.length) {
        const panesRaw = (this.state["panes"] as unknown[]) ?? [];
        for (const p of panesRaw) {
          const pr = (p ?? {}) as Record<string, unknown>;
          if (pr["workspace_id"] !== id) continue;
          const cwd = typeof pr["cwd"] === "string" ? (pr["cwd"] as string)
            : typeof pr["foreground_cwd"] === "string" ? (pr["foreground_cwd"] as string) : null;
          if (cwd && !roots.includes(cwd)) roots.push(cwd);
        }
      }
      out.push({ id, label, roots });
    }
    return out;
  }

  /** All panes (incl. agentless terminals visible in herdr sidebar). */
  paneViews(): {
    id: string;
    workspace: string | null;
    cwd: string | null;
    label: string | null;
    agent: { name: string | null; status: string | null; terminal_title: string | null } | null;
  }[] {
    const panesRaw = (this.state["panes"] as unknown[]) ?? [];
    const byPane = new Map(this.agentViews().map((a) => [a.pane, a]));
    const out: {
      id: string;
      workspace: string | null;
      cwd: string | null;
      label: string | null;
      agent: { name: string | null; status: string | null; terminal_title: string | null } | null;
    }[] = [];
    for (const p of panesRaw) {
      const rec = (p ?? {}) as Record<string, unknown>;
      const id = typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null;
      if (!id) continue;
      const ag = byPane.get(id);
      out.push({
        id,
        workspace: typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null,
        cwd: typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
          : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null,
        label: typeof rec["label"] === "string" ? (rec["label"] as string) : null,
        agent: ag ? {
          name: ag.name,
          status: ag.status,
          terminal_title: typeof ag.terminal_title === "string" ? ag.terminal_title : null,
        } : null,
      });
    }
    return out;
  }

  /** Agents[] view with started_at (from session path) + last_activity_at added. */
  agentViews(): AgentView[] {
    const agents = (this.state["agents"] as unknown[]) ?? [];
    const out: AgentView[] = [];
    for (const a of agents) {
      const rec = (a ?? {}) as Record<string, unknown>;
      const pane = typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null;
      const sess = rec["agent_session"] as Record<string, unknown> | undefined;
      out.push({
        name: typeof rec["agent"] === "string" ? (rec["agent"] as string) : null,
        pane,
        status: typeof rec["agent_status"] === "string" ? (rec["agent_status"] as string)
          : typeof rec["status"] === "string" ? (rec["status"] as string) : null,
        workspace: typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null,
        cwd: typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
          : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null,
        started_at: parseSessionStarted(sess?.["value"]),
        last_activity_at: pane && this.lastActivityByPane.has(pane)
          ? new Date(this.lastActivityByPane.get(pane)!).toISOString()
          : null,
        state_change_seq: rec["state_change_seq"],
        terminal_title: rec["terminal_title"],
        session_ref: sess ?? undefined,
      });
    }
    return out;
  }

  /** ❺: incremental digest since a cursor. Returns the events with cursor > the
   * given value, plus current agents/workspaces summary and a new cursor. */
  digestSince(cursor: number): { cursor: number; events: DigestEvent[]; agents: AgentView[]; workspaces: Record<string, unknown>[] } {
    const events = cursor > 0 ? this.digestHistory.filter((e) => e.cursor > cursor) : this.digestHistory.slice(-Math.min(this.digestHistory.length, 64));
    const newCursor = this.digestCursor;
    const agents = this.agentViews();
    const workspaces = ((this.state["workspaces"] as unknown[]) ?? []) as Record<string, unknown>[];
    return { cursor: newCursor, events, agents, workspaces };
  }
}

let singleton: SnapshotCache | null = null;
/** Shared cache for the server process (single source of truth). */
export function getSnapshotCache(client: HerdrClient): SnapshotCache {
  if (!singleton) {
    singleton = new SnapshotCache(client);
    singleton.start();
  }
  return singleton;
}
