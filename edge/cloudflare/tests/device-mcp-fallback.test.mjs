import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";

// #1094 P0-b: /mcp accepts an enrolled device's own credential as a
// `device:<id>` principal — fleet-control capable, never fleet admin, never
// force takeover.

class Storage {
  constructor() { this.map = new Map(); this._queue = Promise.resolve(); }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(keyOrKeys) {
    if (Array.isArray(keyOrKeys)) {
      let deleted = 0;
      for (const key of keyOrKeys) if (this.map.delete(key)) deleted += 1;
      return deleted;
    }
    return this.map.delete(keyOrKeys);
  }
  async list({ prefix } = {}) { return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix))); }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
}

const namespace = (stub) => ({ idFromName: () => "singleton", get: () => stub });

function makeHarness() {
  const storage = new Storage();
  const forwarded = [];
  const workstationStub = {
    async fetch() {
      forwarded.push(true);
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
    async fetch() {
      return new Response(JSON.stringify({ ok: false, code: "invalid_token" }), { status: 401 });
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

async function enrollDevice(registry) {
  const create = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ttl_seconds: 60, worker_context: "herdr-edge@test" }),
  }));
  const created = await create.json();
  const consume = await registry.fetch(new Request("https://registry.internal/internal/devices/pairings/consume", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ pairing_id: created.pairing_id, code: created.code, worker_context: "herdr-edge@test" }),
  }));
  return consume.json(); // { device_id, credential_id, device_secret, workstation_id }
}

function deviceCall(env, device, id, name, args = {}, includeWorkstation = true) {
  const headers = {
    "content-type": "application/json",
    authorization: `Bearer ${device.device_secret}`,
  };
  if (includeWorkstation) headers["x-herdr-workstation"] = device.workstation_id;
  return worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id, method: "tools/call", params: { name, arguments: args } }),
  }), env);
}

async function deviceCallJson(env, device, id, name, args = {}, includeWorkstation = true) {
  const response = await deviceCall(env, device, id, name, args, includeWorkstation);
  const body = await response.json();
  return { response, body, result: body.result?.structuredContent };
}

async function deviceFleetCall(env, device, id, method, params = {}) {
  return deviceCallJson(env, device, id, "herdr_call", { method, params: JSON.stringify(params) });
}

test("user reaches /mcp with an enrolled device credential | Given a paired device | When the device presents its own credential | Then ordinary tools and fleet control work as a bound device:<id> principal", async () => {
  const h = makeHarness();
  const device = await enrollDevice(h.registry);

  // Ordinary edge-local read tool: bound to its own device only.
  const devices = await deviceCallJson(h.env, device, 1, "herdr_devices");
  assert.equal(devices.result.ok, true);
  assert.equal(devices.result.scope, "bound_device");
  assert.equal(devices.result.bound_device_id, device.device_id);
  assert.deepEqual(devices.result.devices.map((d) => d.device_id), [device.device_id]);

  // Fleet control: dedicated device authority, never force takeover.
  const created = await deviceFleetCall(h.env, device, 2, "herdr_mcp.work_chain.create", { idempotency_key: "device-create" });
  assert.equal(created.result.ok, true);
  assert.equal(created.result.chain.creator_principal, `device:${device.device_id}`);

  const inspected = await deviceFleetCall(h.env, device, 3, "herdr_mcp.work_chain.inspect", {
    work_chain_id: created.result.chain.work_chain_id,
  });
  assert.equal(inspected.result.ok, true);

  const takeover = await deviceFleetCall(h.env, device, 4, "herdr_mcp.planner_lease.takeover", {
    work_chain_id: created.result.chain.work_chain_id,
    reason: "device",
    idempotency_key: "device-takeover",
  });
  assert.equal(takeover.result.code, "planner_lease_takeover_forbidden");
});

test("user cannot mint or revoke fleet identity with a device credential | Given a paired device | When edge_local management methods are invoked | Then all are denied before dispatch", async () => {
  const h = makeHarness();
  const device = await enrollDevice(h.registry);
  for (const method of [
    "herdr_mcp.device.pair",
    "herdr_mcp.device.revoke",
    "herdr_mcp.connector.approve",
    "herdr_mcp.connector.revoke",
  ]) {
    const { response, body } = await deviceCallJson(h.env, device, 10, "herdr_call", {
      method,
      params: method === "herdr_mcp.device.pair" ? JSON.stringify({ name: "other-device" }) : JSON.stringify({}),
    });
    assert.equal(response.status, 403, method);
    assert.equal(body.code, "device_principal_denied", method);
  }
});

test("user cannot use a device credential without workstation identity or after revocation | Given a paired then revoked device | When /mcp is called | Then requests are unauthorized", async () => {
  const h = makeHarness();
  const device = await enrollDevice(h.registry);

  // Missing x-herdr-workstation: no device principal can be minted.
  const noWs = await deviceCallJson(h.env, device, 20, "herdr_devices", {}, false);
  assert.equal(noWs.response.status, 401);

  // Revoked devices are refused at auth (revocation propagates centrally).
  await h.registry.fetch(new Request(`https://registry.internal/internal/devices/${device.device_id}/revoke`, { method: "POST" }));
  const revoked = await deviceCallJson(h.env, device, 21, "herdr_devices");
  assert.equal(revoked.response.status, 401);
});

test("operator principals keep unbound fleet visibility | Given a paired device | When an operator credential calls herdr_devices | Then the full fleet is returned", async () => {
  const h = makeHarness();
  const device = await enrollDevice(h.registry);
  const response = await worker.fetch(new Request("https://edge.example/mcp", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer dev-operator-secret" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 30, method: "tools/call", params: { name: "herdr_devices", arguments: {} } }),
  }), h.env);
  const body = await response.json();
  const result = body.result?.structuredContent;
  assert.equal(result.ok, true);
  assert.equal(result.scope, undefined);
  assert.equal(result.devices.length, 1);
  assert.equal(result.devices[0].device_id, device.device_id);
});
