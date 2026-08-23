// Relay Protocol v1 — delivery-state and retryability taxonomy.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyDelivery,
  classifyRetryable,
  classifyReason,
  offlineError,
  reconnectingError,
  drainingError,
  timeoutError,
  uncertainError,
  capacityError,
  deliveredError,
  rejectedError,
  toToolError,
} from "../dist/relay/index.js";

test("classifyRetryable: not_delivered is always retryable", () => {
  for (const safety of ["read", "idempotent", "unsafe"]) {
    assert.equal(classifyRetryable("not_delivered", safety), true);
  }
});

test("classifyRetryable: delivery_unknown retryable only for read/idempotent", () => {
  assert.equal(classifyRetryable("delivery_unknown", "read"), true);
  assert.equal(classifyRetryable("delivery_unknown", "idempotent"), true);
  assert.equal(classifyRetryable("delivery_unknown", "unsafe"), false);
});

test("classifyRetryable: delivered is not retryable from delivery alone", () => {
  for (const safety of ["read", "idempotent", "unsafe"]) {
    assert.equal(classifyRetryable("delivered", safety), false);
  }
});

test("classifyDelivery returns reason text per state", () => {
  const r = classifyDelivery("delivery_unknown", "unsafe");
  assert.deepEqual(r, {
    delivery: "delivery_unknown",
    retryable: false,
    reason: "delivery unknown and operation is mutating — do not blind-retry",
  });
  assert.match(classifyReason("not_delivered", "unsafe"), /never reached/);
  assert.match(classifyReason("delivered", "read"), /concrete error code/);
});

test("error constructors map to stable codes + delivery states", () => {
  assert.equal(offlineError().code, "workstation_offline");
  assert.equal(offlineError().retryable, true);
  assert.equal(offlineError().delivery_state, "not_delivered");

  assert.equal(reconnectingError().code, "workstation_reconnecting");
  assert.equal(reconnectingError().retryable, true);

  assert.equal(drainingError().code, "workstation_draining");
  assert.equal(drainingError().retryable, true);

  assert.equal(capacityError().code, "edge_capacity_exceeded");
  assert.equal(capacityError().retryable, true);

  assert.equal(rejectedError().retryable, false);
  assert.equal(rejectedError().delivery_state, "not_delivered");
});

test("timeoutError: retryability follows operation safety", () => {
  assert.equal(timeoutError({ safety: "read" }).retryable, true);
  assert.equal(timeoutError({ safety: "idempotent" }).retryable, true);
  assert.equal(timeoutError({ safety: "read" }).delivery_state, "delivery_unknown");
  assert.equal(timeoutError({ safety: "unsafe" }).retryable, false);
  assert.match(timeoutError({ safety: "unsafe" }).message, /do not blindly retry/);
  assert.equal(timeoutError().retryable, false); // conservative default
});

test("uncertainError: idempotent ops retryable, unsafe not", () => {
  assert.equal(uncertainError({ safety: "idempotent" }).retryable, true);
  assert.equal(uncertainError({ safety: "unsafe" }).retryable, false);
  assert.equal(uncertainError().delivery_state, "delivery_unknown");
});

test("deliveredError: runtime-reported errors are delivered, not retryable by default", () => {
  const e = deliveredError({ code: "runtime_error", message: "boom" });
  assert.equal(e.delivery_state, "delivered");
  assert.equal(e.retryable, false);
  assert.equal(e.code, "runtime_error");
});

test("toToolError builds a valid wire tool_error payload", () => {
  const err = uncertainError({ safety: "unsafe", requestId: "req-x", workstationId: "w1" });
  const msg = toToolError(err, { workstation_id: "w1", request_id: "req-x", served_at_ms: 1234 });
  assert.equal(msg.kind, "tool_error");
  assert.equal(msg.request_id, "req-x");
  assert.equal(msg.workstation_id, "w1");
  assert.equal(msg.code, "delivery_uncertain");
  assert.equal(msg.retryable, false);
  assert.equal(msg.delivery_state, "delivery_unknown");
  assert.equal(msg.served_at_ms, 1234);
});

test("delivery classification is conservative end-to-end", () => {
  // A mutating op that timed out after being sent must NOT be blindly retried.
  const verdict = classifyDelivery("delivery_unknown", "unsafe");
  assert.equal(verdict.retryable, false);

  // A read that never left the edge is safe to retry.
  assert.equal(classifyDelivery("not_delivered", "read").retryable, true);

  // A mutating op with an idempotency key may be retried after ambiguity.
  assert.equal(classifyDelivery("delivery_unknown", "idempotent").retryable, true);
});