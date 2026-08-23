/**
 * /push — browser-extension push channel (herdr → web wake-up).
 *
 * Direction: when a herdr agent (for example, p1) finishes work
 * (working → idle/done/blocked), notify the browser extension over SSE so it
 * can write a message into the bound web chat input and submit it.
 *
 * ## Design decisions (phase 1)
 *
 * 1. **Reuse the existing SSE event source**: the server already maintains a
 *    live events.subscribe connection (SnapshotCache, A-2, 25s chunks plus a
 *    30s snapshot TTL). PushHub attaches an onEvent listener to the cache
 *    (via the hook in state.ts) instead of opening another daemon subscription
 *    per client. This adds no sockets, uses normalized events, and avoids polling.
 *
 * 2. **Reuse HERDR_MCP_TOKEN for authentication**: no separate token mechanism.
 *    - The threat model is identical: token holders can already read all agent
 *      state through herdr_inspect, so the push channel exposes nothing extra.
 *    - This requires no additional secret management: one token in the plist
 *      and one token in the extension configuration.
 *    - When /mcp runs locally without a token, /push behaves the same way
 *      (open when AUTH_TOKEN is empty).
 *    The extension fetches the local 127.0.0.1 endpoint from its background
 *    worker and can attach an Authorization header, unlike EventSource.
 *
 * 3. **Event filtering**: the server optionally filters by ?agent=NAME,
 *    ?pane=PANE_ID, or ?workspace=WS_ID. Bindings remain extension-side, so
 *    the server is stateless. The extension binds a **workspace** by default
 *    and receives working/settled/output events for any agent in that scope.
 *    The initial hello event contains an authoritative, optionally scoped snapshot.
 *
 * 4. **Recovery after missed events**: if an agent is already settled when
 *    the extension connects, the hello snapshot reports it as settled. The
 *    extension deduplicates by the last notified settle sequence, so the server
 *    does not replay history. PushHub reconciles with cache.agentViews() every
 *    10 seconds to recover state transitions missed by the event stream.
 */
import { Router, Request, Response } from "express";
import type { Express } from "express";
import { HerdrClient, HerdrEvent } from "./herdr.js";
import { getSnapshotCache, SnapshotCache, AgentView } from "./state.js";
import { cleanTerminalOutput } from "./clean.js";
import { queryMcpActivity } from "./mcp-activity.js";

const SETTLED = new Set(["idle", "done", "blocked"]);
const RECONCILE_MS = 10_000;   // periodic re-seed vs cache.agentViews() (missed-event recovery)
const HEARTBEAT_MS = 15_000;   // SSE keepalive comment (proxy-safe)
const OUTPUT_READ_MS = 2000;   // bounded best-effort agent.read for settled output snippet
const OUTPUT_LINES = 40;
const WORKING_SNIPPET_MS = 60_000; // while working, refresh output snippet at most once/min
const DEBUG = process.env.HERDR_MCP_PUSH_DEBUG === "1";

/** Match extension binding-core progressOutputFingerprint — spinner/clock noise ≠ new output. */
function progressSnippetFingerprint(output: string | undefined): string {
  return String(output ?? "")
    .replace(/\u001b\[[0-9;]*[A-Za-z]/g, "")
    .replace(/[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏◐◓◑◒■□▪▫•●○◎◉]+/g, "")
    .replace(/\b\d{1,2}:\d{2}(:\d{2})?\b/g, "")
    .replace(/\b\d+[ms]\b/gi, "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(-1200);
}

interface PushFilters {
  agent?: string;
  pane?: string;
  workspace?: string;
}

interface PushClient {
  res: Response;
  filters: PushFilters;
}

interface AgentRef {
  name: string | null;
  pane: string | null;
  status: string | null;
  workspace: string | null;
  cwd: string | null;
  label: string | null;
  terminalTitle: string | null;
  seq: number | null;
}

/** Extract agent-ish fields from a normalized cache event (defensive). */
function agentRefOf(ev: HerdrEvent): AgentRef {
  const data = (ev["data"] ?? ev) as Record<string, unknown>;
  const pane = (data["pane"] as Record<string, unknown> | undefined) ?? data;
  const name = typeof pane["agent"] === "string" ? (pane["agent"] as string)
    : typeof data["agent"] === "string" ? (data["agent"] as string) : null;
  const paneId = typeof pane["pane_id"] === "string" ? (pane["pane_id"] as string)
    : typeof data["pane_id"] === "string" ? (data["pane_id"] as string) : null;
  let status = pane["agent_status"] ?? data["agent_status"] ?? pane["status"] ?? data["status"];
  if (typeof status === "object" && status !== null && "status" in (status as object)) {
    status = (status as Record<string, unknown>)["status"];
  }
  return {
    name,
    pane: paneId,
    status: typeof status === "string" ? status : null,
    workspace: typeof pane["workspace_id"] === "string" ? (pane["workspace_id"] as string)
      : typeof data["workspace_id"] === "string" ? (data["workspace_id"] as string) : null,
    cwd: typeof pane["cwd"] === "string" ? (pane["cwd"] as string)
      : typeof pane["foreground_cwd"] === "string" ? (pane["foreground_cwd"] as string) : null,
    label: typeof pane["label"] === "string" ? (pane["label"] as string) : null,
    terminalTitle: typeof pane["terminal_title"] === "string" ? (pane["terminal_title"] as string)
      : typeof pane["terminal_title_stripped"] === "string" ? (pane["terminal_title_stripped"] as string) : null,
    seq: typeof pane["state_change_seq"] === "number" ? (pane["state_change_seq"] as number)
      : typeof data["state_change_seq"] === "number" ? (data["state_change_seq"] as number) : null,
  };
}

function eventNameOf(ev: HerdrEvent): string {
  const data = (ev["data"] ?? ev) as Record<string, unknown>;
  const e = ev["event"] ?? data["type"];
  return typeof e === "string" ? e : "unknown";
}

function sseWrite(res: Response, event: string, data: unknown): void {
  res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
}

class PushHub {
  private readonly cache: SnapshotCache;
  private readonly client: HerdrClient;
  private readonly clients = new Set<PushClient>();
  private lastStatusByPane = new Map<string, { status: string | null; name: string | null }>();
  private readonly unsub: () => void;
  private reconcileTimer: NodeJS.Timeout | null = null;
  private heartbeatTimers = new Set<NodeJS.Timeout>();
  private lastOutputSnippet = new Map<string, { at: number; output: string }>();
  private lastWorkingSnippetAt = new Map<string, number>();

  constructor(client: HerdrClient) {
    this.client = client;
    this.cache = getSnapshotCache(client);
    // Seed from the authoritative cache view so a fresh server never emits
    // spurious transitions for agents already settled before startup.
    for (const a of this.cache.agentViews()) {
      if (a.pane) this.lastStatusByPane.set(a.pane, { status: a.status, name: a.name });
    }
    this.unsub = this.cache.onEvent((ev) => this.onEvent(ev));
    this.reconcileTimer = setInterval(() => this.reconcile(), RECONCILE_MS);
    this.reconcileTimer.unref();
  }

  private onEvent(ev: HerdrEvent): void {
    const name = eventNameOf(ev);
    const ref = agentRefOf(ev);
    if (DEBUG) console.log(`[push] event=${name} pane=${ref.pane} status=${ref.status} agent=${ref.name}`);
    if (!ref.pane) return;

    // Pane removed — notify so clients can clear busy state.
    if (/closed|exited/.test(name)) {
      const prev = this.lastStatusByPane.get(ref.pane);
      if (prev?.status && prev.status !== "gone") {
        this.lastStatusByPane.set(ref.pane, { status: "gone", name: prev.name });
        this.emit("agent_gone", { agent: prev.name, pane: ref.pane, at: new Date().toISOString() });
      }
      return;
    }

    const prev = this.lastStatusByPane.get(ref.pane);
    this.lastStatusByPane.set(ref.pane, { status: ref.status, name: ref.name ?? prev?.name ?? null });
    if (!ref.status || prev?.status === ref.status) return;

    const base = {
      agent: ref.name,
      pane: ref.pane,
      workspace: ref.workspace,
      cwd: ref.cwd,
      label: ref.label,
      terminal_title: ref.terminalTitle,
      seq: ref.seq ?? undefined,
      at: new Date().toISOString(),
    };
    if (ref.status === "working") {
      this.emit("agent_working", { ...base, status: ref.status });
      return;
    }
    if (SETTLED.has(ref.status) && prev?.status === "working") {
      this.emit("agent_settled", { ...base, status: ref.status });
      void this.readOutputSnippet(ref); // async enrichment, never blocks fan-out
    }
  }

  /** Best-effort bounded agent.read → emit agent_output only when text changes. */
  private async readOutputSnippet(ref: AgentRef): Promise<void> {
    if (!ref.pane) return;
    try {
      const r = await this.client.call("agent.read", {
        target: ref.pane, source: "recent_unwrapped", lines: OUTPUT_LINES, strip_ansi: true,
      }, OUTPUT_READ_MS);
      const rd = (r["read"] ?? r) as Record<string, unknown>;
      const text = String(rd["content"] ?? rd["text"] ?? rd["output"] ?? "");
      const output = cleanTerminalOutput(text).slice(0, 2000);
      if (!output.trim()) return;
      const prev = this.lastOutputSnippet.get(ref.pane)?.output;
      // Fingerprint-level deduplication: spinner and clock changes are not new summaries.
      if (progressSnippetFingerprint(prev) === progressSnippetFingerprint(output)) return;
      this.lastOutputSnippet.set(ref.pane, { at: Date.now(), output });
      this.emit("agent_output", {
        agent: ref.name, pane: ref.pane, workspace: ref.workspace,
        at: new Date().toISOString(), output,
      });
    } catch {
      /* snippet is best-effort */
    }
  }

  /** Periodic reconcile against the authoritative cache — heals event gaps. */
  private reconcile(): void {
    for (const a of this.cache.agentViews()) {
      if (!a.pane) continue;
      const prev = this.lastStatusByPane.get(a.pane);
      const next = { status: a.status, name: a.name };
      if (!prev || prev.status === next.status) {
        // Throttle summary reads while working so progress is sent only for new output.
        if (next.status === "working") {
          const last = this.lastWorkingSnippetAt.get(a.pane) ?? 0;
          if (Date.now() - last >= WORKING_SNIPPET_MS) {
            this.lastWorkingSnippetAt.set(a.pane, Date.now());
            const title = typeof a.terminal_title === "string" ? a.terminal_title : null;
            void this.readOutputSnippet({
              name: a.name, pane: a.pane, status: a.status, workspace: a.workspace,
              cwd: a.cwd, label: null, terminalTitle: title, seq: typeof a.state_change_seq === "number" ? a.state_change_seq : null,
            });
          }
        }
        continue;
      }
      this.lastStatusByPane.set(a.pane, next);
      const seq = typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined;
      if (next.status === "working") {
        this.emit("agent_working", { agent: a.name, pane: a.pane, workspace: a.workspace, cwd: a.cwd, status: a.status, seq, at: new Date().toISOString() });
      } else if (next.status && SETTLED.has(next.status) && prev.status === "working") {
        this.emit("agent_settled", { agent: a.name, pane: a.pane, workspace: a.workspace, cwd: a.cwd, status: a.status, seq, at: new Date().toISOString() });
        const snippet = this.lastOutputSnippet.get(a.pane);
        if (snippet && Date.now() - snippet.at < 30_000) {
          this.emit("agent_output", { agent: a.name, pane: a.pane, workspace: a.workspace, at: new Date().toISOString(), output: snippet.output });
        }
      }
    }
  }

  private matches(c: PushClient, data: Record<string, unknown>): boolean {
    if (c.filters.agent) {
      const name = data["agent"];
      // pane.agent holds the agent KIND (e.g. "pi"), not the custom start name;
      // match kind directly or a scoped kind (workspace:kind).
      const want = c.filters.agent.split(":")[1] ?? c.filters.agent;
      if (name !== c.filters.agent && name !== want) return false;
    }
    if (c.filters.pane && data["pane"] !== c.filters.pane) return false;
    if (c.filters.workspace && data["workspace"] !== c.filters.workspace) return false;
    return true;
  }

  private agentsForHello(filters: PushFilters): unknown[] {
    let agents = this.cache.agentViews();
    if (filters.workspace) {
      agents = agents.filter((a) => a.workspace === filters.workspace);
    } else if (filters.pane) {
      agents = agents.filter((a) => a.pane === filters.pane);
    } else if (filters.agent) {
      const want = filters.agent.split(":")[1] ?? filters.agent;
      agents = agents.filter((a) => a.name === filters.agent || a.name === want);
    }
    return agents.map((a: AgentView) => ({
      name: a.name, pane: a.pane, status: a.status, workspace: a.workspace,
      cwd: a.cwd, started_at: a.started_at, last_activity_at: a.last_activity_at,
      terminal_title: typeof a.terminal_title === "string" ? a.terminal_title : undefined,
      seq: typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined,
    }));
  }

  private workspacePayload(): unknown[] {
    return this.cache.workspaceViews().map((w) => ({
      id: w.id,
      label: w.label,
      roots: w.roots,
    }));
  }

  private emit(event: string, data: Record<string, unknown>): void {
    for (const c of this.clients) {
      if (!this.matches(c, data)) continue;
      try {
        sseWrite(c.res, event, data);
      } catch {
        this.drop(c);
      }
    }
  }

  private drop(c: PushClient): void {
    this.clients.delete(c);
    try { c.res.end(); } catch { /* already closed */ }
  }

  /** Register an SSE client, send hello (authoritative snapshot), start heartbeat. */
  addClient(res: Response, filters: PushFilters): void {
    const c: PushClient = { res, filters };
    this.clients.add(c);
    sseWrite(res, "hello", {
      protocol: "herdr-mcp-push/v1",
      server_time: new Date().toISOString(),
      filters,
      agents: this.agentsForHello(filters),
      workspaces: this.workspacePayload(),
    });
    const hb = setInterval(() => {
      try { res.write(": keepalive\n\n"); } catch { this.drop(c); }
    }, HEARTBEAT_MS);
    hb.unref();
    this.heartbeatTimers.add(hb);
    res.on("close", () => {
      this.heartbeatTimers.delete(hb);
      clearInterval(hb);
      this.clients.delete(c);
    });
  }

  /** Current agent snapshot (for GET /push/state reconciliation). */
  stateView(): { agents: unknown[]; workspaces: unknown[]; panes: unknown[]; server_time: string } {
    return {
      server_time: new Date().toISOString(),
      agents: this.cache.agentViews().map((a: AgentView) => ({
        name: a.name, pane: a.pane, status: a.status, workspace: a.workspace,
        cwd: a.cwd, started_at: a.started_at, last_activity_at: a.last_activity_at,
        terminal_title: typeof a.terminal_title === "string" ? a.terminal_title : undefined,
        seq: typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined,
      })),
      workspaces: this.workspacePayload(),
      panes: this.cache.paneViews(),
    };
  }

  close(): void {
    this.unsub();
    if (this.reconcileTimer) clearInterval(this.reconcileTimer);
    for (const hb of this.heartbeatTimers) clearInterval(hb);
    this.heartbeatTimers.clear();
    for (const c of [...this.clients]) this.drop(c);
  }
}

let hub: PushHub | null = null;

/** Bearer auth for /push (same token as /mcp; 401 plain-text for browser consumers). */
function pushAuth(req: Request, res: Response, next: () => void): void {
  const token = process.env.HERDR_MCP_TOKEN ?? "";
  if (token) {
    const auth = req.get("authorization") ?? "";
    if (auth !== `Bearer ${token}`) {
      res.status(401).send("unauthorized\n");
      return;
    }
  }
  next();
}

/**
 * Mount /push routes. getClient returns the shared HerdrClient (created
 * lazily in server.ts) so the hub reuses the same SnapshotCache singleton.
 */
export function registerPushRoutes(app: Express, getClient: () => HerdrClient): void {
  const router = Router();
  router.use(pushAuth);

  router.get("/events", (req: Request, res: Response) => {
    const h = hub ?? (hub = new PushHub(getClient()));
    res.status(200);
    res.set({
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    });
    res.flushHeaders();
    res.write("retry: 2000\n\n");
    h.addClient(res, {
      agent: typeof req.query.agent === "string" ? req.query.agent : undefined,
      pane: typeof req.query.pane === "string" ? req.query.pane : undefined,
      workspace: typeof req.query.workspace === "string" ? req.query.workspace : undefined,
    });
  });

  router.get("/state", (_req: Request, res: Response) => {
    const h = hub ?? (hub = new PushHub(getClient()));
    res.status(200).json(h.stateView());
  });

  // Extension idle-nudge: tools/call counts in a wall-clock window (ChatGPT connector UA).
  router.get("/mcp-activity", (req: Request, res: Response) => {
    const sinceRaw = req.query.since ?? req.query.since_ms;
    const untilRaw = req.query.until ?? req.query.until_ms;
    const since_ms = Number(sinceRaw);
    const until_ms = untilRaw === undefined || untilRaw === "" ? Date.now() : Number(untilRaw);
    if (!Number.isFinite(since_ms) || !Number.isFinite(until_ms) || until_ms < since_ms) {
      res.status(400).json({
        ok: false,
        reason: "bad_window",
        message: "query since (ms) and optional until (ms) required; until >= since",
      });
      return;
    }
    // Cap lookback at 30 minutes to keep responses small.
    const minSince = Date.now() - 30 * 60_000;
    const clippedSince = Math.max(since_ms, minSince);
    const ua_includes = typeof req.query.ua === "string" ? req.query.ua
      : typeof req.query.ua_includes === "string" ? req.query.ua_includes
      : "openai-mcp";
    const out = queryMcpActivity({ since_ms: clippedSince, until_ms, ua_includes });
    res.status(200).json({ ok: true, ...out });
  });

  app.use("/push", router);
}
