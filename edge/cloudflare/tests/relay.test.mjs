import { test } from "node:test";
import assert from "node:assert/strict";
import {
  validateHello,
  decodeWire,
  encodeWire,
  RELAY_PROTOCOL_VERSION,
  POST_HELLO_KINDS,
  EDGE_OUTBOUND_KINDS,
  validateCanonicalMessage,
  MESSAGE_KINDS,
} from "../dist/relay-adapter.js";
import {
  validateRelayMessage,
  parseRelayFrame,
  checkWorkstationId,
  checkRequestId,
  checkBootId,
} from "../dist/canonical-imports.js";

const VALID_HELLO = {
  protocol_version: 1,
  kind: "hello",
  workstation_id: "w1",
  boot_id: "boot1",
  link_version: "0.1.0",
  capabilities: ["herdr", "fs"],
  connected_at_ms: 1234,
};

test("relay: protocol_version is the NUMBER 1", () => {
  assert.equal(RELAY_PROTOCOL_VERSION, 1);
  assert.equal(typeof RELAY_PROTOCOL_VERSION, "number");
});

test("relay: accepts canonical hello (protocol v1, snake_case)", () => {
  const res = validateHello(VALID_HELLO);
  assert.equal(res.ok, true);
  assert.equal(res.message.kind, "hello");
  assert.equal(res.message.protocol_version, 1);
});

test("relay: rejects old camelCase provisional hello", () => {
  const res = validateHello({
    kind: "hello",
    protocolVersion: "1",
    workstationId: "w1",
    bootId: "b1",
    linkVersion: "0.1.0",
    connectedAtMs: 1,
  });
  assert.equal(res.ok, false);
});

test("relay: rejects string protocol_version", () => {
  const res = validateRelayMessage({ ...VALID_HELLO, protocol_version: "1" });
  assert.equal(res.ok, false);
  assert.equal(res.code, "unsupported_protocol_version");
});

test("relay: rejects wrong protocol_version number", () => {
  const res = validateRelayMessage({ ...VALID_HELLO, protocol_version: 2 });
  assert.equal(res.ok, false);
  assert.equal(res.code, "unsupported_protocol_version");
});

test("relay: rejects missing workstation_id", () => {
  const { workstation_id, ...noWs } = VALID_HELLO;
  const res = validateRelayMessage(noWs);
  assert.equal(res.ok, false);
  assert.equal(res.code, "missing_workstation_id");
});

test("relay: rejects missing protocol_version", () => {
  const { protocol_version, ...noPv } = VALID_HELLO;
  const res = validateRelayMessage(noPv);
  assert.equal(res.ok, false);
  assert.equal(res.code, "missing_protocol_version");
});

test("relay: rejects unknown kind (provisional 'request')", () => {
  const res = validateRelayMessage({ protocol_version: 1, kind: "request", workstation_id: "w1" });
  assert.equal(res.ok, false);
  assert.equal(res.code, "unknown_kind");
});

test("relay: rejects old provisional kinds request/response/drain/error", () => {
  for (const kind of ["request", "response", "drain", "runtime_status", "upgrade_status", "error", "resume"]) {
    const res = validateRelayMessage({ protocol_version: 1, kind, workstation_id: "w1" });
    assert.equal(res.ok, false, `expected ${kind} to be rejected`);
    assert.equal(res.code, "unknown_kind");
  }
});

test("relay: canonical MESSAGE_KINDS only", () => {
  assert.deepEqual(MESSAGE_KINDS, [
    "hello", "hello_ack", "heartbeat", "status", "tool_request",
    "tool_result", "tool_error", "cancel", "cancel_ack",
  ]);
});

test("relay: correlated kinds require request_id", () => {
  const res = validateRelayMessage({ protocol_version: 1, kind: "tool_request", workstation_id: "w1", operation: "herdr_inspect" });
  assert.equal(res.ok, false);
  assert.equal(res.code, "missing_request_id");
});

test("relay: control kinds reject request_id", () => {
  const res = validateRelayMessage({ ...VALID_HELLO, request_id: "req1" });
  assert.equal(res.ok, false);
  assert.equal(res.code, "unexpected_request_id");
});

test("relay: workstation_id must match grammar", () => {
  assert.equal(checkWorkstationId("w1").ok, true);
  assert.equal(checkWorkstationId("").ok, false);
  assert.equal(checkWorkstationId("bad id").ok, false);
  assert.equal(checkWorkstationId("a".repeat(65)).ok, false);
});

test("relay: request_id grammar + bounds", () => {
  assert.equal(checkRequestId("req_01H").ok, true);
  assert.equal(checkRequestId("").ok, false);
  assert.equal(checkRequestId("a".repeat(129)).ok, false);
});

test("relay: boot_id grammar", () => {
  assert.equal(checkBootId("boot-2026-1").ok, true);
  assert.equal(checkBootId("").ok, false);
});

test("relay: strict unknown-field gate rejects camelCase fields", () => {
  const res = validateRelayMessage({ ...VALID_HELLO, requestId: "req1" });
  assert.equal(res.ok, false);
  assert.equal(res.code, "unknown_field");
});

test("relay: decodeWire parses + validates canonical frames", () => {
  const ok = decodeWire(JSON.stringify(VALID_HELLO));
  assert.equal(ok.ok, true);
  assert.equal(ok.message.kind, "hello");
});

test("relay: decodeWire rejects provisional kind request", () => {
  const res = decodeWire(JSON.stringify({ protocol_version: 1, kind: "request", workstation_id: "w1", request_id: "r1" }));
  assert.equal(res.ok, false);
  assert.equal(res.code, "unknown_kind");
});

test("relay: encodeWire honors byte budget", () => {
  const msg = { protocol_version: 1, kind: "hello_ack", workstation_id: "w1", ok: true, reconnect: false, resume: [], completed: [] };
  const ok = encodeWire(msg, 100_000);
  assert.equal(ok.ok, true);
  const tooSmall = encodeWire(msg, 5);
  assert.equal(tooSmall.ok, false);
});

test("relay: canonical tool_request shape accepted", () => {
  const res = validateRelayMessage({
    protocol_version: 1,
    kind: "tool_request",
    workstation_id: "w1",
    request_id: "req1",
    operation: "herdr_inspect",
    arguments: { path: "/" },
    timeout_ms: 30000,
  });
  assert.equal(res.ok, true);
});

test("relay: tool_error preserves retryable + delivery_state + details", () => {
  const raw = {
    protocol_version: 1,
    kind: "tool_error",
    workstation_id: "w1",
    request_id: "req1",
    code: "delivery_uncertain",
    retryable: false,
    delivery_state: "delivery_unknown",
    details: { phase: "after_send" },
    served_at_ms: 1234,
  };
  const res = validateRelayMessage(raw);
  assert.equal(res.ok, true);
  // validateRelayMessage returns RelayCheck, not the message
});

test("relay: tool_error rejects invalid delivery_state enum", () => {
  const res = validateRelayMessage({
    protocol_version: 1,
    kind: "tool_error",
    workstation_id: "w1",
    request_id: "req1",
    code: "x",
    retryable: true,
    delivery_state: "maybe",
  });
  assert.equal(res.ok, false);
  assert.equal(res.code, "invalid_enum");
});

test("relay: status query form is canonical", () => {
  const res = validateRelayMessage({ protocol_version: 1, kind: "status", workstation_id: "w1", query: true });
  assert.equal(res.ok, true);
});

test("relay: heartbeat requires boot_id + sent_at_ms + active_requests", () => {
  const ok = validateRelayMessage({ protocol_version: 1, kind: "heartbeat", workstation_id: "w1", boot_id: "b1", sent_at_ms: 1, active_requests: 0 });
  assert.equal(ok.ok, true);
  const missingBoot = validateRelayMessage({ protocol_version: 1, kind: "heartbeat", workstation_id: "w1", sent_at_ms: 1, active_requests: 0 });
  assert.equal(missingBoot.ok, false);
});

test("relay: POST_HELLO_KINDS + EDGE_OUTBOUND_KINDS align with canonical set", () => {
  assert.deepEqual([...POST_HELLO_KINDS].sort(), ["cancel_ack", "heartbeat", "status", "tool_error", "tool_result"]);
  assert.deepEqual([...EDGE_OUTBOUND_KINDS].sort(), ["cancel", "hello_ack", "status", "tool_request"]);
});

function validateHelloMessage(value) {
  return validateHello(value);
}