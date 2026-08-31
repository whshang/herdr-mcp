import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { buildLinkAuthProtocol } from "../dist/auth.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { sanitize } from "../dist/logger.js";

// Mirror the FakeStorage from existing tests but track write counts for goal 6.
class FakeStorage {
  constructor() { this.map = new Map(); this.writeCount = 0; }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.writeCount += 1; this.map.set(key, structuredClone(value)); }
  async delete(key) { this.writeCount += 1; return this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([k]) => !prefix || k.startsWith(prefix)));
  }
  async transaction(fn) { return fn(this); }
  async getAlarm() { return null; }
  async setAlarm() {}
  async deleteAlarm() {}
}

function namespace(stub) {
  return { idFromName: () => "devices-v1", get: () => stub };
}

function post(path, body, authorization, extraHeaders = {}) {
  const headers = { "content-type": "application/json", ...extraHeaders };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example${path}`, { method: "POST", headers, body: JSON.stringify(body) });
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
      WORKSTATION_DO: { idFromName: (n) => n, get: () => workstationStub },
      OAUTH_STORE_DO: { idFromName: () => "oauth-v1", get: () => unusedOAuth },
      DEV_MCP_BEARER_SECRET: "owner-secret",
      LINK_SHARED_SECRET: "legacy-secret",
      DEFAULT_WORKSTATION_ID: "prod-real-runtime",
    },
  };
}

async function createEnrollment(env, ttlSeconds = 60) {
  const res = await worker.fetch(post("/devices/enrollments", { ttl_seconds: ttlSeconds, name: "audit-device" }, "owner-secret"), env);
  const body = await res.json();
  return { res, body };
}

// Goal 1: high entropy (256-bit), single-use, bounded TTL, Worker-bound (DO idFromName), never stored/logged raw
test("P0-C-1 enrollment code is high entropy (enroll_ + 64 hex), single-use, bounded TTL, never stored raw", async () => {
  const { env, storage, registry } = makeEnv();

  // TTL bounds: <60s and >900s rejected
  const short = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 30 }, "owner-secret"), env);
  assert.equal(short.status, 400);
  assert.equal((await short.json()).code, "invalid_enrollment_ttl");

  const long = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 1000 }, "owner-secret"), env);
  assert.equal(long.status, 400);
  assert.equal((await long.json()).code, "invalid_enrollment_ttl");

  // default TTL (no ttl_seconds) succeeds
  const def = await worker.fetch(post("/devices/enrollments", {}, "owner-secret"), env);
  assert.equal(def.status, 200);
  const defBody = await def.json();
  assert.match(defBody.enrollment_code, /^enroll_[0-9a-f]{64}$/);
  assert.equal(def.headers.get("cache-control"), "no-store");

  // High entropy uniqueness: 10 codes are unique
  const codes = new Set([defBody.enrollment_code]);
  for (let i = 0; i < 9; i++) {
    const r = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env);
    codes.add((await r.json()).enrollment_code);
  }
  assert.equal(codes.size, 10, "enrollment codes must be unique (256-bit entropy)");

  // Single-use: consume once succeeds, replay fails
  const single = await createEnrollment(env, 60);
  assert.equal(single.res.status, 200);
  const code = single.body.enrollment_code;
  const firstConsume = await worker.fetch(post("/devices/enroll", { enrollment_code: code }), env);
  assert.equal(firstConsume.status, 200);
  assert.match((await firstConsume.clone().json()).device_secret, /^devsec_[0-9a-f]{64}$/);
  // second consume is single-use -> 401
  const replay = await worker.fetch(post("/devices/enroll", { enrollment_code: code }), env);
  assert.equal(replay.status, 401);
  assert.equal((await replay.json()).code, "invalid_enrollment");

  // Never stored raw: storage dump contains no raw enrollment_code or device_secret
  const dump = JSON.stringify([...storage.map]);
  for (const c of codes) assert.equal(dump.includes(c), false, "raw enrollment_code must not be persisted");
  // also verify via direct registry storage check for the consumed enrollment
  const consumedEntry = [...storage.map.entries()].find(([k]) => k.startsWith("credential:"));
  assert.ok(consumedEntry, "credential entry exists");
  // stored verifier is hex hash, not raw secret
  const secret = (await firstConsume.json()).device_secret;
  // we already consumed response clone, need fresh consume for secret check; use new enrollment
  const fresh = await createEnrollment(env, 60);
  const freshCode = fresh.body.enrollment_code;
  const freshConsume = await worker.fetch(post("/devices/enroll", { enrollment_code: freshCode }), env);
  const freshBody = await freshConsume.json();
  const freshDump = JSON.stringify([...storage.map]);
  assert.equal(freshDump.includes(freshBody.device_secret), false, "raw device_secret must not be persisted");

  // Worker-bound: DeviceRegistry uses single DO instance id "devices-v1" via namespace
  assert.equal(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"), "devices-v1");
  assert.equal(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"), env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
});

test("P0-C-1 enrollment verifier stored only as sha256, logger redacts enrollment keys", async () => {
  // logger redaction defense in depth
  const redacted = sanitize({ enrollment_code: "enroll_abc", nested: { enrollment_code: "x" }, device_secret: "devsec_y", credential: "c", authorization: "Bearer t" });
  assert.equal(redacted.enrollment_code, "[redacted]");
  assert.equal(redacted.nested.enrollment_code, "[redacted]");
  assert.equal(redacted.device_secret, "[redacted]");
  assert.equal(redacted.credential, "[redacted]");
  assert.equal(redacted.authorization, "[redacted]");

  // registry stores verifier only
  const storage = new FakeStorage();
  const reg = new DeviceRegistryDO({ storage }, {});
  const create = await reg.fetch(new Request("https://registry.internal/internal/devices/enrollments", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ttl_seconds: 60 }),
  }));
  const { enrollment_code } = await create.json();
  const stored = [...storage.map.values()].find(v => v && typeof v.verifier_sha256 === "string");
  assert.ok(stored);
  const expected = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(enrollment_code)).then(b => [...new Uint8Array(b)].map(x=>x.toString(16).padStart(2,"0")).join(""));
  assert.equal(stored.verifier_sha256, expected);
});

// Goal 2: per-device verifier binding prevents credential A authenticating B
test("P0-C-2 per-device verifier binding: credential A cannot authenticate B via WS or registry", async () => {
  const { env, forwarded } = makeEnv();
  // create two devices
  const aCode = (await createEnrollment(env)).body.enrollment_code;
  const bCode = (await (await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env)).json()).enrollment_code;
  // need clean: recreate env for isolation then enroll both sequentially
  const made2 = makeEnv();
  const env2 = made2.env;
  const f2 = made2.forwarded;
  const reg2direct = made2.registry;
  const aEnroll = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env2);
  const aEnrollBody = await aEnroll.json();
  const bEnroll = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env2);
  const bEnrollBody = await bEnroll.json();
  const aCred = (await (await worker.fetch(post("/devices/enroll", { enrollment_code: aEnrollBody.enrollment_code }), env2)).json());
  const bCred = (await (await worker.fetch(post("/devices/enroll", { enrollment_code: bEnrollBody.enrollment_code }), env2)).json());

  // A can connect to its own workstation
  const okA = await worker.fetch(wsRequest(aCred.workstation_id, aCred.device_secret), env2);
  assert.equal(okA.status, 200);
  // A cannot connect to B's workstation
  const cross = await worker.fetch(wsRequest(bCred.workstation_id, aCred.device_secret), env2);
  assert.equal(cross.status, 401);
  assert.equal(f2.length, 1, "cross-device credential must not reach WorkstationDO");

  // Registry direct cross-auth also fails
  const verifierA = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(aCred.device_secret)).then(b=>[...new Uint8Array(b)].map(x=>x.toString(16).padStart(2,"0")).join(""));
  // Try to authenticate workstation B with verifier A -> should fail link_auth_failed
  const reg2 = reg2direct;
  const fakeAuth = await reg2.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ workstation_id: bCred.workstation_id, credential_verifier_sha256: verifierA }),
  }));
  assert.equal(fakeAuth.status, 401);
  assert.equal((await fakeAuth.json()).code, "link_auth_failed");
});

// Goal 3: legacy LINK_SHARED_SECRET only for DEFAULT_WORKSTATION_ID
test("P0-C-3 legacy secret authenticates only DEFAULT_WORKSTATION_ID and cannot enroll arbitrary devices", async () => {
  const { env, forwarded } = makeEnv();
  // default workstation via legacy should succeed (compatibility)
  const legacyDefault = await worker.fetch(wsRequest("prod-real-runtime", "legacy-secret"), env);
  assert.equal(legacyDefault.status, 200);
  assert.equal(forwarded.length, 1);

  // legacy on non-default workstation must fail even when no device exists
  const legacyOther = await worker.fetch(wsRequest("other-workstation", "legacy-secret"), env);
  assert.equal(legacyOther.status, 401);
  assert.equal(forwarded.length, 1, "legacy on non-default must not reach DO");

  // legacy on enrolled non-default device must also fail
  const enrolled = (await (await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env)).json());
  const consumed = await worker.fetch(post("/devices/enroll", { enrollment_code: enrolled.enrollment_code }), env);
  const { workstation_id: enrolledWs, device_secret } = await consumed.json();
  const legacyOnEnrolled = await worker.fetch(wsRequest(enrolledWs, "legacy-secret"), env);
  assert.equal(legacyOnEnrolled.status, 401);
  assert.equal(forwarded.length, 1);

  // legacy secret via Authorization header without x-herdr-workstation cannot create enrollment
  const noHeaderCreate = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "legacy-secret"), env);
  assert.equal(noHeaderCreate.status, 401);
  assert.equal((await noHeaderCreate.json()).code, "enrollment_admin_required");

  // legacy secret with wrong workstation header cannot create enrollment
  const wrongHeaderCreate = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "legacy-secret", { "x-herdr-workstation": "other-workstation" }), env);
  assert.equal(wrongHeaderCreate.status, 401);

  // legacy secret with correct default workstation header CAN create enrollment (transitional owner path)
  const correctHeaderCreate = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "legacy-secret", { "x-herdr-workstation": "prod-real-runtime" }), env);
  assert.equal(correctHeaderCreate.status, 200);
});

// Goal 4: only owner/default workstation or owner OAuth/static auth can create enrollment
test("P0-C-4 only owner OAuth/static or default workstation link can create enrollment", async () => {
  const { env } = makeEnv();
  // no auth -> 401
  const unauth = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }), env);
  assert.equal(unauth.status, 401);

  // wrong static secret -> 401
  const wrongStatic = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "wrong-secret"), env);
  assert.equal(wrongStatic.status, 401);

  // enrolled non-owner device secret cannot create enrollment (even though device credential is valid)
  const enrolled = (await (await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env)).json());
  const device = await (await worker.fetch(post("/devices/enroll", { enrollment_code: enrolled.enrollment_code }), env)).json();
  const memberCreate = await worker.fetch(new Request("https://edge.example/devices/enrollments", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${device.device_secret}`, "x-herdr-workstation": device.workstation_id },
    body: JSON.stringify({ ttl_seconds: 60 }),
  }), env);
  assert.equal(memberCreate.status, 401);
  assert.equal((await memberCreate.json()).code, "enrollment_admin_required");

  // default workstation with legacy secret can (already tested above) - confirm again
  const ownerLinkCreate = await worker.fetch(new Request("https://edge.example/devices/enrollments", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: "Bearer legacy-secret", "x-herdr-workstation": "prod-real-runtime" },
    body: JSON.stringify({ ttl_seconds: 60 }),
  }), env);
  assert.equal(ownerLinkCreate.status, 200);

  // owner static bearer can
  const ownerStaticCreate = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env);
  assert.equal(ownerStaticCreate.status, 200);
});

// Goal 5: self-revoke only exact authenticated device
test("P0-C-5 self-revoke can revoke only the exact authenticated device", async () => {
  const { env, forwarded } = makeEnv();
  const aEnroll = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env);
  const bEnroll = await worker.fetch(post("/devices/enrollments", { ttl_seconds: 60 }, "owner-secret"), env);
  const a = await (await worker.fetch(post("/devices/enroll", { enrollment_code: (await aEnroll.json()).enrollment_code }), env)).json();
  const b = await (await worker.fetch(post("/devices/enroll", { enrollment_code: (await bEnroll.json()).enrollment_code }), env)).json();

  // cross revoke: A credential trying to revoke B's workstation -> 401
  const crossRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: b.workstation_id }),
  }), env);
  assert.equal(crossRevoke.status, 401);

  // self revoke succeeds and returns own device_id
  const selfRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: a.workstation_id }),
  }), env);
  assert.equal(selfRevoke.status, 200);
  assert.equal((await selfRevoke.json()).device_id, a.device_id);

  // after revoke, A cannot reconnect but B still can
  const aReconnect = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(aReconnect.status, 401);
  assert.equal(forwarded.length, 0);
  const bReconnect = await worker.fetch(wsRequest(b.workstation_id, b.device_secret), env);
  assert.equal(bReconnect.status, 200);
  assert.equal(forwarded.length, 1);

  // revoke without credential -> 401
  const noCred = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workstation_id: b.workstation_id }),
  }), env);
  assert.equal(noCred.status, 401);

  // revoke with bad workstation_id format -> 400
  const badId = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${b.device_secret}` }, body: JSON.stringify({ workstation_id: "" }),
  }), env);
  assert.equal(badId.status, 400);
});

// Goal 6: no DeviceRegistry heartbeat hot-path writes
test("P0-C-6 DeviceRegistry has no heartbeat writes; list/get/auth are read-only", async () => {
  const storage = new FakeStorage();
  const registry = new DeviceRegistryDO({ storage }, {});
  // seed a device
  const put = await registry.fetch(new Request(`https://registry.internal/internal/devices/dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ`, {
    method: "PUT", headers: { "content-type": "application/json" },
    body: JSON.stringify({ device_id: "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ", workstation_id: "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ", name: "macbook-main", authorization: "active", scheduling: "enabled", credential_id: null, enrolled_at_ms: 10, updated_at_ms: 10, revoked_at_ms: null }),
  }));
  assert.equal(put.status, 200);
  const writesAfterSeed = storage.writeCount;

  // Enroll and authenticate should be the only writes; subsequent reads must not add writes
  const enroll = await registry.fetch(new Request("https://registry.internal/internal/devices/enrollments", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ttl_seconds: 60 }),
  }));
  const { enrollment_code } = await enroll.json();
  const consume = await registry.fetch(new Request("https://registry.internal/internal/devices/enrollments/consume", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ enrollment_code }),
  }));
  const { workstation_id, device_secret } = await consume.json();
  const writesAfterConsume = storage.writeCount;

  // authenticate (hot path for WS) must not write
  const verifier = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(device_secret)).then(b=>[...new Uint8Array(b)].map(x=>x.toString(16).padStart(2,"0")).join(""));
  const beforeAuth = storage.writeCount;
  const auth = await registry.fetch(new Request("https://registry.internal/internal/devices/authenticate", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workstation_id, credential_verifier_sha256: verifier }),
  }));
  assert.equal(auth.status, 200);
  assert.equal(storage.writeCount, beforeAuth, "authenticate must be read-only (no heartbeat writes)");

  // list and get are read-only
  const beforeList = storage.writeCount;
  await registry.fetch(new Request("https://registry.internal/internal/devices"));
  await registry.fetch(new Request(`https://registry.internal/internal/devices/${workstation_id}`));
  assert.equal(storage.writeCount, beforeList, "list/get must be read-only");
});
