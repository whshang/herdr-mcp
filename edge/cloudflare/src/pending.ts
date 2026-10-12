/**
 * pending.ts — bounded pending-request registry + request-id generation.
 *
 * The DO persists pending entries to its own storage; the registry held in
 * memory is a cache (plan §11: state that must survive hibernation lives in
 * DO storage). This module implements the bounded maps, idempotency-key
 * dedup window and (de)serialization so all capacity/expiry logic is pure and
 * unit-testable.
 *
 * Idempotency replays are authorized, not just deduplicated: a recorded
 * completion is only replayed when the *same* authenticated principal,
 * routed device and connector grant generation present the *same* canonical
 * request, and any Page Assist endpoint the cached request required is still
 * granted to that principal. A caller-supplied key is therefore an identity
 * hint, never an authorization to read another caller's result. A record that
 * owns the key but predates this binding fails closed instead of being
 * forwarded again, so the upgrade window cannot re-execute a settled mutation.
 */

import type { RelayErrorResult } from "./errors.js";
import { classifyAmbiguousDelivery } from "./errors.js";
import type { EdgeLimits, OpClass } from "./limits.js";

/** Ownership key used when a request arrives without a trusted principal. */
export const LEGACY_PRINCIPAL_REF = "principal:legacy";

export type PendingState = "queued" | "sent" | "settled";

export interface PendingRequest {
  requestId: string;
  workstationId: string;
  /** Trusted Edge-only ownership key used for per-principal admission. */
  resourcePrincipalRef?: string;
  op: string;
  opClass: OpClass;
  /** Diagnostic summary only — NO argument values, ever. */
  argsSummary: { argKeys: string[] };
  state: PendingState;
  createdAtMs: number;
  sentAtMs?: number;
  deadlineMs: number;
  /** Raw caller-supplied idempotency key (relay dedup hint, diagnostics). */
  idempotencyKey?: string;
  /** Trusted Edge-only routed device; part of the replay-scope identity. */
  routeDeviceId?: string;
  /** Edge-canonical hash of inner method + canonical args + endpoint_ref. */
  requestHash?: string;
  /** Page Assist endpoint this page-scoped request requires, when any. */
  pageScopedEndpointRef?: string;
  /** Authenticated connector grant generation at admission. */
  grantGeneration?: number;
  contractEpoch?: number;
}

export type Completion =
  | { status: "ok"; result: unknown; servedAtMs: number; runtimeGeneration?: string }
  | { status: "error"; error: RelayErrorResult; servedAtMs: number };

/**
 * One settled idempotent mutation. `idempotencyKey` is the *scoped* ledger
 * identity (see `idempotencyScopeKey`); the raw caller key and the request
 * binding are retained so a replay can be authorized rather than assumed.
 */
export interface IdempotencyRecord {
  idempotencyKey: string;
  /** Raw caller-supplied key (diagnostics only). */
  callerKey?: string;
  requestId: string;
  op: string;
  settledAtMs: number;
  principalRef?: string;
  routeDeviceId?: string;
  grantGeneration?: number;
  requestHash?: string;
  pageScopedEndpointRef?: string;
}

/** Trusted replay scope for one idempotent mutation request. */
export interface ReplayScope {
  /** Trusted Edge-derived principal identity (capacity + replay ownership). */
  principalRef: string;
  routeDeviceId?: string;
  /** Authenticated connector grant generation at this request. */
  grantGeneration?: number;
  /** Raw caller-supplied idempotency key. */
  callerKey: string;
  /** Edge-canonical hash binding the key to this exact request payload. */
  requestHash: string;
  /** Page Assist endpoint this request requires, when the method is scoped. */
  pageScopedEndpointRef?: string;
  /** Page Assist endpoints the caller is authorized for right now. */
  authorizedPageAssistEndpoints?: readonly string[];
}

/**
 * Deterministic, injective ledger identity for one caller idempotency key.
 * The tuple is JSON-encoded, so a crafted caller key can never collide with a
 * different principal / device / generation scope.
 */
export function idempotencyScopeKey(scope: {
  principalRef: string;
  routeDeviceId?: string;
  grantGeneration?: number;
  callerKey: string;
}): string {
  return JSON.stringify([
    scope.principalRef,
    scope.routeDeviceId ?? null,
    scope.grantGeneration ?? null,
    scope.callerKey,
  ]);
}

export type ReplayOutcome =
  | { status: "none" }
  | { status: "hit"; completion: Completion }
  /** Same caller re-used a key for a different request payload. */
  | { status: "payload_mismatch" }
  /** Cached page-scoped completion whose endpoint grant is no longer held. */
  | { status: "grant_required"; endpointRef?: string }
  /**
   * A settled record already owns this caller key but carries no principal or
   * payload binding, so its completion can be neither served nor safely
   * re-executed. Callers must refuse the request instead of forwarding it.
   */
  | { status: "binding_unavailable"; settledAtMs?: number };

/**
 * A record is replayable only when it proves both ownership (the trusted
 * principal it was settled under) and payload identity (the canonical request
 * hash). Records written before either existed can never be replayed.
 */
function hasReplayBinding(rec: IdempotencyRecord): boolean {
  return rec.requestHash !== undefined && rec.principalRef !== undefined;
}

/**
 * Durable idempotency record for one settled request entry, or undefined when
 * the request carried no caller key (or its binding was released). Both the
 * in-memory registry and the DO persistence path build the record through this
 * single function so the scope can never drift between them.
 */
export function idempotencyRecordFor(
  entry: PendingRequest,
  settledAtMs: number,
): IdempotencyRecord | undefined {
  if (entry.idempotencyKey === undefined) return undefined;
  const principalRef = entry.resourcePrincipalRef ?? LEGACY_PRINCIPAL_REF;
  const idempotencyKey = idempotencyScopeKey({
    principalRef,
    ...(entry.routeDeviceId !== undefined ? { routeDeviceId: entry.routeDeviceId } : {}),
    ...(entry.grantGeneration !== undefined ? { grantGeneration: entry.grantGeneration } : {}),
    callerKey: entry.idempotencyKey,
  });
  return {
    idempotencyKey,
    callerKey: entry.idempotencyKey,
    requestId: entry.requestId,
    op: entry.op,
    settledAtMs,
    principalRef,
    ...(entry.routeDeviceId !== undefined ? { routeDeviceId: entry.routeDeviceId } : {}),
    ...(entry.grantGeneration !== undefined ? { grantGeneration: entry.grantGeneration } : {}),
    ...(entry.requestHash !== undefined ? { requestHash: entry.requestHash } : {}),
    ...(entry.pageScopedEndpointRef !== undefined
      ? { pageScopedEndpointRef: entry.pageScopedEndpointRef }
      : {}),
  };
}

export interface PrunedCompletion {
  requestId: string;
  idempotencyKey?: string;
}

/** Decode a pending request persisted in Durable Object storage. */
export function decodeStoredPendingRequest(value: unknown): PendingRequest | null {
  let candidate: unknown = value;
  if (typeof candidate === "string") {
    try {
      candidate = JSON.parse(candidate) as unknown;
    } catch {
      return null;
    }
  }
  if (candidate === null || typeof candidate !== "object") return null;
  const p = candidate as Partial<PendingRequest>;
  if (
    typeof p.requestId !== "string" ||
    typeof p.workstationId !== "string" ||
    typeof p.op !== "string" ||
    (p.opClass !== "read" && p.opClass !== "mutating" && p.opClass !== "unknown") ||
    (p.state !== "queued" && p.state !== "sent" && p.state !== "settled") ||
    typeof p.createdAtMs !== "number" ||
    typeof p.deadlineMs !== "number" ||
    p.argsSummary === null ||
    typeof p.argsSummary !== "object" ||
    !Array.isArray(p.argsSummary.argKeys)
  ) {
    return null;
  }
  return candidate as PendingRequest;
}

export type AddOutcome =
  | { status: "added"; entry: PendingRequest }
  | { status: "capacity_full"; entry: PendingRequest }
  | { status: "evicted_oldest"; evicted: PendingRequest; entry: PendingRequest };

/** Cryptographically random hex request id (16 bytes). */
export function newRequestId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export interface RegistrySnapshot {
  pending: PendingRequest[];
  completed: Array<{ requestId: string; completion: Completion }>;
}

export class PendingRequestRegistry {
  private readonly limits: EdgeLimits;
  private readonly now: () => number;
  private readonly pending = new Map<string, PendingRequest>();
  private readonly completed = new Map<string, Completion>();
  private readonly idemByKey = new Map<string, IdempotencyRecord>();

  constructor(opts: { limits: EdgeLimits; now?: () => number }) {
    this.limits = opts.limits;
    this.now = opts.now ?? Date.now;
  }

  /** Persist-ready view (bounded). */
  snapshot(): RegistrySnapshot {
    return {
      pending: [...this.pending.values()].sort((a, b) => a.createdAtMs - b.createdAtMs),
      completed: [...this.completed.entries()].map(([requestId, completion]) => ({ requestId, completion })),
    };
  }

  /** Restore from a persisted snapshot during init (bounded validation). */
  restore(snapshot: RegistrySnapshot): void {
    this.pending.clear();
    this.completed.clear();
    this.idemByKey.clear();
    // Pre-fix WorkstationDO versions persisted `state=settled` rows under the
    // pending prefix. They are historical completions, never active capacity.
    // Ignore them defensively even if an old DO snapshot still contains them.
    for (const p of snapshot.pending) {
      if (p.state === "settled") continue;
      if (this.pending.size >= this.limits.maxPendingRequests) break;
      this.pending.set(p.requestId, p);
    }
    for (const c of snapshot.completed.slice(0, this.limits.maxCompletedRecords)) {
      this.completed.set(c.requestId, c.completion);
    }
  }

  /** Seed the idempotency index from persisted records (durable dedup). */
  restoreIdem(records: IdempotencyRecord[]): void {
    this.idemByKey.clear();
    for (const rec of records) this.idemByKey.set(rec.idempotencyKey, rec);
  }

  /** Idempotency key recorded for a requestId, if any. */
  idempotencyKeyFor(requestId: string): string | undefined {
    for (const rec of this.idemByKey.values()) {
      if (rec.requestId === requestId) return rec.idempotencyKey;
    }
    return undefined;
  }

  /**
   * Authorize an idempotency replay. Returns `none` unless a record exists
   * under this exact (principal, device, generation, caller key) identity;
   * a record bound to a different payload is a mismatch, and a page-scoped
   * record whose endpoint grant is no longer held is refused. Records written
   * before replay binding existed are `binding_unavailable`: they can never be
   * replayed, and they must never be forwarded either, because the mutation
   * they describe already settled and re-sending it would execute it twice.
   */
  replay(scope: ReplayScope, op: string): ReplayOutcome {
    const rec = this.idemByKey.get(idempotencyScopeKey(scope));
    if (rec !== undefined) {
      // Upgrade window: an Edge that predates the replay binding (or any path
      // that recorded the key without one) leaves a scoped record with no
      // binding. Failing closed here is what stops a same-key retry inside the
      // completed TTL from re-executing the mutation.
      if (!hasReplayBinding(rec)) {
        return rec.op === op
          ? { status: "binding_unavailable", settledAtMs: rec.settledAtMs }
          // A different op reusing the key is a new request: the pre-binding
          // ledger identity was (key, op), so it was forwarded then too.
          : { status: "none" };
      }
      if (rec.op !== op || rec.requestHash !== scope.requestHash) return { status: "payload_mismatch" };
      if (rec.pageScopedEndpointRef !== undefined) {
        const granted = scope.authorizedPageAssistEndpoints ?? [];
        if (!granted.includes(rec.pageScopedEndpointRef)) {
          return { status: "grant_required", endpointRef: rec.pageScopedEndpointRef };
        }
      }
      const completion = this.completed.get(rec.requestId);
      if (!completion) return { status: "none" };
      return { status: "hit", completion };
    }
    // Pre-scope records were indexed by the raw caller key and carry no
    // ownership or payload binding. Settled completions always keep their
    // idempotency record (see `recordSettlement`/`restoreIdem`), so a raw-key
    // record for this caller key and op can only mean "already settled,
    // unprovable": refuse rather than forward a second mutation.
    const preScope = this.idemByKey.get(scope.callerKey);
    if (preScope !== undefined && !hasReplayBinding(preScope) && preScope.op === op) {
      return { status: "binding_unavailable", settledAtMs: preScope.settledAtMs };
    }
    return { status: "none" };
  }

  /**
   * Admit a request into the capacity-bound registry. Idempotency replay is
   * NOT decided here: callers must authorize a replay through `replay()`
   * before admitting a fresh request, so capacity never implies authorization.
   */
  add(req: Omit<PendingRequest, "state" | "createdAtMs">): AddOutcome {
    const now = this.now();
    if (this.pending.size >= this.limits.maxPendingRequests) {
      // Prefer evicting the oldest queued (unsent) request to make room.
      let oldestQueued: PendingRequest | undefined;
      for (const p of this.pending.values()) {
        if (p.state !== "queued") continue;
        if (!oldestQueued || p.createdAtMs < oldestQueued.createdAtMs) oldestQueued = p;
      }
      if (oldestQueued) {
        this.pending.delete(oldestQueued.requestId);
        this.pruneCompletedCache();
        const entry: PendingRequest = { ...req, state: "queued", createdAtMs: now };
        this.pending.set(req.requestId, entry);
        return { status: "evicted_oldest", evicted: oldestQueued, entry };
      }
      return { status: "capacity_full", entry: { ...req, state: "queued", createdAtMs: now } };
    }
    const entry: PendingRequest = { ...req, state: "queued", createdAtMs: now };
    this.pending.set(req.requestId, entry);
    return { status: "added", entry };
  }

  get(requestId: string): PendingRequest | undefined {
    return this.pending.get(requestId);
  }

  /** Remove an active entry without recording a completion. Defensive escape
   * hatch for entries that must never enter the durable completion ledger. */
  removeActive(requestId: string): PendingRequest | undefined {
    const entry = this.pending.get(requestId);
    if (entry) this.pending.delete(requestId);
    return entry;
  }

  markSent(requestId: string, atMs: number): PendingRequest | undefined {
    const p = this.pending.get(requestId);
    if (!p) return undefined;
    p.state = "sent";
    p.sentAtMs = atMs;
    return p;
  }

  settle(
    requestId: string,
    completion: Completion,
  ): { entry: PendingRequest; pruned: PrunedCompletion[] } | undefined {
    const p = this.pending.get(requestId);
    if (!p) return undefined;
    p.state = "settled";
    // `pending` is the capacity-bound registry of active requests, not a
    // historical ledger. Completed responses live in `completed` (and the
    // idempotency index) and are persisted separately by WorkstationDO.
    // Keeping settled entries here leaks one capacity slot per tools/call and
    // eventually makes every new request fail with edge_capacity_exceeded.
    this.pending.delete(requestId);
    const pruned = this.recordSettlement(p, completion);
    return { entry: p, pruned };
  }

  /**
   * Record a completion + idempotency entry for an explicit PendingRequest
   * WITHOUT re-occupying pending capacity. Used by WorkstationDO to close out
   * a request that was evicted from the capacity-bound pending map (its entry
   * is already gone from `pending`), so durable storage/completion/idempotency
   * still settle correctly and the evicted request never resurrects on
   * rehydrate. Normal `settle()` reuses this after deleting the active slot.
   */
  recordSettlement(entry: PendingRequest, completion: Completion): PrunedCompletion[] {
    this.completed.set(entry.requestId, completion);
    const record = idempotencyRecordFor(entry, this.now());
    if (record !== undefined) this.idemByKey.set(record.idempotencyKey, record);
    return this.pruneCompletedCache();
  }

  /** Completion for a request the caller has a requestId for (replay after settle). */
  completedFor(requestId: string): Completion | undefined {
    return this.completed.get(requestId);
  }

  /** Requests past deadline that still need timeout settlement. */
  expired(now: number): PendingRequest[] {
    const out: PendingRequest[] = [];
    for (const p of this.pending.values()) {
      if (p.state === "settled") continue;
      if (p.deadlineMs <= now) out.push(p);
    }
    return out;
  }

  /** Earliest deadline among active requests; undefined when none. */
  nextDeadlineMs(): number | undefined {
    let next: number | undefined;
    for (const p of this.pending.values()) {
      if (p.state === "settled") continue;
      if (next === undefined || p.deadlineMs < next) next = p.deadlineMs;
    }
    return next;
  }

  /**
   * After a drop, classify every active request per persisted state and return
   * the mapping requestId -> RelayErrorResult (caller persists them).
   */
  classifyAllOnClose(now: number): Map<string, RelayErrorResult> {
    const out = new Map<string, RelayErrorResult>();
    for (const p of this.pending.values()) {
      if (p.state === "settled") continue;
      const c = classifyAmbiguousDelivery(p.state, p.opClass);
      out.set(p.requestId, {
        ok: false,
        code: c.code,
        retryable: c.retryable,
        message: c.retryable
          ? "connection lost before a confirmed result; safe to retry"
          : "connection lost; mutation outcome is unknown and may already have been applied",
        requestId: p.requestId,
        workstationId: p.workstationId,
        atMs: now,
      });
    }
    return out;
  }

  /** Bounded resume summaries for HelloAck (never includes arg values). */
  resumeSummaries(now: number): PendingRequest[] {
    return [...this.pending.values()]
      .filter((p) => p.state !== "settled")
      .sort((a, b) => a.createdAtMs - b.createdAtMs)
      .slice(0, this.limits.maxPendingRequests)
      .map((p) => ({ ...p, argsSummary: { argKeys: p.argsSummary.argKeys.slice(0, 32) } }));
  }

  /** Completed records older than TTL — candidates for pruning. */
  completedExpired(now: number): string[] {
    const out: string[] = [];
    for (const [requestId, c] of this.completed) {
      if (now - c.servedAtMs >= this.limits.completedRecordTtlMs) out.push(requestId);
    }
    return out;
  }

  /** All completed entries (for alarm pruning + storage reconciliation). */
  completedEntries(): Array<{ requestId: string; completion: Completion }> {
    return [...this.completed.entries()].map(([requestId, completion]) => ({ requestId, completion }));
  }

  /** Earliest completion TTL expiry; undefined when none. */
  completedExpiryAtMs(): number | undefined {
    let earliest: number | undefined;
    for (const c of this.completed.values()) {
      const expiry = c.servedAtMs + this.limits.completedRecordTtlMs;
      if (earliest === undefined || expiry < earliest) earliest = expiry;
    }
    return earliest;
  }

  dropCompleted(requestId: string, idempotencyKey?: string): void {
    this.completed.delete(requestId);
    if (idempotencyKey !== undefined) this.idemByKey.delete(idempotencyKey);
  }

  activeCount(): number {
    let n = 0;
    for (const p of this.pending.values()) if (p.state !== "settled") n++;
    return n;
  }

  /** Active durable work that has not already passed its deadline. Useful for
   * admission control where stale persisted rows must not starve fresh reads. */
  liveCount(now: number): number {
    let n = 0;
    for (const p of this.pending.values()) {
      if (p.state !== "settled" && p.deadlineMs > now) n++;
    }
    return n;
  }

  totalPendingSize(): number {
    return this.pending.size;
  }

  private pruneCompletedCache(): PrunedCompletion[] {
    if (this.completed.size <= this.limits.maxCompletedRecords) return [];
    const sorted = [...this.completed.entries()].sort((a, b) => a[1].servedAtMs - b[1].servedAtMs);
    const drop = sorted.slice(0, sorted.length - this.limits.maxCompletedRecords);
    const pruned: PrunedCompletion[] = [];
    for (const [requestId] of drop) {
      const idempotencyKey = this.idempotencyKeyFor(requestId);
      this.completed.delete(requestId);
      if (idempotencyKey !== undefined) this.idemByKey.delete(idempotencyKey);
      pruned.push({ requestId, ...(idempotencyKey !== undefined ? { idempotencyKey } : {}) });
    }
    return pruned;
  }
}