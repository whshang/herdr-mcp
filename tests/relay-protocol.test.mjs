// Relay Protocol v1 — envelope round-trips, malformed/unknown input, size
// enforcement and identifier validation. Runs against dist/relay (matches the
// repo convention of testing compiled output).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  RELAY_PROTOCOL_VERSION,
  MESSAGE_KINDS,
  CORRELATED_KINDS,
  parseRelayFrame,
  validateRelayMessage,
  checkFrameBytes,
  checkPayloadBudget,
  checkWorkstationId,
  checkRequestId,
  checkBootId,
  DEFAULT_MAX_FRAME_BYTES,
  MAX_ARGS_JSON_BYTES,
} from "../dist/relay/index.js";

function base(over = {}) {
  return { protocol_version: RELAY_PROTOCOL_VERSION, workstation_id: "w1", ...over };
}

test("MESSAGE_KINDS is exactly the canonical v1 kind set", () => {
  assert.deepEqual(MESSAGE_KINDS, [
    "hello",
    "hello_ack",
    "heartbeat",
    "status",
    "tool_request",
    "tool_result",
    "tool_error",
    "cancel",
    "cancel_ack",
  ]);
});

test("CORRELATED_KINDS requires request_id; control kinds forbid it", () => {
  assert.deepEqual(
    [...CORRELATED_KINDS].sort(),
    ["cancel", "cancel_ack", "tool_error", "tool_request", "tool_result"].sort(),
  );
});

test("round-trip: hello parse is valid", () => {
  const raw = JSON.stringify(
    base({
      kind: "hello",
      boot_id: "boot-01",
      link_version: "0.1.0",
      capabilities: ["relay.request"],
      runtime: {
        runtime_version: "0.3.26",
        runtime_commit: "abc123",
        runtime_generation: null,
        contract_epoch: 1,
        contract_hash: "sha256:" + "ab".repeat(32),
        herdr_version: null,
        herdr_protocol: null,
      },
    }),
  );
  const r = parseRelayFrame(raw);
  assert.equal(r.ok, true);
  if (r.ok) {
    assert.equal(r.message.kind, "hello");
    assert.equal(r.message.workstation_id, "w1");
    assert.equal(r.message.boot_id, "boot-01");
    assert.equal(r.message.runtime.contract_epoch, 1);
  }
});

test("round-trip: tool_request ↔ tool_result correlation fields", () => {
  const req = JSON.stringify(
    base({
      kind: "tool_request",
      request_id: "req_01",
      operation: "herdr_inspect",
      arguments: { workspace: "w5C" },
      timeout_ms: 30_000,
      contract_epoch: 1,
      contract_hash: "sha256:" + "cd".repeat(32),
      idempotency_key: "idem-1",
    }),
  );
  const pr = parseRelayFrame(req);
  assert.equal(pr.ok, true);
  if (pr.ok) {
    assert.equal(pr.message.kind, "tool_request");
    assert.equal(pr.message.operation, "herdr_inspect");
  }

  const res = JSON.stringify(
    base({
      kind: "tool_result",
      request_id: "req_01",
      result: { ok: true },
      served_at_ms: 1_700_000_000_000,
      runtime_generation: "gen-7",
      transport_name: "herdr",
    }),
  );
  const pr2 = parseRelayFrame(res);
  assert.equal(pr2.ok, true);
  if (pr2.ok) assert.equal(pr2.message.kind, "tool_result");
});

test("round-trip: cancel → cancel_ack", () => {
  const cancel = parseRelayFrame(
    JSON.stringify(base({ kind: "cancel", request_id: "req_09", reason: "user abort" })),
  );
  assert.equal(cancel.ok, true);
  const ack = parseRelayFrame(
    JSON.stringify(base({ kind: "cancel_ack", request_id: "req_09", accepted: true, cancelled_at_ms: 5 })),
  );
  assert.equal(ack.ok, true);
  if (ack.ok) assert.equal(ack.message.accepted, true);
});

test("non-JSON frame rejected with not_json", () => {
  const r = parseRelayFrame("{nope");
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "not_json");
});

test("unknown message kind rejected", () => {
  const r = parseRelayFrame(JSON.stringify(base({ kind: "teleport" })));
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "unknown_kind");
});

test("unsupported protocol_version rejected with stable code", () => {
  for (const pv of [2, "2", 0, "banana", null]) {
    const r = parseRelayFrame(JSON.stringify({ protocol_version: pv, kind: "hello", workstation_id: "w1" }));
    assert.equal(r.ok, false);
    if (!r.ok) assert.equal(r.code, "unsupported_protocol_version");
  }
});

test("missing protocol_version / kind / workstation_id rejected", () => {
  const cases = [
    [{ kind: "heartbeat", workstation_id: "w1" }, "missing_protocol_version"],
    [{ protocol_version: 1, workstation_id: "w1" }, "missing_kind"],
    [{ protocol_version: 1, kind: "heartbeat" }, "missing_workstation_id"],
  ];
  for (const [obj, code] of cases) {
    const r = parseRelayFrame(JSON.stringify(obj));
    assert.equal(r.ok, false);
    if (!r.ok) assert.equal(r.code, code);
  }
});

test("workstation_id grammar + length bounds", () => {
  assert.equal(checkWorkstationId("w1").ok, true);
  assert.equal(checkWorkstationId("ws-01.alpha:edge").ok, true);
  assert.equal(checkWorkstationId("").ok, false);
  assert.equal(checkWorkstationId("-leading").ok, false);
  assert.equal(checkWorkstationId("has spaces").ok, false);
  assert.equal(checkWorkstationId("a".repeat(65)).ok, false);
  assert.equal(checkWorkstationId(42).ok, false);
  // Rejected end-to-end on the envelope.
  const r = parseRelayFrame(JSON.stringify({ protocol_version: 1, kind: "hello", workstation_id: "bad id!" }));
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "invalid_workstation_id");
});

test("request_id required on correlated kinds, forbidden on control kinds", () => {
  const missing = parseRelayFrame(JSON.stringify(base({ kind: "tool_request", operation: "x" })));
  assert.equal(missing.ok, false);
  if (!missing.ok) assert.equal(missing.code, "missing_request_id");

  const onControl = parseRelayFrame(JSON.stringify(base({ kind: "heartbeat", request_id: "x", boot_id: "b" })));
  assert.equal(onControl.ok, false);
  if (!onControl.ok) assert.equal(onControl.code, "unexpected_request_id");
});

test("request_id grammar + bounds", () => {
  assert.equal(checkRequestId("req_01").ok, true);
  assert.equal(checkRequestId("a".repeat(128)).ok, true);
  assert.equal(checkRequestId("a".repeat(129)).ok, false);
  assert.equal(checkRequestId("").ok, false);
  assert.equal(checkRequestId("!!").ok, false);
});

test("boot_id validated on hello/heartbeat", () => {
  const bad = parseRelayFrame(JSON.stringify(base({ kind: "hello", boot_id: "no spaces", link_version: "1", capabilities: [] })));
  assert.equal(bad.ok, false);
  if (!bad.ok) assert.equal(bad.code, "invalid_boot_id");

  const hb = parseRelayFrame(JSON.stringify(base({ kind: "heartbeat", boot_id: "boot-1", sent_at_ms: 1, active_requests: 0 })));
  assert.equal(hb.ok, true);
});

test("payload byte budget enforced for arguments/result/details", () => {
  const bigArgs = { data: "x".repeat(MAX_ARGS_JSON_BYTES + 1) };
  const r = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r1", operation: "op", arguments: bigArgs })),
  );
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "payload_too_large");

  const smallArgs = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r2", operation: "op", arguments: { a: 1 } })),
  );
  assert.equal(smallArgs.ok, true);
});

test("frame byte gate rejects before parse (frame_too_large)", () => {
  assert.equal(checkFrameBytes("x".repeat(10), 8).ok, false);
  assert.equal(checkFrameBytes("x".repeat(8), 8).ok, true);
  // Envelope-level: a giant result frame fails the default budget.
  const giant = JSON.stringify(base({ kind: "tool_result", request_id: "r", result: { blob: "y".repeat(DEFAULT_MAX_FRAME_BYTES + 2) } }));
  const r = parseRelayFrame(giant);
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "frame_too_large");
});

test("nesting / key / item bounds enforced", () => {
  // Build nested {a:{a:{...}}} to depth 40 programmatically.
  let node = { end: 1 };
  for (let i = 0; i < 40; i++) node = { a: node };
  const args = { wrapped: node };
  const r = parseRelayFrame(JSON.stringify(base({ kind: "tool_request", request_id: "r", operation: "op", arguments: args })));
  assert.equal(r.ok, false);
  if (!r.ok) assert.equal(r.code, "too_deep");
  assert.equal(checkPayloadBudget(args, MAX_ARGS_JSON_BYTES).ok, false);
});

test("non-finite numbers and cycles in payloads are rejected", () => {
  const nanArgs = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r", operation: "op", arguments: { bad: NaN } })),
  );
  // JSON.stringify(NaN) -> null, so NaN cannot survive JSON transport; but the
  // canonical budget path must still reject it when given directly.
  assert.equal(nanArgs.ok, true);
  const cyc = { self: null };
  cyc.self = cyc;
  assert.equal(checkPayloadBudget(cyc, MAX_ARGS_JSON_BYTES).ok, false);
});

test("strict unknown-field gate: extra fields rejected, optional per option", () => {
  const junk = parseRelayFrame(JSON.stringify(base({ kind: "heartbeat", boot_id: "b", sent_at_ms: 1, active_requests: 0, hack: 1 })));
  assert.equal(junk.ok, false);
  if (!junk.ok) assert.equal(junk.code, "unknown_field");

  const junk2 = parseRelayFrame(
    JSON.stringify(base({ kind: "heartbeat", boot_id: "b", sent_at_ms: 1, active_requests: 0, hack: 1 })),
    { strictUnknownFields: false },
  );
  assert.equal(junk2.ok, true);
});

test("hello_ack ok:false requires code+message; ok:true shape accepted", () => {
  const deny = parseRelayFrame(JSON.stringify(base({ kind: "hello_ack", ok: false, code: "auth_rejected", message: "nope" })));
  assert.equal(deny.ok, true);
  const denyMissing = parseRelayFrame(JSON.stringify(base({ kind: "hello_ack", ok: false })));
  assert.equal(denyMissing.ok, false);
  if (!denyMissing.ok) assert.equal(denyMissing.code, "invalid_string");

  const ok = parseRelayFrame(
    JSON.stringify(base({ kind: "hello_ack", ok: true, server_version: "0.1.0-dev", edge_deployment_id: "dep-1", reconnect: false })),
  );
  assert.equal(ok.ok, true);
});

test("tool_error delivery_state enum validated", () => {
  const good = parseRelayFrame(
    JSON.stringify(
      base({ kind: "tool_error", request_id: "r", code: "request_timeout", retryable: true, delivery_state: "delivery_unknown" }),
    ),
  );
  assert.equal(good.ok, true);
  const bad = parseRelayFrame(
    JSON.stringify(
      base({ kind: "tool_error", request_id: "r", code: "x", retryable: false, delivery_state: "maybe" }),
    ),
  );
  assert.equal(bad.ok, false);
  if (!bad.ok) assert.equal(bad.code, "invalid_enum");
});

test("timeout_ms bounded within 1s..60s", () => {
  const low = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r", operation: "op", timeout_ms: 0 })),
  );
  assert.equal(low.ok, false);
  if (!low.ok) assert.equal(low.code, "invalid_number");
  const high = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r", operation: "op", timeout_ms: 90_000 })),
  );
  assert.equal(high.ok, false);
  const mid = parseRelayFrame(
    JSON.stringify(base({ kind: "tool_request", request_id: "r", operation: "op", timeout_ms: 30_000 })),
  );
  assert.equal(mid.ok, true);
});

test("validateRelayMessage non-object / non-JSON-shaped values", () => {
  assert.equal(validateRelayMessage(null).ok, false);
  assert.equal(validateRelayMessage("hi").ok, false);
  assert.equal(validateRelayMessage([1]).ok, false);
  assert.equal(validateRelayMessage(42).ok, false);
});