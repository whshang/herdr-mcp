import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { EPOCH3_CONTRACT } from "../dist/contracts/epoch3.js";
import { RUNTIME_EXECUTION_CONTRACT } from "../dist/contracts/runtime.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";

const req = (id, method, params = {}) => ({ jsonrpc: "2.0", id, method, params });

class Storage {
  constructor() { this.map = new Map(); this._queue = Promise.resolve(); }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(key) { return this.map.delete(key); }
  async list({ prefix } = {}) { return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix))); }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
}

const namespace = (stub) => ({ idFromName: () => "singleton", get: () => stub });

function mcpRequest(token, id, method, params) {
  return new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify(req(id, method, params)),
  });
}

function deps(over = {}) {
  const forwarded = [];
  return {
    forwarded,
    value: {
      limits: makeLimits(),
      logger: { warn() {} },
      getStub: (workstationId) => ({ workstationId }),
      resolveDevice: async () => ({ ok: true, device_id: "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ", workstation_id: "w1" }),
      fleetControl: over.fleetControl,
      forward: async (_stub, body) => {
        forwarded.push(JSON.parse(body));
        return new Response(JSON.stringify({ status: "ok", completion: { status: "ok", result: { served: true } } }));
      },
      now: () => 1000,
    },
  };
}

function makeWorkerHarness() {
  const storage = new Storage();
  const forwarded = [];
  const workstationStub = {
    async fetch(request) {
      forwarded.push(new URL(request.url).pathname);
      return new Response(JSON.stringify({ status: "ok", completion: { status: "ok", result: { served: true } } }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    },
  };
  const registry = new DeviceRegistryDO(
    { storage },
    { LINK_SHARED_SECRET: "legacy-secret", WORKSTATION_DO: namespace(workstationStub) },
  );
  const oauthStub = {
    async fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === "/internal/oauth/access/verify") {
        const body = await request.json();
        if (body.token === "oauth-legacy") return new Response(JSON.stringify({ ok: true, client_id: "legacy-client" }));
        if (body.token === "oauth-ordinary") return new Response(JSON.stringify({ ok: true, client_id: "ordinary-client" }));
        return new Response(JSON.stringify({ ok: false, code: "invalid_token" }), { status: 401 });
      }
      if (url.pathname === "/internal/oauth/grant/get") {
        return new Response(JSON.stringify({
          ok: true,
          record: { client_id: "ordinary-client", status: "active", scope: "mcp", can_approve_connectors: false },
        }));
      }
      return new Response(JSON.stringify({ ok: false, code: "not_found" }), { status: 404 });
    },
  };
  const env = {
    DEVICE_REGISTRY_DO: namespace(registry),
    WORKSTATION_DO: namespace(workstationStub),
    OAUTH_STORE_DO: namespace(oauthStub),
    DEV_MCP_BEARER_SECRET: "dev-operator-secret",
    STATIC_MCP_BEARER_SECRET: "static-operator-secret",
    LINK_SHARED_SECRET: "legacy-secret",
    DEFAULT_WORKSTATION_ID: "w1",
  };
  return { storage, forwarded, registry, env };
}

async function publicFleetCall(env, token, id, method, params) {
  const response = await worker.fetch(mcpRequest(token, id, "tools/call", {
    name: "herdr_call",
    arguments: { method, params: JSON.stringify(params) },
  }), env);
  const body = await response.json();
  return { response, body, result: body.result?.structuredContent };
}

test("fleet private method schemas are discoverable without changing the base public tool contract", async () => {
  const d = deps();
  const r = await handleMcp(req(1, "tools/call", {
    name: "herdr_methods",
    arguments: { query: "planner_lease" },
  }), "w1", d.value);
  assert.equal(r.body.result.structuredContent.ok, true);
  assert.equal(r.body.result.structuredContent.source, "edge_fleet_control_v1");
  const acquire = r.body.result.structuredContent.methods.find((entry) => entry.method === "herdr_mcp.planner_lease.acquire");
  assert.ok(acquire);
  assert.ok(acquire.params.required.includes("expected_chain_revision"));
  assert.ok(acquire.params.required.includes("idempotency_key"));

  const chains = await handleMcp(req(100, "tools/call", {
    name: "herdr_methods",
    arguments: { query: "work_chain" },
  }), "w1", d.value);
  const createChain = chains.body.result.structuredContent.methods.find((entry) => entry.method === "herdr_mcp.work_chain.create");
  assert.equal("portable_evidence_refs" in createChain.params.properties, false);

  const lanes = await handleMcp(req(101, "tools/call", {
    name: "herdr_methods",
    arguments: { query: "execution_lane" },
  }), "w1", d.value);
  const laneCreate = lanes.body.result.structuredContent.methods.find((entry) => entry.method === "herdr_mcp.execution_lane.create");
  const laneUpdate = lanes.body.result.structuredContent.methods.find((entry) => entry.method === "herdr_mcp.execution_lane.update");
  assert.equal("validation_refs" in laneCreate.params.properties, false);
  assert.equal("validation_refs" in laneUpdate.params.properties, false);

  const listed = await handleMcp(req(2, "tools/list", {}), "w1", d.value);
  assert.deepEqual(listed.body.result.tools, EPOCH3_CONTRACT.tools);
  assert.equal(EPOCH3_CONTRACT.contract_epoch, 3);
  assert.equal(listed.body.result.tools.length, 19);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_epoch, 2);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.tools.length, 18);
});

test("herdr_call routes a fleet mutation to the edge-local authority and never forwards it to a workstation", async () => {
  const seen = [];
  const d = deps({
    fleetControl: async (method, params) => {
      seen.push({ method, params });
      return { ok: true, chain: { work_chain_id: "wc_test", revision: 1 } };
    },
  });
  const r = await handleMcp(req(3, "tools/call", {
    name: "herdr_call",
    arguments: {
      method: "herdr_mcp.work_chain.create",
      params: JSON.stringify({ idempotency_key: "create-1" }),
    },
  }), "w1", d.value);
  assert.deepEqual(seen, [{ method: "herdr_mcp.work_chain.create", params: { idempotency_key: "create-1" } }]);
  assert.equal(r.body.result.structuredContent.ok, true);
  assert.equal(r.body.result.structuredContent.chain.work_chain_id, "wc_test");
  assert.equal(d.forwarded.length, 0);
});

test("fleet methods reject public device routing metadata and unsupported runtimes fail explicitly", async () => {
  const d = deps({ fleetControl: async () => ({ ok: true }) });
  const routed = await handleMcp(req(4, "tools/call", {
    name: "herdr_call",
    arguments: {
      method: "herdr_mcp.work_chain.create",
      params: "{\"idempotency_key\":\"create-2\"}",
      device: "some-device",
    },
  }), "w1", d.value);
  assert.equal(routed.body.result.structuredContent.code, "device_selector_not_allowed");

  const missing = deps();
  const unsupported = await handleMcp(req(5, "tools/call", {
    name: "herdr_call",
    arguments: { method: "herdr_mcp.work_chain.create", params: "{\"idempotency_key\":\"create-3\"}" },
  }), "w1", missing.value);
  assert.equal(unsupported.body.result.structuredContent.code, "fleet_control_unsupported");
});

test("public /mcp derives operator identity and rejects caller-supplied identity or capability fields", async () => {
  const h = makeWorkerHarness();
  for (const field of ["principal", "client_id", "controller_id", "conversation_id", "holder_principal", "can_control_fleet", "can_force_takeover"]) {
    const injected = await publicFleetCall(h.env, "dev-operator-secret", 10, "herdr_mcp.work_chain.create", {
      idempotency_key: `inject-${field}`,
      [field]: field === "can_control_fleet" || field === "can_force_takeover" ? true : "attacker",
    });
    assert.equal(injected.result.code, "invalid_params");
    assert.equal(injected.result.field, field);
  }

  const created = await publicFleetCall(h.env, "dev-operator-secret", 11, "herdr_mcp.work_chain.create", {
    idempotency_key: "public-create",
  });
  assert.equal(created.result.ok, true);
  assert.equal(created.result.chain.creator_principal, "operator:dev_bearer");

  const sameCredential = await publicFleetCall(h.env, "dev-operator-secret", 12, "herdr_mcp.work_chain.create", {
    idempotency_key: "public-create-same-credential",
  });
  assert.equal(sameCredential.result.ok, true);
  assert.equal(sameCredential.result.chain.creator_principal, "operator:dev_bearer");

  const publicInternalBypass = await worker.fetch(new Request("https://edge.example/internal/devices/fleet-control", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer dev-operator-secret" },
    body: JSON.stringify({
      method: "herdr_mcp.work_chain.create",
      params: { idempotency_key: "bypass" },
      authority: { principal: "operator:forged", can_force_takeover: true },
    }),
  }), h.env);
  assert.notEqual(publicInternalBypass.status, 200);
  assert.equal(h.forwarded.length, 0);
});

test("legacy and ordinary OAuth tokens remain fail-closed for Fleet Control", async () => {
  const h = makeWorkerHarness();
  for (const [id, token] of [[20, "oauth-legacy"], [21, "oauth-ordinary"]]) {
    const denied = await publicFleetCall(h.env, token, id, "herdr_mcp.work_chain.create", {
      idempotency_key: `oauth-denied-${id}`,
    });
    assert.equal(denied.response.status, 200);
    assert.equal(denied.result.code, "fleet_control_authorization_required");
  }
  assert.equal(h.forwarded.length, 0);
});

test("public private-method vertical slice reaches durable authority, force-takeover fences old planner, and never forwards", async () => {
  const h = makeWorkerHarness();
  const deviceId = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
  await h.storage.put(`device:${deviceId}`, {
    device_id: deviceId,
    workstation_id: deviceId,
    name: "worker-a",
    authorization: "active",
    scheduling: "enabled",
    credential_id: null,
    enrolled_at_ms: 1,
    updated_at_ms: 1,
    revoked_at_ms: null,
  });

  const created = await publicFleetCall(h.env, "dev-operator-secret", 30, "herdr_mcp.work_chain.create", {
    idempotency_key: "vertical-create",
  });
  assert.equal(created.result.ok, true);
  const chainId = created.result.chain.work_chain_id;

  const acquired = await publicFleetCall(h.env, "dev-operator-secret", 31, "herdr_mcp.planner_lease.acquire", {
    work_chain_id: chainId,
    expected_chain_revision: 1,
    ttl_ms: 30000,
    idempotency_key: "vertical-acquire",
  });
  assert.equal(acquired.result.planner_lease.generation, 1);

  const lane = await publicFleetCall(h.env, "dev-operator-secret", 32, "herdr_mcp.execution_lane.create", {
    work_chain_id: chainId,
    expected_chain_revision: acquired.result.chain.revision,
    expected_lease_generation: 1,
    idempotency_key: "vertical-lane",
    device_id: deviceId,
    repo_id: "github.com/whshang/herdr-mcp",
    base_commit: "e9281b488e093f522020db2a2c6100d92b69499f",
    branch_ref: "feat/vertical-lane",
    status: "active",
  });
  assert.equal(lane.result.ok, true);

  const takeover = await publicFleetCall(h.env, "static-operator-secret", 33, "herdr_mcp.planner_lease.takeover", {
    work_chain_id: chainId,
    expected_chain_revision: lane.result.chain.revision,
    expected_lease_generation: 1,
    ttl_ms: 30000,
    reason: "operator recovery after controller handoff",
    idempotency_key: "vertical-takeover",
  });
  assert.equal(takeover.result.ok, true);
  assert.equal(takeover.result.planner_lease.generation, 2);
  assert.equal(takeover.result.takeover.previous_holder_principal, "operator:dev_bearer");
  assert.equal(takeover.result.takeover.new_holder_principal, "operator:static_bearer");

  const stale = await publicFleetCall(h.env, "dev-operator-secret", 34, "herdr_mcp.execution_lane.update", {
    work_chain_id: chainId,
    expected_chain_revision: takeover.result.chain.revision,
    expected_lease_generation: 1,
    lane_id: lane.result.lane.lane_id,
    expected_lane_generation: 1,
    status: "blocked",
    idempotency_key: "vertical-stale-update",
  });
  assert.equal(stale.result.code, "stale_lease_generation");

  const inspected = await publicFleetCall(h.env, "static-operator-secret", 35, "herdr_mcp.work_chain.inspect", {
    work_chain_id: chainId,
  });
  assert.equal(inspected.result.chain.planner_lease.generation, 2);
  assert.equal(inspected.result.chain.planner_lease.holder_principal, "operator:static_bearer");
  assert.equal(h.forwarded.length, 0);
});
