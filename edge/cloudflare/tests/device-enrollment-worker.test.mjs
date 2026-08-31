import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { buildLinkAuthProtocol } from "../dist/auth.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";

class FakeStorage {
  constructor() { this.map = new Map(); }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, structuredClone(value)); }
  async delete(key) { return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  async transaction(fn) { return fn(this); }
}

function namespace(stub) {
  return {
    idFromName: (name) => name,
    get: () => stub,
  };
}

function post(path, body, authorization) {
  const headers = { "content-type": "application/json" };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example${path}`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
}

function postAsWorkstation(path, body, workstationId, secret) {
  return new Request(`https://edge.example${path}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${secret}`,
      "x-herdr-workstation": workstationId,
    },
    body: JSON.stringify(body),
  });
}

function wsRequest(workstationId, secret) {
  return new Request(`https://edge.example/ws/${encodeURIComponent(workstationId)}`, {
    method: "GET",
    headers: {
      Upgrade: "websocket",
      "sec-websocket-protocol": `herdr-link.v1, ${buildLinkAuthProtocol(secret)}`,
    },
  });
}

function makeEnv() {
  const storage = new FakeStorage();
  const registry = new DeviceRegistryDO({ storage }, {});
  const forwarded = [];
  const workstationStub = {
    async fetch(request) {
      forwarded.push(new URL(request.url).pathname);
      return new Response(JSON.stringify({ ok: true }), { status: 200 });
    },
  };
  const unusedOAuth = { fetch: async () => new Response(JSON.stringify({ ok: false }), { status: 401 }) };
  return {
    storage,
    forwarded,
    registry,
    env: {
      DEVICE_REGISTRY_DO: namespace(registry),
      WORKSTATION_DO: namespace(workstationStub),
      OAUTH_STORE_DO: namespace(unusedOAuth),
      DEV_MCP_BEARER_SECRET: "owner-secret",
      LINK_SHARED_SECRET: "legacy-secret",
      DEFAULT_WORKSTATION_ID: "prod-real-runtime",
    },
  };
}

async function enroll(env, name) {
  const create = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60, name }, "owner-secret"), env);
  assert.equal(create.status, 200);
  assert.equal(create.headers.get("cache-control"), "no-store");
  const enrollment = await create.json();
  const consume = await worker.fetch(post("/devices/enroll", { enrollment_code: enrollment.enrollment_code }), env);
  assert.equal(consume.status, 200);
  assert.equal(consume.headers.get("cache-control"), "no-store");
  return consume.json();
}

test("worker enrollment requires owner auth, returns a device secret once, and binds WS auth to that device", async () => {
  const { env, storage, forwarded } = makeEnv();
  const denied = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }), env);
  assert.equal(denied.status, 401);

  const a = await enroll(env, "mac-a");
  const b = await enroll(env, "mac-b");
  assert.equal(a.workstation_id, a.device_id);
  assert.equal(b.workstation_id, b.device_id);
  assert.equal(JSON.stringify([...storage.map]).includes(a.device_secret), false);
  assert.equal(JSON.stringify([...storage.map]).includes(b.device_secret), false);

  const aConnect = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(aConnect.status, 200);
  assert.equal(forwarded.length, 1);

  const impersonateB = await worker.fetch(wsRequest(b.workstation_id, a.device_secret), env);
  assert.equal(impersonateB.status, 401);
  assert.equal(forwarded.length, 1, "credential A must never reach workstation B");

  const legacyOnNewDevice = await worker.fetch(wsRequest(a.workstation_id, "legacy-secret"), env);
  assert.equal(legacyOnNewDevice.status, 401);
  assert.equal(forwarded.length, 1, "legacy global secret cannot authenticate a formally enrolled device");
});

test("only the default owner workstation may use Link credentials to create another enrollment", async () => {
  const { env } = makeEnv();
  const ownerCreate = await worker.fetch(postAsWorkstation(
    "/devices/enrollments",
    { ttl_seconds: 60, name: "joined-from-owner" },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(ownerCreate.status, 200);

  const joined = await enroll(env, "member-device");
  const memberCreate = await worker.fetch(postAsWorkstation(
    "/devices/enrollments",
    { ttl_seconds: 60 },
    joined.workstation_id,
    joined.device_secret,
  ), env);
  assert.equal(memberCreate.status, 401);
  assert.equal((await memberCreate.json()).code, "enrollment_admin_required");
});

test("a device may revoke only itself with its exact credential", async () => {
  const { env, forwarded } = makeEnv();
  const a = await enroll(env, "self-revoke-a");
  const b = await enroll(env, "self-revoke-b");

  const impersonatedRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${a.device_secret}`,
    },
    body: JSON.stringify({ workstation_id: b.workstation_id }),
  }), env);
  assert.equal(impersonatedRevoke.status, 401);

  const selfRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${a.device_secret}`,
    },
    body: JSON.stringify({ workstation_id: a.workstation_id }),
  }), env);
  assert.equal(selfRevoke.status, 200);
  assert.equal((await selfRevoke.json()).device_id, a.device_id);

  const reconnect = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(reconnect.status, 401);
  assert.equal(forwarded.length, 0);
});

test("legacy shared secret is compatibility-only for the default workstation and revoke blocks reconnect", async () => {
  const { env, registry, forwarded } = makeEnv();
  const legacy = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret"), env);
  assert.equal(legacy.status, 200);
  assert.equal(forwarded.length, 1);

  const enrolled = await enroll(env, "revoked-mac");
  const recordResponse = await registry.fetch(new Request(`https://registry.internal/internal/devices/${enrolled.device_id}`));
  const recordBody = await recordResponse.json();
  const now = Date.now();
  const revoked = { ...recordBody.device, authorization: "revoked", revoked_at_ms: now, updated_at_ms: now };
  const put = await registry.fetch(new Request(`https://registry.internal/internal/devices/${enrolled.device_id}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(revoked),
  }));
  assert.equal(put.status, 200);

  const reconnect = await worker.fetch(wsRequest(enrolled.workstation_id, enrolled.device_secret), env);
  assert.equal(reconnect.status, 401);
  assert.equal(forwarded.length, 1, "revoked device must not reach its WorkstationDO");
});
