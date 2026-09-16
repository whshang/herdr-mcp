import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

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
import { EPOCH5_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch5.js";
import { EPOCH6_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch6.js";
import { EPOCH7_CONTRACT } from "../edge/cloudflare/dist/contracts/epoch7.js";
import { PUBLIC_CONTRACT, resolvePublicContract } from "../edge/cloudflare/dist/contracts/public.js";
import {
  PREVIOUS_RUNTIME_EXECUTION_CONTRACT,
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

test("public epoch 3 evolves independently while runtime execution advances to epoch 4", () => {
  assert.equal(PUBLIC_CONTRACT, EPOCH3_CONTRACT);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_epoch, 4);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_hash, "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135");
  assert.equal(RUNTIME_EXECUTION_CONTRACT.tool_count, 18);
  assert.equal(PREVIOUS_RUNTIME_EXECUTION_CONTRACT.contract_epoch, 3);
  assert.equal(PREVIOUS_RUNTIME_EXECUTION_CONTRACT.contract_hash, "sha256:05350993b3e964ab28c8b586c3fdbffa5fa615025bc7f3e93eb6aa960c901fc5");
  assert.equal(PREVIOUS_RUNTIME_EXECUTION_CONTRACT.tool_count, 18);
  assert.equal(PUBLIC_CONTRACT.contract_epoch, 3);
  assert.equal(PUBLIC_CONTRACT.tool_count, 19);
  assert.equal(PUBLIC_CONTRACT.tools.some((tool) => tool.name === "herdr_devices"), true);
  assert.equal(computeContractHash(PUBLIC_CONTRACT.tools), PUBLIC_CONTRACT.contract_hash);
  const publicInspect = PUBLIC_CONTRACT.tools.find((tool) => tool.name === "herdr_inspect");
  const runtimeInspect = EPOCH2_CONTRACT.tools.find((tool) => tool.name === "herdr_inspect");
  assert.equal(publicInspect.inputSchema.properties.device.type, "string");
  assert.equal(Object.hasOwn(runtimeInspect.inputSchema.properties, "device"), false);
  // Edge/Relay acceptance window: current runtime contract plus the previous
  // runtime execution identity and the frozen epoch-2 catalog, and nothing older.
  assert.equal(isCompatibleRuntimeContract(4, RUNTIME_EXECUTION_CONTRACT.contract_hash), true);
  assert.equal(isCompatibleRuntimeContract(3, PREVIOUS_RUNTIME_EXECUTION_CONTRACT.contract_hash), true);
  assert.equal(isCompatibleRuntimeContract(2, EPOCH2_CONTRACT.contract_hash), true);
  assert.equal(isCompatibleRuntimeContract(1, EPOCH1_CONTRACT.contract_hash), false);
  assert.equal(isCompatibleRuntimeContract(2, EPOCH1_CONTRACT.contract_hash), false);
  assert.equal(isCompatibleRuntimeContract(3, EPOCH2_CONTRACT.contract_hash), false);
  assert.equal(isCompatibleRuntimeContract(4, PREVIOUS_RUNTIME_EXECUTION_CONTRACT.contract_hash), false);
});

test("public contract resolver enables epoch 7 for explicit first-party dev and prod environments", () => {
  assert.equal(resolvePublicContract("dev"), EPOCH7_CONTRACT);
  assert.equal(resolvePublicContract("prod"), EPOCH7_CONTRACT);
  assert.equal(resolvePublicContract(), EPOCH3_CONTRACT);
  assert.equal(resolvePublicContract("unknown"), EPOCH3_CONTRACT);
});

test("epoch 4 annotates only read-only tools and leaves prior hashes unchanged", () => {
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
  // The exported fallback surface stays on epoch 3; explicit dev/prod select epoch 4.
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
  const epoch3Exec = EPOCH3_CONTRACT.tools.find((candidate) => candidate.name === "herdr_exec");
  const epoch4Exec = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === "herdr_exec");
  const epoch3ExecStart = EPOCH3_CONTRACT.tools.find((candidate) => candidate.name === "herdr_exec_start");
  const epoch4ExecStart = EPOCH4_CONTRACT.tools.find((candidate) => candidate.name === "herdr_exec_start");
  assert.ok(epoch3Exec && epoch4Exec, "herdr_exec must exist in both epochs");
  assert.ok(epoch3ExecStart && epoch4ExecStart, "herdr_exec_start must exist in both epochs");
  assert.ok(epoch4Exec.inputSchema.properties.steps, "epoch 4 must advertise structured exec steps");
  assert.deepEqual(epoch4Exec.inputSchema.required, ["workspace"]);
  assert.equal("root" in epoch4Exec.inputSchema.properties, false);
  assert.match(epoch4Exec.description, /requires workspace.*project_root.*does not take root/i);
  assert.match(epoch4Exec.inputSchema.properties.workspace.description, /Do not pass root here/i);
  assert.match(epoch4Exec.inputSchema.properties.project_root.description, /herdr_exec_start uses root instead/i);
  assert.equal(epoch4Exec.inputSchema.oneOf.length, 2);
  assert.equal(epoch4Exec.inputSchema.properties.steps.maxItems, 16);
  assert.equal(epoch4Exec.inputSchema.properties.steps.items.properties.args.maxItems, 128);
  assert.deepEqual(epoch4ExecStart.inputSchema.required, ["root"]);
  assert.equal("workspace" in epoch4ExecStart.inputSchema.properties, false);
  assert.equal("project_root" in epoch4ExecStart.inputSchema.properties, false);
  assert.equal("steps" in epoch4ExecStart.inputSchema.properties, false);
  assert.ok(epoch4ExecStart.inputSchema.properties.program);
  assert.ok(epoch4ExecStart.inputSchema.properties.args);
  assert.equal(epoch4ExecStart.inputSchema.properties.args.maxItems, 128);
  assert.deepEqual(epoch4ExecStart.inputSchema.oneOf, [
    {
      required: ["command"],
      not: { anyOf: [{ required: ["program"] }, { required: ["args"] }] },
    },
    { required: ["program"], not: { required: ["command"] } },
  ]);
  assert.match(
    epoch4ExecStart.description,
    /does not take workspace or project_root.*does not take steps.*Prefer program \+ args.*Use legacy command only when shell semantics/is,
  );
  assert.match(epoch4ExecStart.inputSchema.properties.root.description, /takes root directly.*workspace or project_root/i);
  assert.match(epoch4ExecStart.inputSchema.properties.command.description, /shell command.*shell syntax.*program \+ args/is);
  assert.match(epoch4ExecStart.inputSchema.properties.program.description, /Executable name or path.*args/is);
  assert.match(epoch4ExecStart.inputSchema.properties.args.description, /Defaults to \[\].*no shell expansion/is);

  // Apart from the deliberate execution descriptions and herdr_exec's
  // structured schema, epoch 4 only adds annotations to the frozen epoch-3 catalog.
  const strip = (tools) => tools.map(({ annotations, ...rest }) => rest);
  assert.deepEqual(
    strip(EPOCH4_CONTRACT.tools.filter((tool) => !["herdr_exec", "herdr_exec_start"].includes(tool.name))),
    strip(EPOCH3_CONTRACT.tools.filter((tool) => !["herdr_exec", "herdr_exec_start"].includes(tool.name))),
  );
  const stripExecDelta = ({ annotations, description, inputSchema, ...rest }) => rest;
  assert.deepEqual(stripExecDelta(epoch4Exec), stripExecDelta(epoch3Exec));
  assert.deepEqual(stripExecDelta(epoch4ExecStart), stripExecDelta(epoch3ExecStart));
  // Frozen prior epochs stayed bit-for-bit identical.
  assert.equal(EPOCH3_CONTRACT.contract_hash, "sha256:b8b4e5d13ccb3a1a7ab0c2e9ccfa913c076d0e1cd978cfe544d1261ea2509071");
  assert.equal(computeContractHash(EPOCH3_CONTRACT.tools), EPOCH3_CONTRACT.contract_hash);
  assert.equal(computeContractHash(EPOCH2_CONTRACT.tools), EPOCH2_CONTRACT.contract_hash);
});

test("public epoch 5 shapes only the herdr_exec description over a frozen epoch 4", () => {
  assert.equal(EPOCH5_CONTRACT.contract_epoch, 5);
  assert.equal(EPOCH5_CONTRACT.tool_count, EPOCH4_CONTRACT.tool_count);
  assert.equal(computeContractHash(EPOCH5_CONTRACT.tools), EPOCH5_CONTRACT.contract_hash);
  assert.notEqual(EPOCH5_CONTRACT.contract_hash, EPOCH4_CONTRACT.contract_hash);
  assert.deepEqual(
    EPOCH5_CONTRACT.tools.map((tool) => tool.name).sort(),
    EPOCH4_CONTRACT.tools.map((tool) => tool.name).sort(),
  );
  const changed = EPOCH5_CONTRACT.tools.filter((tool, index) => {
    const previous = EPOCH4_CONTRACT.tools[index];
    return JSON.stringify(tool) !== JSON.stringify(previous);
  });
  assert.deepEqual(changed.map((tool) => tool.name), ["herdr_exec"]);
  const epoch5Exec = EPOCH5_CONTRACT.tools.find((tool) => tool.name === "herdr_exec");
  const epoch4Exec = EPOCH4_CONTRACT.tools.find((tool) => tool.name === "herdr_exec");
  assert.notEqual(epoch5Exec.description, epoch4Exec.description);
  assert.match(epoch5Exec.description, /native session/i);
  assert.match(epoch5Exec.description, /protected roots.*visible utility pane/i);
  assert.deepEqual(epoch5Exec.inputSchema, epoch4Exec.inputSchema);
  // Frozen prior epochs stayed bit-for-bit identical.
  assert.equal(EPOCH4_CONTRACT.contract_hash, "sha256:0c756a7479ff5d5c70891d7cf5c9810841a1e936327d91aa0770abe67faf83af");
  assert.equal(computeContractHash(EPOCH4_CONTRACT.tools), EPOCH4_CONTRACT.contract_hash);
});

test("public epoch 6 removes planner workflow policy from model-visible descriptions", () => {
  assert.equal(EPOCH6_CONTRACT.contract_epoch, 6);
  assert.equal(EPOCH6_CONTRACT.tool_count, EPOCH5_CONTRACT.tool_count);
  assert.equal(computeContractHash(EPOCH6_CONTRACT.tools), EPOCH6_CONTRACT.contract_hash);
  assert.equal(
    EPOCH6_CONTRACT.contract_hash,
    "sha256:addcde324850f88cd2f87bf38de1edb7fc34e90e8cd178b37cc49694b56636cf",
  );
  assert.deepEqual(
    EPOCH6_CONTRACT.tools.map((tool) => tool.name).sort(),
    EPOCH5_CONTRACT.tools.map((tool) => tool.name).sort(),
  );
  const banned = [
    /before any agent operation/i,
    /call herdr_skill/i,
    /call herdr_/i,
    /typical session start/i,
    /you \(web\) are the planner/i,
    /\bplanner\b/i,
    /\bprefer\b/i,
    /\bshould\b/i,
    /never blind[- ]retry/i,
    /verify with herdr_/i,
    /do not prompt/i,
    /should already have been read/i,
    /prefer herdr_prompt/i,
  ];
  const descriptionStrings = (value) => {
    if (typeof value === "string") return [];
    if (!value || typeof value !== "object") return [];
    return Object.entries(value).flatMap(([key, child]) => [
      ...(key === "description" && typeof child === "string" ? [child] : []),
      ...descriptionStrings(child),
    ]);
  };
  for (const tool of EPOCH6_CONTRACT.tools) {
    for (const description of descriptionStrings(tool)) {
      for (const pattern of banned) {
        assert.doesNotMatch(description, pattern, `${tool.name}: ${description}`);
      }
    }
  }
  const stripDescriptions = (value) => {
    if (Array.isArray(value)) return value.map(stripDescriptions);
    if (!value || typeof value !== "object") return value;
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) => key !== "description")
        .map(([key, child]) => [key, stripDescriptions(child)]),
    );
  };
  assert.deepEqual(
    stripDescriptions(EPOCH6_CONTRACT.tools.map((tool) => tool.inputSchema)),
    stripDescriptions(EPOCH5_CONTRACT.tools.map((tool) => tool.inputSchema)),
  );
  assert.equal(EPOCH5_CONTRACT.contract_hash, "sha256:560dc151053f2a54fc1271c299fb6ed4464d5b9651a6c172c5eb6c9d63396e39");
  assert.equal(EPOCH4_CONTRACT.contract_hash, "sha256:0c756a7479ff5d5c70891d7cf5c9810841a1e936327d91aa0770abe67faf83af");
});

test("public epoch 7 removes safety-sensitive wording without changing tool behavior", () => {
  assert.equal(EPOCH7_CONTRACT.contract_epoch, 7);
  assert.equal(EPOCH7_CONTRACT.tool_count, EPOCH6_CONTRACT.tool_count);
  assert.equal(computeContractHash(EPOCH7_CONTRACT.tools), EPOCH7_CONTRACT.contract_hash);
  assert.equal(
    EPOCH7_CONTRACT.contract_hash,
    "sha256:971ee73a86eef74b1f046b1097806e8d71a4e63b0b57d6f6d5ea35c4e54c44d3",
  );
  assert.notEqual(EPOCH7_CONTRACT.contract_hash, EPOCH6_CONTRACT.contract_hash);
  assert.deepEqual(
    EPOCH7_CONTRACT.tools.map((tool) => tool.name).sort(),
    EPOCH6_CONTRACT.tools.map((tool) => tool.name).sort(),
  );

  const readOnly = new Set([
    "herdr_methods", "herdr_inspect", "herdr_skill", "herdr_since", "herdr_fs_read",
    "herdr_fs_list", "herdr_fs_grep", "herdr_fs_image", "herdr_git", "herdr_exec_read",
    "herdr_devices",
  ]);
  const destructive = new Set([
    "herdr_call", "herdr_fs_patch", "herdr_exec_start", "herdr_exec_kill", "herdr_exec",
    "herdr_fs_edit", "herdr_fs_write", "herdr_prompt",
  ]);
  const openWorld = new Set([
    "herdr_skill", "herdr_call", "herdr_exec_start", "herdr_exec", "herdr_prompt",
  ]);
  for (const tool of EPOCH7_CONTRACT.tools) {
    assert.equal(typeof tool.title, "string", `${tool.name} title`);
    assert.ok(tool.title.length >= 3, `${tool.name} title`);
    assert.match(tool.description, /^Use this /, `${tool.name} description`);
    assert.equal(tool.annotations.readOnlyHint, readOnly.has(tool.name), `${tool.name} readOnlyHint`);
    assert.equal(tool.annotations.destructiveHint, destructive.has(tool.name), `${tool.name} destructiveHint`);
    assert.equal(tool.annotations.openWorldHint, openWorld.has(tool.name), `${tool.name} openWorldHint`);
  }

  const risky = [
    /before any agent operation/i,
    /typical session start/i,
    /you \(web\) are the planner/i,
    /\bplanner\b/i,
    /\bprefer\b/i,
    /\bshould\b/i,
    /\brecommended\b/i,
    /\bdo not\b/i,
    /\bnever\b/i,
    /\bcall herdr_/i,
    /verify with herdr_/i,
    /\bhigh-capability\b/i,
    /arbitrary commands/i,
    /\bbypass\b/i,
    /\bpolicy\b/i,
    /local api burn/i,
    /\bcheap\b/i,
    /\bexpensive\b/i,
  ];
  const descriptionStrings = (value) => {
    if (typeof value === "string") return [];
    if (!value || typeof value !== "object") return [];
    return Object.entries(value).flatMap(([key, child]) => [
      ...(key === "description" && typeof child === "string" ? [child] : []),
      ...descriptionStrings(child),
    ]);
  };
  for (const tool of EPOCH7_CONTRACT.tools) {
    for (const description of descriptionStrings(tool)) {
      for (const pattern of risky) {
        assert.doesNotMatch(description, pattern, `${tool.name}: ${description}`);
      }
    }
  }

  const stripDescriptions = (value) => {
    if (Array.isArray(value)) return value.map(stripDescriptions);
    if (!value || typeof value !== "object") return value;
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) => key !== "description")
        .map(([key, child]) => [key, stripDescriptions(child)]),
    );
  };
  assert.deepEqual(
    stripDescriptions(EPOCH7_CONTRACT.tools.map((tool) => tool.inputSchema)),
    stripDescriptions(EPOCH6_CONTRACT.tools.map((tool) => tool.inputSchema)),
  );
  assert.equal(EPOCH6_CONTRACT.contract_hash, "sha256:addcde324850f88cd2f87bf38de1edb7fc34e90e8cd178b37cc49694b56636cf");
});

test("runtime execution epoch 4 neutralizes herdr_exec metadata without changing the tool list", async () => {
  const root = new URL("../", import.meta.url);
  const descriptor = JSON.parse(
    await readFile(new URL("contracts/runtime-exec-v4.json", root), "utf8"),
  );
  const previous = JSON.parse(
    await readFile(new URL("contracts/runtime-exec-v3.json", root), "utf8"),
  );
  const base = JSON.parse(
    await readFile(new URL("contracts/epoch2.json", root), "utf8"),
  );

  // Epoch 4 shapes only the herdr_exec description over the frozen catalog.
  const shaped = base.tools.map((tool) =>
    tool.name === descriptor.shape.tool
      ? { ...tool, description: descriptor.shape.set.description }
      : tool,
  );
  assert.equal(descriptor.contract_epoch, 4);
  assert.equal(descriptor.tool_count, base.tool_count);
  assert.equal(computeContractHash(shaped), descriptor.contract_hash);

  // The previous epoch-3 descriptor stays byte-frozen with its pinned identity.
  assert.equal(previous.contract_epoch, 3);
  assert.equal(previous.contract_hash, "sha256:05350993b3e964ab28c8b586c3fdbffa5fa615025bc7f3e93eb6aa960c901fc5");
  const previousShaped = base.tools.map((tool) =>
    tool.name === previous.shape.tool
      ? { ...tool, description: previous.shape.set.description }
      : tool,
  );
  assert.equal(computeContractHash(previousShaped), previous.contract_hash);
  // The frozen epoch-2 catalog hash is unchanged as well.
  assert.equal(computeContractHash(base.tools), base.contract_hash);

  // The current runtime herdr_exec metadata carries the same neutral wording
  // gate as the ChatGPT-visible public contract.
  const risky = [
    /high-capability/i,
    /arbitrary commands/i,
    /secret-path gated/i,
    /\bbypass\b/i,
    /\bplanner\b/i,
    /\bdo not\b/i,
    /\bnever\b/i,
    /\bprefer\b/i,
    /\bshould\b/i,
    /\bmust\b/i,
    /\bpolicy\b/i,
  ];
  for (const pattern of risky) {
    assert.doesNotMatch(descriptor.shape.set.description, pattern);
  }
  // The real constraints stay stated as facts.
  assert.match(descriptor.shape.set.description, /HERDR_MCP_READONLY/);
  assert.match(descriptor.shape.set.description, /HERDR_MCP_WRITE_ROOTS/);
  assert.match(descriptor.shape.set.description, /execution\.started/);
  assert.match(descriptor.shape.set.description, /execution\.exit_code/);

  // Metadata-only change: the tool list itself is untouched.
  assert.deepEqual(
    shaped.map((tool) => tool.name),
    base.tools.map((tool) => tool.name),
  );
  for (const [index, tool] of shaped.entries()) {
    if (tool.name === descriptor.shape.tool) continue;
    assert.deepEqual(tool, base.tools[index]);
  }
});
