import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { EPOCH3_CONTRACT } from "../dist/contracts/epoch3.js";
import { RUNTIME_EXECUTION_CONTRACT } from "../dist/contracts/runtime.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";
import { OAuthStoreDO } from "../dist/oauth-store-do.js";

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

function plannerHarness() {
  const h = makeWorkerHarness();
  const oauthStorage = new Storage();
  const oauth = new OAuthStoreDO({ storage: oauthStorage }, {});
  const connectorId = "conn_planner_test_1";
  const identity = { client_id: "planner-client", connector_id: connectorId, grant_generation: 1 };
  const tuple = { device_id: "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ", endpoint_ref: "be_endpoint",
    provider: "chatgpt", account_ref: "br_account", space_ref: null, session_ref: "br_session", observation_generation: 1 };
  oauthStorage.map.set(`connector:${connectorId}`, { ...identity, status: "active", principal_type: "connector" });
  oauthStorage.map.set("grant:planner-client", { client_id: identity.client_id, status: "active",
    webchat_control: [{ ...identity, ...tuple }] });
  h.env.OAUTH_STORE_DO = namespace({ async fetch(request) {
    if (new URL(request.url).pathname === "/internal/oauth/access/verify") {
      const { token } = await request.json();
      const auth = token === "planner" || token === "worker" ? identity
        : token === "wrong-generation" ? { ...identity, grant_generation: 2 }
        : token === "wrong-connector" ? { ...identity, connector_id: "conn_other_planner" }
        : token === "wrong-client" ? { ...identity, client_id: "other-client" } : null;
      return Response.json(auth ? { ok: true, ...auth } : { ok: false });
    }
    return oauth.fetch(request);
  } });
  const call = async (method, params, token = "planner") => publicFleetCall(h.env, token, 1, `herdr_mcp.${method}`, params);
  const owner = async (action, requestId, token = "legacy-secret", workstation = "w1") => {
    const response = await worker.fetch(new Request("https://edge.example/connectors/planner-control", {
      method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${token}`, "x-herdr-workstation": workstation },
      body: JSON.stringify({ action, ...(requestId ? { request_id: requestId } : {}) }),
    }), h.env);
    return { response, result: await response.json() };
  };
  return { ...h, oauthStorage, identity, tuple, call, owner };
}

test("owner-approved possession capability controls fleet without authorizing sibling OAuth conversations", async () => {
  const h = plannerHarness();
  const denied = async (method, params, token = "worker") => {
    assert.equal((await h.call(method, params, token)).result.code, "fleet_control_authorization_required");
  };
  await denied("work_chain.create", { idempotency_key: "ordinary" });
  for (const key of ["device_id", "endpoint_ref", "provider", "account_ref"]) {
    assert.equal((await h.call("planner_control.request", { ...h.tuple, [key]: "different" })).result.ok, false);
  }
  assert.equal((await h.call("planner_control.request", { ...h.tuple, connector_id: h.identity.connector_id })).result.code, "invalid_params");
  const requested = (await h.call("planner_control.request", h.tuple)).result;
  assert.equal(requested.ok, true);
  const requestId = requested.request.request_id;
  const claim = { request_id: requestId, claim_secret: requested.claim_secret };
  assert.equal((await h.call("planner_control.claim", claim)).result.ok, false, "pending cannot claim");
  await denied("work_chain.create", { idempotency_key: "pending", _controller_capability: requested.claim_secret });
  for (const token of ["planner", "dev-operator-secret"]) {
    assert.equal((await h.owner("approve", requestId, token)).response.status, 401, "Connector and operator cannot self-approve");
  }
  assert.equal((await h.owner("approve", requestId, "legacy-secret", "other-device")).response.status, 401);
  const approved = await h.owner("approve", requestId);
  assert.equal(approved.result.ok, true);
  const listing = await h.owner("list");
  assert.equal(listing.result.requests[0].session_ref, h.tuple.session_ref);
  assert.doesNotMatch(JSON.stringify([approved.result, listing.result]), /digest|claim_secret|controller_capability/);
  assert.equal((await h.call("planner_control.claim", { ...claim, claim_secret: "wrong" })).result.ok, false);
  for (const token of ["wrong-client", "wrong-connector", "wrong-generation"]) {
    assert.equal((await h.call("planner_control.claim", claim, token)).result.ok, false);
  }
  const claimed = (await h.call("planner_control.claim", claim)).result;
  assert.equal(claimed.ok, true);
  assert.equal(claimed.can_force_takeover, false);
  assert.equal((await h.call("planner_control.claim", claim)).result.ok, false, "claim is one-time");
  const cap = { _controller_capability: claimed.controller_capability };
  assert.doesNotMatch(JSON.stringify((await h.owner("list")).result), /digest|claim_secret|controller_capability/);
  await denied("work_chain.create", { idempotency_key: "wrong-capability", _controller_capability: "wrong" }, "planner");
  for (const token of ["wrong-client", "wrong-connector", "wrong-generation"]) {
    await denied("work_chain.create", { idempotency_key: token, ...cap }, token);
  }
  const created = (await h.call("work_chain.create", { idempotency_key: "controller-create", ...cap })).result;
  assert.equal(created.ok, true);
  assert.match(created.chain.creator_principal, /^controller:/);
  const acquire = { work_chain_id: created.chain.work_chain_id, expected_chain_revision: 1, idempotency_key: "acquire" };
  await denied("planner_lease.acquire", acquire);
  const acquired = (await h.call("planner_lease.acquire", { ...acquire, ...cap })).result;
  assert.equal(acquired.ok, true);
  const lease = { work_chain_id: created.chain.work_chain_id, expected_chain_revision: acquired.chain.revision,
    expected_lease_generation: 1, idempotency_key: "renew" };
  await denied("work_chain.create", { idempotency_key: "sibling" });
  await denied("planner_lease.renew", lease);
  await denied("planner_lease.takeover", { ...lease, reason: "worker" });
  assert.equal((await h.call("planner_lease.takeover", { ...lease, reason: "controller", ...cap })).result.code, "planner_lease_takeover_forbidden");
  assert.equal((await h.call("device.pair", { ...cap })).result.code, "invalid_params");
  assert.equal((await h.call("pane.list", { ...cap })).result.code, "invalid_params");
  const tools = await worker.fetch(mcpRequest("planner", 2, "tools/list", {}), h.env);
  assert.deepEqual((await tools.json()).result.tools, EPOCH3_CONTRACT.tools);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.contract_epoch, 2);
  assert.equal(RUNTIME_EXECUTION_CONTRACT.tools.length, 18);
  assert.equal(tools.headers.get("mcp-session-id"), null);
  const records = JSON.stringify([...h.oauthStorage.map]);
  const fleetRecords = JSON.stringify([...h.storage.map]);
  for (const secret of [requested.claim_secret, claimed.controller_capability]) {
    assert.equal(records.includes(secret), false);
    assert.equal(fleetRecords.includes(secret), false);
  }
  assert.equal(fleetRecords.includes("_controller_capability"), false);
  const revoked = await h.owner("revoke", requestId);
  assert.equal(revoked.result.ok, true);
  assert.doesNotMatch(JSON.stringify(revoked.result), /digest|claim_secret|controller_capability/);
  await denied("planner_lease.renew", { ...lease, ...cap }, "planner");
  assert.equal(h.forwarded.length, 0);
});

test("planner capability rechecks expiry, generation, revocation and exact WebChat grant on every use", async () => {
  const h = plannerHarness();
  const requested = (await h.call("planner_control.request", h.tuple)).result;
  const id = requested.request.request_id;
  await h.owner("approve", id);
  const claimed = (await h.call("planner_control.claim", { request_id: id, claim_secret: requested.claim_secret })).result;
  const params = { idempotency_key: "revalidate", _controller_capability: claimed.controller_capability };
  const key = `connector:${h.identity.connector_id}`;
  const connector = h.oauthStorage.map.get(key);
  h.oauthStorage.map.set(key, { ...connector, grant_generation: 2 });
  assert.equal((await h.call("work_chain.create", params)).result.code, "fleet_control_authorization_required");
  h.oauthStorage.map.set(key, { ...connector, status: "revoked" });
  assert.equal((await h.call("work_chain.create", params)).result.ok, false);
  h.oauthStorage.map.set(key, connector);
  const grant = h.oauthStorage.map.get("grant:planner-client");
  h.oauthStorage.map.set("grant:planner-client", { ...grant, webchat_control: [] });
  assert.equal((await h.call("work_chain.create", params)).result.ok, false);
  h.oauthStorage.map.set("grant:planner-client", { ...grant, status: "revoked" });
  assert.equal((await h.call("work_chain.create", params)).result.ok, false);
  h.oauthStorage.map.set("grant:planner-client", grant);
  h.oauthStorage.map.get(`planner-control:${id}`).expires_at_ms = Date.now() - 1;
  assert.equal((await h.call("work_chain.create", params)).result.ok, false);
  const pending = (await h.call("planner_control.request", h.tuple)).result;
  h.oauthStorage.map.get(`planner-control:${pending.request.request_id}`).expires_at_ms = Date.now() - 1;
  assert.equal((await h.owner("approve", pending.request.request_id)).result.ok, false);
});

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
