import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyAmbiguousDelivery,
  mapLinkErrorCode,
  offlineResult,
  reconnectingResult,
  timeoutResult,
  uncertainResult,
  capacityResult,
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
  assert.equal(timeoutResult({ opClass: "read" }).retryable, true);
  assert.equal(timeoutResult({ opClass: "mutating" }).retryable, false);
  assert.equal(timeoutResult({ opClass: "unknown" }).retryable, false);
});

test("errors: mapLinkErrorCode keeps known, degrades unknown to internal", () => {
  assert.equal(mapLinkErrorCode("workstation_offline"), "workstation_offline");
  assert.equal(mapLinkErrorCode("request_timeout"), "request_timeout");
  assert.equal(mapLinkErrorCode("local_mcp_unreachable"), "runtime_unavailable");
  assert.equal(mapLinkErrorCode("local_mcp_http_error"), "runtime_unavailable");
  assert.equal(mapLinkErrorCode("local_mcp_timeout"), "request_timeout");
  assert.equal(mapLinkErrorCode("local_mcp_response_too_large"), "payload_too_large");
  assert.equal(mapLinkErrorCode("some_runtime_weirdness"), "internal_error");
});

test("errors: structured results carry requestId", () => {
  assert.equal(offlineResult({ requestId: "r1" }).requestId, "r1");
  assert.equal(reconnectingResult({ requestId: "r1" }).retryable, true);
  assert.equal(uncertainResult({ requestId: "r1" }).retryable, false);
  assert.equal(capacityResult({ requestId: "r1" }).retryable, true);
});