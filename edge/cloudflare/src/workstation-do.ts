/**
 * workstation-do.ts — Durable Object per workstation identity.
 *
 * One DO per workstation_id (deterministic idFromName mapping, plan §11). It
 * owns:
 *  - the inbound, hibernatable workstation WSS (server-side acceptWebSocket);
 *  - hello protocol-version gate (bearer check ran at the Worker upgrade edge);
 *  - heartbeat/last_seen presence persisted to DO storage;
 *  - request_id correlation via the bounded PendingRequestRegistry, persisted;
 *  - forwarding tool requests onto the link socket with deadline alarms;
 *  - offline / reconnecting / delivery_uncertain classification.
 *
 * WIRE KINDS (canonical Relay Protocol v1):
 *   Edge → link: hello_ack, tool_request, cancel, status (query=true)
 *   Link → edge: hello, heartbeat, status, tool_result, tool_error, cancel_ack
 *
 * Pre-hello: only "hello" is accepted. Non-hello before hello → WS close 1008.
 * Exactly one active link per workstation after validation.
 * Draining/upgrade state is local DO state only — no wire kind for them.
 *
 * HIBERNATION RULES:
 *  - Storage is authoritative. The in-memory registry/resolvers are caches.
 *  - Timers (setTimeout) may not fire while hibernated — deadlines are backed
 *    by Durable Object alarms.
 *  - On restart, state is rebuilt from storage; any resolver lost is
 *    reconciled by the alarm sweep from persisted state (never replays a
 *    mutating request).
 */

import type { Env } from "./env.js";
import {
  errorResult,
  mapLinkErrorCode,
  offlineResult,
  reconnectingResult,
  drainingResult,
  timeoutResult,
  capacityResult,
  classifyAmbiguousDelivery,
  type RelayErrorCode,
  type RelayErrorResult,
} from "./errors.js";
import { makeLimits, HEARTBEAT_PERSIST_THROTTLE_MS } from "./limits.js";
import { checkArgsBudget, parseJsonFrame, readBodyBounded } from "./payload.js";
import {
  PendingRequestRegistry,
  decodeStoredPendingRequest,
  newRequestId,
  type Completion,
  type IdempotencyRecord,
  type PendingRequest,
} from "./pending.js";
import {
  decodeWire,
  encodeWire,
  validateHello,
  POST_HELLO_KINDS,
  EDGE_OUTBOUND_KINDS,
  RELAY_PROTOCOL_VERSION,
  type HelloMessage,
  type HelloAckMessage,
  type HeartbeatMessage,
  type StatusMessage,
  type ToolRequestMessage,
  type ToolResultMessage,
  type ToolErrorMessage,
  type CancelMessage,
  type CancelAckMessage,
  type RelayMessage,
} from "./relay-adapter.js";
import {
  applyRuntimeStatusGlimpse,
  parseSession,
  serializeSession,
  sessionFromClaims,
  sessionSummary,
  makeEmptySession,
  isStale,
  type WorkstationSession,
} from "./state.js";
import { createLogger } from "./logger.js";
import { LINK_APPLICATION_PROTOCOL } from "./auth.js";
import { EPOCH1_CONTRACT } from "./contracts/epoch1.js";

const PREFIX_PENDING = "pending:";
const PREFIX_COMPLETED = "completed:";
const PREFIX_IDEM = "idem:";
const KEY_SESSION = "session";

export interface InternalForwardRequest {
  kind: "request";
  requestId?: string;
  op: string;
  opClass?: "read" | "mutating" | "unknown";
  args?: unknown;
  deadlineMs?: number;
  contractEpoch?: number;
  contractHash?: string;
  idempotencyKey?: string;
}

export type ForwardOutcome =
  | { status: "ok"; completion: Completion }
  | { status: "error"; error: RelayErrorCode };

interface LinkAttachment {
  active: boolean;
  registered: boolean;
  bootId?: string;
}

export class WorkstationDO {
  private readonly state: DurableObjectState;
  private readonly env: Env;
  private readonly limits: ReturnType<typeof makeLimits>;
  private readonly logger = createLogger("workstation-do");
  private readonly registry: PendingRequestRegistry;
  private session: WorkstationSession | undefined;
  private initialized = false;
  private initPromise: Promise<void> | undefined;
  private lastSeenPersistedAtMs = 0;
  /** In-memory resolver cache only; storage remains authoritative. */
  private readonly resolvers = new Map<string, (completion: Completion) => void>();

  constructor(state: DurableObjectState, env: Env) {
    this.state = state;
    this.env = env;
    this.limits = makeLimits(env);
    this.registry = new PendingRequestRegistry({ limits: this.limits });
  }

  // ------------------------------------------------------------------ init

  private ensureInit(): Promise<void> {
    if (this.initialized) return Promise.resolve();
    if (this.initPromise) return this.initPromise;
    this.initPromise = this.state.blockConcurrencyWhile(async () => {
      const sessionRaw = await this.state.storage.get<string>(KEY_SESSION);
      if (sessionRaw !== undefined) {
        const parsed = parseSession(sessionRaw);
        if (parsed.ok) this.session = parsed.session;
      }
      const workstationId = this.workstationId();
      if (this.session === undefined) this.session = makeEmptySession(workstationId, Date.now());
      const pending = await this.loadPending();
      const completed = await this.loadCompleted();
      const idem = await this.loadIdem();
      this.registry.restore({ pending, completed });
      this.registry.restoreIdem(idem);
      this.initialized = true;
    });
    return this.initPromise;
  }

  private async loadPending(): Promise<PendingRequest[]> {
    const map = await this.state.storage.list<unknown>({ prefix: PREFIX_PENDING });
    const out: PendingRequest[] = [];
    for (const value of map.values()) {
      const decoded = decodeStoredPendingRequest(value);
      if (decoded) out.push(decoded);
    }
    return out;
  }

  private async loadCompleted(): Promise<Array<{ requestId: string; completion: Completion }>> {
    const map = await this.state.storage.list<Completion>({ prefix: PREFIX_COMPLETED });
    const out: Array<{ requestId: string; completion: Completion }> = [];
    for (const [key, value] of map) {
      if (value !== undefined) out.push({ requestId: key.slice(PREFIX_COMPLETED.length), completion: value });
    }
    return out;
  }

  private async loadIdem(): Promise<IdempotencyRecord[]> {
    const map = await this.state.storage.list<IdempotencyRecord>({ prefix: PREFIX_IDEM });
    const out: IdempotencyRecord[] = [];
    for (const value of map.values()) if (value !== undefined) out.push(value);
    return out;
  }

  private workstationId(): string {
    return this.state.id.name as string;
  }

  // ---------------------------------------------------------------- fetch

  async fetch(request: Request): Promise<Response> {
    await this.ensureInit();
    const url = new URL(request.url);
    const upgrade = request.headers.get("Upgrade")?.toLowerCase();

    if (upgrade === "websocket") {
      const pair = new WebSocketPair();
      const server = pair[1];
      this.state.acceptWebSocket(server, ["link"]);
      server.serializeAttachment({ active: false, registered: false } satisfies LinkAttachment);
      this.logger.info("ws.upgrade.accepted", { workstationId: this.session?.workstationId });
      return new Response(null, {
        status: 101,
        webSocket: pair[0],
        headers: { "Sec-WebSocket-Protocol": LINK_APPLICATION_PROTOCOL },
      });
    }

    if (request.method === "GET" && url.pathname === "/internal/status") {
      return this.handleStatus();
    }
    if (request.method === "POST" && url.pathname === "/internal/forward") {
      return this.handleForward(request);
    }
    return this.json({ ok: false, code: "not_found", retryable: false }, 404);
  }

  private json(body: unknown, status = 200): Response {
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  }

  private async handleStatus(): Promise<Response> {
    const now = Date.now();
    const online = this.session !== undefined && !isStale(this.limits, this.session, now);
    const payload = sessionSummary(this.session, {
      now,
      linkStaleAfterMs: this.limits.linkStaleAfterMs,
      activeRequests: this.registry.activeCount(),
      edgeVersion: this.env.EDGE_VERSION ?? "0.1.0-dev",
    });
    return this.json({ ...payload, online });
  }

  // ------------------------------------------------- internal forward

  private async handleForward(request: Request): Promise<Response> {
    const parsedBody = await readBodyBounded(request, this.limits.maxFrameBytes);
    if (!parsedBody.ok) {
      return this.json(
        { status: "error", error: errorResult(parsedBody.code, { retryable: false }) },
        parsedBody.code === "payload_too_large" ? 413 : 400,
      );
    }
    const body = parsedBody.value as Partial<InternalForwardRequest> & { kind?: string };
    if (body?.kind !== "request") {
      return this.json({ status: "error", error: errorResult("bad_request", { retryable: false }) }, 400);
    }
    return this.forwardInternal(body as InternalForwardRequest);
  }

  private async forwardInternal(req: InternalForwardRequest): Promise<Response> {
    const now = Date.now();
    const workstationId = this.workstationId();
    const requestId = req.requestId ?? newRequestId();

    const wire: ToolRequestMessage = {
      protocol_version: RELAY_PROTOCOL_VERSION,
      kind: "tool_request",
      workstation_id: workstationId,
      request_id: requestId,
      operation: req.op,
      timeout_ms: req.deadlineMs ? req.deadlineMs - now : this.limits.requestTimeoutMs,
      contract_epoch: req.contractEpoch,
      contract_hash: req.contractHash,
      idempotency_key: req.idempotencyKey,
      arguments: (req.args ?? undefined) as Record<string, unknown> | undefined,
    };

    if (this.session?.status === "draining") {
      return this.json({ status: "error", error: drainingResult({ requestId, workstationId, atMs: now }) }, 503);
    }

    const links = this.state.getWebSockets("link");
    if (links.length === 0) {
      return this.json({ status: "error", error: offlineResult({ requestId, workstationId, atMs: now }) }, 503);
    }

    const budget = checkArgsBudget(wire.arguments, this.limits.maxFrameBytes);
    if (!budget.ok) {
      return this.json(
        { status: "error", error: errorResult("payload_too_large", { requestId, workstationId, atMs: now }) },
        413,
      );
    }

    const deadlineMs = now + (wire.timeout_ms ?? this.limits.requestTimeoutMs);
    const add = this.registry.add({
      requestId,
      workstationId,
      op: req.op,
      opClass: req.opClass ?? "mutating",
      argsSummary: { argKeys: Object.keys((req.args ?? {}) as Record<string, unknown>).slice(0, 32) },
      deadlineMs,
      idempotencyKey: req.idempotencyKey,
      contractEpoch: req.contractEpoch,
    });

    if (add.status === "idem_hit") {
      return this.json({ status: "ok", completion: add.completion });
    }
    if (add.status === "capacity_full") {
      return this.json({ status: "error", error: capacityResult({ requestId, workstationId, atMs: now }) }, 429);
    }
    if (add.status === "evicted_oldest") {
      this.logger.warn("pending.evicted", {
        workstationId,
        evictedRequestId: add.evicted.requestId,
        evictedOp: add.evicted.op,
        requestId,
      });
      await this.persistEvictedSettlement(add.evicted, {
        status: "error",
        error: reconnectingResult({ requestId: add.evicted.requestId, workstationId, atMs: now }),
        servedAtMs: now,
      });
    }
    const entry = add.entry;
    await this.state.storage.put(PREFIX_PENDING + entry.requestId, entry);

    // Interleave guard: the storage.put above yielded, so a concurrent invoke may
    // have evicted this request from the capacity-bound pending map (oldest-queued
    // eviction). If so, this handler must NOT send to the link — the request is
    // already settled with a recorded completion. Re-confirm we still own the
    // active entry before any send.
    const stillOwned = this.registry.get(entry.requestId) === entry;
    if (!stillOwned) {
      const evictedCompletion = this.registry.completedFor(entry.requestId);
      if (evictedCompletion) {
        return this.json({ status: "ok", completion: evictedCompletion });
      }
      // Evicted but not yet settled (rare): fail closed rather than send.
      return this.json(
        { status: "error", error: reconnectingResult({ requestId: entry.requestId, workstationId, atMs: now }) },
        503,
      );
    }

    const encoded = encodeWire(wire, this.limits.maxFrameBytes);
    if (!encoded.ok) {
      await this.persistSettlement(entry.requestId, {
        status: "error",
        error: errorResult("payload_too_large", { requestId: entry.requestId, workstationId, atMs: now }),
        servedAtMs: now,
      });
      return this.json(
        { status: "error", error: errorResult("payload_too_large", { requestId: entry.requestId, workstationId, atMs: now }) },
        413,
      );
    }

    const sent = this.sendToActiveLink(wire);
    if (!sent) {
      await this.handleLinkGone("ws.send.race", { requestId: entry.requestId });
      return this.json({ status: "error", error: offlineResult({ requestId: entry.requestId, workstationId, atMs: now }) }, 503);
    }
    this.registry.markSent(entry.requestId, now);
    await this.state.storage.put(PREFIX_PENDING + entry.requestId, entry);

    await this.armAlarm();
    const completion = await this.awaitCompletion(entry.requestId, deadlineMs);
    return this.json({ status: "ok", completion });
  }

  /** Wait for a settled completion or timeout classification. */
  private awaitCompletion(requestId: string, deadlineMs: number): Promise<Completion> {
    const existing = this.registry.completedFor(requestId);
    if (existing) return Promise.resolve(existing);
    return new Promise<Completion>((resolve) => {
      this.resolvers.set(requestId, resolve);
      const remaining = deadlineMs - Date.now();
      if (remaining <= 0) {
        void this.settleAsTimeout(requestId);
        return;
      }
      setTimeout(() => {
        if (this.resolvers.has(requestId)) void this.settleAsTimeout(requestId);
      }, remaining);
    });
  }

  private async settleAsTimeout(requestId: string): Promise<void> {
    const entry = this.registry.get(requestId);
    if (!entry || entry.state === "settled") return;
    const now = Date.now();
    const err = timeoutResult({
      requestId,
      workstationId: entry.workstationId,
      atMs: now,
      opClass: entry.opClass,
    });
    await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: now });
  }

  private async persistSettlement(requestId: string, completion: Completion): Promise<void> {
    const entry = this.registry.settle(requestId, completion);
    if (!entry) return;
    await this.state.storage.delete(PREFIX_PENDING + requestId);
    await this.state.storage.put(PREFIX_COMPLETED + requestId, completion as unknown as string);
    if (entry.idempotencyKey !== undefined) {
      const record: IdempotencyRecord = {
        idempotencyKey: entry.idempotencyKey,
        requestId,
        op: entry.op,
        settledAtMs: completion.servedAtMs,
      };
      await this.state.storage.put(PREFIX_IDEM + entry.idempotencyKey, record as unknown as string);
    }
    const resolve = this.resolvers.get(requestId);
    if (resolve) {
      this.resolvers.delete(requestId);
      resolve(completion);
    }
    void this.armAlarm();
  }

  /**
   * Close out a request that was evicted from the capacity-bound pending map
   * (its registry entry no longer exists, so settle() would no-op). Records the
   * completion + idempotency without re-occupying pending capacity, deletes the
   * durable pending:<id> key so the evicted request can never resurrect on
   * rehydrate, and resolves any waiter. Mirrors persistSettlement but takes the
   * explicit evicted entry.
   */
  private async persistEvictedSettlement(entry: PendingRequest, completion: Completion): Promise<void> {
    const requestId = entry.requestId;
    this.registry.recordSettlement(entry, completion);
    await this.state.storage.delete(PREFIX_PENDING + requestId);
    await this.state.storage.put(PREFIX_COMPLETED + requestId, completion as unknown as string);
    if (entry.idempotencyKey !== undefined) {
      const record: IdempotencyRecord = {
        idempotencyKey: entry.idempotencyKey,
        requestId,
        op: entry.op,
        settledAtMs: completion.servedAtMs,
      };
      await this.state.storage.put(PREFIX_IDEM + entry.idempotencyKey, record as unknown as string);
    }
    const resolve = this.resolvers.get(requestId);
    if (resolve) {
      this.resolvers.delete(requestId);
      resolve(completion);
    }
    void this.armAlarm();
  }

  // ---------------------------------------------------- ws lifecycle

  private readLinkAttachment(ws: WebSocket): LinkAttachment {
    try {
      const value = ws.deserializeAttachment() as Partial<LinkAttachment> | null;
      return {
        active: value?.active === true,
        registered: value?.registered === true,
        ...(typeof value?.bootId === "string" ? { bootId: value.bootId } : {}),
      };
    } catch {
      return { active: false, registered: false };
    }
  }

  private isActiveLink(ws: WebSocket): boolean {
    const attachment = this.readLinkAttachment(ws);
    return attachment.active && attachment.registered;
  }

  private sendToSocket(ws: WebSocket, message: RelayMessage): boolean {
    const encoded = encodeWire(message, this.limits.maxFrameBytes);
    if (!encoded.ok) {
      this.logger.warn("ws.send.encode_failed", { reason: encoded.reason });
      return false;
    }
    try {
      ws.send(encoded.text);
      return true;
    } catch {
      this.logger.warn("ws.send.failed", { workstationId: this.session?.workstationId });
      return false;
    }
  }

  /** Send to the one active link only. Returns true if at least one socket
   *  received the message. */
  private sendToActiveLink(message: RelayMessage): boolean {
    for (const ws of this.state.getWebSockets("link")) {
      if (!this.isActiveLink(ws)) continue;
      return this.sendToSocket(ws, message);
    }
    return false;
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    await this.ensureInit();
    const raw = typeof message === "string" ? message : new TextDecoder().decode(message);
    const decoded = decodeWire(raw);
    if (!decoded.ok) {
      // Pre-hello: close the socket with a protocol error. No wire "error" kind.
      if (!this.isActiveLink(ws)) {
        try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
        return;
      }
      this.logger.warn("ws.frame.invalid", { code: decoded.code, workstationId: this.session?.workstationId });
      try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
      return;
    }
    const msg = decoded.message;

    // Pre-hello: only "hello" is accepted.
    if (!this.isActiveLink(ws) && msg.kind !== "hello") {
      try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
      return;
    }

    // Post-hello: validate against allowed link→edge kinds.
    if (msg.kind !== "hello" && !POST_HELLO_KINDS.has(msg.kind)) {
      this.logger.warn("ws.kind.unexpected", { kind: msg.kind, workstationId: this.session?.workstationId });
      try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
      return;
    }

    switch (msg.kind) {
      case "hello":
        await this.handleHello(msg, ws);
        break;
      case "heartbeat":
        await this.handleHeartbeat(msg, ws);
        break;
      case "status":
        await this.handleLinkStatus(msg);
        break;
      case "tool_result":
        await this.handleToolResult(msg);
        break;
      case "tool_error":
        await this.handleToolError(msg);
        break;
      case "cancel_ack":
        await this.handleCancelAck(msg);
        break;
    }
  }

  private async handleHello(hello: HelloMessage, ws: WebSocket): Promise<void> {
    const expected = this.workstationId();
    if (hello.workstation_id !== expected) {
      this.logger.warn("ws.hello.mismatch", { workstationId: expected, claimedId: hello.workstation_id });
      try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
      return;
    }

    if (
      hello.runtime?.contract_epoch !== EPOCH1_CONTRACT.contract_epoch ||
      hello.runtime?.contract_hash !== EPOCH1_CONTRACT.contract_hash
    ) {
      const ack: HelloAckMessage = {
        protocol_version: RELAY_PROTOCOL_VERSION,
        kind: "hello_ack",
        workstation_id: expected,
        ok: false,
        code: "contract_mismatch",
        message: `edge requires contract epoch ${EPOCH1_CONTRACT.contract_epoch} hash ${EPOCH1_CONTRACT.contract_hash}`,
      };
      this.sendToSocket(ws, ack);
      this.logger.warn("ws.hello.contract_mismatch", {
        workstationId: expected,
        claimedEpoch: hello.runtime?.contract_epoch,
        claimedHash: hello.runtime?.contract_hash,
      });
      try { ws.close(1008, "contract mismatch"); } catch { /* already closed */ }
      return;
    }

    // Exactly one active link per workstation.
    ws.serializeAttachment({ active: true, registered: true, bootId: hello.boot_id } satisfies LinkAttachment);
    for (const other of this.state.getWebSockets("link")) {
      if (other === ws) continue;
      other.serializeAttachment({ ...this.readLinkAttachment(other), active: false });
      try { other.close(4409, "superseded by newer workstation link"); } catch { /* already closed */ }
    }

    const now = Date.now();
    this.session = sessionFromClaims({
      workstationId: hello.workstation_id,
      linkVersion: hello.link_version,
      bootId: hello.boot_id,
      protocolVersion: String(RELAY_PROTOCOL_VERSION),
      connectedAtMs: hello.connected_at_ms ?? now,
      runtimeVersion: hello.runtime?.runtime_version,
      runtimeCommit: hello.runtime?.runtime_commit ?? undefined,
      runtimeGeneration: hello.runtime?.runtime_generation ?? undefined,
      herdProtocolVersion: hello.runtime?.herdr_protocol ?? undefined,
      contractHash: hello.runtime?.contract_hash ?? undefined,
      contractEpoch: hello.runtime?.contract_epoch ?? undefined,
      capabilities: hello.capabilities,
    });
    await this.state.storage.put(KEY_SESSION, serializeSession(this.session));

    const resume = this.registry.resumeSummaries(now).map((p) => ({
      request_id: p.requestId,
      operation: p.op,
      state: p.state as "queued" | "sent" | "settled",
      deadline_ms: p.deadlineMs,
    }));

    const ack: HelloAckMessage = {
      protocol_version: RELAY_PROTOCOL_VERSION,
      kind: "hello_ack",
      workstation_id: expected,
      ok: true,
      server_version: this.env.EDGE_VERSION ?? "0.1.0-dev",
      reconnect: resume.length > 0,
      resume,
      completed: [],
    };
    this.sendToSocket(ws, ack);
    this.logger.info("ws.hello.accepted", {
      workstationId: hello.workstation_id,
      linkVersion: hello.link_version,
      bootId: hello.boot_id,
    });
  }

  private async handleHeartbeat(msg: HeartbeatMessage, ws: WebSocket): Promise<void> {
    const now = Date.now();
    if (this.session) {
      this.session.lastSeenAtMs = now;
      const runtimeChanged = applyRuntimeStatusGlimpse(this.session, msg.runtime, true);
      if (runtimeChanged || now - this.lastSeenPersistedAtMs >= HEARTBEAT_PERSIST_THROTTLE_MS) {
        this.lastSeenPersistedAtMs = now;
        await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
      }
    }
    const edgeSeen: StatusMessage = {
      protocol_version: RELAY_PROTOCOL_VERSION,
      kind: "status",
      workstation_id: this.workstationId(),
      healthy: true,
      sent_at_ms: now,
    };
    this.sendToSocket(ws, edgeSeen);
    this.logger.info("ws.heartbeat", { workstationId: this.session?.workstationId, bootId: msg.boot_id });
  }

  private async handleLinkStatus(msg: StatusMessage): Promise<void> {
    if (!this.session) return;
    // Workstation status report: update runtime info from the link.
    if (msg.runtime) {
      this.session.runtimeStatus = {
        runtimeVersion: msg.runtime.runtime_version ?? undefined,
        runtimeGeneration: msg.runtime.runtime_generation ?? undefined,
        herdProtocolVersion: msg.runtime.herdr_protocol ?? undefined,
        health: msg.healthy === false ? "degraded" : "ok",
      };
    }
    if (msg.runtime_generation !== undefined) {
      this.session.runtimeStatus = {
        ...this.session.runtimeStatus,
        runtimeGeneration: msg.runtime_generation ?? undefined,
      };
    }
    if (this.session.runtimeStatus) {
      await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
    }
    this.logger.info("ws.status", {
      workstationId: this.session.workstationId,
      healthy: msg.healthy,
      runtimeGeneration: msg.runtime_generation,
    });
  }

  private async handleToolResult(msg: ToolResultMessage): Promise<void> {
    const requestId = msg.request_id;
    const entry = this.registry.get(requestId);
    if (!entry) {
      this.logger.warn("ws.tool_result.orphan", { requestId });
      return;
    }
    const completion: Completion = {
      status: "ok",
      result: msg.result ?? null,
      servedAtMs: msg.served_at_ms,
      runtimeGeneration: msg.runtime_generation ?? undefined,
    };
    await this.persistSettlement(requestId, completion);
    this.logger.info("ws.tool_result.settled", {
      requestId,
      workstationId: entry.workstationId,
      op: entry.op,
    });
  }

  private async handleToolError(msg: ToolErrorMessage): Promise<void> {
    const requestId = msg.request_id;
    const entry = this.registry.get(requestId);
    if (!entry || entry.state === "settled") {
      this.logger.warn("ws.tool_error.orphan_or_stale", { requestId });
      return;
    }
    const err: RelayErrorResult = {
      ok: false,
      code: mapLinkErrorCode(msg.code),
      retryable: msg.retryable,
      message: msg.message,
      details: msg.details,
      delivery_state: msg.delivery_state,
      requestId,
      workstationId: entry.workstationId,
      atMs: msg.served_at_ms ?? Date.now(),
    };
    await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: Date.now() });
    this.logger.info("ws.tool_error.settled", {
      requestId,
      code: msg.code,
      retryable: msg.retryable,
      deliveryState: msg.delivery_state,
    });
  }

  private async handleCancelAck(msg: CancelAckMessage): Promise<void> {
    this.logger.info("ws.cancel_ack", {
      requestId: msg.request_id,
      accepted: msg.accepted,
      cancelledAtMs: msg.cancelled_at_ms,
    });
    // Cancel acknowledged — the link handled it. The alarm will settle
    // the timeout; no explicit settlement needed here.
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string, wasClean: boolean): Promise<void> {
    await this.ensureInit();
    // With compatibility dates before 2026-04-07, Cloudflare Hibernation
    // WebSockets require the handler to reciprocate the peer Close frame.
    // Calling close() is also safe on newer dates where the runtime auto-replies.
    try { ws.close(code, reason); } catch { /* already closed */ }
    if (!this.isActiveLink(ws)) {
      this.logger.info("ws.close.inactive", { code });
      return;
    }
    await this.handleLinkGone("ws.close", { code });
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    await this.ensureInit();
    if (!this.isActiveLink(ws)) {
      this.logger.info("ws.error.inactive", {});
      return;
    }
    await this.handleLinkGone("ws.error", {});
  }

  /** Unified drop path: mark offline, classify in-flight, resolve caches. */
  private async handleLinkGone(event: string, fields: Record<string, unknown>): Promise<void> {
    const now = Date.now();
    if (this.session) {
      this.session.status = "offline";
      this.session.disconnectedAtMs = now;
      await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
    }
    const classifications = this.registry.classifyAllOnClose(now);
    for (const [requestId, err] of classifications) {
      await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: now });
    }
    this.logger.warn(event, {
      workstationId: this.session?.workstationId,
      pendingSettled: classifications.size,
      ...fields,
    });
  }

  // --------------------------------------------------------------- alarm

  async alarm(): Promise<void> {
    await this.ensureInit();
    const now = Date.now();
    for (const entry of this.registry.expired(now)) {
      const err = timeoutResult({
        requestId: entry.requestId,
        workstationId: entry.workstationId,
        atMs: now,
        opClass: entry.opClass,
      });
      await this.persistSettlement(entry.requestId, { status: "error", error: err, servedAtMs: now });
      // Send cancel to the active link.
      const cancel: CancelMessage = {
        protocol_version: RELAY_PROTOCOL_VERSION,
        kind: "cancel",
        workstation_id: entry.workstationId,
        request_id: entry.requestId,
        reason: "deadline exceeded",
      };
      this.sendToActiveLink(cancel);
    }
    for (const requestId of this.registry.completedExpired(now)) {
      const entry = this.registry.get(requestId);
      const idemKey = entry?.idempotencyKey ?? this.registry.idempotencyKeyFor(requestId);
      this.registry.dropCompleted(requestId, idemKey);
      await this.state.storage.delete(PREFIX_COMPLETED + requestId);
      if (idemKey !== undefined) await this.state.storage.delete(PREFIX_IDEM + idemKey);
    }
    await this.armAlarm();
  }

  /** Keep the next deadline/expiry armed (alarms coalesce; earliest fires). */
  private async armAlarm(): Promise<void> {
    const pendingAt = this.registry.nextDeadlineMs();
    const expiredAt = this.registry.completedExpiryAtMs();
    let next: number | undefined;
    if (pendingAt !== undefined && expiredAt !== undefined) next = Math.min(pendingAt, expiredAt);
    else next = pendingAt ?? expiredAt;
    const current = await this.state.storage.getAlarm();
    if (next === undefined) {
      if (current !== null) await this.state.storage.deleteAlarm();
      return;
    }
    if (current === null || next < current) {
      await this.state.storage.setAlarm(next);
    }
  }
}