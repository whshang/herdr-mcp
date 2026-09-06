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
 *  - RFC WebSocket ping/pong is handled by the Cloudflare runtime without
 *    calling webSocketMessage or waking a hibernated isolate. Application JSON
 *    heartbeats are throttled on the Link side; steady-state beats update
 *    in-memory last_seen only and avoid Durable Object storage writes.
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
import {
  classifyOp,
  makeLimits,
  HEARTBEAT_PERSIST_THROTTLE_MS,
  EDGE_STATUS_REPLY_INTERVAL_MS,
} from "./limits.js";
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
import {
  RUNTIME_EXECUTION_CONTRACT,
  isCompatibleRuntimeContract,
} from "./contracts/runtime.js";

const PREFIX_PENDING = "pending:";
const PREFIX_COMPLETED = "completed:";
const PREFIX_IDEM = "idem:";
const KEY_SESSION = "session";
/** Immutable revocation tombstone. Written once on first revoke; never cleared. */
const KEY_REVOKED = "revoked";
/** Policy close code for a revoked device link. */
const REVOKED_CLOSE_CODE = 4401;

/**
 * Durable settlement row (completed:<id>). The completed row is the
 * authoritative, self-contained settlement record: it carries the completion
 * AND the idempotency binding (idempotencyKey + op) for that request. This is
 * what makes settlement crash-recoverable without relying on write ordering:
 * even if the separate idem:<key> row is lost to a crash/failure, the binding
 * is recovered from the completed row on init, so a same-key retry returns the
 * prior completion and the mutation is never re-executed. Legacy rows written
 * before this shape store the bare Completion; they are tolerated on load.
 */
interface CompletedRow {
  completion: Completion;
  idempotencyKey?: string;
  op?: string;
  /** Settled-at evidence for idem reconstruction (mirrors IdempotencyRecord). */
  settledAtMs?: number;
}

function encodeCompletedRow(
  entry: { idempotencyKey?: string; op: string } | undefined,
  completion: Completion,
): CompletedRow {
  if (entry?.idempotencyKey !== undefined) {
    return {
      completion,
      idempotencyKey: entry.idempotencyKey,
      op: entry.op,
      settledAtMs: completion.servedAtMs,
    };
  }
  return { completion };
}

function releasesIdempotencyBinding(completion: Completion): boolean {
  return completion.status === "error"
    && completion.error.code === "runtime_generation_superseded_before_dispatch"
    && completion.error.retryable === true
    && completion.error.delivery_state === "not_delivered";
}

/** Tolerate both the current envelope and legacy bare-Completion rows. */
function decodeCompletedRow(value: unknown): CompletedRow | null {
  if (value === null || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (v.completion !== undefined && typeof v.completion === "object" && v.completion !== null) {
    return {
      completion: v.completion as Completion,
      ...(typeof v.idempotencyKey === "string" ? { idempotencyKey: v.idempotencyKey } : {}),
      ...(typeof v.op === "string" ? { op: v.op } : {}),
      ...(typeof v.settledAtMs === "number" ? { settledAtMs: v.settledAtMs } : {}),
    };
  }
  // Legacy: bare Completion value.
  if (typeof v.status === "string") return { completion: v as unknown as Completion };
  return null;
}

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
  trace?: Record<string, unknown>;
}

export type ForwardOutcome =
  | { status: "ok"; completion: Completion }
  | { status: "error"; error: RelayErrorCode };

interface LinkAttachment {
  active: boolean;
  registered: boolean;
  bootId?: string;
}

interface EphemeralReadRequest {
  requestId: string;
  workstationId: string;
  op: string;
  state: "queued" | "sent";
  createdAtMs: number;
  sentAtMs?: number;
  deadlineMs: number;
  timer?: ReturnType<typeof setTimeout>;
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
  private lastEdgeStatusReplyAtMs = 0;
  /**
   * In-memory revocation fence. Set immediately on revoke so no new work is
   * admitted even if a later storage write fails. Fail-closed once set.
   */
  private revoked = false;
  private revokedAtMs: number | undefined;
  /** True only after the tombstone has been durably persisted (or was loaded
   *  from storage on init). A revoke without a persisted tombstone must not
   *  report success; retries keep re-attempting persistence. */
  private revokedPersisted = false;
  /** Known-safe reads are correlated only in memory and never enter DO storage. */
  private readonly ephemeralReads = new Map<string, EphemeralReadRequest>();
  /** In-memory resolver cache only; storage remains authoritative. */
  private readonly resolvers = new Map<string, (completion: Completion) => void>();
  /** Brief reconnect grace waiters. Process-local only: zero DO storage/alarm writes. */
  private readonly linkWaiters = new Set<() => void>();
  /**
   * Test seam (undefined in production): when set, a durable mutation's
   * forwardInternal awaits it at the pre-send seam — after the pre-send
   * durability fence has persisted the pending row and before the
   * stillOwned/revoked re-check and the actual socket send — so a test can
   * deterministically interleave a revoke that must be caught by the
   * pre-send revoked gate. Production always proceeds immediately.
   */
  beforeDurableSendHook: (() => Promise<void>) | undefined;

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
      const revokedRaw = await this.state.storage.get<{ revoked_at_ms: number }>(KEY_REVOKED);
      if (revokedRaw && typeof revokedRaw.revoked_at_ms === "number") {
        this.revoked = true;
        this.revokedPersisted = true;
        this.revokedAtMs = revokedRaw.revoked_at_ms;
      }
      const workstationId = this.workstationId();
      if (this.session === undefined) this.session = makeEmptySession(workstationId, Date.now());
      this.lastSeenPersistedAtMs = this.session.lastSeenAtMs ?? 0;
      const pending = await this.loadPending();
      const completed = await this.loadCompleted();
      const idem = await this.loadIdem();
      this.registry.restore({
        pending,
        completed: completed.map(({ requestId, completion }) => ({ requestId, completion })),
      });
      // Rebuild the idempotency index from BOTH the durable idem:<key> rows and
      // the idempotency bindings embedded in completed:<id> rows. A crash
      // between writing completed:<id> and idem:<key> (or after idem:<key> but
      // before the pending delete) therefore still recovers the full
      // completion + binding: a same-key retry returns the prior completion and
      // never re-executes a mutation that may already have run.
      const idemByKey = new Map<string, IdempotencyRecord>();
      for (const idemRecord of idem) idemByKey.set(idemRecord.idempotencyKey, idemRecord);
      for (const row of completed) {
        if (row.idempotencyKey === undefined) continue;
        const existing = idemByKey.get(row.idempotencyKey);
        // Prefer the durable idem:<key> row when present (it is the freshest
        // evidence); otherwise reconstruct the binding from the completed row.
        if (!existing) {
          idemByKey.set(row.idempotencyKey, {
            idempotencyKey: row.idempotencyKey,
            requestId: row.requestId,
            op: row.op ?? "",
            settledAtMs: row.completion.servedAtMs,
          });
        }
      }
      this.registry.restoreIdem([...idemByKey.values()]);
      // Initialization is deliberately read-only. A storage write quota must
      // never make inspect/fs-read/etc unavailable before business logic runs.
      // Durable mutation reconciliation is performed lazily before the next
      // mutation is admitted; known reads are never restored at all.
      this.initialized = true;
    });
    return this.initPromise;
  }

  private async loadPending(): Promise<PendingRequest[]> {
    const map = await this.state.storage.list<unknown>({ prefix: PREFIX_PENDING });
    const completedIds = new Set<string>();
    const completedMap = await this.state.storage.list<Completion>({ prefix: PREFIX_COMPLETED });
    for (const key of completedMap.keys()) completedIds.add(key.slice(PREFIX_COMPLETED.length));
    const out: PendingRequest[] = [];
    for (const [key, value] of map) {
      const requestId = key.slice(PREFIX_PENDING.length);
      const decoded = decodeStoredPendingRequest(value);
      if (!decoded) continue;
      // Completed-wins on restart: if a completed row already settled this
      // request, the pending row is stale (a crash after completed was written
      // but before the pending delete). Skip it and delete it best-effort so a
      // fully-settled request can never resurrect as active.
      if (completedIds.has(requestId)) {
        try { await this.state.storage.delete(PREFIX_PENDING + requestId); } catch { /* best-effort */ }
        continue;
      }
      // Pre-fix releases persisted safe reads. Never rehydrate or resume them:
      // after a restart their caller no longer has an in-memory waiter and a
      // read is safe to retry. Leave historical rows untouched for now rather
      // than spending scarce write quota cleaning them up.
      const opClass = classifyOp(decoded.op);
      if (opClass === "read") continue;
      decoded.opClass = opClass;
      out.push(decoded);
    }
    return out;
  }

  private async loadCompleted(): Promise<Array<{ requestId: string; completion: Completion; idempotencyKey?: string; op?: string }>> {
    const map = await this.state.storage.list<unknown>({ prefix: PREFIX_COMPLETED });
    const out: Array<{ requestId: string; completion: Completion; idempotencyKey?: string; op?: string }> = [];
    for (const [key, value] of map) {
      const row = decodeCompletedRow(value);
      if (!row) continue;
      out.push({
        requestId: key.slice(PREFIX_COMPLETED.length),
        completion: row.completion,
        ...(row.idempotencyKey !== undefined ? { idempotencyKey: row.idempotencyKey } : {}),
        ...(row.op !== undefined ? { op: row.op } : {}),
      });
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
    if (request.method === "POST" && url.pathname === "/internal/revoke") {
      return this.handleRevoke();
    }
    return this.json({ ok: false, code: "not_found", retryable: false }, 404);
  }

  private json(body: unknown, status = 200): Response {
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  }

  private async handleStatus(): Promise<Response> {
    const now = Date.now();
    const hasActiveLink = this.state.getWebSockets("link").some((ws) => this.isActiveLink(ws));
    // Fail closed: a revoked device, or a device whose session is offline
    // (cold start, post-revoke, expired presence), is never reported online.
    const revoked = this.revoked;
    const sessionOffline = this.session === undefined || this.session.status === "offline";
    const stale = this.session !== undefined && isStale(this.limits, this.session, now);
    const online = !revoked && !sessionOffline && (hasActiveLink || !stale);
    const payload = sessionSummary(this.session, {
      now,
      linkStaleAfterMs: this.limits.linkStaleAfterMs,
      activeRequests: this.registry.activeCount() + this.ephemeralReads.size,
      edgeVersion: this.env.EDGE_VERSION ?? "0.1.0-dev",
    });
    const connected = !revoked && payload.connected;
    const status = revoked ? "offline" : payload.status;
    return this.json({ ...payload, online, connected, status });
  }

  // ------------------------------------------------- internal revoke

  /**
   * Kill switch: tear down the live link and settle all in-flight work for a
   * revoked device. Idempotent — a repeated revoke performs no additional
   * tombstone/session writes but still re-closes any lingering sockets.
   */
  private async handleRevoke(): Promise<Response> {
    const now = Date.now();
    // The in-memory fence is set immediately so no new work is admitted even if
    // a later storage write fails. Durability is tracked separately: success is
    // reported only after the tombstone is persisted.
    if (!this.revoked) {
      this.revoked = true;
      this.revokedAtMs = now;
    }
    let tombstonePersisted = this.revokedPersisted;
    if (!tombstonePersisted) {
      try {
        await this.state.storage.put(KEY_REVOKED, { revoked_at_ms: this.revokedAtMs ?? now });
        this.revokedPersisted = true;
        tombstonePersisted = true;
      } catch (persistErr) {
        this.logger.warn("ws.revoke.tombstone_persist_failed", {
          workstationId: this.session?.workstationId,
          reason: persistErr instanceof Error ? persistErr.message : "storage_put_failed",
        });
      }
    }

    // Mark every link socket inactive before closing, so isActiveLink() is
    // false and no later frame can be treated as a live link. This runs even
    // when the tombstone write failed: the live link is always torn down.
    for (const ws of this.state.getWebSockets("link")) {
      ws.serializeAttachment({ ...this.readLinkAttachment(ws), active: false });
      try { ws.close(REVOKED_CLOSE_CODE, "device revoked"); } catch { /* already closed */ }
    }

    // Wake every reconnect-grace waiter immediately: revoke is not a transient
    // link blip, so waits must fail fast instead of running to their timeout.
    this.notifyLinkAvailable();

    // Settle in-flight work immediately (ephemeral reads + durable mutations)
    // without relying on a later close callback. Each settlement persists
    // durably first and keeps a retryable state on failure, so no waiter hangs.
    let ephemeralSettled = 0;
    for (const read of [...this.ephemeralReads.values()]) {
      const err: RelayErrorResult = {
        ok: false,
        code: "link_auth_failed",
        retryable: false,
        message: "device revoked; request not delivered",
        requestId: read.requestId,
        workstationId: read.workstationId,
        atMs: now,
      };
      this.settleEphemeralRead(read.requestId, { status: "error", error: err, servedAtMs: now });
      ephemeralSettled += 1;
    }
    const classifications = this.registry.classifyAllOnClose(now);
    let pendingTotal = 0;
    let pendingSettled = 0;
    for (const [requestId, err] of classifications) {
      pendingTotal += 1;
      const settled = await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: now });
      if (settled) pendingSettled += 1;
    }

    if (this.session && this.session.status !== "offline") {
      this.session.status = "offline";
      this.session.disconnectedAtMs = now;
      try {
        await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
      } catch (persistErr) {
        this.logger.warn("ws.revoke.session_persist_failed", {
          workstationId: this.session.workstationId,
          reason: persistErr instanceof Error ? persistErr.message : "storage_put_failed",
        });
      }
    }

    this.logger.warn("ws.revoke.teardown", {
      workstationId: this.session?.workstationId,
      tombstonePersisted,
      pendingSettled,
      pendingTotal,
      ephemeralSettled,
    });

    // Never report success before the tombstone is durable: a restart must
    // fail closed from the persisted tombstone, and a retry must keep trying
    // to persist rather than short-circuiting on the in-memory fence.
    if (!tombstonePersisted) {
      return this.json(
        {
          ok: false,
          code: "revoke_tombstone_pending",
          retryable: true,
          revoked: true,
          revoked_at_ms: this.revokedAtMs ?? now,
        },
        503,
      );
    }
    // Partial settlement: some durable settlement writes failed, so the full
    // teardown is not complete. The retained pending entries are retryable and
    // a subsequent revoke retry re-classifies and re-persists them.
    if (pendingTotal > 0 && pendingSettled < pendingTotal) {
      return this.json(
        {
          ok: false,
          code: "revoke_settlement_pending",
          retryable: true,
          revoked: true,
          revoked_at_ms: this.revokedAtMs ?? now,
          pending_settled: pendingSettled,
          pending_total: pendingTotal,
        },
        503,
      );
    }
    return this.json({ ok: true, revoked: true, revoked_at_ms: this.revokedAtMs ?? now });
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

    // Fail closed after revocation: reject immediately, without reconnect-grace
    // waiting, so a stolen link can never continue tool requests.
    if (this.revoked) {
      return this.json(
        {
          status: "error",
          error: errorResult("link_auth_failed", {
            requestId,
            workstationId,
            atMs: now,
            retryable: false,
            message: "device revoked; workstation link is no longer authorized",
          }),
        },
        401,
      );
    }

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
      trace: req.trace,
    };

    if (this.session?.status === "draining") {
      return this.json({ status: "error", error: drainingResult({ requestId, workstationId, atMs: now }) }, 503);
    }

    const deadlineMs = now + (wire.timeout_ms ?? this.limits.requestTimeoutMs);
    if (!this.hasActiveLink()) {
      const waited = await this.waitForActiveLink(deadlineMs);
      // Re-check revocation after waiting: a revoke that woke the waiter must
      // fail fast with a non-retryable auth error, not a retryable offline.
      if (this.revoked) {
        return this.json(
          {
            status: "error",
            error: errorResult("link_auth_failed", {
              requestId,
              workstationId,
              atMs: Date.now(),
              retryable: false,
              message: "device revoked; workstation link is no longer authorized",
            }),
          },
          401,
        );
      }
      if (!waited) {
        return this.json({ status: "error", error: offlineResult({ requestId, workstationId, atMs: now }) }, 503);
      }
    }

    const budget = checkArgsBudget(wire.arguments, this.limits.maxFrameBytes);
    if (!budget.ok) {
      return this.json(
        { status: "error", error: errorResult("payload_too_large", { requestId, workstationId, atMs: now }) },
        413,
      );
    }

    const opClass = classifyOp(req.op);

    // Known reads are safe to retry after ambiguity. Keep their request
    // lifecycle process-local so read-heavy traffic consumes zero Durable
    // Storage rows and zero Durable Object alarm writes.
    if (opClass === "read") {
      return this.forwardEphemeralRead({ requestId, workstationId, op: req.op, deadlineMs, wire, now });
    }

    // Reconcile any durable mutation left past deadline by a previous isolate
    // before admitting a new mutation. If storage is over quota this fails
    // before the new mutation is sent, preserving fail-closed semantics.
    for (const expired of this.registry.expired(now)) {
      const err = timeoutResult({
        requestId: expired.requestId,
        workstationId: expired.workstationId,
        atMs: now,
        opClass: expired.opClass,
      });
      await this.persistSettlement(expired.requestId, { status: "error", error: err, servedAtMs: now });
    }

    // Preserve the original global in-flight bound when ephemeral reads are
    // present. With no reads, registry.add keeps its existing queued-eviction
    // behavior at the durable capacity limit.
    if (
      this.ephemeralReads.size > 0 &&
      this.registry.activeCount() + this.ephemeralReads.size >= this.limits.maxPendingRequests
    ) {
      return this.json({ status: "error", error: capacityResult({ requestId, workstationId, atMs: now }) }, 429);
    }

    const add = this.registry.add({
      requestId,
      workstationId,
      op: req.op,
      opClass,
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

    const encoded = encodeWire(wire, this.limits.maxFrameBytes);
    if (!encoded.ok) {
      this.registry.removeActive(entry.requestId);
      return this.json(
        { status: "error", error: errorResult("payload_too_large", { requestId: entry.requestId, workstationId, atMs: now }) },
        413,
      );
    }

    // Pre-send durability fence: persist the request conservatively as "sent"
    // and arm its deadline before any Link delivery is attempted. If Durable
    // Storage is at quota, the mutation fails closed here and is never sent.
    // This also removes the former second pending-row write after socket send.
    this.registry.markSent(entry.requestId, now);
    await this.state.storage.put(PREFIX_PENDING + entry.requestId, entry);
    await this.armAlarm();

    // Test seam: deterministic pre-send interleave point. A test can pause a
    // durable mutation here (after the pre-send durability fence, before the
    // stillOwned/revoked re-check and actual socket send) and run a revoke to
    // prove the revoked gate below never delivers a mutation to a revoked
    // device. Production always proceeds immediately.
    if (this.beforeDurableSendHook) await this.beforeDurableSendHook();

    // Storage/alarm awaits may yield. Re-confirm the durable request is still
    // active before sending; if another event already settled it, do not send.
    // Also re-gate on revoke: a revoke that landed between the pre-send storage
    // await and this point must prevent delivery of the mutation to the (now)
    // revoked device.
    const stillOwned = this.registry.get(entry.requestId) === entry;
    if (!stillOwned) {
      const settledCompletion = this.registry.completedFor(entry.requestId);
      if (settledCompletion) {
        return this.json({ status: "ok", completion: settledCompletion });
      }
      return this.json(
        { status: "error", error: reconnectingResult({ requestId: entry.requestId, workstationId, atMs: now }) },
        503,
      );
    }
    if (this.revoked) {
      const revokeErr: RelayErrorResult = {
        ok: false,
        code: "link_auth_failed",
        retryable: false,
        message: "device revoked; mutation not delivered",
        requestId: entry.requestId,
        workstationId,
        atMs: Date.now(),
      };
      // Persist the denial durably (completed-first ordering) and settle the
      // waiter so it does not hang; the pending row is removed last. The entry
      // stays in the registry so persistSettlement (not removeActive) both
      // persists and resolves the waiter; settle() is idempotent against a
      // concurrent revoke settlement.
      const settled = await this.persistSettlement(entry.requestId, { status: "error", error: revokeErr, servedAtMs: Date.now() });
      if (settled) {
        return this.json(
          {
            status: "error",
            error: errorResult("link_auth_failed", {
              requestId: entry.requestId,
              workstationId,
              atMs: now,
              retryable: false,
              message: "device revoked; mutation not delivered",
            }),
          },
          401,
        );
      }
      return this.json(
        {
          status: "error",
          error: errorResult("link_auth_failed", {
            requestId: entry.requestId,
            workstationId,
            atMs: now,
            retryable: true,
            message: "device revoked; settlement persistence pending, retry required",
          }),
        },
        503,
      );
    }

    const sent = this.sendToActiveLink(wire);
    if (!sent) {
      await this.handleLinkGone("ws.send.race", { requestId: entry.requestId });
      return this.json({ status: "error", error: offlineResult({ requestId: entry.requestId, workstationId, atMs: now }) }, 503);
    }
    const completion = await this.awaitCompletion(entry.requestId, deadlineMs);
    return this.json({ status: "ok", completion });
  }

  private hasActiveLink(): boolean {
    return this.state.getWebSockets("link").some((ws) => this.isActiveLink(ws));
  }

  private waitForActiveLink(deadlineMs: number): Promise<boolean> {
    if (this.hasActiveLink()) return Promise.resolve(true);
    const now = Date.now();
    const recentlyConnected = this.session?.connectedAtMs !== undefined
      && (this.session.status === "online"
        || this.session.status === "connecting"
        || (this.session.disconnectedAtMs !== undefined
          && now - this.session.disconnectedAtMs <= this.limits.linkReconnectGraceMs));
    if (!recentlyConnected) return Promise.resolve(false);
    const waitMs = Math.max(0, Math.min(this.limits.linkReconnectGraceMs, deadlineMs - now));
    if (waitMs === 0) return Promise.resolve(false);
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const finish = (online: boolean) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        this.linkWaiters.delete(onLink);
        resolve(online);
      };
      const onLink = () => finish(this.hasActiveLink());
      const timer = setTimeout(() => finish(this.hasActiveLink()), waitMs);
      this.linkWaiters.add(onLink);
      if (this.hasActiveLink()) finish(true);
    });
  }

  private notifyLinkAvailable(): void {
    for (const waiter of [...this.linkWaiters]) waiter();
  }

  private async forwardEphemeralRead(opts: {
    requestId: string;
    workstationId: string;
    op: string;
    deadlineMs: number;
    wire: ToolRequestMessage;
    now: number;
  }): Promise<Response> {
    const { requestId, workstationId, op, deadlineMs, wire, now } = opts;
    // Preserve one coherent live-request capacity bound, but exclude expired
    // durable rows so historical mutation backlog cannot starve fresh reads.
    if (this.registry.liveCount(now) + this.ephemeralReads.size >= this.limits.maxPendingRequests) {
      return this.json({ status: "error", error: capacityResult({ requestId, workstationId, atMs: now }) }, 429);
    }
    if (this.ephemeralReads.has(requestId) || this.registry.get(requestId)) {
      return this.json(
        { status: "error", error: errorResult("bad_request", { requestId, workstationId, atMs: now }) },
        400,
      );
    }

    const entry: EphemeralReadRequest = {
      requestId,
      workstationId,
      op,
      state: "queued",
      createdAtMs: now,
      deadlineMs,
    };
    this.ephemeralReads.set(requestId, entry);

    const encoded = encodeWire(wire, this.limits.maxFrameBytes);
    if (!encoded.ok) {
      this.ephemeralReads.delete(requestId);
      return this.json(
        { status: "error", error: errorResult("payload_too_large", { requestId, workstationId, atMs: now }) },
        413,
      );
    }

    if (!this.sendToActiveLink(wire)) {
      this.ephemeralReads.delete(requestId);
      await this.handleLinkGone("ws.send.race", { requestId });
      return this.json({ status: "error", error: offlineResult({ requestId, workstationId, atMs: now }) }, 503);
    }
    entry.state = "sent";
    entry.sentAtMs = now;
    const completion = await this.awaitEphemeralRead(requestId, deadlineMs);
    return this.json({ status: "ok", completion });
  }

  private awaitEphemeralRead(requestId: string, deadlineMs: number): Promise<Completion> {
    return new Promise<Completion>((resolve) => {
      this.resolvers.set(requestId, resolve);
      const entry = this.ephemeralReads.get(requestId);
      const remaining = deadlineMs - Date.now();
      if (remaining <= 0) {
        void this.settleAsTimeout(requestId);
        return;
      }
      const timer = setTimeout(() => {
        if (this.ephemeralReads.has(requestId)) void this.settleAsTimeout(requestId);
      }, remaining);
      if (entry) entry.timer = timer;
    });
  }

  private settleEphemeralRead(requestId: string, completion: Completion): EphemeralReadRequest | undefined {
    const entry = this.ephemeralReads.get(requestId);
    if (!entry) return undefined;
    this.ephemeralReads.delete(requestId);
    if (entry.timer !== undefined) clearTimeout(entry.timer);
    const resolve = this.resolvers.get(requestId);
    if (resolve) {
      this.resolvers.delete(requestId);
      resolve(completion);
    }
    return entry;
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
    const read = this.ephemeralReads.get(requestId);
    if (read) {
      const now = Date.now();
      const err = timeoutResult({
        requestId,
        workstationId: read.workstationId,
        atMs: now,
        opClass: "read",
      });
      const wasSent = read.state === "sent";
      this.settleEphemeralRead(requestId, { status: "error", error: err, servedAtMs: now });
      if (wasSent) {
        const cancel: CancelMessage = {
          protocol_version: RELAY_PROTOCOL_VERSION,
          kind: "cancel",
          workstation_id: read.workstationId,
          request_id: read.requestId,
          reason: "deadline exceeded",
        };
        this.sendToActiveLink(cancel);
      }
      return;
    }
    const entry = this.registry.get(requestId);
    if (!entry || entry.state === "settled") return;
    const wasSent = entry.state === "sent";
    const now = Date.now();
    const err = timeoutResult({
      requestId,
      workstationId: entry.workstationId,
      atMs: now,
      opClass: entry.opClass,
    });
    await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: now });
    // The normal in-memory deadline timer must propagate cancellation too.
    // Otherwise the Edge waiter is settled while Link/local HTTP work keeps
    // running until its own longer timeout and continues occupying runtime
    // generation in-flight capacity.
    if (wasSent) {
      const cancel: CancelMessage = {
        protocol_version: RELAY_PROTOCOL_VERSION,
        kind: "cancel",
        workstation_id: entry.workstationId,
        request_id: entry.requestId,
        reason: "deadline exceeded",
      };
      this.sendToActiveLink(cancel);
    }
  }

  /**
   * Persist a durable settlement and resolve its waiter. Returns true only when
   * the settlement was durably persisted and the waiter resolved.
   *
   * Durable ordering is recoverable: the completed row (and any idempotency
   * evidence) are written FIRST, and the pending row is deleted LAST. A crash
   * or write failure mid-sequence therefore never leaves a state with neither
   * pending nor completed (which would be an unrecoverable hole). On restart,
   * completed-wins: loadPending skips any pending row whose completed twin
   * exists and deletes the stale row best-effort. With a partial failure the
   * pending entry and resolver are retained (retryable state), so a retry
   * (revoke retry, alarm sweep, or a later forward's expired reconciliation)
   * can converge and no waiter hangs.
   */
  private async persistSettlement(requestId: string, completion: Completion): Promise<boolean> {
    const candidate = this.registry.get(requestId);
    if (candidate && classifyOp(candidate.op) === "read") {
      this.logger.warn("pending.read_rejected_from_durable_settlement", { requestId, op: candidate.op });
      this.registry.removeActive(requestId);
      const resolve = this.resolvers.get(requestId);
      if (resolve) {
        this.resolvers.delete(requestId);
        resolve(completion);
      }
      return true;
    }
    const entry = this.registry.get(requestId);
    if (!entry || entry.state === "settled") return false;
    const releaseIdempotency = releasesIdempotencyBinding(completion);
    const settlementEntry = releaseIdempotency
      ? { ...entry, idempotencyKey: undefined }
      : entry;

    // Persist durably FIRST (completed + idem evidence before the pending
    // delete). The completed row embeds the idempotency binding, so even if the
    // separate idem:<key> write fails or a crash lands between writes, the
    // binding is recovered from the completed row on init. Only after every
    // write succeeds do we settle the in-memory entry and resolve the waiter. A
    // mid-sequence failure leaves the pending entry and resolver intact so a
    // retry can converge; on restart the completed twin wins and the stale
    // pending row is cleaned up.
    try {
      await this.state.storage.put(PREFIX_COMPLETED + requestId, encodeCompletedRow(settlementEntry, completion) as unknown as string);
      if (settlementEntry.idempotencyKey !== undefined) {
        const record: IdempotencyRecord = {
          idempotencyKey: settlementEntry.idempotencyKey,
          requestId,
          op: settlementEntry.op,
          settledAtMs: completion.servedAtMs,
        };
        await this.state.storage.put(PREFIX_IDEM + settlementEntry.idempotencyKey, record as unknown as string);
      }
      await this.state.storage.delete(PREFIX_PENDING + requestId);
    } catch (persistErr) {
      this.logger.warn("pending.settlement_persist_failed", {
        requestId,
        op: entry.op,
        reason: persistErr instanceof Error ? persistErr.message : "storage_write_failed",
      });
      return false;
    }

    if (releaseIdempotency) entry.idempotencyKey = undefined;
    this.registry.settle(requestId, completion);
    const resolve = this.resolvers.get(requestId);
    if (resolve) {
      this.resolvers.delete(requestId);
      resolve(completion);
    }
    void this.armAlarm();
    return true;
  }

  /**
   * Close out a request that was evicted from the capacity-bound pending map
   * (its registry entry no longer exists, so settle() would no-op). Records the
   * completion + idempotency without re-occupying pending capacity, deletes the
   * durable pending:<id> key so the evicted request can never resurrect on
   * rehydrate, and resolves any waiter. Mirrors persistSettlement but takes the
   * explicit evicted entry.
   */
  private async persistEvictedSettlement(entry: PendingRequest, completion: Completion): Promise<boolean> {
    const requestId = entry.requestId;
    if (classifyOp(entry.op) === "read") {
      this.logger.warn("pending.read_rejected_from_durable_eviction", { requestId, op: entry.op });
      const resolve = this.resolvers.get(requestId);
      if (resolve) {
        this.resolvers.delete(requestId);
        resolve(completion);
      }
      return true;
    }
    // Persist durably FIRST (completed + idem evidence before the pending
    // delete); on failure retain the evicted entry's completion state so a
    // retry can converge without dropping the waiter. The completed row embeds
    // the idempotency binding, so completed-wins on restart recovers both the
    // completion and the binding even if the idem:<key> write failed.
    try {
      await this.state.storage.put(PREFIX_COMPLETED + requestId, encodeCompletedRow(entry, completion) as unknown as string);
      if (entry.idempotencyKey !== undefined) {
        const record: IdempotencyRecord = {
          idempotencyKey: entry.idempotencyKey,
          requestId,
          op: entry.op,
          settledAtMs: completion.servedAtMs,
        };
        await this.state.storage.put(PREFIX_IDEM + entry.idempotencyKey, record as unknown as string);
      }
      await this.state.storage.delete(PREFIX_PENDING + requestId);
    } catch (persistErr) {
      this.logger.warn("pending.evicted_settlement_persist_failed", {
        requestId,
        op: entry.op,
        reason: persistErr instanceof Error ? persistErr.message : "storage_write_failed",
      });
      return false;
    }
    this.registry.recordSettlement(entry, completion);
    const resolve = this.resolvers.get(requestId);
    if (resolve) {
      this.resolvers.delete(requestId);
      resolve(completion);
    }
    void this.armAlarm();
    return true;
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
    // Defense in depth: a revoked DO rejects every frame (including heartbeat
    // and tool_result) so a stolen link cannot mutate session or settle requests.
    if (this.revoked) {
      try { ws.close(REVOKED_CLOSE_CODE, "device revoked"); } catch { /* already closed */ }
      return;
    }
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
    // Defense in depth: the Worker upgrade already returns 401 from the registry
    // check, but a revoked DO must never accept a hello either.
    if (this.revoked) {
      try { ws.close(REVOKED_CLOSE_CODE, "device revoked"); } catch { /* already closed */ }
      return;
    }
    const expected = this.workstationId();
    if (hello.workstation_id !== expected) {
      this.logger.warn("ws.hello.mismatch", { workstationId: expected, claimedId: hello.workstation_id });
      try { ws.close(1008, "close_rejected"); } catch { /* already closed */ }
      return;
    }

    if (!isCompatibleRuntimeContract(hello.runtime?.contract_epoch, hello.runtime?.contract_hash)) {
      const ack: HelloAckMessage = {
        protocol_version: RELAY_PROTOCOL_VERSION,
        kind: "hello_ack",
        workstation_id: expected,
        ok: false,
        code: "contract_mismatch",
        message: `edge requires runtime contract epoch ${RUNTIME_EXECUTION_CONTRACT.contract_epoch} or the immediately previous rollback baseline`,
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
    this.notifyLinkAvailable();

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
    this.lastSeenPersistedAtMs = now;

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
    let runtimeChanged = false;
    let persisted = false;
    let statusReplied = false;
    if (this.session) {
      this.session.lastSeenAtMs = now;
      runtimeChanged = applyRuntimeStatusGlimpse(this.session, msg.runtime, true);
      if (runtimeChanged || now - this.lastSeenPersistedAtMs >= HEARTBEAT_PERSIST_THROTTLE_MS) {
        this.lastSeenPersistedAtMs = now;
        await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
        persisted = true;
      }
      if (runtimeChanged || now - this.lastEdgeStatusReplyAtMs >= EDGE_STATUS_REPLY_INTERVAL_MS) {
        this.lastEdgeStatusReplyAtMs = now;
        statusReplied = true;
        const edgeSeen: StatusMessage = {
          protocol_version: RELAY_PROTOCOL_VERSION,
          kind: "status",
          workstation_id: this.workstationId(),
          healthy: true,
          sent_at_ms: now,
        };
        this.sendToSocket(ws, edgeSeen);
      }
    }
    if (runtimeChanged || persisted || statusReplied) {
      this.logger.info("ws.heartbeat", {
        workstationId: this.session?.workstationId,
        bootId: msg.boot_id,
        runtimeChanged,
        persisted,
        statusReplied,
      });
    }
  }

  private async handleLinkStatus(msg: StatusMessage): Promise<void> {
    if (!this.session) return;
    const previous = this.session.runtimeStatus;
    const previousRuntimeVersion = previous?.runtimeVersion;
    const previousRuntimeGeneration = previous?.runtimeGeneration;
    const previousHerdProtocolVersion = previous?.herdProtocolVersion;
    const previousHealth = previous?.health;
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
    const current = this.session.runtimeStatus;
    const runtimeChanged =
      previousRuntimeVersion !== current?.runtimeVersion ||
      previousRuntimeGeneration !== current?.runtimeGeneration ||
      previousHerdProtocolVersion !== current?.herdProtocolVersion ||
      previousHealth !== current?.health;
    if (runtimeChanged) {
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
    const read = this.ephemeralReads.get(requestId);
    if (read) {
      const completion: Completion = {
        status: "ok",
        result: msg.result ?? null,
        servedAtMs: msg.served_at_ms,
        runtimeGeneration: msg.runtime_generation ?? undefined,
      };
      this.settleEphemeralRead(requestId, completion);
      this.logger.info("ws.tool_result.read_settled", {
        requestId,
        workstationId: read.workstationId,
        op: read.op,
      });
      return;
    }
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
    const read = this.ephemeralReads.get(requestId);
    if (read) {
      const err: RelayErrorResult = {
        ok: false,
        code: mapLinkErrorCode(msg.code),
        retryable: msg.retryable,
        message: msg.message,
        details: msg.details,
        delivery_state: msg.delivery_state,
        requestId,
        workstationId: read.workstationId,
        atMs: msg.served_at_ms ?? Date.now(),
      };
      this.settleEphemeralRead(requestId, { status: "error", error: err, servedAtMs: Date.now() });
      this.logger.info("ws.tool_error.read_settled", {
        requestId,
        code: msg.code,
        retryable: msg.retryable,
        deliveryState: msg.delivery_state,
      });
      return;
    }
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
    // Settle ephemeral reads before any Durable Storage write. Under a
    // rows_written quota breach, session/mutation persistence may throw;
    // known reads must still resolve with zero request-ledger writes.
    let ephemeralSettled = 0;
    for (const read of [...this.ephemeralReads.values()]) {
      const classification = classifyAmbiguousDelivery(read.state, "read");
      const err: RelayErrorResult = {
        ok: false,
        code: classification.code,
        retryable: classification.retryable,
        message: classification.retryable
          ? "connection lost before a confirmed read result; safe to retry"
          : "connection lost; delivery outcome unknown",
        requestId: read.requestId,
        workstationId: read.workstationId,
        atMs: now,
      };
      this.settleEphemeralRead(read.requestId, { status: "error", error: err, servedAtMs: now });
      ephemeralSettled += 1;
    }
    if (this.session && this.session.status !== "offline") {
      this.session.status = "offline";
      this.session.disconnectedAtMs = now;
      try {
        await this.state.storage.put(KEY_SESSION, serializeSession(this.session));
      } catch (persistErr) {
        this.logger.warn("ws.link_gone.session_persist_failed", {
          workstationId: this.session.workstationId,
          reason: persistErr instanceof Error ? persistErr.message : "storage_put_failed",
        });
      }
    }
    const classifications = this.registry.classifyAllOnClose(now);
    for (const [requestId, err] of classifications) {
      await this.persistSettlement(requestId, { status: "error", error: err, servedAtMs: now });
    }
    this.logger.warn(event, {
      workstationId: this.session?.workstationId,
      pendingSettled: classifications.size,
      ephemeralSettled,
      ...fields,
    });
  }

  // --------------------------------------------------------------- alarm

  async alarm(): Promise<void> {
    await this.ensureInit();
    const now = Date.now();
    for (const entry of this.registry.expired(now)) {
      await this.settleAsTimeout(entry.requestId);
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