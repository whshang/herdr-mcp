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
import { EPOCH2_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch2.js";
import { EPOCH3_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch3.js";
import { EPOCH4_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch4.js";
import { PUBLIC_CONTRACT } from "../edge/cloudflare/dist/contracts/public.js";
import {
  RUNTIME_EXECUTION_CONTRACT,
  isCompatibleRuntimeContract,
} from "../edge/cloudflare/dist/contracts/runtime.js";

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

test("tracked Cloudflare epoch-2 catalog stays frozen to the captured 18-tool contract", () => {
  const expected = "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8";
  assert.equal(EPOCH2_CONTRACT.contract_epoch, 2);
  assert.equal(EPOCH2_CONTRACT.tool_count, 18);
  assert.equal(EPOCH2_CONTRACT.contract_hash, expected);
  assert.equal(EPOCH2_CONTRACT.tools.length, 18);
  assert.equal(EPOCH2_CONTRACT.tools.some((tool) => tool.name === "herdr_skill"), true);
  assert.equal(computeContractHash(EPOCH2_CONTRACT.tools), expected);
});

test("public epoch 3 evolves independently while runtime execution stays epoch 2", () => {
  assert.equal(PUBLIC_CONTRACT, EPOCH3_CONTRACT);
  assert.equal(RUNTIME_EXECUTION_CONTRACT, EPOCH2_CONTRACT);
  assert.equal(PUBLIC_CONTRACT.contract_epoch, 3);
  assert.equal(PUBLIC_CONTRACT.tool_count, 19);
  assert.equal(PUBLIC_CONTRACT.tools.some((tool) => tool.name === "herdr_devices"), true);
  assert.equal(computeContractHash(PUBLIC_CONTRACT.tools), PUBLIC_CONTRACT.contract_hash);
  const publicInspect = PUBLIC_CONTRACT.tools.find((tool) => tool.name === "herdr_inspect");
  const runtimeInspect = EPOCH2_CONTRACT.tools.find((tool) => tool.name === "herdr_inspect");
  assert.equal(publicInspect.inputSchema.properties.device.type, "string");
  assert.equal(Object.hasOwn(runtimeInspect.inputSchema.properties, "device"), false);
  assert.equal(isCompatibleRuntimeContract(2, EPOCH2_CONTRACT.contract_hash), true);
  assert.equal(isCompatibleRuntimeContract(1, EPOCH1_CONTRACT.contract_hash), true);
  assert.equal(isCompatibleRuntimeContract(2, EPOCH1_CONTRACT.contract_hash), false);
  assert.equal(isCompatibleRuntimeContract(1, EPOCH2_CONTRACT.contract_hash), false);
});

test("future epoch 4 annotates only read-only tools and leaves prior hashes unchanged", () => {
  const readOnly = [
    "herdr_methods",
    "herdr_inspect",
    "herdr_skill",
    "herdr_since",
    "herdr_fs_read",
    "herdr_fs_list",
    "herdr_fs_grep",
    "herdr_fs_image",
    "herdr_git",
    "herdr_exec_read",
    "herdr_devices",
  ];
  const mutationCapable = [
    "herdr_call",
    "herdr_fs_patch",
    "herdr_fs_edit",
    "herdr_fs_write",
    "herdr_exec_start",
    "herdr_exec_kill",
    "herdr_exec",
    "herdr_prompt",
  ];
  // Epoch 4 is not yet active: the public surface still resolves to epoch 3.
  assert.equal(PUBLIC_CONTRACT, EPOCH3_CONTRACT);
  assert.equal(EPOCH4_CONTRACT.contract_epoch, 4);
  assert.equal(EPOCH4_CONTRACT.tool_count, EPOCH3_CONTRACT.tool_count);
  assert.deepEqual(
    EPOCH4_CONTRACT.tools.map((tool) => tool.name).sort(),
    EPOCH3_CONTRACT.tools.map((tool) => tool.name).sort(),
  );
  // The declared hash must be the real hash of the annotated catalog.
  assert.equal(computeContractHash(EPOCH4_CONTRACT.tools), EPOCH4_CONTRACT.contract_hash);
  for (const name of readOnly) {
    const tool = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === name);
    assert.ok(tool, `${name} must exist in epoch 4`);
    assert.equal(tool.annotations?.readOnlyHint, true, `${name} must be truthfully read-only`);
  }
  const closedWorldReadOnly = [
    "herdr_methods",
    "herdr_inspect",
    "herdr_since",
    "herdr_fs_read",
    "herdr_fs_list",
    "herdr_fs_grep",
    "herdr_fs_image",
    "herdr_git",
    "herdr_exec_read",
    "herdr_devices",
  ];
  for (const name of closedWorldReadOnly) {
    const tool = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === name);
    assert.ok(tool, `${name} must exist in epoch 4`);
    assert.equal(tool.annotations?.readOnlyHint, true, `${name} must be truthfully read-only`);
    assert.equal(tool.annotations?.openWorldHint, false, `${name} must be truthfully closed-world`);
  }
  const skill = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === "herdr_skill");
  assert.equal(skill.annotations?.readOnlyHint, true);
  assert.notEqual(skill.annotations?.openWorldHint, false, "herdr_skill may refresh configured upstream policy");
  for (const name of mutationCapable) {
    const tool = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === name);
    assert.ok(tool, `${name} must exist in epoch 4`);
    assert.notEqual(tool.annotations?.readOnlyHint, true, `${name} must not be mislabeled read-only`);
  }
  // Only annotations differ from epoch 3; the underlying tool definitions are untouched.
  const strip = (tools) => tools.map(({ annotations, ...rest }) => rest);
  assert.deepEqual(strip(EPOCH4_CONTRACT.tools), strip(EPOCH3_CONTRACT.tools));
  // Frozen prior epochs stayed bit-for-bit identical.
  assert.equal(EPOCH3_CONTRACT.contract_hash, "sha256:b8b4e5d13ccb3a1a7ab0c2e9ccfa913c076d0e1cd978cfe544d1261ea2509071");
  assert.equal(computeContractHash(EPOCH3_CONTRACT.tools), EPOCH3_CONTRACT.contract_hash);
  assert.equal(computeContractHash(EPOCH2_CONTRACT.tools), EPOCH2_CONTRACT.contract_hash);
});
