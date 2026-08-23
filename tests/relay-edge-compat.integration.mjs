import { test } from "node:test";
import assert from "node:assert/strict";

import {
  RELAY_PROTOCOL_VERSION as ROOT_VERSION,
  MESSAGE_KINDS as ROOT_KINDS,
  CORRELATED_KINDS as ROOT_CORRELATED,
  computeContractHash,
  validateRelayMessage as validateRoot,
} from "../dist/relay/index.js";
import {
  RELAY_PROTOCOL_VERSION as EDGE_VERSION,
  MESSAGE_KINDS as EDGE_KINDS,
  CORRELATED_KINDS as EDGE_CORRELATED,
  validateRelayMessage as validateEdge,
} from "../edge/cloudflare/dist/canonical-imports.js";
import { EPOCH1_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch1.js";

const base = { protocol_version: 1, workstation_id: "w1" };

const validFrames = [
  { ...base, kind: "hello", boot_id: "boot1", link_version: "0.1.0", capabilities: [] },
  { ...base, kind: "hello_ack", ok: true },
  { ...base, kind: "heartbeat", boot_id: "boot1", sent_at_ms: 1, active_requests: 0 },
  { ...base, kind: "status", query: true },
  { ...base, kind: "tool_request", request_id: "r1", operation: "herdr_inspect" },
  { ...base, kind: "tool_result", request_id: "r1", result: { ok: true }, served_at_ms: 1 },
  {
    ...base,
    kind: "tool_error",
    request_id: "r1",
    code: "runtime_error",
    retryable: false,
    delivery_state: "delivered",
    served_at_ms: 1,
  },
  { ...base, kind: "cancel", request_id: "r1", reason: "test" },
  { ...base, kind: "cancel_ack", request_id: "r1", accepted: true, cancelled_at_ms: 1 },
];

const invalidFrames = [
  { frame: { ...base, kind: "request", request_id: "r1" }, code: "unknown_kind" },
  { frame: { ...base, protocol_version: "1", kind: "status" }, code: "unsupported_protocol_version" },
  { frame: { protocol_version: 1, kind: "status" }, code: "missing_workstation_id" },
  { frame: { ...base, kind: "tool_request", operation: "herdr_inspect" }, code: "missing_request_id" },
  { frame: { ...base, kind: "status", request_id: "r1" }, code: "unexpected_request_id" },
  { frame: { ...base, kind: "tool_error", request_id: "r1", code: "x", retryable: false, delivery_state: "maybe" }, code: "invalid_enum" },
  { frame: { ...base, kind: "hello", boot_id: "b", link_version: "v", capabilities: [], protocolVersion: 1 }, code: "unknown_field" },
];

test("root and Cloudflare edge expose the same Relay v1 kind contract", () => {
  assert.equal(EDGE_VERSION, ROOT_VERSION);
  assert.deepEqual([...EDGE_KINDS], [...ROOT_KINDS]);
  assert.deepEqual([...EDGE_CORRELATED].sort(), [...ROOT_CORRELATED].sort());
});

test("root and Cloudflare edge accept the same canonical Relay v1 frames", () => {
  for (const frame of validFrames) {
    const root = validateRoot(frame);
    const edge = validateEdge(frame);
    assert.equal(root.ok, true, `root rejected ${frame.kind}: ${root.ok ? "" : root.reason}`);
    assert.equal(edge.ok, true, `edge rejected ${frame.kind}: ${edge.ok ? "" : edge.reason}`);
  }
});

test("root and Cloudflare edge reject representative drift with the same code", () => {
  for (const { frame, code } of invalidFrames) {
    const root = validateRoot(frame);
    const edge = validateEdge(frame);
    assert.equal(root.ok, false, `root unexpectedly accepted ${JSON.stringify(frame)}`);
    assert.equal(edge.ok, false, `edge unexpectedly accepted ${JSON.stringify(frame)}`);
    assert.equal(root.code, code);
    assert.equal(edge.code, code);
  }
});

test("tracked Cloudflare epoch-1 catalog stays frozen to the captured 17-tool contract", () => {
  const frozen = EPOCH1_CONTRACT;
  const expected = "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";
  assert.equal(frozen.contract_epoch, 1);
  assert.equal(frozen.tool_count, 17);
  assert.equal(frozen.contract_hash, expected);
  assert.equal(frozen.tools.length, 17);
  assert.equal(frozen.tools.some((tool) => tool.name === "herdr_skill"), false);
  assert.equal(computeContractHash(frozen.tools), expected);
});
