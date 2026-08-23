/**
 * pending.ts — bounded pending-request registry + request-id generation.
 *
 * The DO persists pending entries to its own storage; the registry held in
 * memory is a cache (plan §11: state that must survive hibernation lives in
 * DO storage). This module implements the bounded maps, idempotency-key
 * dedup window and (de)serialization so all capacity/expiry logic is pure and
 * unit-testable.
 */

import type { RelayErrorResult } from "./errors.js";
import { classifyAmbiguousDelivery } from "./errors.js";
import type { EdgeLimits, OpClass } from "./limits.js";

export type PendingState = "queued" | "sent" | "settled";

export interface PendingRequest {
  requestId: string;
  workstationId: string;
  op: string;
  opClass: OpClass;
  /** Diagnostic summary only — NO argument values, ever. */
  argsSummary: { argKeys: string[] };
  state: PendingState;
  createdAtMs: number;
  sentAtMs?: number;
  deadlineMs: number;
  idempotencyKey?: string;
  contractEpoch?: number;
}

export type Completion =
  | { status: "ok"; result: unknown; servedAtMs: number; runtimeGeneration?: string }
  | { status: "error"; error: RelayErrorResult; servedAtMs: number };

export interface IdempotencyRecord {
  idempotencyKey: string;
  requestId: string;
  op: string;
  settledAtMs: number;
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
  | { status: "idem_hit"; completion: Completion }
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

  add(req: Omit<PendingRequest, "state" | "createdAtMs">): AddOutcome {
    const now = this.now();
    // Idempotency-key replay is a pure dedup decision; mutating ops should
    // include one (plan §6).
    if (req.idempotencyKey !== undefined) {
      const rec = this.idemByKey.get(req.idempotencyKey);
      if (rec && rec.op === req.op) {
        const completion = this.completed.get(rec.requestId);
        if (completion) return { status: "idem_hit", completion };
      }
    }
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

  markSent(requestId: string, atMs: number): PendingRequest | undefined {
    const p = this.pending.get(requestId);
    if (!p) return undefined;
    p.state = "sent";
    p.sentAtMs = atMs;
    return p;
  }

  settle(requestId: string, completion: Completion): PendingRequest | undefined {
    const p = this.pending.get(requestId);
    if (!p) return undefined;
    p.state = "settled";
    // `pending` is the capacity-bound registry of active requests, not a
    // historical ledger. Completed responses live in `completed` (and the
    // idempotency index) and are persisted separately by WorkstationDO.
    // Keeping settled entries here leaks one capacity slot per tools/call and
    // eventually makes every new request fail with edge_capacity_exceeded.
    this.pending.delete(requestId);
    this.recordSettlement(p, completion);
    return p;
  }

  /**
   * Record a completion + idempotency entry for an explicit PendingRequest
   * WITHOUT re-occupying pending capacity. Used by WorkstationDO to close out
   * a request that was evicted from the capacity-bound pending map (its entry
   * is already gone from `pending`), so durable storage/completion/idempotency
   * still settle correctly and the evicted request never resurrects on
   * rehydrate. Normal `settle()` reuses this after deleting the active slot.
   */
  recordSettlement(entry: PendingRequest, completion: Completion): void {
    this.completed.set(entry.requestId, completion);
    if (entry.idempotencyKey !== undefined) {
      this.idemByKey.set(entry.idempotencyKey, {
        idempotencyKey: entry.idempotencyKey,
        requestId: entry.requestId,
        op: entry.op,
        settledAtMs: this.now(),
      });
    }
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
          : "connection lost; mutation outcome unknown — do not blindly retry",
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

  totalPendingSize(): number {
    return this.pending.size;
  }

  private pruneCompletedCache(): void {
    if (this.completed.size <= this.limits.maxCompletedRecords) return;
    const sorted = [...this.completed.entries()].sort((a, b) => a[1].servedAtMs - b[1].servedAtMs);
    const drop = sorted.slice(0, sorted.length - this.limits.maxCompletedRecords);
    for (const [requestId] of drop) this.completed.delete(requestId);
  }
}