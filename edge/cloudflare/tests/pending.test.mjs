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
  assert.equal(r.get("r1").state, "settled");
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
  assert.equal(classifyOp("herdr_exec"), "mutating");
  assert.equal(classifyOp("herdr_prompt"), "mutating");
  assert.equal(classifyOp("some_mystery_tool"), "mutating");
  // herdr_skill is intentionally absent (not part of epoch-1 baseline).
  assert.equal(classifyOp("herdr_skill"), "mutating");
});