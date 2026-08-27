import { test } from "node:test";
import assert from "node:assert/strict";
import { makeLimits, classifyOp } from "../dist/limits.js";
import {
  decodeStoredPendingRequest,
  PendingRequestRegistry,
  newRequestId,
} from "../dist/pending.js";

function pendingReq(over = {}) {
  return {
    requestId: newRequestId(),
    workstationId: "w1",
    op: "herdr_inspect",
    opClass: "read",
    argsSummary: { argKeys: ["path"] },
    deadlineMs: Date.now() + 10_000,
    ...over,
  };
}

test("pending storage decoder accepts structured-clone objects and legacy JSON text", () => {
  const value = {
    requestId: "r-storage",
    workstationId: "ws-1",
    op: "herdr_inspect",
    opClass: "read",
    argsSummary: { argKeys: [] },
    state: "sent",
    createdAtMs: 100,
    sentAtMs: 101,
    deadlineMs: 200,
  };
  assert.deepEqual(decodeStoredPendingRequest(value), value);
  assert.deepEqual(decodeStoredPendingRequest(JSON.stringify(value)), value);
  assert.equal(decodeStoredPendingRequest("{broken"), null);
  assert.equal(decodeStoredPendingRequest({ requestId: "incomplete" }), null);
});

test("newRequestId: hex, 32 chars, unique", () => {
  const a = newRequestId();
  const b = newRequestId();
  assert.equal(a.length, 32);
  assert.match(a, /^[0-9a-f]{32}$/);
  assert.notEqual(a, b);
});

test("registry: add -> markSent -> settle -> completedFor", () => {
  const r = new PendingRequestRegistry({ limits: makeLimits() });
  const { status, entry } = r.add(pendingReq({ requestId: "r1" }));
  assert.equal(status, "added");
  assert.equal(entry.state, "queued");
  r.markSent("r1", 100);
  assert.equal(r.get("r1").state, "sent");
  const completion = { status: "ok", result: { x: 1 }, servedAtMs: 200 };
  r.settle("r1", completion);
  assert.equal(r.get("r1"), undefined);
  assert.equal(r.totalPendingSize(), 0);
  assert.deepEqual(r.completedFor("r1"), completion);
});

test("registry: idempotency key dedups mutating op", () => {
  const r = new PendingRequestRegistry({ limits: makeLimits() });
  r.add(pendingReq({ requestId: "r1", op: "herdr_prompt", opClass: "mutating", idempotencyKey: "k1" }));
  r.settle("r1", { status: "ok", result: "done", servedAtMs: 300 });
  const again = r.add(pendingReq({ requestId: "r2", op: "herdr_prompt", opClass: "mutating", idempotencyKey: "k1" }));
  assert.equal(again.status, "idem_hit");
  assert.equal(again.completion.status, "ok");
});

test("registry: capacity bounds, evicts oldest queued, keeps sent", () => {
  const limits = makeLimits({});
  limits.maxPendingRequests = 2;
  const r = new PendingRequestRegistry({ limits });
  r.add(pendingReq({ requestId: "a1", op: "herdr_inspect" }));
  r.markSent("a1", 1); // in-flight, must not be evicted
  r.add(pendingReq({ requestId: "b2", op: "herdr_since" })); // oldest queued
  const third = r.add(pendingReq({ requestId: "c3", op: "herdr_git" }));
  assert.equal(third.status, "evicted_oldest");
  assert.equal(third.evicted.requestId, "b2");
  // All remaining entries are sent -> nothing queued to evict -> capacity_full.
  r.markSent("c3", 2);
  const full = r.add(pendingReq({ requestId: "d4" }));
  assert.equal(full.status, "capacity_full");
});

test("registry: settled requests immediately release pending capacity", () => {
  const limits = makeLimits({});
  limits.maxPendingRequests = 1;
  const r = new PendingRequestRegistry({ limits });

  for (let i = 0; i < 300; i += 1) {
    const requestId = `r${i}`;
    const added = r.add(pendingReq({ requestId }));
    assert.equal(added.status, "added", `iteration ${i} must not leak a capacity slot`);
    r.markSent(requestId, i + 1);
    r.settle(requestId, { status: "ok", result: i, servedAtMs: i + 2 });
    assert.equal(r.totalPendingSize(), 0, `iteration ${i} must release pending capacity`);
    assert.equal(r.activeCount(), 0);
  }
});

test("registry: queued eviction records completion/idem without re-occupying capacity and survives rehydrate", () => {
  const limits = makeLimits({});
  limits.maxPendingRequests = 1;
  const r = new PendingRequestRegistry({ limits });

  // First request occupies the single active slot and stays QUEUED (unsent) so
  // it is the evictable oldest-queued candidate. Do NOT markSent it — add() only
  // evicts a queued request, never a sent one.
  const first = pendingReq({ requestId: "evicted-1", idempotencyKey: "idem-1" });
  const a1 = r.add(first);
  assert.equal(a1.status, "added");

  // Second request evicts the oldest queued (the first, still queued).
  const second = pendingReq({ requestId: "new-1", idempotencyKey: "idem-2" });
  const a2 = r.add(second);
  assert.equal(a2.status, "evicted_oldest");
  assert.equal(a2.evicted.requestId, "evicted-1");
  // The evicted entry is no longer in pending; the new one holds the only slot.
  assert.equal(r.get("evicted-1"), undefined);
  assert.equal(r.totalPendingSize(), 1);
  assert.equal(r.get("new-1").requestId, "new-1");

  // Close out the evicted request via recordSettlement (explicit entry, no
  // capacity re-occupation) — mirrors WorkstationDO.persistEvictedSettlement.
  const completion = { status: "error", error: { code: "reconnecting" }, servedAtMs: 5 };
  r.recordSettlement(a2.evicted, completion);
  assert.equal(r.totalPendingSize(), 1, "recordSettlement must not re-occupy pending capacity");
  assert.deepEqual(r.completedFor("evicted-1"), completion);

  // Rehydrate from a snapshot: the evicted request must NOT resurrect as pending,
  // but its completion must be replayable. The idempotency index is NOT part of
  // RegistrySnapshot — the DO persists IdempotencyRecords separately and restores
  // them via restoreIdem, exactly like WorkstationDO.persistEvictedSettlement.
  const snap = r.snapshot();
  assert.equal(snap.pending.some((p) => p.requestId === "evicted-1"), false);
  assert.equal(snap.pending.some((p) => p.requestId === "new-1"), true);
  const r2 = new PendingRequestRegistry({ limits });
  r2.restore(snap);
  r2.restoreIdem([
    { idempotencyKey: "idem-1", requestId: "evicted-1", op: "herdr_inspect", settledAtMs: 5 },
  ]);
  assert.equal(r2.get("evicted-1"), undefined);
  assert.equal(r2.get("new-1").requestId, "new-1");
  assert.deepEqual(r2.completedFor("evicted-1"), completion);
  assert.equal(r2.idempotencyKeyFor("evicted-1"), "idem-1");

  // A replay of the evicted request's idempotency key returns the recorded completion.
  const replay = r2.add(pendingReq({ requestId: "replay-1", op: "herdr_inspect", idempotencyKey: "idem-1" }));
  assert.equal(replay.status, "idem_hit");
  assert.deepEqual(replay.completion, completion);
});

test("registry: expired() returns overdue active requests only", () => {
  const r = new PendingRequestRegistry({ limits: makeLimits() });
  r.add(pendingReq({ requestId: "old", deadlineMs: 100 }));
  r.add(pendingReq({ requestId: "fresh", deadlineMs: Date.now() + 60_000 }));
  const expired = r.expired(Date.now());
  assert.deepEqual(expired.map((p) => p.requestId), ["old"]);
});

test("registry: classifyAllOnClose maps state+opClass", () => {
  const r = new PendingRequestRegistry({ limits: makeLimits() });
  r.add(pendingReq({ requestId: "q1", op: "herdr_inspect" }));
  r.add(pendingReq({ requestId: "s1", op: "herdr_inspect" }));
  r.add(pendingReq({ requestId: "s2", op: "herdr_prompt", opClass: "mutating" }));
  r.markSent("s1", 1);
  r.markSent("s2", 2);
  const map = r.classifyAllOnClose(Date.now());
  assert.equal(map.get("q1").code, "workstation_reconnecting");
  assert.equal(map.get("s1").retryable, true);
  assert.equal(map.get("s2").code, "delivery_uncertain");
  assert.equal(map.get("s2").retryable, false);
});

test("registry: resumeSummaries never includes args values", () => {
  const r = new PendingRequestRegistry({ limits: makeLimits() });
  r.add(pendingReq({ argsSummary: { argKeys: ["cmd"] } }));
  const summaries = r.resumeSummaries(Date.now());
  assert.equal(summaries[0].argsSummary.argKeys.length, 1);
  assert.deepEqual(summaries[0].argsSummary.argKeys, ["cmd"]);
  assert.equal(Object.hasOwn(summaries[0], "args"), false);
});

test("registry: snapshot/restore + restoreIdem round trip", () => {
  const a = new PendingRequestRegistry({ limits: makeLimits() });
  a.add(pendingReq({ requestId: "a1", idempotencyKey: "kk" }));
  a.markSent("a1", 5);
  a.settle("a1", { status: "ok", result: 1, servedAtMs: 6 });
  const snap = a.snapshot();
  const b = new PendingRequestRegistry({ limits: makeLimits() });
  b.restore(snap);
  b.restoreIdem([{ idempotencyKey: "kk", requestId: "a1", op: "herdr_inspect", settledAtMs: 6 }]);
  assert.equal(b.completedFor("a1").status, "ok");
  const again = b.add(pendingReq({ requestId: "b2", op: "herdr_inspect", idempotencyKey: "kk" }));
  assert.equal(again.status, "idem_hit");
});

test("registry: completed TTL expiry + drop", () => {
  const limits = makeLimits();
  limits.completedRecordTtlMs = 100;
  const r = new PendingRequestRegistry({ limits });
  r.add(pendingReq({ requestId: "x1" }));
  r.settle("x1", { status: "ok", result: 1, servedAtMs: 1000 });
  assert.deepEqual(r.completedExpired(1100), ["x1"]);
  assert.equal(r.completedExpiryAtMs(), 1100);
  r.dropCompleted("x1");
  assert.equal(r.completedFor("x1"), undefined);
});

test("registry: liveCount excludes expired durable backlog", () => {
  const limits = makeLimits();
  let now = 1000;
  const r = new PendingRequestRegistry({ limits, now: () => now });
  r.add(pendingReq({ requestId: "expired", deadlineMs: 900 }));
  r.add(pendingReq({ requestId: "live", deadlineMs: 1100 }));
  assert.equal(r.activeCount(), 2);
  assert.equal(r.liveCount(now), 1);
  now = 1200;
  assert.equal(r.liveCount(now), 0);
});

test("limits: defaults + request timeout clamp", () => {
  const l = makeLimits();
  assert.equal(l.maxPendingRequests, 256);
  assert.equal(l.maxFrameBytes, 1_048_576);
  assert.equal(l.requestTimeoutMs, 30_000);
  const clamped = makeLimits({ DEFAULT_REQUEST_TIMEOUT_MS: "999999" });
  assert.equal(clamped.requestTimeoutMs, 60_000);
  const min = makeLimits({ DEFAULT_REQUEST_TIMEOUT_MS: "1" });
  assert.equal(min.requestTimeoutMs, 1_000);
});

test("limits: classifyOp only marks known-read ops retryable", () => {
  assert.equal(classifyOp("herdr_inspect"), "read");
  assert.equal(classifyOp("herdr_fs_list"), "read");
  assert.equal(classifyOp("herdr_fs_image"), "read");
  assert.equal(classifyOp("herdr_git"), "read");
  assert.equal(classifyOp("herdr_exec_read"), "read");
  assert.equal(classifyOp("herdr_exec"), "mutating");
  assert.equal(classifyOp("herdr_prompt"), "mutating");
  assert.equal(classifyOp("some_mystery_tool"), "mutating");
  assert.equal(classifyOp("herdr_skill"), "read");
});