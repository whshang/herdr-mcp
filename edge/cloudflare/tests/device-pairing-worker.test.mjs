import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { buildLinkAuthProtocol } from "../dist/auth.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";

// Transaction-capable fake: concurrent transactions are serialized like a real
// Durable Object storage transaction, so the race test exercises the same
// atomicity guarantees as production.
class FakeStorage {
  constructor() {
    this.map = new Map();
    this.writeCount = 0;
    this._queue = Promise.resolve();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.writeCount += 1; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writeCount += 1; return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
  async getAlarm() { return null; }
  async setAlarm() { throw new Error("pairing flow must not schedule alarms"); }
  async deleteAlarm() {}
}

function namespace(stub) {
  return { idFromName: () => "devices-v1", get: () => stub };
}

function post(path, body, authorization) {
  const headers = { "content-type": "application/json" };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example${path}`, { method: "POST", headers, body: JSON.stringify(body) });
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

function makeEnv(extra = {}) {
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
      ...extra,
    },
  };
}

async function pair(env, name) {
  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name }, "owner-secret"), env);
  assert.equal(create.status, 200);
  assert.equal(create.headers.get("cache-control"), "no-store");
  const session = await create.json();
  const consume = await worker.fetch(post("/devices/pairings/consume", { pairing_id: session.pairing_id, code: session.code }), env);
  assert.equal(consume.status, 200);
  assert.equal(consume.headers.get("cache-control"), "no-store");
  return consume.json();
}

test("pairing creation requires owner auth and returns one-time material with worker origin metadata", async () => {
  const { env, storage } = makeEnv();
  const denied = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }), env);
  assert.equal(denied.status, 401);
  assert.equal((await denied.json()).code, "pairing_admin_required");

  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name: "mac-a" }, "owner-secret"), env);
  assert.equal(create.status, 200);
  const body = await create.json();
  assert.match(body.pairing_id, /^pair_[0-9a-f]{64}$/);
  assert.match(body.code, /^[0-9]{6}$/);
  assert.equal(typeof body.expires_at_ms, "number");
  assert.equal(body.worker_origin, "https://edge.example");
  assert.equal(body.device_secret, undefined, "no final device secret exists before consumption");

  const snapshot = JSON.stringify([...storage.map]);
  assert.equal(snapshot.includes(body.pairing_id), false, "raw pairing_id must never be stored");
  assert.equal(snapshot.includes(body.code), false, "raw code must never be stored");
});

test("pairing consume is unauthenticated, single-use, and returns the device secret exactly once", async () => {
  const { env, storage } = makeEnv();
  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name: "once-only" }, "owner-secret"), env);
  const session = await create.json();
  const consumeBody = { pairing_id: session.pairing_id, code: session.code };

  const first = await worker.fetch(post("/devices/pairings/consume", consumeBody), env);
  assert.equal(first.status, 200);
  const credential = await first.json();
  assert.match(credential.device_id, /^dev_[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.equal(credential.workstation_id, credential.device_id, "device_id must be canonical workstation identity");
  assert.match(credential.credential_id, /^cred_[0-9a-f]{32}$/);
  assert.match(credential.device_secret, /^devsec_[0-9a-f]{64}$/);
  assert.equal(JSON.stringify([...storage.map]).includes(credential.device_secret), false);

  const replay = await worker.fetch(post("/devices/pairings/consume", consumeBody), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "pairing_rejected");
});

test("joined device credentials cannot create pairings; only the owner workstation link may", async () => {
  const { env } = makeEnv();
  const ownerCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60, name: "joined-from-owner" },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(ownerCreate.status, 200);

  const consume = await (await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"), env)).json();
  const joined = await worker.fetch(post("/devices/pairings/consume", { pairing_id: consume.pairing_id, code: consume.code }), env);
  const credential = await joined.json();

  const memberCreate = await worker.fetch(postAsWorkstation(
    "/devices/pairings",
    { ttl_seconds: 60 },
    credential.workstation_id,
    credential.device_secret,
  ), env);
  assert.equal(memberCreate.status, 401);
  assert.equal((await memberCreate.json()).code, "pairing_admin_required");
});

test("device credentials are isolated: credential A never authenticates or routes as device B", async () => {
  const { env, forwarded } = makeEnv();
  const a = await pair(env, "iso-a");
  const b = await pair(env, "iso-b");

  const okA = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(okA.status, 200, "paired credential authenticates its own workstation");

  const impersonateB = await worker.fetch(wsRequest(b.workstation_id, a.device_secret), env);
  assert.equal(impersonateB.status, 401);
  assert.equal(forwarded.length, 1, "credential A must never reach workstation B");

  const legacyOnNewDevice = await worker.fetch(wsRequest(a.workstation_id, "legacy-secret"), env);
  assert.equal(legacyOnNewDevice.status, 401);
  assert.equal(forwarded.length, 1, "legacy global secret cannot authenticate a paired device");
});

test("a device may revoke only itself with its exact credential", async () => {
  const { env, forwarded } = makeEnv();
  const a = await pair(env, "self-revoke-a");
  const b = await pair(env, "self-revoke-b");

  const impersonatedRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: b.workstation_id }),
  }), env);
  assert.equal(impersonatedRevoke.status, 401);

  const selfRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
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

  const paired = await pair(env, "revoked-mac");
  const recordResponse = await registry.fetch(new Request(`https://registry.internal/internal/devices/${paired.device_id}`));
  const recordBody = await recordResponse.json();
  const now = Date.now();
  const revoked = { ...recordBody.device, authorization: "revoked", revoked_at_ms: now, updated_at_ms: now };
  const put = await registry.fetch(new Request(`https://registry.internal/internal/devices/${paired.device_id}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(revoked),
  }));
  assert.equal(put.status, 200);

  const reconnect = await worker.fetch(wsRequest(paired.workstation_id, paired.device_secret), env);
  assert.equal(reconnect.status, 401);
  assert.equal(forwarded.length, 1, "revoked device must not reach its WorkstationDO");
});

test("pairing minted by one Worker deployment fails closed against another", async () => {
  const { registry } = makeEnv();
  const stub = {
    async fetch() { return new Response(JSON.stringify({ ok: true }), { status: 200 }); },
  };
  const envA = makeEnv({ EDGE_PROJECT: "worker-alpha", LINK_SHARED_SECRET: undefined }).env;
  const envB = makeEnv({ EDGE_PROJECT: "worker-beta", LINK_SHARED_SECRET: undefined }).env;
  envA.DEVICE_REGISTRY_DO = namespace(registry);
  envB.DEVICE_REGISTRY_DO = namespace(registry);
  void stub;

  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60 }, "owner-secret"), envA);
  assert.equal(create.status, 200);
  const session = await create.json();

  const crossWorker = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), envB);
  assert.equal(crossWorker.status, 401);
  assert.equal((await crossWorker.json()).code, "pairing_rejected");

  const sameWorker = await worker.fetch(post(
    "/devices/pairings/consume",
    { pairing_id: session.pairing_id, code: session.code },
  ), envA);
  assert.equal(sameWorker.status, 200);
});
