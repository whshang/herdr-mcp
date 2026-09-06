import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyAmbiguousDelivery,
  mapLinkErrorCode,
  offlineResult,
  reconnectingResult,
  drainingResult,
  timeoutResult,
  uncertainResult,
  capacityResult,
  relayErrorRequiresHuman,
} from "../dist/errors.js";

test("errors: queued request on drop -> reconnecting, retryable", () => {
  const r = classifyAmbiguousDelivery("queued", "mutating");
  assert.equal(r.code, "workstation_reconnecting");
  assert.equal(r.retryable, true);
});

test("errors: sent read on drop -> uncertain but retryable", () => {
  const r = classifyAmbiguousDelivery("sent", "read");
  assert.equal(r.code, "delivery_uncertain");
  assert.equal(r.retryable, true);
});

test("errors: sent mutating on drop -> uncertain, NOT retryable", () => {
  const r = classifyAmbiguousDelivery("sent", "mutating");
  assert.equal(r.code, "delivery_uncertain");
  assert.equal(r.retryable, false);
});

test("errors: timeout read retryable, mutating not", () => {
  const read = timeoutResult({ opClass: "read" });
  const mutating = timeoutResult({ opClass: "mutating" });
  const unknown = timeoutResult({ opClass: "unknown" });
  assert.equal(read.retryable, true);
  assert.equal(mutating.retryable, false);
  assert.equal(unknown.retryable, false);
  assert.match(read.message, /retrying a read is safe/);
  assert.match(mutating.message, /mutating operation/);
  assert.match(unknown.message, /verify live state before replay/);
  assert.equal(read.requires_human, false);
  assert.equal(mutating.requires_human, false);
  assert.equal(unknown.requires_human, false);
});

test("errors: transient recovery classes do not request human intervention", () => {
  for (const code of [
    "workstation_offline",
    "workstation_reconnecting",
    "workstation_draining",
    "runtime_generation_superseded_before_dispatch",
    "runtime_unavailable",
    "request_timeout",
    "delivery_uncertain",
    "edge_capacity_exceeded",
  ]) {
    assert.equal(relayErrorRequiresHuman(code), false, code);
  }
  assert.equal(relayErrorRequiresHuman("link_auth_failed"), undefined);
});

test("errors: mapLinkErrorCode keeps known, degrades unknown to internal", () => {
  assert.equal(mapLinkErrorCode("workstation_offline"), "workstation_offline");
  assert.equal(mapLinkErrorCode("request_timeout"), "request_timeout");
  assert.equal(
    mapLinkErrorCode("runtime_generation_superseded_before_dispatch"),
    "runtime_generation_superseded_before_dispatch",
  );
  assert.equal(mapLinkErrorCode("local_mcp_unreachable"), "runtime_unavailable");
  assert.equal(mapLinkErrorCode("local_mcp_http_error"), "runtime_unavailable");
  assert.equal(mapLinkErrorCode("local_mcp_timeout"), "request_timeout");
  assert.equal(mapLinkErrorCode("local_mcp_response_too_large"), "payload_too_large");
  assert.equal(mapLinkErrorCode("some_runtime_weirdness"), "internal_error");
});

test("errors: structured results carry requestId", () => {
  const offline = offlineResult({ requestId: "r1" });
  assert.equal(offline.requestId, "r1");
  assert.equal(offline.retryable, true);
  assert.equal(offline.requires_human, false);
  assert.equal(offline.delivery_state, "not_delivered");
  assert.equal(offline.retry_after_ms, 5_000);
  assert.deepEqual(offline.recovery, {
    action: "retry_read_only_probe",
    probe_tool: "herdr_inspect",
    max_attempts: 3,
    backoff_ms: [5_000, 10_000, 20_000],
    mutation_replay: "only_after_not_delivered_or_verified_not_applied",
  });
  const reconnecting = reconnectingResult({ requestId: "r1" });
  assert.equal(reconnecting.retryable, true);
  assert.equal(reconnecting.requires_human, false);
  assert.equal(reconnecting.delivery_state, "not_delivered");
  assert.equal(drainingResult({ requestId: "r1" }).requires_human, false);
  const uncertain = uncertainResult({ requestId: "r1" });
  assert.equal(uncertain.retryable, false);
  assert.equal(uncertain.requires_human, false);
  const capacity = capacityResult({ requestId: "r1" });
  assert.equal(capacity.retryable, true);
  assert.equal(capacity.requires_human, false);
  assert.equal(capacity.delivery_state, "not_delivered");
  assert.equal(capacity.retry_after_ms, 1_000);
});