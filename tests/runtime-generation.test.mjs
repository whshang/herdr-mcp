import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { computeContractHash } from "../dist/relay/contract.js";
import { RuntimeControlLoop, validateRuntimeControlDocument } from "../dist/link/runtime-control.js";
import { RuntimeGenerationManager, validateRuntimeGenerationSpec } from "../dist/link/runtime-generation.js";

const TOKEN = "runtime-generation-test-token";
const CATALOG = [{
  name: "herdr_inspect",
  description: "inspect",
  inputSchema: { type: "object", properties: {} },
}];
const HASH = computeContractHash(CATALOG);

function frame(id, operation = "herdr_inspect") {
  return { v: 1, type: "request", workstation_id: "w1", request_id: id, operation, arguments: {} };
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function runtimeFetch(options = {}) {
  const state = {
    healthCalls: new Map(),
    toolCalls: [],
    pendingOld: null,
    releaseOld: null,
    ...options.state,
  };
  const fn = async (input, init = {}) => {
    const u = new URL(typeof input === "string" ? input : input.url);
    const port = u.port;
    const body = JSON.parse(init.body || "{}");
    if (body.method === "server/discover") {
      const n = (state.healthCalls.get(port) || 0) + 1;
      state.healthCalls.set(port, n);
      const failAfter = options.failHealthAfter?.[port];
      if (failAfter != null && n > failAfter) return json({ error: "down" }, 503);
      const version = port === "8773" ? "0.3.26" : "0.3.23";
      return json({ jsonrpc: "2.0", id: body.id, result: { serverInfo: { name: "herdr", version } } });
    }
    if (body.method === "tools/list") {
      const tools = options.driftPort === port
        ? [{ ...CATALOG[0], description: "drift" }]
        : CATALOG;
      return json({ jsonrpc: "2.0", id: body.id, result: { tools } });
    }
    if (body.method === "tools/call") {
      state.toolCalls.push({ port, name: body.params?.name, id: body.id });
      if (options.deferPort === port) {
        return new Promise((resolve) => {
          state.pendingOld = { resolve, id: body.id, port };
          state.releaseOld = () => resolve(json({ jsonrpc: "2.0", id: body.id, result: { port } }));
          init.signal?.addEventListener("abort", () => resolve(json({ jsonrpc: "2.0", id: body.id, error: { code: -32800 } })), { once: true });
        });
      }
      return json({ jsonrpc: "2.0", id: body.id, result: { port } });
    }
    return json({ error: "unexpected" }, 500);
  };
  fn.state = state;
  return fn;
}

function manager(fetchFn, overrides = {}) {
  return new RuntimeGenerationManager({
    base: { generation: "stable", endpoint: "http://127.0.0.1:8772/mcp", expected_runtime_version: "0.3.23" },
    bearerToken: TOKEN,
    contractHash: HASH,
    fetch: fetchFn,
    observationChecks: 2,
    observationIntervalMs: 0,
    sleep: async () => {},
    ...overrides,
  });
}

test("runtime generation specs stay loopback-only", () => {
  validateRuntimeGenerationSpec({ generation: "candidate-1", endpoint: "http://127.0.0.1:8773/mcp" });
  assert.throws(() => validateRuntimeGenerationSpec({ generation: "candidate", endpoint: "http://10.0.0.5:8773/mcp" }), /loopback-only/);
  assert.throws(() => validateRuntimeGenerationSpec({ generation: "../bad", endpoint: "http://127.0.0.1:8773/mcp" }), /generation id/);
});

test("candidate registration verifies real tools/list contract hash and runtime version", async () => {
  const goodFetch = runtimeFetch();
  const m = manager(goodFetch);
  const good = await m.registerGeneration({ generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp", expected_runtime_version: "0.3.26" });
  assert.equal(good.ok, true);
  assert.equal(good.contract_hash, HASH);
  assert.equal(good.tool_count, 1);

  const badFetch = runtimeFetch({ driftPort: "8773" });
  const bad = manager(badFetch);
  const rejected = await bad.registerGeneration({ generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp", expected_runtime_version: "0.3.26" });
  assert.equal(rejected.ok, false);
  assert.equal(rejected.code, "contract_mismatch");
  assert.notEqual(rejected.contract_hash, HASH);
});

test("activation is atomic: old in-flight request drains on old generation while new requests use candidate", async () => {
  const fetchFn = runtimeFetch({ deferPort: "8772" });
  const m = manager(fetchFn);
  assert.equal((await m.registerGeneration({ generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp", expected_runtime_version: "0.3.26" })).ok, true);

  const oldPromise = m.dispatchRequest(frame("old-r1"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(fetchFn.state.pendingOld);

  const activated = await m.activateGeneration("candidate", { checks: 1, intervalMs: 0 });
  assert.equal(activated.ok, true);
  let status = m.getStatus();
  assert.equal(status.active_generation, "candidate");
  assert.equal(status.generations.find((g) => g.generation === "stable").phase, "draining");
  assert.equal(status.generations.find((g) => g.generation === "stable").in_flight, 1);

  const fresh = await m.dispatchRequest(frame("new-r1"));
  assert.deepEqual(fresh, { ok: true, result: { port: "8773" } });
  fetchFn.state.releaseOld();
  const old = await oldPromise;
  assert.deepEqual(old, { ok: true, result: { port: "8772" } });
  status = m.getStatus();
  assert.equal(status.generations.find((g) => g.generation === "stable").phase, "standby");
  assert.equal(status.generations.find((g) => g.generation === "stable").in_flight, 0);
});

test("candidate health failure during observation automatically rolls active pointer back", async () => {
  // Candidate health: register=1, activation revalidation=2, first observation=3 -> fail.
  const fetchFn = runtimeFetch({ failHealthAfter: { "8773": 2 } });
  const m = manager(fetchFn);
  assert.equal((await m.registerGeneration({ generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp", expected_runtime_version: "0.3.26" })).ok, true);
  const result = await m.activateGeneration("candidate", { checks: 2, intervalMs: 0 });
  assert.equal(result.ok, false);
  assert.equal(result.rolled_back, true);
  assert.equal(m.activeGenerationId, "stable");
  assert.equal(m.getStatus().last_transition?.outcome, "rolled_back");
});

test("runtime control document is bounded, unique, and desired generation must exist", () => {
  const good = validateRuntimeControlDocument({
    schema_version: 1,
    revision: 2,
    desired_active: "candidate",
    generations: [
      { generation: "stable", endpoint: "http://127.0.0.1:8772/mcp" },
      { generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp" },
    ],
  });
  assert.equal(good.desired_active, "candidate");
  assert.throws(() => validateRuntimeControlDocument({ schema_version: 1, revision: 1, desired_active: "missing", generations: [{ generation: "stable", endpoint: "http://127.0.0.1:8772/mcp" }] }), /desired_active/);
});

test("file control revision validates and activates a candidate without restarting the manager", async () => {
  const dir = await mkdtemp(join(tmpdir(), "herdr-runtime-control-"));
  const controlPath = join(dir, "control.json");
  const statusPath = join(dir, "status.json");
  const fetchFn = runtimeFetch();
  const m = manager(fetchFn);
  const loop = new RuntimeControlLoop({
    manager: m,
    base: { generation: "stable", endpoint: "http://127.0.0.1:8772/mcp", expected_runtime_version: "0.3.23" },
    controlPath,
    statusPath,
    pollIntervalMs: 100,
  });
  await loop.initialize();
  assert.equal(m.activeGenerationId, "stable");

  await writeFile(controlPath, JSON.stringify({
    schema_version: 1,
    revision: 2,
    desired_active: "candidate",
    generations: [
      { generation: "stable", endpoint: "http://127.0.0.1:8772/mcp", expected_runtime_version: "0.3.23" },
      { generation: "candidate", endpoint: "http://127.0.0.1:8773/mcp", expected_runtime_version: "0.3.26" },
    ],
    observation: { checks: 1, interval_ms: 0 },
  }));
  const applied = await loop.tick();
  assert.equal(applied.outcome, "activated");
  assert.equal(m.activeGenerationId, "candidate");
  const saved = JSON.parse(await readFile(statusPath, "utf8"));
  assert.equal(saved.processed_revision, 2);
  assert.equal(saved.manager.active_generation, "candidate");
  loop.close();
});
