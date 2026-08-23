// Relay Protocol v1 — deterministic canonical JSON + contract manifest hash,
// contract diff/compatibility. Runs against dist/relay.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import {
  canonicalJson,
  canonicalBytes,
  CanonicalJsonError,
  buildContractManifest,
  computeContractHash,
  verifyContractHash,
  diffContracts,
  contractHashSource,
  normalizeTools,
  isContractHashShape,
} from "../dist/relay/index.js";

test("canonicalJson: key order is irrelevant, values preserved", () => {
  const a = canonicalJson({ b: 1, a: { d: [1, 2], c: "x" } });
  const b = canonicalJson({ a: { c: "x", d: [1, 2] }, b: 1 });
  assert.equal(a, b);
  assert.ok(a.includes('"a"'));
  assert.ok(a.indexOf('"a"') < a.indexOf('"b"'));
});

test("canonicalJson: object inside arrays is also canonicalized, array order kept", () => {
  // Array element order is semantically significant and must be preserved.
  const a = canonicalJson([{ x: 1 }, { y: 2 }]);
  const b = canonicalJson([{ y: 2 }, { x: 1 }]);
  assert.notEqual(a, b);
});

test("canonicalJson rejects unsupported values", () => {
  assert.throws(() => canonicalJson(undefined), CanonicalJsonError);
  assert.throws(() => canonicalJson(() => {}), CanonicalJsonError);
  assert.throws(() => canonicalJson(Symbol("s")), CanonicalJsonError);
  assert.throws(() => canonicalJson({ n: NaN }), CanonicalJsonError);
  assert.throws(() => canonicalJson({ n: Infinity }), CanonicalJsonError);
  assert.throws(() => canonicalJson({ n: -Infinity }), CanonicalJsonError);
  assert.throws(() => canonicalJson(10n), CanonicalJsonError);
});

test("canonicalJson rejects cyclic structures", () => {
  const cyc = {};
  cyc.self = cyc;
  assert.throws(() => canonicalJson(cyc), CanonicalJsonError);
});

test("canonicalJson rejects sparse arrays", () => {
  // eslint-disable-next-line no-sparse-arrays
  const sparse = [1, , 3];
  assert.throws(() => canonicalJson(sparse), CanonicalJsonError);
});

test("canonicalBytes equals UTF-8 of canonicalJson", () => {
  const obj = { z: [1, { q: "q", p: "p" }], a: "あいう" };
  const text = canonicalJson(obj);
  const bytes = canonicalBytes(obj);
  assert.equal(new TextDecoder().decode(bytes), text);
  assert.equal(bytes.length, new TextEncoder().encode(text).length);
});

test("hash independent of tool-list ordering but sensitive to schema-internal order", () => {
  const toolsA = [
    { name: "herdr_a", description: "A", inputSchema: { required: ["x"], properties: { x: { type: "string" } } } },
    { name: "herdr_b", description: "B", inputSchema: { properties: { y: { type: "number" } } } },
  ];
  const toolsB = [...toolsA].reverse(); // different top-level order
  const h1 = computeContractHash(toolsA);
  const h2 = computeContractHash(toolsB);
  assert.equal(h1, h2); // tool-LIST order is semantically irrelevant

  // Schema-internal array order IS semantically meaningful and must differ.
  const toolsReqOrdered = [
    { name: "herdr_a", inputSchema: { required: ["a", "b"], properties: { a: {}, b: {} } } },
  ];
  const toolsReqReversed = [
    { name: "herdr_a", inputSchema: { required: ["b", "a"], properties: { b: {}, a: {} } } },
  ];
  assert.notEqual(computeContractHash(toolsReqOrdered), computeContractHash(toolsReqReversed));
});

test("hash independent of object key insertion order within a tool", () => {
  const t1 = { name: "t", description: "d", inputSchema: { type: "object", properties: { a: { type: "string" } } } };
  const t2 = { inputSchema: { properties: { a: { type: "string" } }, type: "object" }, description: "d", name: "t" };
  assert.equal(computeContractHash([t1]), computeContractHash([t2]));
});

test("duplicate tool names are rejected", () => {
  const tools = [
    { name: "dup", description: "first" },
    { name: "dup", description: "second" },
  ];
  assert.throws(() => computeContractHash(tools), /duplicate tool name/);
  assert.throws(() => buildContractManifest({ contract_epoch: 1, runtime_version: "0", git_commit: null, tools }), /duplicate tool name/);
  assert.throws(() => normalizeTools(tools), /duplicate tool name/);
});

test("contract_hash covers ALL supplied tool metadata (not just name/desc/schema)", () => {
  const baseTool = { name: "t", description: "d", inputSchema: { type: "object" } };
  const annotatedTool = { ...baseTool, annotations: { readOnlyHint: true } };
  // annotations are externally visible contract metadata → they must change the hash.
  assert.notEqual(computeContractHash([baseTool]), computeContractHash([annotatedTool]));
});

test("manifest build + verify round-trip", () => {
  const m = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.26",
    git_commit: "3f42195",
    tools: [
      { name: "herdr_b", description: "B" },
      { name: "herdr_a", description: "A" },
    ],
  });
  assert.equal(m.contract_epoch, 1);
  assert.equal(m.runtime_version, "0.3.26");
  assert.equal(m.git_commit, "3f42195");
  assert.equal(isContractHashShape(m.contract_hash), true);
  assert.equal(verifyContractHash(m), true);
  // tools are normalized (sorted) inside the manifest
  assert.deepEqual(m.tools.map((t) => t.name), ["herdr_a", "herdr_b"]);
  // runtime_version/git_commit/epoch do NOT perturb the hash
  const m2 = buildContractManifest({ ...m, runtime_version: "0.3.27", git_commit: "zzz", contract_epoch: 2 });
  assert.equal(m2.contract_hash, m.contract_hash);
});

test("contractHashSource exposes the hash-covered projection", () => {
  const src = contractHashSource([{ name: "b" }, { name: "a" }]);
  assert.deepEqual(src, [{ name: "a" }, { name: "b" }]);
});

test("epoch-1 live baseline hash is reproducible", () => {
  const baseline = JSON.parse(fs.readFileSync("docs/_wip/.contract-epoch1-baseline.json", "utf8"));
  const expected = "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";
  assert.equal(baseline.tool_count, 17);
  assert.equal(baseline.contract_hash, expected);
  assert.equal(computeContractHash(baseline.tools), expected);
  assert.equal(computeContractHash([...baseline.tools].reverse()), expected);
});

test("contract diff: identical manifests are equal", () => {
  const m = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.26",
    git_commit: "bf",
    tools: [
      { name: "a", description: "A" },
      { name: "b", description: "B" },
    ],
  });
  const m2 = buildContractManifest(m);
  const d = diffContracts(m, m2);
  assert.equal(d.equal, true);
  assert.equal(d.sameHash, true);
  assert.equal(d.sameEpoch, true);
  assert.deepEqual(d.toolDiff, []);
});

test("contract diff: additions/removals/changes reported", () => {
  const base = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.26",
    git_commit: "bf",
    tools: [
      { name: "a", description: "A" },
      { name: "b", description: "B" },
      { name: "c", description: "C" },
    ],
  });
  const next = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.27",
    git_commit: "bf2",
    tools: [
      { name: "a", description: "A" },
      { name: "c", description: "C" },
      { name: "d", description: "D" }, // added
      { name: "b", description: "B2" }, // changed description
    ],
  });
  const d = diffContracts(base, next);
  assert.equal(d.equal, false);
  assert.equal(d.sameEpoch, true);
  assert.equal(d.sameHash, false);
  assert.deepEqual(
    d.toolDiff.map((t) => t.kind).sort(),
    ["added", "changed"].sort(),
  );
  assert.deepEqual(
    d.toolDiff.filter((t) => t.kind === "added").map((t) => t.name),
    ["d"],
  );
  assert.deepEqual(
    d.toolDiff.filter((t) => t.kind === "changed").map((t) => t.name),
    ["b"],
  );
});

test("contract diff: removal + epoch change both flagged, removal is incompatible", () => {
  const base = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "r1",
    git_commit: null,
    tools: [{ name: "keep" }, { name: "gone" }],
  });
  const next = buildContractManifest({
    contract_epoch: 2,
    runtime_version: "r2",
    git_commit: null,
    tools: [{ name: "keep" }],
  });
  const d = diffContracts(base, next);
  assert.equal(d.equal, false);
  assert.equal(d.sameEpoch, false);
  assert.deepEqual(d.toolDiff, [{ kind: "removed", name: "gone" }]);
});

test("compat: purely internal changes (runtime_version) keep hash+epoch stable", () => {
  const base = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.26",
    git_commit: "bf",
    tools: [{ name: "a", description: "A" }],
  });
  const internal = buildContractManifest({
    contract_epoch: 1,
    runtime_version: "0.3.27",
    git_commit: "newer",
    tools: [{ name: "a", description: "A" }],
  });
  const d = diffContracts(base, internal);
  assert.equal(d.sameHash, true);
  assert.equal(d.sameEpoch, true);
  assert.deepEqual(d.toolDiff, []);
  assert.equal(d.equal, true);
});