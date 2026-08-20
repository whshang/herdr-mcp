/**
 * /push — browser-extension push channel (herdr → 网页唤醒).
 *
 * 方向: 某个 herdr agent (如 p1) 干完活 (working → idle/done/blocked) 时,
 * 通过 SSE 通知浏览器插件,插件向绑定的网页会话输入框写入消息并提交。
 *
 * ## 设计决策 (阶段 1)
 *
 * 1. **SSE 复用现有事件源**: 服务器已经维护一条活的 events.subscribe 长连接
 *    (SnapshotCache, A-2, 25s chunk + 30s TTL 重快照)。PushHub 给 cache 挂
 *    onEvent 监听 (state.ts 新增钩子),而不是每个客户端再开一条 daemon
 *    订阅 — 零额外 socket,事件已归一化。不做轮询。
 *
 * 2. **鉴权复用 HERDR_MCP_TOKEN**: 不加新 token 机制。理由:
 *    - 威胁模型相同: 持有 token 者本来就能经 herdr_inspect 读全部 agent 状态,
 *      push 通道不外泄更多信息;
 *    - 零新密钥管理 (plist 只存一份 token,扩展配置也只填一份);
 *    - /mcp 未配 token 时本地裸跑, /push 保持一致 (AUTH_TOKEN 为空则开放)。
 *    插件侧从 background fetch 本地 127.0.0.1 端点,可以带 Authorization 头
 *    (EventSource 不能带头,所以不用 EventSource)。
 *
 * 3. **事件过滤**: 服务端按 ?agent=NAME / ?pane=PANE_ID 可选过滤 (绑定在扩展
 *    侧,服务端保持无状态)。初始 hello 事件带权威 agent 快照,扩展据此恢复
 *    绑定 (阶段 3 的错峰恢复依赖它)。
 *
 * 4. **错峰恢复**: 若扩展连接时 agent 已 settled,hello 快照里就是 settled;
 *    扩展用自己的去重 (记录已通知的 settle seq) 决定是否唤醒,服务端不需要
 *    补发历史。PushHub 内部每 10s 与 cache.agentViews() 对账一次,事件流有
 *    缺口时也能补发转换 (自愈)。
 */
import { Router, Request, Response } from "express";
import type { Express } from "express";
import { HerdrClient, HerdrEvent } from "./herdr.js";
import { getSnapshotCache, SnapshotCache, AgentView } from "./state.js";
import { cleanTerminalOutput } from "./clean.js";

const SETTLED = new Set(["idle", "done", "blocked"]);
const RECONCILE_MS = 10_000;   // periodic re-seed vs cache.agentViews() (missed-event recovery)
const HEARTBEAT_MS = 15_000;   // SSE keepalive comment (cloudflared-safe)
const OUTPUT_READ_MS = 2000;   // bounded best-effort agent.read for settled output snippet
const OUTPUT_LINES = 40;
const DEBUG = process.env.HERDR_MCP_PUSH_DEBUG === "1";

interface PushFilters {
  agent?: string;
  pane?: string;
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

  /** Best-effort bounded agent.read → second event with cleaned output. */
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
      this.lastOutputSnippet.set(ref.pane, { at: Date.now(), output });
      this.emit("agent_output", { agent: ref.name, pane: ref.pane, at: new Date().toISOString(), output });
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
      if (!prev || prev.status === next.status) continue;
      this.lastStatusByPane.set(a.pane, next);
      const seq = typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined;
      if (next.status === "working") {
        this.emit("agent_working", { agent: a.name, pane: a.pane, workspace: a.workspace, cwd: a.cwd, status: a.status, seq, at: new Date().toISOString() });
      } else if (next.status && SETTLED.has(next.status) && prev.status === "working") {
        this.emit("agent_settled", { agent: a.name, pane: a.pane, workspace: a.workspace, cwd: a.cwd, status: a.status, seq, at: new Date().toISOString() });
        const snippet = this.lastOutputSnippet.get(a.pane);
        if (snippet && Date.now() - snippet.at < 30_000) {
          this.emit("agent_output", { agent: a.name, pane: a.pane, at: new Date().toISOString(), output: snippet.output });
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
    return true;
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
      agents: this.cache.agentViews().map((a: AgentView) => ({
        name: a.name, pane: a.pane, status: a.status, workspace: a.workspace,
        cwd: a.cwd, started_at: a.started_at, last_activity_at: a.last_activity_at,
        seq: typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined,
      })),
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
  stateView(): { agents: unknown[]; server_time: string } {
    return {
      server_time: new Date().toISOString(),
      agents: this.cache.agentViews().map((a: AgentView) => ({
        name: a.name, pane: a.pane, status: a.status, workspace: a.workspace,
        cwd: a.cwd, started_at: a.started_at, last_activity_at: a.last_activity_at,
        seq: typeof a.state_change_seq === "number" ? (a.state_change_seq as number) : undefined,
      })),
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
    });
  });

  router.get("/state", (_req: Request, res: Response) => {
    const h = hub ?? (hub = new PushHub(getClient()));
    res.status(200).json(h.stateView());
  });

  app.use("/push", router);
}
